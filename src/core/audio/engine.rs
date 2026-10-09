//! Device discovery, the single `cpal` output stream, and the `"audio-engine"`
//! thread that owns it.

use std::sync::Arc;
use std::thread::{Builder, park};
use std::time::{Duration, Instant, SystemTime};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, Error, ErrorKind, FromSample, I24, SampleFormat, SizedSample, Stream,
    StreamConfig, SupportedBufferSize, default_host,
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::journal::{self, JournalWriter, MAX_JOURNAL_SOURCES, OverrunRecord};
use super::load::micros;
use super::{
    AudioClock, AudioLoad, AudioSource, DESIRED_BUFFER_FRAMES, MAX_FRAMES, RenderCtx, WorkerPool,
    enter_render_thread, max_dsp_threads,
};
use crate::core::config::MAX_TRACKS;

/// Capacity of the ring carrying late-added sources (only the macOS instrument
/// mixer in practice) to the running mixer.
const ADD_SOURCE_RING_CAPACITY: usize = 4;

/// How many worker threads the engine's [`WorkerPool`] gets.
///
/// Three caps, all of them real:
///
/// - **`max_dsp_threads() - 1`** — one is left for the callback thread, which is
///   a runner too. That ceiling is architecture-dependent (SMT siblings are
///   worth using when there is more work than cores; efficiency cores never
///   are), and it is only a ceiling: `WorkerPool::for_each`'s `runners` argument
///   is what stops a quiet block from engaging anyone. See
///   [`topology`](super::topology).
/// - **`MAX_TRACKS - 1`** — the only source with per-item-independent work is
///   the macOS instrument mixer, one item per instrument track, so no more helpers can
///   ever be used.
/// - **0 off macOS** — nothing to parallelise there, so the pool stays empty
///   rather than parking threads that would never run.
fn worker_thread_count() -> usize {
    if !cfg!(target_os = "macos") {
        return 0;
    }
    max_dsp_threads().saturating_sub(1).min(MAX_TRACKS - 1)
}

/// Handle to the running engine, kept by `main`. Most accessors are only used
/// where a second source is added at runtime — currently macOS (the plugin
/// host); `load` is read on every platform.
pub(crate) struct EngineHandle {
    /// The device sample rate the stream opened at — the plugin host builds
    /// its plugin processors against this.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) sample_rate: f64,
    /// How much of each block's deadline the render is actually using. Shared
    /// with the audio callback; handed to `Display` for the header readout.
    pub(crate) load: Arc<AudioLoad>,
    /// Ring for handing a new [`AudioSource`] to the running mixer (the
    /// instrument mixer, on macOS).
    add_source_tx: Producer<Box<dyn AudioSource>>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl EngineHandle {
    /// Hands an additional source to the running mixer. Returns `Err` if the
    /// engine thread is gone or the ring is full.
    pub(crate) fn add_source(&mut self, source: Box<dyn AudioSource>) -> Result<(), String> {
        self.add_source_tx
            .push(source)
            .map_err(|_| "audio engine unavailable (add-source ring full)".to_string())
    }
}

/// Namespace for the engine start-up entry point; the running engine lives in
/// its thread, reached through [`EngineHandle`].
pub(crate) struct AudioEngine;

impl AudioEngine {
    /// Discovers the default output device, spawns the `"audio-engine"` thread
    /// owning the `cpal` output stream, and mixes the sources `build_sources`
    /// returns (it runs on the engine thread once the device sample rate is
    /// known). Returns `Err` — and leaves the app silent — if no output device
    /// is available.
    pub(crate) fn start<F>(build_sources: F) -> Result<EngineHandle, String>
    where
        F: FnOnce(f64) -> Vec<Box<dyn AudioSource>> + Send + 'static,
    {
        let host = default_host();
        let device = host
            .default_output_device()
            .ok_or("no default output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;

        let sample_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let buffer_size = pick_buffer_size(supported.buffer_size(), DESIRED_BUFFER_FRAMES);
        let mut config: StreamConfig = supported.into();
        config.buffer_size = buffer_size;
        let sample_rate_f64 = f64::from(sample_rate);

        let (add_source_tx, add_source_rx) = RingBuffer::new(ADD_SOURCE_RING_CAPACITY);
        let load = Arc::new(AudioLoad::new());
        let callback_load = load.clone();

        Builder::new()
            .name("audio-engine".to_string())
            .spawn(move || {
                // `pick_buffer_size`'s clamp against the device's reported range
                // doesn't guarantee the driver accepts every value in it — probe
                // with a throwaway no-op stream before committing.
                if matches!(config.buffer_size, BufferSize::Fixed(_))
                    && !probe_buffer_size_supported(&device, &config, sample_format)
                {
                    dprintln!(
                        "audio engine: fixed buffer size rejected by the device; using the device default"
                    );
                    config.buffer_size = BufferSize::Default;
                }

                let mixer = Mixer::new(
                    build_sources(sample_rate_f64),
                    sample_rate_f64,
                    add_source_rx,
                    callback_load,
                );
                let stream = match build_stream(&device, &config, sample_format, channels, mixer) {
                    Ok(stream) => stream,
                    Err(_e) => {
                        dprintln!("audio engine disabled: {_e}");
                        return;
                    }
                };
                if let Err(_e) = stream.play() {
                    dprintln!("audio engine: stream.play() failed: {_e}");
                    return;
                }
                // The stream lives exactly as long as this thread — held here;
                // the stream's own callback thread does the rendering.
                loop {
                    park();
                }
            })
            .expect("failed to spawn audio-engine thread");

        dprintln!("audio engine: started @ {sample_rate} Hz, {channels} channel(s)");
        dprintln!(
            "audio engine: overrun journal at {}",
            journal::journal_path().display()
        );

        Ok(EngineHandle {
            sample_rate: sample_rate_f64,
            load,
            add_source_tx,
        })
    }
}

/// The audio-callback state: the source list, the shared clock, and the mix
/// scratch.
struct Mixer {
    /// Every registered [`AudioSource`], summed each callback.
    sources: Vec<Box<dyn AudioSource>>,
    /// Consumer end of the add-source ring.
    add_source_rx: Consumer<Box<dyn AudioSource>>,
    /// Wall-clock → output-frame mapping shared by all sources.
    clock: AudioClock,
    /// Device sample rate — with the callback's frame count, this is the
    /// real-time budget each callback has to beat. See [`AudioLoad`].
    sample_rate: f64,
    /// Deadline-utilisation meter, shared with the header readout.
    load: Arc<AudioLoad>,
    /// Handed to every source through [`RenderCtx`], so a source with
    /// independent per-item work can spread it across cores instead of running
    /// the whole block on this one thread.
    pool: WorkerPool,
    /// Per-channel sum scratch, pre-reserved to `MAX_FRAMES`.
    mix: [Vec<f32>; 2],
    /// Monotonic sample counter — the frame index of the next callback's first
    /// sample.
    steady: u64,
    /// This callback's render time per source, parallel to `sources` and
    /// pre-reserved to its capacity so a late-added source never reallocates
    /// here. Summed across a callback's sub-blocks; zeroed at the top of each.
    source_elapsed: Vec<Duration>,
    /// Where an overrun's post-mortem goes. See [`journal`].
    journal: JournalWriter,
}

impl Mixer {
    /// Builds the callback state, sizing `sources` so runtime additions never
    /// reallocate on the audio thread.
    fn new(
        initial: Vec<Box<dyn AudioSource>>,
        sample_rate: f64,
        add_source_rx: Consumer<Box<dyn AudioSource>>,
        load: Arc<AudioLoad>,
    ) -> Self {
        let capacity = initial.len() + ADD_SOURCE_RING_CAPACITY;
        let mut sources = Vec::with_capacity(capacity);
        sources.extend(initial);
        Self {
            sources,
            add_source_rx,
            clock: AudioClock::new(sample_rate),
            sample_rate,
            load,
            pool: WorkerPool::new(worker_thread_count()),
            mix: [
                Vec::with_capacity(MAX_FRAMES),
                Vec::with_capacity(MAX_FRAMES),
            ],
            steady: 0,
            source_elapsed: Vec::with_capacity(capacity),
            journal: journal::start(),
        }
    }

    /// One audio callback: observe the clock, drain the add-source ring, sum
    /// every source into the scratch, hard-clip to ±1.0, and convert to the
    /// device format. Records the deadline utilisation.
    fn render<T>(&mut self, data: &mut [T], device_channels: usize)
    where
        T: SizedSample + FromSample<f32>,
    {
        // Sampled before anything else so the load figure covers the whole
        // callback, not just the source loop.
        let started = Instant::now();

        // Both halves are per-thread and this thread belongs to `cpal`, so
        // there is no "start of thread" hook to do it in — and at a few dozen
        // cycles against a block of millions, per-callback is not worth
        // optimising away. See `thread_role`.
        enter_render_thread();

        for s in data.iter_mut() {
            *s = T::EQUILIBRIUM;
        }

        while let Ok(source) = self.add_source_rx.pop() {
            if self.sources.len() < self.sources.capacity() {
                self.sources.push(source);
            } else {
                dprintln!("audio engine: source list full, dropping added source");
            }
        }

        let ch = device_channels.max(1);
        let total_frames = data.len() / ch;
        if total_frames == 0 {
            return;
        }

        let now = Instant::now();
        self.clock.observe(now, self.steady);

        self.source_elapsed.clear();
        self.source_elapsed
            .resize(self.sources.len(), Duration::ZERO);

        let mut offset = 0;
        while offset < total_frames {
            let frames = (total_frames - offset).min(MAX_FRAMES);
            let first_frame = self.steady;

            for m in &mut self.mix {
                m.clear();
                m.resize(frames, 0.0);
            }

            let ctx = RenderCtx {
                frames,
                first_frame,
                clock: &self.clock,
                pool: &self.pool,
                load: &self.load,
            };
            for (source, elapsed) in self.sources.iter_mut().zip(&mut self.source_elapsed) {
                let source_started = Instant::now();
                source.render_into(&mut self.mix, &ctx);
                *elapsed += source_started.elapsed();
            }

            for f in 0..frames {
                let base = (offset + f) * ch;
                for (i, out) in data[base..base + ch].iter_mut().enumerate() {
                    let v = match i {
                        0 => self.mix[0][f],
                        1 => self.mix[1][f],
                        // Mono device: channel 0 only. >2-channel device: L/R on
                        // 1-2, silence on 3+.
                        _ => 0.0,
                    };
                    // Safety net, not a limiter: the sum of the click and every
                    // plugin can exceed full scale (wraps/folds on the integer
                    // formats), and a buggy plugin can emit NaN (which would
                    // corrupt the whole mix, click included).
                    let v = if v.is_finite() {
                        v.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    *out = T::from_sample(v);
                }
            }

            self.steady = self.steady.wrapping_add(frames as u64);
            offset += frames;
        }

        let elapsed = started.elapsed();
        if self.load.record(elapsed, total_frames, self.sample_rate) {
            self.journal
                .push(self.overrun_record(elapsed, total_frames));
        }
    }

    /// The journal's view of the block that just overran: whole-callback time,
    /// the per-source split just measured, and the per-track split the
    /// instrument mixer left in [`AudioLoad`]. Fixed-size and `Copy` — nothing
    /// here allocates.
    fn overrun_record(&self, elapsed: Duration, total_frames: usize) -> OverrunRecord {
        let mut sources = [("", 0u32); MAX_JOURNAL_SOURCES];
        let mut source_count = 0u8;
        for ((source, elapsed), slot) in self
            .sources
            .iter()
            .zip(&self.source_elapsed)
            .zip(sources.iter_mut())
        {
            *slot = (source.name(), micros(*elapsed));
            source_count += 1;
        }

        let tracks_us = std::array::from_fn(|track| self.load.track_last_us(track));

        OverrunRecord {
            at: SystemTime::now(),
            frames: total_frames as u32,
            sample_rate: self.sample_rate as f32,
            total_us: micros(elapsed),
            sources,
            source_count,
            tracks_us,
        }
    }
}

/// Expands `$body` with `$T` bound to the sample type for `$format`, or
/// evaluates `$unsupported` for a format the engine doesn't render. `F32` is
/// what CoreAudio's virtual format gives on macOS; the integer and `F64` arms
/// cover what `default_output_config` can hand back elsewhere (since cpal 0.18
/// it ranks `I32`/`I24` above `I16`). One list, so the probe and the real
/// stream can't disagree on which formats exist.
macro_rules! with_sample_type {
    ($format:expr, $T:ident => $body:expr, _ => $unsupported:expr) => {
        match $format {
            SampleFormat::F32 => {
                type $T = f32;
                $body
            }
            SampleFormat::F64 => {
                type $T = f64;
                $body
            }
            SampleFormat::I32 => {
                type $T = i32;
                $body
            }
            SampleFormat::I24 => {
                type $T = I24;
                $body
            }
            SampleFormat::I16 => {
                type $T = i16;
                $body
            }
            SampleFormat::U16 => {
                type $T = u16;
                $body
            }
            _ => $unsupported,
        }
    };
}

/// Builds the `cpal` output stream, dispatching on the device sample format.
fn build_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    mixer: Mixer,
) -> Result<Stream, Error> {
    with_sample_type!(sample_format, T => build_typed::<T>(device, config, channels, mixer), _ => {
        dprintln!("audio engine: unsupported sample format {sample_format:?}");
        Err(ErrorKind::UnsupportedConfig.into())
    })
}

/// Builds the stream for one concrete sample type `T`, moving `mixer` into the
/// data callback and its meter into the error callback, where the backend's
/// overload notifications (`ErrorKind::Xrun`) are latched rather than logged —
/// CoreAudio fires them on the real-time thread, and they are a count the UI
/// shows, not a fault.
fn build_typed<T>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    mut mixer: Mixer,
) -> Result<Stream, Error>
where
    T: SizedSample + FromSample<f32>,
{
    let load = mixer.load.clone();
    device.build_output_stream(
        *config,
        move |data: &mut [T], _| mixer.render(data, channels),
        move |e| {
            if e.kind() == ErrorKind::Xrun {
                load.record_xrun();
            } else {
                dprintln!("audio engine stream error: {e}");
            }
        },
        None,
    )
}

/// Tests whether `device` actually accepts `config` (a `Fixed` buffer size),
/// with a no-op stream that is built (not played) and dropped — enough to
/// surface `ErrorKind::UnsupportedConfig` on the backends this app targets.
fn probe_buffer_size_supported(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
) -> bool {
    with_sample_type!(sample_format, T => device
        .build_output_stream(*config, move |_data: &mut [T], _| {}, |_e| {}, None)
        .is_ok(), _ => false)
}

/// Resolves the requested callback buffer size against what the device reports.
/// A device that gives a concrete range gets `desired` clamped into it; one that
/// won't report a range keeps its own default.
fn pick_buffer_size(supported: &SupportedBufferSize, desired: u32) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(desired.clamp(*min, *max)),
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_count_leaves_a_core_for_the_callback_thread() {
        let workers = worker_thread_count();
        assert!(
            workers < MAX_TRACKS,
            "{workers} workers cannot all be used with {MAX_TRACKS} tracks"
        );
        if cfg!(target_os = "macos") {
            let threads = max_dsp_threads();
            assert!(
                workers < threads,
                "{workers} workers of a {threads}-thread ceiling leaves none for the callback"
            );
        } else {
            assert_eq!(workers, 0, "no source parallelises off macOS");
        }
    }

    #[test]
    fn buffer_size_within_range_is_requested_verbatim() {
        let got = pick_buffer_size(&SupportedBufferSize::Range { min: 16, max: 4096 }, 256);
        assert_eq!(got, BufferSize::Fixed(256));
    }

    #[test]
    fn buffer_size_is_clamped_to_the_device_range() {
        let lo = pick_buffer_size(
            &SupportedBufferSize::Range {
                min: 512,
                max: 4096,
            },
            256,
        );
        assert_eq!(lo, BufferSize::Fixed(512));
        let hi = pick_buffer_size(&SupportedBufferSize::Range { min: 16, max: 128 }, 256);
        assert_eq!(hi, BufferSize::Fixed(128));
    }

    #[test]
    fn unknown_range_falls_back_to_device_default() {
        let got = pick_buffer_size(&SupportedBufferSize::Unknown, 256);
        assert_eq!(got, BufferSize::Default);
    }
}
