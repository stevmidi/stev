//! Device discovery, the single `cpal` output stream, and the `"audio-engine"`
//! thread that owns it — and rebuilds it whenever the device changes under it
//! or the user picks another in the Audio tab (`480-audio-settings.md`
//! decision 5).
//!
//! The thread waits on a channel of commands. Each restart drops the stream,
//! which hands the [`Mixer`] back ([`Returning`]), resolves the device as it
//! is now (the chosen one, else the system default), rebuilds the
//! [`WorkerPool`] so its workers join that device's workgroup, and opens a
//! new stream at the chosen rate and buffer. A restart comes from the Audio
//! tab ([`AudioControl::configure`]), from the stream's error callback (the
//! device gone, its rate changed) or from the thread's own poll (the system
//! default moved, the chosen device came back, or a device appeared while
//! none was open). The same poll keeps the tab's device list current.

use std::sync::Arc;
use std::thread::Builder;
use std::time::{Duration, Instant, SystemTime};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, DeviceId, Error, ErrorKind, FromSample, Host, I24, OutputCallbackInfo,
    SampleFormat, SizedSample, Stream, StreamConfig, SupportedBufferSize, default_host,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use log::{debug, error, info, warn};
use rtrb::{Consumer, Producer, RingBuffer};

use super::journal::{self, JournalWriter, MAX_JOURNAL_SOURCES, OverrunRecord};
use super::load::micros;
use super::prefs::{buffer_choices, rate_choices};
use super::{
    AudioClock, AudioDevice, AudioLoad, AudioPrefs, AudioSource, DeviceWorkgroup, EngineNews,
    FALLBACK_SAMPLE_RATE, MAX_FRAMES, RenderCtx, RunningAudio, StreamStatus, WorkerPool,
    enter_render_thread, khz, max_dsp_threads,
};
use crate::core::config::MAX_TRACKS;

/// Capacity of the ring carrying late-added sources (only the macOS instrument
/// mixer in practice) to the running mixer.
const ADD_SOURCE_RING_CAPACITY: usize = 4;

/// How often the engine thread looks at the system default output (to follow
/// it), for the chosen device coming back and, with no stream open, whether
/// a device has appeared.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How often the poll lists every output device for the Audio tab. A restart
/// lists them at once.
const DEVICE_LIST_INTERVAL: Duration = Duration::from_secs(2);

/// How long a restart waits for the dropped stream to hand the mixer back.
/// The hand-back is the stream's own teardown, so this only runs out if
/// CoreAudio hangs; the engine then stops for the session.
const MIXER_RETURN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the poll waits before trying again a device that failed to open.
/// A retry rebuilds the worker pool, so it isn't one to run every poll.
const FAILED_OPEN_RETRY: Duration = Duration::from_secs(5);

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

/// Handle to the running engine, kept by `main`. `add_source` is only used
/// where a second source is added at runtime — currently macOS (the plugin
/// host).
pub(crate) struct EngineHandle {
    /// The running stream's rate, measured buffer and latency, and how many
    /// times it has restarted: the plugin host builds its plugin processors
    /// at the rate, the audio export writes at it ([`FALLBACK_SAMPLE_RATE`]
    /// while no device is open), and the Audio tab reads it all out.
    pub(crate) status: Arc<StreamStatus>,
    /// How much of each block's deadline the render is actually using. Shared
    /// with the audio callback; handed to `Display` for the header readout.
    pub(crate) load: Arc<AudioLoad>,
    /// Where the Audio tab sends the user's choices.
    pub(crate) control: AudioControl,
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

/// The Audio tab's line to the engine thread: a choice there restarts the
/// stream on it (`480-audio-settings.md` decision 2).
#[derive(Clone)]
pub(crate) struct AudioControl {
    /// The engine thread's command channel.
    tx: Sender<EngineCommand>,
}

impl AudioControl {
    /// Has the engine run as `prefs` say from now on: it restarts the stream
    /// unless it already runs that way.
    pub(crate) fn configure(&self, prefs: AudioPrefs) {
        self.tx.send(EngineCommand::Configure(prefs)).ok();
    }
}

/// A command to the `"audio-engine"` thread.
#[derive(Debug)]
enum EngineCommand {
    /// A stream error asks for a restart. `generation` is the restart count
    /// the stream opened under, so an error from one already replaced is
    /// ignored.
    Restart {
        /// The restart count the reporting stream opened under.
        generation: u64,
        /// What happened to it.
        cause: RestartCause,
    },
    /// The user's choice in the Audio tab.
    Configure(AudioPrefs),
}

/// Namespace for the engine start-up entry point; the running engine lives in
/// its thread, reached through [`EngineHandle`].
pub(crate) struct AudioEngine;

impl AudioEngine {
    /// Spawns the `"audio-engine"` thread mixing `sources`, opens the stream as
    /// `prefs` say (the saved Audio tab choices), and returns once that first
    /// open has been tried, so [`EngineHandle::status`] has the device's rate.
    /// With no device the app starts silent, and the engine opens one when it
    /// appears. `notify` gets every [`EngineNews`]: after each restart (once
    /// [`EngineHandle::status`] has the new rate), with the line for the
    /// footer when an outside change caused it (`480-audio-settings.md`
    /// decision 11), and whenever the device list changes. The caller wakes
    /// the UI each time: that is what has it reactivate the plugins at a new
    /// rate.
    pub(crate) fn start<N>(
        sources: Vec<Box<dyn AudioSource>>,
        prefs: AudioPrefs,
        notify: N,
    ) -> EngineHandle
    where
        N: Fn(EngineNews) + Send + 'static,
    {
        let (add_source_tx, add_source_rx) = RingBuffer::new(ADD_SOURCE_RING_CAPACITY);
        let load = Arc::new(AudioLoad::new());
        let status = Arc::new(StreamStatus::new(FALLBACK_SAMPLE_RATE));
        let (mixer_tx, mixer_rx) = bounded(1);
        // Unbounded so neither the tab nor a stream's error callback ever
        // blocks on it; a pile-up is coalesced.
        let (command_tx, command_rx) = unbounded();
        // The mixer waits in its channel for the first open, as it does
        // between any two streams.
        mixer_tx
            .send(Mixer::new(
                sources,
                f64::from(FALLBACK_SAMPLE_RATE),
                add_source_rx,
                load.clone(),
                status.clone(),
            ))
            .ok();
        let mut thread = EngineThread {
            host: default_host(),
            prefs,
            running: None,
            opened: None,
            listed: None,
            listed_at: None,
            failed_at: None,
            chosen_refused: false,
            generation: 0,
            command_tx: command_tx.clone(),
            command_rx,
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
            control: AudioControl { tx: command_tx },
            add_source_tx,
        }
    }
}

/// Why the engine rebuilds its stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartCause {
    /// The app is starting: the first open.
    Launch,
    /// The user picked a device, a rate or a buffer in the Audio tab.
    Chosen,
    /// The device the stream ran on went away (`ErrorKind::DeviceNotAvailable`).
    DeviceLost,
    /// The device's sample rate changed under the stream, in Audio MIDI Setup
    /// or by another app (`ErrorKind::StreamInvalidated`).
    RateChanged,
    /// The system default output moved to another device.
    DefaultChanged,
    /// The chosen device, missing until now, is connected again.
    Returned,
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

/// The footer line for a restart: `previous` is the last stream that opened,
/// `now` the one this restart opened, if any. Nothing for the launch, for a
/// choice in the tab that opened (the readout changes in front of the user),
/// or for a retry that still found no device. Pure, unit-tested.
fn restart_message(
    cause: RestartCause,
    previous: Option<&RunningAudio>,
    now: Option<&RunningAudio>,
) -> Option<String> {
    if cause == RestartCause::Launch || (cause == RestartCause::Chosen && now.is_some()) {
        return None;
    }
    let lost = || match previous {
        Some(p) => format!("{} disconnected", p.device.name),
        None => "The audio output disconnected".to_string(),
    };
    let Some(now) = now else {
        return match cause {
            RestartCause::Launch | RestartCause::Retry => None,
            RestartCause::DeviceLost => Some(format!("{}; no audio output", lost())),
            RestartCause::Chosen
            | RestartCause::RateChanged
            | RestartCause::DefaultChanged
            | RestartCause::Returned => Some("No audio output".to_string()),
        };
    };
    let name = &now.device.name;
    Some(match cause {
        RestartCause::DeviceLost => format!("{}; using {name}", lost()),
        RestartCause::RateChanged if previous.is_some_and(|p| p.device.name == *name) => {
            format!("Device rate changed to {}", khz(now.sample_rate))
        }
        RestartCause::DefaultChanged => {
            format!("Audio switched to {name} (System default changed)")
        }
        RestartCause::Returned => format!("Audio switched to {name} (reconnected)"),
        RestartCause::Launch
        | RestartCause::Chosen
        | RestartCause::RateChanged
        | RestartCause::Retry => format!("Audio switched to {name}"),
    })
}

/// The footer line when the chosen device is connected but won't open:
/// what plays instead, `now`, if anything. Pure, unit-tested.
fn refused_message(chosen: &str, now: Option<&RunningAudio>) -> String {
    match now {
        Some(now) => format!("Couldn't open {chosen}; using {}", now.device.name),
        None => format!("Couldn't open {chosen}; no audio output"),
    }
}

/// Whether a stream opened as `opened` already runs as `prefs` ask, with the
/// chosen device connected (`chosen_connected`) or not: then a choice in the
/// tab needs no restart. A rate the device doesn't offer would open at the
/// device's own rate, so it matches whatever runs. Pure, unit-tested.
fn runs_as(opened: &RunningAudio, prefs: &AudioPrefs, chosen_connected: bool) -> bool {
    let device = match &prefs.device {
        Some(chosen) if chosen_connected => opened.on_chosen && opened.device.id == chosen.id,
        _ => !opened.on_chosen,
    };
    let rate = prefs
        .sample_rate
        .is_none_or(|hz| hz == opened.sample_rate || !opened.rates.contains(&hz));
    let buffer = match opened.buffer {
        Some(frames) => frames == prefs.buffer_frames,
        None => opened.buffers.is_empty(),
    };
    device && rate && buffer
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

/// The `"audio-engine"` thread's state: the user's choice, the open stream,
/// if any, and the channels the mixer and the commands travel on.
struct EngineThread {
    /// The audio host devices are looked up in.
    host: Host,
    /// What the user chose in the Audio tab.
    prefs: AudioPrefs,
    /// The running stream, if one is open.
    running: Option<Running>,
    /// The last stream that opened, kept while none is.
    opened: Option<RunningAudio>,
    /// The device list as last sent to the tab, with the default's name.
    listed: Option<(Vec<AudioDevice>, Option<String>)>,
    /// When the devices were last listed.
    listed_at: Option<Instant>,
    /// When the last open failed with a device there, so the poll doesn't
    /// rebuild the pool every half second for a device that won't open.
    failed_at: Option<Instant>,
    /// Whether the chosen device, connected, failed to open and the stream
    /// fell back to the default. The poll then leaves it alone until it has
    /// gone away and come back, or the user picks again: retrying it on a
    /// timer would drop the working stream out each time.
    chosen_refused: bool,
    /// Counts restarts. Each stream's error callback carries the count it
    /// opened under, so an error from a stream already replaced is ignored.
    generation: u64,
    /// Handed to each stream's error callback.
    command_tx: Sender<EngineCommand>,
    /// Commands from the tab and the error callbacks.
    command_rx: Receiver<EngineCommand>,
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
    /// Tells the UI what happened, and wakes it.
    notify: Box<dyn Fn(EngineNews) + Send>,
}

impl EngineThread {
    /// The thread's loop: act on commands, coalesced, and between them poll
    /// the devices. Returns only if a restart lost the mixer.
    fn run(mut self) {
        loop {
            let cause = match self.command_rx.recv_timeout(DEVICE_POLL_INTERVAL) {
                Ok(first) => {
                    let pending: Vec<EngineCommand> = std::iter::once(first)
                        .chain(self.command_rx.try_iter())
                        .collect();
                    self.take_commands(pending)
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

    /// The restart a pile of commands calls for: the user's latest choice
    /// (unless the stream already runs that way), else the last error from
    /// the stream running now.
    fn take_commands(&mut self, commands: Vec<EngineCommand>) -> Option<RestartCause> {
        let mut cause = None;
        let mut chosen = false;
        for command in commands {
            match command {
                EngineCommand::Restart {
                    generation,
                    cause: error,
                } if generation == self.generation => cause = Some(error),
                EngineCommand::Restart { .. } => {}
                EngineCommand::Configure(prefs) => {
                    self.prefs = prefs;
                    self.chosen_refused = false;
                    chosen = true;
                }
            }
        }
        if chosen && cause.is_none() && self.runs_as_chosen() {
            debug!("audio engine: already running as chosen");
            return None;
        }
        if chosen {
            Some(RestartCause::Chosen)
        } else {
            cause
        }
    }

    /// Whether the running stream already runs as the user chose.
    fn runs_as_chosen(&self) -> bool {
        self.running.is_some()
            && self
                .opened
                .as_ref()
                .is_some_and(|opened| runs_as(opened, &self.prefs, self.chosen_device().is_some()))
    }

    /// The restart the device list calls for: a device to open when none is
    /// (a device that just failed to, only every [`FAILED_OPEN_RETRY`]), the
    /// chosen device back while the stream runs on the default, or the
    /// system default having moved off the stream's device. Lists the
    /// devices for the tab on the way, every [`DEVICE_LIST_INTERVAL`].
    fn poll(&mut self) -> Option<RestartCause> {
        if self
            .listed_at
            .is_none_or(|at| at.elapsed() >= DEVICE_LIST_INTERVAL)
        {
            self.list_devices();
        }
        let Some(running) = &self.running else {
            let due = self
                .failed_at
                .is_none_or(|at| at.elapsed() >= FAILED_OPEN_RETRY);
            let any = due
                && (self.host.default_output_device().is_some() || self.chosen_device().is_some());
            return any.then_some(RestartCause::Retry);
        };
        // On the chosen device the poll follows nothing: its going away
        // comes through the error callback.
        if self.opened.as_ref().is_some_and(|opened| opened.on_chosen) {
            return None;
        }
        let chosen_there = self.chosen_device().is_some();
        if self.chosen_refused {
            // Gone away: it may open when it comes back.
            self.chosen_refused = chosen_there;
        } else if chosen_there {
            return Some(RestartCause::Returned);
        }
        let default_id = self.host.default_output_device()?.id().ok()?;
        let moved = running
            .device_id
            .as_ref()
            .is_some_and(|id| *id != default_id);
        moved.then_some(RestartCause::DefaultChanged)
    }

    /// Lists every output device and the default's name, and tells the tab
    /// when that changed.
    fn list_devices(&mut self) {
        self.listed_at = Some(Instant::now());
        let devices: Vec<AudioDevice> = match self.host.output_devices() {
            Ok(devices) => devices
                .filter_map(|device| {
                    let id = device.id().ok()?;
                    Some(AudioDevice {
                        id: id.to_string(),
                        name: device_name(&device),
                    })
                })
                .collect(),
            Err(e) => {
                debug!("audio engine: could not list the output devices: {e}");
                Vec::new()
            }
        };
        let default_name = self.host.default_output_device().map(|d| device_name(&d));
        let listed = Some((devices, default_name));
        if listed != self.listed {
            self.listed = listed.clone();
            if let Some((devices, default_name)) = listed {
                (self.notify)(EngineNews::Devices {
                    devices,
                    default_name,
                });
            }
        }
    }

    /// The chosen device, if one is and it is connected, with its ID.
    fn chosen_device(&self) -> Option<(Device, DeviceId)> {
        let id: DeviceId = self.prefs.device.as_ref()?.id.parse().ok()?;
        let device = self.host.device_by_id(&id)?;
        Some((device, id))
    }

    /// Drops the stream, takes the mixer back and opens a new stream as the
    /// user chose on the devices as they are now (the system default when the
    /// chosen device is missing or won't open), then says what happened.
    /// Returns `false` if the mixer never came back, which leaves the app
    /// silent for the session.
    fn restart(&mut self, cause: RestartCause) -> bool {
        if self.running.take().is_some() {
            self.status.count_restart();
        }
        self.status.clear_measurements();
        self.generation += 1;
        let Some(mixer) = self.take_mixer() else {
            return false;
        };

        let chosen_there = self.chosen_device().is_some();
        let mut result = self.open(mixer, true);
        let mut refused = None;
        if let Err(e) = &result
            && chosen_there
        {
            warn!("audio engine: the chosen device won't open ({e}); trying the default");
            self.chosen_refused = true;
            refused = self.prefs.device.as_ref().map(|d| d.name.clone());
            let Some(mixer) = self.take_mixer() else {
                return false;
            };
            result = self.open(mixer, false);
        }
        // A device plugged in or out is in the list before the tab hears
        // what runs, so it can find the row in use.
        self.list_devices();
        match result {
            Ok((running, opened)) => {
                info!(
                    "audio engine: {} @ {} Hz, buffer {:?} ({cause:?})",
                    opened.device.name, opened.sample_rate, opened.buffer
                );
                // Only a stream that plays is the rate plugins are built at.
                self.status.set_sample_rate(opened.sample_rate);
                self.notify_restart(cause, Some(&opened), refused.as_deref());
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
                self.notify_restart(cause, None, refused.as_deref());
                self.failed_at = Some(Instant::now());
            }
        }
        true
    }

    /// The mixer, back from the stream just dropped (or one that failed to
    /// build). `None`, said in the footer, if it never comes.
    fn take_mixer(&self) -> Option<Mixer> {
        let mixer = self.mixer_rx.recv_timeout(MIXER_RETURN_TIMEOUT).ok();
        if mixer.is_none() {
            error!("audio engine: the stream never handed the mixer back; audio is off");
            (self.notify)(EngineNews::Restarted {
                running: None,
                message: Some("Audio stopped; restart Stev to bring it back".to_string()),
            });
        }
        mixer
    }

    /// Tells the UI a restart opened `now`, if anything, with its footer
    /// line, if any: why the chosen device isn't playing when it `refused`
    /// to open (its name), else what the cause calls for.
    fn notify_restart(
        &self,
        cause: RestartCause,
        now: Option<&RunningAudio>,
        refused: Option<&str>,
    ) {
        let message = match refused {
            Some(chosen) => Some(refused_message(chosen, now)),
            None => restart_message(cause, self.opened.as_ref(), now),
        };
        (self.notify)(EngineNews::Restarted {
            running: now.cloned(),
            message,
        });
    }

    /// Opens a stream with `mixer` moved into the callback: on the chosen
    /// device when `use_chosen` and it is connected, else on the system
    /// default; at the chosen rate when the device offers it, else at its
    /// own; with the chosen buffer clamped to the device's range. On any
    /// failure the mixer goes back on `mixer_tx`, through [`Returning`]'s
    /// drop.
    fn open(&mut self, mixer: Mixer, use_chosen: bool) -> Result<(Running, RunningAudio), String> {
        let mut held = Returning {
            mixer: Some(mixer),
            home: self.mixer_tx.clone(),
        };
        let chosen = use_chosen.then(|| self.chosen_device()).flatten();
        let on_chosen = chosen.is_some();
        let (device, device_id) = match chosen {
            Some((device, id)) => (device, Some(id)),
            None => default_device(&self.host).ok_or("no output device")?,
        };
        let supported = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;
        let ranges: Vec<(u32, u32)> = device
            .supported_output_configs()
            .map(|configs| {
                configs
                    .map(|c| (c.min_sample_rate(), c.max_sample_rate()))
                    .collect()
            })
            .unwrap_or_default();
        // The device's own rate unless the user chose one it offers: building
        // the stream sets the device to it (decision 3).
        let sample_rate = self
            .prefs
            .sample_rate
            .filter(|hz| ranges.iter().any(|(min, max)| (min..=max).contains(&hz)))
            .unwrap_or(supported.sample_rate());
        let buffer_range = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            SupportedBufferSize::Unknown => None,
        };

        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let buffer_size = pick_buffer_size(supported.buffer_size(), self.prefs.buffer_frames);
        let mut config: StreamConfig = supported.into();
        config.sample_rate = sample_rate;
        config.buffer_size = buffer_size;
        // `pick_buffer_size`'s clamp against the device's reported range
        // doesn't guarantee the driver accepts every value in it — probe with
        // a throwaway no-op stream before committing. Twice: building the
        // first probe is what sets a new rate on the device, and the buffer
        // set straight after it can be refused while the device is still
        // changing rate (seen on a Scarlett 4i4 on every rate change); by the
        // second the rate has settled.
        let probe = || probe_buffer_size_supported(&device, &config, sample_format);
        if matches!(config.buffer_size, BufferSize::Fixed(_)) && !probe() && !probe() {
            info!(
                "audio engine: fixed buffer size rejected by the device; using the device default"
            );
            config.buffer_size = BufferSize::Default;
        }
        // A device that picks its own buffer has it measured on the first
        // callback instead.
        let buffer = match config.buffer_size {
            BufferSize::Fixed(frames) => Some(frames),
            BufferSize::Default => None,
        };

        let workgroup = Arc::new(DeviceWorkgroup::for_device(&device));
        held.mixer()
            .restart(f64::from(sample_rate), workgroup, buffer.unwrap_or(0));

        let generation = self.generation;
        let command_tx = self.command_tx.clone();
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
                    command_tx
                        .send(EngineCommand::Restart { generation, cause })
                        .ok();
                }
                None => error!("audio engine stream error: {e}"),
            }
        };
        let stream = build_stream(&device, &config, sample_format, channels, held, on_error)
            .map_err(|e| format!("could not build the stream: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("stream.play() failed: {e}"))?;

        let opened = RunningAudio {
            device: AudioDevice {
                id: device_id
                    .as_ref()
                    .map(DeviceId::to_string)
                    .unwrap_or_default(),
                name: device_name(&device),
            },
            on_chosen,
            sample_rate,
            rates: rate_choices(&ranges),
            buffer,
            buffers: buffer_choices(buffer_range),
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

/// `device`'s name for the list and the footer.
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
    /// Where each callback leaves its buffer and latency for the Audio tab.
    status: Arc<StreamStatus>,
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
        status: Arc<StreamStatus>,
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
            status,
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
    /// device format. Records the deadline utilisation, and the buffer and
    /// `latency_us` (the callback's output latency) for the Audio tab.
    fn render<T>(&mut self, data: &mut [T], device_channels: usize, latency_us: u32)
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
        self.status.record_callback(total_frames as u32, latency_us);

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
        move |data: &mut [T], info: &OutputCallbackInfo| {
            let at = info.timestamp();
            let latency = micros(at.playback.duration_since(at.callback));
            mixer.mixer().render(data, channels, latency)
        },
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

    fn opened(name: &str, sample_rate: u32) -> RunningAudio {
        RunningAudio {
            device: AudioDevice {
                id: format!("coreaudio:{name}"),
                name: name.to_string(),
            },
            on_chosen: false,
            sample_rate,
            rates: vec![44_100, 48_000, 96_000],
            buffer: Some(256),
            buffers: vec![64, 128, 256, 512],
        }
    }

    fn mixer() -> Mixer {
        Mixer::new(
            Vec::new(),
            48_000.0,
            RingBuffer::new(1).1,
            Arc::new(AudioLoad::new()),
            Arc::new(StreamStatus::new(48_000)),
        )
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
    fn a_new_rate_says_so_and_nothing_about_the_plugins() {
        let at_48 = opened("Scarlett 2i2", 48_000);
        let at_44 = opened("Scarlett 2i2", 44_100);
        assert_eq!(
            restart_message(RestartCause::RateChanged, Some(&at_48), Some(&at_44)).as_deref(),
            Some("Device rate changed to 44.1 kHz")
        );
        // A first device, after a launch with none, says only where it went.
        assert_eq!(
            restart_message(RestartCause::Retry, None, Some(&at_44)).as_deref(),
            Some("Audio switched to Scarlett 2i2")
        );
    }

    #[test]
    fn a_choice_in_the_tab_says_nothing_unless_nothing_opened() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        let scarlett = opened("Scarlett 2i2", 96_000);
        assert_eq!(
            restart_message(RestartCause::Chosen, Some(&speakers), Some(&scarlett)),
            None
        );
        assert_eq!(
            restart_message(RestartCause::Chosen, Some(&speakers), None).as_deref(),
            Some("No audio output")
        );
    }

    #[test]
    fn a_chosen_device_that_wont_open_says_what_plays_instead() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        assert_eq!(
            refused_message("Scarlett 2i2", Some(&speakers)),
            "Couldn't open Scarlett 2i2; using MacBook Pro Speakers"
        );
        assert_eq!(
            refused_message("Scarlett 2i2", None),
            "Couldn't open Scarlett 2i2; no audio output"
        );
    }

    #[test]
    fn the_chosen_device_coming_back_says_so() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        let scarlett = opened("Scarlett 2i2", 48_000);
        assert_eq!(
            restart_message(RestartCause::Returned, Some(&speakers), Some(&scarlett)).as_deref(),
            Some("Audio switched to Scarlett 2i2 (reconnected)")
        );
    }

    #[test]
    fn a_choice_matching_the_stream_needs_no_restart() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        let prefs = AudioPrefs::default();
        assert!(runs_as(&speakers, &prefs, false));
        // The rate it already runs at, or one the device doesn't offer.
        for hz in [48_000, 22_050] {
            let prefs = AudioPrefs {
                sample_rate: Some(hz),
                ..AudioPrefs::default()
            };
            assert!(runs_as(&speakers, &prefs, false), "{hz}");
        }
    }

    #[test]
    fn another_rate_buffer_or_device_needs_a_restart() {
        let speakers = opened("MacBook Pro Speakers", 48_000);
        let faster = AudioPrefs {
            sample_rate: Some(96_000),
            ..AudioPrefs::default()
        };
        assert!(!runs_as(&speakers, &faster, false));
        let smaller = AudioPrefs {
            buffer_frames: 128,
            ..AudioPrefs::default()
        };
        assert!(!runs_as(&speakers, &smaller, false));
        let scarlett = AudioPrefs {
            device: Some(AudioDevice {
                id: "coreaudio:Scarlett".into(),
                name: "Scarlett 2i2".into(),
            }),
            ..AudioPrefs::default()
        };
        assert!(!runs_as(&speakers, &scarlett, true));
        // Missing, so the default the stream follows is what it would get.
        assert!(runs_as(&speakers, &scarlett, false));
    }

    #[test]
    fn the_chosen_device_runs_as_chosen_and_the_default_does_not() {
        let mut scarlett = opened("Scarlett 2i2", 48_000);
        scarlett.on_chosen = true;
        let chosen = AudioPrefs {
            device: Some(scarlett.device.clone()),
            ..AudioPrefs::default()
        };
        assert!(runs_as(&scarlett, &chosen, true));
        // Back to System default from a chosen device.
        assert!(!runs_as(&scarlett, &AudioPrefs::default(), false));
    }

    #[test]
    fn a_dropped_callback_hands_the_mixer_back() {
        let (home, back) = bounded(1);
        drop(Returning {
            mixer: Some(mixer()),
            home,
        });
        assert!(back.try_recv().is_ok());
    }

    #[test]
    fn a_restart_starts_a_new_journal_session_at_the_new_rate() {
        let mut mixer = mixer();
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
