//! Device discovery, the single `cpal` output stream, and the `"audio-engine"`
//! thread that owns it — and rebuilds it whenever the device changes under it
//! (`480-audio-settings.md` decision 5).
//!
//! The thread waits on a channel of restart requests. Each one drops the
//! stream, which hands the [`Mixer`] back ([`Returning`]), resolves the
//! device as it is now, rebuilds the [`WorkerPool`] so its workers join that
//! device's workgroup, and opens a new stream at the device's rate. A request
//! comes from the stream's error callback (the device gone, its rate
//! changed) or from the thread's own poll (the system default moved to
//! another device, or a device appeared while none was open).

use std::sync::Arc;
use std::thread::Builder;
use std::time::{Duration, Instant, SystemTime};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, DeviceId, Error, ErrorKind, FromSample, Host, I24, SampleFormat,
    SizedSample, Stream, StreamConfig, SupportedBufferSize, default_host,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use log::{debug, error, info, warn};
use rtrb::{Consumer, Producer, RingBuffer};

use super::journal::{self, JournalWriter, MAX_JOURNAL_SOURCES, OverrunRecord};
use super::load::micros;
use super::{
    AudioClock, AudioLoad, AudioSource, DESIRED_BUFFER_FRAMES, DeviceWorkgroup,
    FALLBACK_SAMPLE_RATE, MAX_FRAMES, RenderCtx, StreamStatus, WorkerPool, enter_render_thread,
    max_dsp_threads,
};
use crate::core::config::MAX_TRACKS;

/// Capacity of the ring carrying late-added sources (only the macOS instrument
/// mixer in practice) to the running mixer.
const ADD_SOURCE_RING_CAPACITY: usize = 4;

/// How often the engine thread looks at the system default output (to follow
/// it) and, with no stream open, whether a device has appeared.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long a restart waits for the dropped stream to hand the mixer back.
/// The hand-back is the stream's own teardown, so this only runs out if
/// CoreAudio hangs; the engine then stops for the session.
const MIXER_RETURN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the poll waits before trying again a device that failed to open.
/// A retry rebuilds the worker pool, so it isn't one to run every poll.
const FAILED_OPEN_RETRY: Duration = Duration::from_secs(5);

/// Restart requests that can queue while one runs. A device being plugged in
/// fires several; they are coalesced, so a full queue loses nothing.
const RESTART_QUEUE_CAPACITY: usize = 8;
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
    /// The sample rate the running stream opened at — the plugin host builds
    /// its plugin processors against it, and the audio export writes at it
    /// ([`FALLBACK_SAMPLE_RATE`] while no device is open) — and how many times
    /// the stream has restarted.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) status: Arc<StreamStatus>,
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
    /// Spawns the `"audio-engine"` thread mixing `sources`, opens the stream on
    /// the system default output, and returns once that first open has been
    /// tried, so [`EngineHandle::status`] has the device's. With no device the
    /// app starts silent, and the engine opens one when it appears.
    /// `notify` puts a line in the footer when an outside change restarts
    /// the stream (`480-audio-settings.md` decision 11).
    pub(crate) fn start<N>(sources: Vec<Box<dyn AudioSource>>, notify: N) -> EngineHandle
    where
        N: Fn(String) + Send + 'static,
    {
        let (add_source_tx, add_source_rx) = RingBuffer::new(ADD_SOURCE_RING_CAPACITY);
        let load = Arc::new(AudioLoad::new());
        let status = Arc::new(StreamStatus::new(FALLBACK_SAMPLE_RATE));
        let (mixer_tx, mixer_rx) = bounded(1);
        let (restart_tx, restart_rx) = bounded(RESTART_QUEUE_CAPACITY);
        // The mixer waits in its channel for the first open, as it does
        // between any two streams.
        mixer_tx
            .send(Mixer::new(
                sources,
                f64::from(FALLBACK_SAMPLE_RATE),
                add_source_rx,
                load.clone(),
            ))
            .ok();
        let mut thread = EngineThread {
            host: default_host(),
            running: None,
            opened: None,
            failed_at: None,
            generation: 0,
            restart_tx,
            restart_rx,
            mixer_tx,
            mixer_rx,
            status: status.clone(),
            load: load.clone(),
            notify: Box::new(notify),
        };

        let (ready_tx, ready_rx) = bounded(1);
        Builder::new()
            .name("audio-engine".to_string())
            .spawn(move || {
                let alive = thread.restart(RestartCause::Launch);
                ready_tx.send(()).ok();
                if alive {
                    thread.run();
                }
            })
            .expect("failed to spawn audio-engine thread");
        // A thread that died on the way leaves the sender dropped: go on.
        ready_rx.recv().ok();

        info!(
            "audio engine: overrun journal at {}",
            journal::journal_path().display()
        );

        EngineHandle {
            status,
            load,
            add_source_tx,
        }
    }
}

/// Why the engine rebuilds its stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartCause {
    /// The app is starting: the first open.
    Launch,
    /// The device the stream ran on went away (`ErrorKind::DeviceNotAvailable`).
    DeviceLost,
    /// The device's sample rate changed under the stream, in Audio MIDI Setup
    /// or by another app (`ErrorKind::StreamInvalidated`).
    RateChanged,
    /// The system default output moved to another device.
    DefaultChanged,
    /// No stream is open and a device now is.
    Retry,
}

impl RestartCause {
    /// The restart a stream error asks for, if any. cpal reports a device
    /// going away as `DeviceNotAvailable` and, for a stream opened on a
    /// specific device, a rate change as `StreamInvalidated`. `DeviceChanged`
    /// (a host that re-routed the stream itself) never comes from the
    /// CoreAudio stream the engine opens, but another host may send it, and it
    /// means what the poll noticing would.
    fn from_error(kind: ErrorKind) -> Option<Self> {
        match kind {
            ErrorKind::DeviceNotAvailable => Some(Self::DeviceLost),
            ErrorKind::StreamInvalidated => Some(Self::RateChanged),
            ErrorKind::DeviceChanged => Some(Self::DefaultChanged),
            _ => None,
        }
    }
}

/// What a stream opened on: the device's name and its rate. Kept through a
/// gap with no stream, so the next open can say what changed.
#[derive(Clone, Debug, PartialEq)]
struct Opened {
    /// The device's name, as macOS shows it.
    name: String,
    /// Its rate, Hz.
    sample_rate: u32,
}

/// The footer line for a restart: `previous` is the last stream that opened,
/// `now` the one this restart opened, if any. Nothing for the launch, or for
/// a retry that still found no device; a change in rate also says that the
/// plugins loaded before it are silent (`480-audio-settings.md` phase 1).
/// Pure, unit-tested.
fn restart_message(
    cause: RestartCause,
    previous: Option<&Opened>,
    now: Option<&Opened>,
) -> Option<String> {
    if cause == RestartCause::Launch {
        return None;
    }
    let lost = || match previous {
        Some(p) => format!("{} disconnected", p.name),
        None => "The audio output disconnected".to_string(),
    };
    let Some(now) = now else {
        return match cause {
            RestartCause::Launch | RestartCause::Retry => None,
            RestartCause::DeviceLost => Some(format!("{}; no audio output", lost())),
            RestartCause::RateChanged | RestartCause::DefaultChanged => {
                Some("No audio output".to_string())
            }
        };
    };
    let line = match cause {
        RestartCause::DeviceLost => format!("{}; using {}", lost(), now.name),
        RestartCause::RateChanged if previous.is_some_and(|p| p.name == now.name) => {
            format!("Device rate changed to {}", khz(now.sample_rate))
        }
        RestartCause::DefaultChanged => {
            format!("Audio switched to {} (System default changed)", now.name)
        }
        RestartCause::Launch | RestartCause::RateChanged | RestartCause::Retry => {
            format!("Audio switched to {}", now.name)
        }
    };
    let rate_moved = previous.is_some_and(|p| p.sample_rate != now.sample_rate);
    Some(if rate_moved {
        format!("{line}; plugins stay silent until Stev is restarted")
    } else {
        line
    })
}

/// `44100` as "44.1 kHz", `48000` as "48 kHz". Pure, unit-tested.
fn khz(hz: u32) -> String {
    let khz = f64::from(hz) / 1000.0;
    format!("{} kHz", (khz * 10.0).round() / 10.0)
}

/// The open stream and the device it runs on.
struct Running {
    /// Held for its life. Dropping it stops the callback and hands the mixer
    /// back on `EngineThread::mixer_tx`.
    _stream: Stream,
    /// The device's persistent ID, compared against the system default on
    /// every poll; `None` if the device wouldn't say, and then not followed.
    device_id: Option<DeviceId>,
}

/// The `"audio-engine"` thread's state: the open stream, if any, and the
/// channels the mixer and the restart requests travel on.
struct EngineThread {
    /// The audio host devices are looked up in.
    host: Host,
    /// The running stream, if one is open.
    running: Option<Running>,
    /// The last stream that opened, kept while none is.
    opened: Option<Opened>,
    /// When the last open failed with a device there, so the poll doesn't
    /// rebuild the pool every half second for a device that won't open.
    failed_at: Option<Instant>,
    /// Counts restarts. Each stream's error callback carries the count it
    /// opened under, so an error from a stream already replaced is ignored.
    generation: u64,
    /// Handed to each stream's error callback.
    restart_tx: Sender<(u64, RestartCause)>,
    /// Restart requests from the error callbacks.
    restart_rx: Receiver<(u64, RestartCause)>,
    /// Where a dropped stream's callback (or a stream that failed to build)
    /// sends the mixer back. Holds at most the one mixer, so a send never
    /// blocks.
    mixer_tx: Sender<Mixer>,
    /// The mixer, whenever no stream holds it.
    mixer_rx: Receiver<Mixer>,
    /// The running rate and the restart count, for the plugin host and the
    /// export: a restart ends a running export (`480-audio-settings.md`
    /// decision 10).
    status: Arc<StreamStatus>,
    /// The engine's meter, which the stream's error callback counts into.
    load: Arc<AudioLoad>,
    /// Puts a line in the footer.
    notify: Box<dyn Fn(String) + Send>,
}

impl EngineThread {
    /// The thread's loop: act on restart requests, coalesced, and between
    /// them poll the system default. Returns only if a restart lost the mixer.
    fn run(mut self) {
        loop {
            let cause = match self.restart_rx.recv_timeout(DEVICE_POLL_INTERVAL) {
                Ok(first) => {
                    // Only the last of a pile-up is acted on, and only from
                    // the stream running now.
                    let generation = self.generation;
                    std::iter::once(first)
                        .chain(self.restart_rx.try_iter())
                        .filter(|(g, _)| *g == generation)
                        .map(|(_, cause)| cause)
                        .last()
                }
                Err(RecvTimeoutError::Timeout) => self.poll(),
                Err(RecvTimeoutError::Disconnected) => return,
            };
            if let Some(cause) = cause
                && !self.restart(cause)
            {
                return;
            }
        }
    }

    /// The restart the device list calls for: a device to open when none is
    /// (a device that just failed to, only every [`FAILED_OPEN_RETRY`]), or
    /// the system default having moved off the running stream's device.
    fn poll(&self) -> Option<RestartCause> {
        let default = self.host.default_output_device();
        let Some(running) = &self.running else {
            let due = self
                .failed_at
                .is_none_or(|at| at.elapsed() >= FAILED_OPEN_RETRY);
            return (default.is_some() && due).then_some(RestartCause::Retry);
        };
        let default_id = default.and_then(|d| d.id().ok())?;
        let moved = running
            .device_id
            .as_ref()
            .is_some_and(|id| *id != default_id);
        moved.then_some(RestartCause::DefaultChanged)
    }

    /// Drops the stream, takes the mixer back and opens a new stream on the
    /// device as it is now, then says what happened. Returns `false` if the
    /// mixer never came back, which leaves the app silent for the session.
    fn restart(&mut self, cause: RestartCause) -> bool {
        if self.running.take().is_some() {
            self.status.count_restart();
        }
        self.generation += 1;
        let Ok(mixer) = self.mixer_rx.recv_timeout(MIXER_RETURN_TIMEOUT) else {
            error!("audio engine: the stream never handed the mixer back; audio is off");
            return false;
        };

        match self.open(mixer) {
            Ok((running, opened)) => {
                info!(
                    "audio engine: {} @ {} Hz ({cause:?})",
                    opened.name, opened.sample_rate
                );
                self.notify_restart(cause, Some(&opened));
                self.running = Some(running);
                self.opened = Some(opened);
                self.failed_at = None;
            }
            Err(e) => {
                if cause == RestartCause::Retry {
                    debug!("audio engine: still no stream: {e}");
                } else {
                    error!("audio engine: no stream ({cause:?}): {e}");
                }
                self.notify_restart(cause, None);
                self.failed_at = Some(Instant::now());
            }
        }
        true
    }

    /// Puts the footer line for a restart that opened `now`, if any.
    fn notify_restart(&self, cause: RestartCause, now: Option<&Opened>) {
        if let Some(line) = restart_message(cause, self.opened.as_ref(), now) {
            (self.notify)(line);
        }
    }

    /// Opens a stream on the system default output at its own rate, with
    /// `mixer` moved into the callback. On any failure the mixer goes back on
    /// `mixer_tx`, through [`Returning`]'s drop.
    fn open(&mut self, mixer: Mixer) -> Result<(Running, Opened), String> {
        let mut held = Returning {
            mixer: Some(mixer),
            home: self.mixer_tx.clone(),
        };
        let (device, device_id) = default_device(&self.host).ok_or("no output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;

        let sample_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let buffer_size = pick_buffer_size(supported.buffer_size(), DESIRED_BUFFER_FRAMES);
        let mut config: StreamConfig = supported.into();
        config.buffer_size = buffer_size;
        // `pick_buffer_size`'s clamp against the device's reported range
        // doesn't guarantee the driver accepts every value in it — probe with
        // a throwaway no-op stream before committing.
        if matches!(config.buffer_size, BufferSize::Fixed(_))
            && !probe_buffer_size_supported(&device, &config, sample_format)
        {
            info!(
                "audio engine: fixed buffer size rejected by the device; using the device default"
            );
            config.buffer_size = BufferSize::Default;
        }
        // A device that picks its own buffer has it measured on the first
        // callback instead.
        let schedule_delay = match config.buffer_size {
            BufferSize::Fixed(frames) => frames,
            BufferSize::Default => 0,
        };

        let workgroup = Arc::new(DeviceWorkgroup::for_device(&device));
        held.mixer()
            .restart(f64::from(sample_rate), workgroup, schedule_delay);
        self.status.set_sample_rate(sample_rate);

        let generation = self.generation;
        let restart_tx = self.restart_tx.clone();
        let load = self.load.clone();
        let on_error = move |e: Error| {
            // Overloads come in on the real-time thread, and they are a count
            // the UI shows, not a fault: latched, never logged.
            if e.kind() == ErrorKind::Xrun {
                load.record_xrun();
                return;
            }
            // Every other error is counted, for the audio export to notice
            // the output going wrong under it.
            load.record_stream_error();
            match RestartCause::from_error(e.kind()) {
                Some(cause) => {
                    warn!("audio engine: {e}; restarting the stream");
                    restart_tx.try_send((generation, cause)).ok();
                }
                None => error!("audio engine stream error: {e}"),
            }
        };
        let stream = build_stream(&device, &config, sample_format, channels, held, on_error)
            .map_err(|e| format!("could not build the stream: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("stream.play() failed: {e}"))?;

        let opened = Opened {
            name: device_name(&device),
            sample_rate,
        };
        let running = Running {
            _stream: stream,
            device_id,
        };
        Ok((running, opened))
    }
}

/// The system default output, opened as the device itself rather than as
/// cpal's default-following unit. That unit re-routes on its own and leaves
/// the workers in the old device's workgroup; a stream on the device itself
/// reports its rate changing and its going away, and the engine follows the
/// default by polling (`480-audio-settings.md` § Things to check).
/// Returns it with its persistent ID, if it has one.
fn default_device(host: &Host) -> Option<(Device, Option<DeviceId>)> {
    let default = host.default_output_device()?;
    let id = default.id().ok();
    let specific = id.as_ref().and_then(|id| host.device_by_id(id));
    Some((specific.unwrap_or(default), id))
}

/// `device`'s name for the footer.
fn device_name(device: &Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "the audio output".to_string())
}

/// The mixer as a stream's data callback owns it. Dropped with the callback —
/// which cpal does only once CoreAudio has stopped calling it (`Stream`'s drop
/// stops the AudioUnit, uninitializes it, then frees the callback) — or with a
/// stream that failed to build, and either way it sends the mixer home to the
/// engine thread, so the sources outlive their stream
/// (`480-audio-settings.md` decision 6).
struct Returning {
    /// The mixer; taken only by the drop.
    mixer: Option<Mixer>,
    /// The engine thread's end.
    home: Sender<Mixer>,
}

impl Returning {
    /// The mixer, present until the drop.
    fn mixer(&mut self) -> &mut Mixer {
        self.mixer
            .as_mut()
            .expect("the mixer is only taken on drop")
    }
}

impl Drop for Returning {
    fn drop(&mut self) {
        if let Some(mixer) = self.mixer.take() {
            self.home.send(mixer).ok();
        }
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
    /// The device workgroup the pool's workers joined; read for the overrun
    /// journal's session header.
    workgroup: Arc<DeviceWorkgroup>,
    /// One buffer of the running stream, handed to sources as
    /// [`RenderCtx::schedule_delay`]. `0` until the first callback measures a
    /// buffer the device picked itself.
    schedule_delay: u32,
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
    /// Counts the streams this mixer has run in, so the journal starts a new
    /// session header for each.
    session: u32,
}

impl Mixer {
    /// Builds the callback state, sizing `sources` so runtime additions never
    /// reallocate on the audio thread. No workers until the first
    /// [`restart`](Self::restart).
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
            pool: WorkerPool::new(0, || ()),
            workgroup: Arc::new(DeviceWorkgroup::default()),
            schedule_delay: 0,
            mix: [
                Vec::with_capacity(MAX_FRAMES),
                Vec::with_capacity(MAX_FRAMES),
            ],
            steady: 0,
            source_elapsed: Vec::with_capacity(capacity),
            journal: journal::start(),
            session: 0,
        }
    }

    /// Readies the mixer for a new stream at `sample_rate` on the device
    /// whose workgroup is `workgroup`, while no stream is running: a new
    /// pool whose workers join that workgroup (the old one's leave theirs as
    /// they exit), a clock to re-anchor, a new journal session, and every
    /// source told ([`AudioSource::restarted`]). `schedule_delay` is the
    /// stream's buffer, or `0` to measure it on the first callback.
    fn restart(&mut self, sample_rate: f64, workgroup: Arc<DeviceWorkgroup>, schedule_delay: u32) {
        // A source still in the ring would miss the news.
        self.take_added_sources();
        // The old workers leave their group before the new ones join.
        self.pool = WorkerPool::new(0, || ());
        let joining = workgroup.clone();
        self.pool = WorkerPool::new(worker_thread_count(), move || joining.join());
        self.workgroup = workgroup;
        self.sample_rate = sample_rate;
        self.clock = AudioClock::new(sample_rate);
        self.schedule_delay = schedule_delay;
        self.session = self.session.wrapping_add(1);
        for source in &mut self.sources {
            source.restarted(sample_rate);
        }
    }

    /// Moves the sources waiting in the add-source ring into the list.
    fn take_added_sources(&mut self) {
        while let Ok(source) = self.add_source_rx.pop() {
            if self.sources.len() < self.sources.capacity() {
                self.sources.push(source);
            } else {
                debug!("audio engine: source list full, dropping added source");
            }
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

        self.take_added_sources();

        let ch = device_channels.max(1);
        let total_frames = data.len() / ch;
        if total_frames == 0 {
            return;
        }
        if self.schedule_delay == 0 {
            self.schedule_delay = total_frames as u32;
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
                schedule_delay: self.schedule_delay,
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
            workers: self.pool.worker_count(),
            workers_joined: self.workgroup.joined(),
            session: self.session,
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
fn build_stream<E>(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    mixer: Returning,
    on_error: E,
) -> Result<Stream, Error>
where
    E: FnMut(Error) + Send + 'static,
{
    with_sample_type!(sample_format, T => build_typed::<T, E>(device, config, channels, mixer, on_error), _ => {
        error!("audio engine: unsupported sample format {sample_format:?}");
        Err(ErrorKind::UnsupportedConfig.into())
    })
}

/// Builds the stream for one concrete sample type `T`, moving `mixer` into the
/// data callback, where it stays until the stream is dropped.
fn build_typed<T, E>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    mut mixer: Returning,
    on_error: E,
) -> Result<Stream, Error>
where
    T: SizedSample + FromSample<f32>,
    E: FnMut(Error) + Send + 'static,
{
    device.build_output_stream(
        *config,
        move |data: &mut [T], _| mixer.mixer().render(data, channels),
        on_error,
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

    fn opened(name: &str, sample_rate: u32) -> Opened {
        Opened {
            name: name.to_string(),
            sample_rate,
        }
    }

    #[test]
    fn rates_read_in_khz() {
        assert_eq!(khz(44_100), "44.1 kHz");
        assert_eq!(khz(48_000), "48 kHz");
        assert_eq!(khz(88_200), "88.2 kHz");
        assert_eq!(khz(96_000), "96 kHz");
    }

    #[test]
    fn the_launch_and_a_fruitless_retry_say_nothing() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        assert_eq!(
            restart_message(RestartCause::Launch, None, Some(&speakers)),
            None
        );
        assert_eq!(restart_message(RestartCause::Launch, None, None), None);
        assert_eq!(
            restart_message(RestartCause::Retry, Some(&speakers), None),
            None
        );
    }

    #[test]
    fn an_outside_change_says_what_happened() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        let scarlett = opened("Scarlett 2i2", 48_000);
        let airpods = opened("AirPods Pro", 48_000);
        assert_eq!(
            restart_message(
                RestartCause::DefaultChanged,
                Some(&speakers),
                Some(&airpods)
            )
            .as_deref(),
            Some("Audio switched to AirPods Pro (System default changed)")
        );
        assert_eq!(
            restart_message(RestartCause::DeviceLost, Some(&scarlett), Some(&speakers)).as_deref(),
            Some("Scarlett 2i2 disconnected; using MacBook Pro Speakers")
        );
        assert_eq!(
            restart_message(RestartCause::DeviceLost, Some(&scarlett), None).as_deref(),
            Some("Scarlett 2i2 disconnected; no audio output")
        );
        assert_eq!(
            restart_message(RestartCause::Retry, Some(&scarlett), Some(&speakers)).as_deref(),
            Some("Audio switched to MacBook Pro Speakers")
        );
    }

    #[test]
    fn a_new_rate_says_the_plugins_are_silent() {
        let at_48 = opened("Scarlett 2i2", 48_000);
        let at_44 = opened("Scarlett 2i2", 44_100);
        assert_eq!(
            restart_message(RestartCause::RateChanged, Some(&at_48), Some(&at_44)).as_deref(),
            Some("Device rate changed to 44.1 kHz; plugins stay silent until Stev is restarted")
        );
        let speakers = opened("MacBook Pro Speakers", 48_000);
        assert_eq!(
            restart_message(RestartCause::DeviceLost, Some(&at_44), Some(&speakers)).as_deref(),
            Some(
                "Scarlett 2i2 disconnected; using MacBook Pro Speakers; \
                 plugins stay silent until Stev is restarted"
            )
        );
    }

    #[test]
    fn a_dropped_callback_hands_the_mixer_back() {
        let (home, back) = bounded(1);
        let mixer = Mixer::new(
            Vec::new(),
            48_000.0,
            RingBuffer::new(1).1,
            Arc::new(AudioLoad::new()),
        );
        drop(Returning {
            mixer: Some(mixer),
            home,
        });
        assert!(back.try_recv().is_ok());
    }

    #[test]
    fn a_restart_starts_a_new_journal_session_at_the_new_rate() {
        let mut mixer = Mixer::new(
            Vec::new(),
            48_000.0,
            RingBuffer::new(1).1,
            Arc::new(AudioLoad::new()),
        );
        mixer.restart(44_100.0, Arc::new(DeviceWorkgroup::default()), 512);
        let first = mixer.overrun_record(Duration::from_millis(20), 512);
        mixer.restart(96_000.0, Arc::new(DeviceWorkgroup::default()), 128);
        let second = mixer.overrun_record(Duration::from_millis(20), 128);
        assert_eq!(first.sample_rate, 44_100.0);
        assert_eq!(second.sample_rate, 96_000.0);
        assert_ne!(first.session, second.session);
        assert_eq!(mixer.schedule_delay, 128);
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
