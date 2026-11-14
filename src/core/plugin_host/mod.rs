//! macOS-only **instrument** plugin host: one hosted plugin per sequencer
//! track, each with its own editor window.
//!
//! The module is split into a format-agnostic core and one submodule per
//! plugin format. The core owns everything that does not care which format a
//! track's plugin is:
//!
//! - [`mixer`] — the single [`AudioSource`] that renders and sums every track's
//!   voice, schedules clip/live MIDI onto output samples, and applies the
//!   per-track gain ramp and mute/solo.
//! - [`export`] / [`export_writer`] — the audio export: the mixer's tap and
//!   the `"audio-export"` thread that writes its WAV files.
//! - [`voice`] / [`editor`] — the two traits a format implements: the `Send`
//!   audio-thread half and the `!Send` main-thread half.
//! - [`buffers`] — the declared audio layout and the `[port][channel]` buffer
//!   set a voice renders into.
//! - [`transport`] — the transport atomics and the per-block snapshot voices
//!   are handed.
//! - [`catalog`] — the merged installed-plugin list the Track modal offers.
//! - [`window`] / [`editor_keys`] — the native editor window and how keys reach
//!   Stev while a plugin has OS keyboard focus.
//! - [`meter`] — the track meters' ballistics: peak, RMS and peak hold.
//! - [`note_ids`] — note ids, for the formats whose notes carry them.
//! - [`shutdown`] — the cross-thread shutdown coordination every format shares.
//! - [`ui_wake`] — waking the reactive UI from the audio thread.
//! - `test_voice.rs` (tests) — a format-free voice for the mixer's and the
//!   export tap's tests.
//!
//! [`clap`] and [`vst3`] are the formats implemented today. VST3 additionally
//! needs [`run_if_scan_child`] called at the very top of `main` — its catalog
//! scan runs out-of-process, and that is how a scan child recognises itself.
//!
//! Threads:
//!
//! - The **eframe main thread** owns every `!Send` plugin instance and drives
//!   each plugin's editor ([`InstrumentEditor`]) — GUI lifecycle, timer
//!   extensions and main-thread callbacks all have to run on the thread with
//!   the AppKit run loop.
//! - The **`"audio-engine"` callback thread** runs `InstrumentMixer::render_into`.
//! - The [`InstrumentMixer`] (`Send`, sums the audio processors of all loaded
//!   plugins) runs as one `AudioSource` **inside the shared `"audio-engine"`**
//!   (`core::audio`) — no `cpal` stream of its own. A small `"plugin-host"`
//!   thread does only off-audio-thread voice reclamation and app-exit
//!   coordination.
//! - The **`"audio-export"`** thread waits for an audio export and writes its
//!   files from the mixer's export rings.
//!
//! [`start_plugin_host`] builds the empty mixer, hands it to the running engine
//! via [`EngineHandle::add_source`], and spawns the reclaim and export threads.
//! [`load_instrument`] runs on the main thread (plugin load + activate), keeps
//! the `!Send` instance for the UI, and sends the voice to the mixer as a
//! [`PluginHostCommand::Insert`].
//!
//! Plugins are picked in the browser panel; discovery of the installed bundles
//! is in [`catalog`]. See `docs/130-plugin-host.md`.

mod buffers;
pub(crate) mod catalog;
mod clap;
pub(crate) mod editor;
mod editor_keys;
mod export;
mod export_writer;
mod meter;
mod mixer;
mod note_ids;
mod shutdown;
#[cfg(test)]
mod test_voice;
mod transport;
mod ui_wake;
mod voice;
mod vst3;
mod window;

use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use log::{debug, info};
use rtrb::{Consumer, Producer, RingBuffer};

use catalog::{PluginCatalogEntry, PluginFormat};
use export::export_tap;
use export_writer::{ExportWriter, start_export_writer};
use mixer::{InstrumentMixer, LeavingVoice, VoiceExit};
use transport::TransportState;
use ui_wake::UiWake;
use voice::{InstrumentVoice, VoiceMix};

use crate::core::audio::{AudioSource, EngineHandle, StreamStatus};
use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::Midi3;
use crate::core::sequencer::{AudioExportJob, ClipInstrumentEvent, ExportMark, SequencerCommand};
use crate::core::shared_atomics::SharedAtomics;

pub(crate) use catalog::scan_catalog;
pub(crate) use editor::InstrumentEditor;
pub(crate) use editor_keys::{install_editor_keys, take_editor_keys};
pub(crate) use mixer::PluginHostCommand;
pub(crate) use shutdown::{HostShutdown, exit_without_destructors};
pub(crate) use vst3::child::run_if_scan_child;

/// The two halves one loaded plugin splits into: the `Send` voice the mixer
/// takes and the `!Send` editor the UI keeps. Every format's own `load`
/// returns this pair, and [`prepare_instrument`] is what routes them.
type LoadedInstrument = (Box<dyn InstrumentVoice>, Box<dyn InstrumentEditor>);

/// Capacity of the `rtrb` ring carrying [`PluginHostCommand`]s from `Display`
/// to the mixer. Generous headroom over realistic traffic: user-driven track
/// plugin picks/removals, not a per-block feed. The largest burst is a
/// project load swapping its staged plugins in within one frame — a `Remove`
/// and an `Insert` per track, `2 * MAX_TRACKS` — so twice that.
const CMD_RING_CAPACITY: usize = 4 * MAX_TRACKS;

/// Capacity of the `rtrb` ring the mixer hands removed voices back on, for the
/// reclaim thread to drop off the audio thread, and returned ones, for it to
/// pass to the UI. The worst realistic burst is a rate change landing on a
/// project reload before the reclaim thread's next drain: a `Return` and a
/// `Remove` per instrument track, `2 * MAX_TRACKS`, so one track's more
/// again. A full ring (the reclaim thread stalled through several plugin
/// swaps — not reachable in practice) falls back to dropping the voice on
/// the audio thread.
const DEAD_VOICE_RING_CAPACITY: usize = 3 * MAX_TRACKS;

/// Handle to the running plugin host, kept by `Display`. Used to load plugins
/// ([`load_instrument`]), send runtime commands, and coordinate app-exit
/// shutdown.
pub(crate) struct PluginAudioHandle {
    /// Runtime commands to the mixer (insert / remove voices). A realtime-safe
    /// `rtrb` ring producer — the mixer pops from the other end on the audio
    /// callback thread, where a `crossbeam_channel`'s internal allocate/free
    /// isn't safe.
    pub(crate) cmd_tx: Producer<PluginHostCommand>,
    /// What the `"plugin-host"` reclaim thread did with each voice that left
    /// the mixer, in the order they left: dropped it, for the UI to drop the
    /// matching `!Send` instance, or passed it back to be reactivated.
    pub(crate) voice_back_rx: Receiver<VoiceBack>,
    /// App-exit coordination — see [`HostShutdown`].
    pub(crate) shutdown: Arc<HostShutdown>,
    /// The engine's stream — each plugin is activated at its rate as it
    /// loads.
    stream: Arc<StreamStatus>,
    /// Shared egui context slot, for a plugin to wake the reactive UI (CLAP's
    /// main-thread callback requests).
    repaint: Arc<OnceLock<egui::Context>>,
    /// Wakes the same UI from the audio thread, through the `"plugin-host"`
    /// thread.
    ui_wake: Arc<UiWake>,
}

/// A voice that left the mixer, as the `"plugin-host"` thread reports it to
/// the UI.
pub(crate) enum VoiceBack {
    /// Track `0`'s removed voice has been dropped: its instance can go.
    Dropped(usize),
    /// Track `0`'s voice, handed back by a [`PluginHostCommand::Return`] for
    /// [`PluginAudioHandle::reactivate`] — `None` if the slot had none.
    Returned(usize, Option<ReturnedVoice>),
}

/// A voice the mixer handed back to be reactivated, stopped, opaque to the
/// UI. Dropping it, on the main thread, before its instance is deactivated is
/// a removal's order too.
pub(crate) struct ReturnedVoice(Box<dyn InstrumentVoice>);

/// A voice activated and ready to go into the mixer, held by the UI until
/// an audio restart's plugins all go back together
/// (`480-audio-settings.md` decision 8): one reactivated in place
/// ([`PluginAudioHandle::reactivate`]) or loaded again
/// ([`PreparedInstrument::into_ready`]). Opaque to the UI, and dropped as a
/// [`ReturnedVoice`] is: before its instance is deactivated.
pub(crate) struct ReadyVoice {
    /// The voice, every note reset queued.
    voice: Box<dyn InstrumentVoice>,
    /// The rate its plugin was activated at.
    sample_rate: f64,
}

/// Capacity of a voice's parameter rings, in both formats. A preset change in
/// a large synth can push a parameter per control at once, so this is sized
/// for a whole panel's worth rather than for incremental knob turns.
const PARAM_RING_CAPACITY: usize = 4096;

impl PluginAudioHandle {
    /// The rate the stream runs at now, which a plugin loaded or reactivated
    /// now is activated at.
    pub(crate) fn sample_rate(&self) -> f64 {
        self.stream.sample_rate()
    }

    /// Reactivates `editor`'s plugin, in place, at the rate the stream runs
    /// at now, with the `voice` the mixer handed back, every note reset and
    /// its gain to ramp in from silence (`480-audio-settings.md` decisions 8
    /// and 9). Returns the voice for [`insert_ready`](Self::insert_ready), or
    /// why the plugin wouldn't reactivate.
    pub(crate) fn reactivate(
        &self,
        voice: ReturnedVoice,
        editor: &mut dyn InstrumentEditor,
    ) -> Result<ReadyVoice, String> {
        let sample_rate = self.sample_rate();
        let mut voice = editor.reactivate(voice.0, sample_rate)?;
        *voice.mix_mut() = VoiceMix::default();
        voice.queue_note_reset();
        Ok(ReadyVoice { voice, sample_rate })
    }

    /// Sends a [`ReadyVoice`] to the mixer for `track`, whose slot must be
    /// empty by then: the one way a voice goes in. Returns the rate it was
    /// activated at.
    pub(crate) fn insert_ready(&mut self, track: usize, ready: ReadyVoice) -> Result<f64, String> {
        let ReadyVoice { voice, sample_rate } = ready;
        self.cmd_tx
            .push(PluginHostCommand::Insert {
                track,
                voice,
                sample_rate,
            })
            .map_err(|_| "plugin host audio thread is gone (command ring full)".to_string())?;
        Ok(sample_rate)
    }
}

/// Builds the instrument mixer, hands it to the running audio engine as a
/// second source, and spawns the `"plugin-host"` thread that reclaims removed
/// plugin voices off the audio thread and the `"audio-export"` thread. Call once from `main`; `clip_midi_rx` /
/// `live_midi_rx` are the tagged MIDI feeds — `(track slot, [`Midi3`])` — for
/// per-track clip events and live keyboard notes respectively (two separate
/// `rtrb` rings, since `rtrb` is strict single-producer and the two feeds have
/// different producer threads). `export_mark_rx` / `audio_export_jobs` are
/// the audio export's marks from the sequencer and files from its workflow;
/// the writer reports back on `sequencer_command_tx`. `repaint` is the app's
/// shared egui context
/// slot, handed to every plugin loaded later. Any failure is returned as `Err`
/// and the whole feature is disabled.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_plugin_host(
    engine: &mut EngineHandle,
    clip_midi_rx: Consumer<ClipInstrumentEvent>,
    live_midi_rx: Consumer<(usize, Midi3)>,
    export_mark_rx: Consumer<ExportMark>,
    audio_export_jobs: Receiver<AudioExportJob>,
    sequencer_command_tx: Sender<SequencerCommand>,
    atomics: &SharedAtomics,
    repaint: Arc<OnceLock<egui::Context>>,
) -> Result<PluginAudioHandle, String> {
    let shutdown = Arc::new(HostShutdown::default());
    let (cmd_tx, cmd_rx) = RingBuffer::new(CMD_RING_CAPACITY);
    let (dead_tx, dead_rx) = RingBuffer::new(DEAD_VOICE_RING_CAPACITY);
    let (voice_back_tx, voice_back_rx) = unbounded::<VoiceBack>();

    let transport = TransportState {
        running: Arc::clone(&atomics.running),
        tempo_us: Arc::clone(&atomics.tempo),
        meter: Arc::clone(&atomics.meter),
        playback_tick: Arc::clone(&atomics.playback_tick),
        region_start: Arc::clone(&atomics.region_start),
        region_end: Arc::clone(&atomics.region_end),
        loop_enabled: Arc::clone(&atomics.loop_enabled),
    };

    let (export, export_rings, export_shared) =
        export_tap(export_mark_rx, engine.status.sample_rate());
    let mixer = InstrumentMixer::new(
        cmd_rx,
        dead_tx,
        clip_midi_rx,
        live_midi_rx,
        transport,
        Arc::clone(&atomics.track_mix),
        Arc::clone(&shutdown),
        export,
        engine.status.sample_rate(),
    );
    engine.add_source(Box::new(mixer) as Box<dyn AudioSource>)?;

    let ui_wake = Arc::new(UiWake::new(Arc::clone(&repaint)));
    let reclaim_shutdown = Arc::clone(&shutdown);
    let reclaim_wake = Arc::clone(&ui_wake);
    thread::Builder::new()
        .name("plugin-host".to_string())
        .spawn(move || reclaim_voices(dead_rx, voice_back_tx, reclaim_shutdown, reclaim_wake))
        .expect("failed to spawn plugin-host thread");

    let writer = ExportWriter::new(
        export_rings,
        export_shared,
        Arc::clone(&atomics.audio_export),
        Arc::clone(&engine.load),
        Arc::clone(&engine.status),
    );
    start_export_writer(audio_export_jobs, writer, sequencer_command_tx);

    debug!("plugin host: mixer attached to the audio engine");

    Ok(PluginAudioHandle {
        cmd_tx,
        voice_back_rx,
        shutdown,
        stream: Arc::clone(&engine.status),
        repaint,
        ui_wake,
    })
}

/// The `"plugin-host"` thread: drops plugin voices the mixer removed (a plugin
/// teardown must not run in the audio callback) and acks the track back to the
/// UI so it can drop the matching `!Send` instance, and passes the voices it
/// returned on to the UI, waking it. Voices still loaded at app
/// exit are deliberately left to leak — dropping them races the plugin bundles'
/// static destructors that `exit()` runs (see [`HostShutdown`]). It also
/// makes the UI wakes the audio thread can't ([`UiWake`]).
fn reclaim_voices(
    mut dead_rx: Consumer<LeavingVoice>,
    voice_back_tx: Sender<VoiceBack>,
    shutdown: Arc<HostShutdown>,
    ui_wake: Arc<UiWake>,
) {
    let drain = |rx: &mut Consumer<LeavingVoice>| {
        while let Ok((track, voice, exit)) = rx.pop() {
            let back = match exit {
                VoiceExit::Removed => {
                    drop(voice);
                    VoiceBack::Dropped(track)
                }
                VoiceExit::Returned => {
                    ui_wake.request();
                    VoiceBack::Returned(track, voice.map(ReturnedVoice))
                }
            };
            voice_back_tx.send(back).ok();
        }
    };

    shutdown.register_thread();
    ui_wake.register_thread();
    while !shutdown.is_requested() {
        drain(&mut dead_rx);
        ui_wake.service();
        thread::park_timeout(Duration::from_millis(100));
    }
    drain(&mut dead_rx);
}

/// Spawns a dedicated `"plugin-catalog-scan"` thread that runs [`scan_catalog`]
/// and sends the catalog back as each format finishes. Discovery loads every
/// installed bundle's executable just to read its metadata, which is far from
/// free — running it here keeps the first Track-modal open from blocking the
/// eframe main thread. Call once from `main`, right after
/// [`start_plugin_host`] succeeds.
///
/// The receiver gets **one message per format**, each carrying the full catalog
/// so far; the sender is dropped when the scan is done, so a disconnected
/// channel is how the caller knows scanning has finished.
pub(crate) fn start_plugin_catalog_scan() -> Receiver<Vec<PluginCatalogEntry>> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("plugin-catalog-scan".to_string())
        .spawn(move || {
            scan_catalog(|catalog| {
                tx.send(catalog).ok();
            });
        })
        .expect("failed to spawn plugin-catalog-scan thread");
    rx
}

/// Main-thread: load `entry`'s plugin in whichever format it belongs to,
/// activate it, keep the `!Send` [`InstrumentEditor`] for the UI and send the
/// voice to the mixer for `track`. `state` is the persisted preset blob from
/// the project (empty / `None` for a fresh pick), applied before activation.
pub(crate) fn load_instrument(
    handle: &mut PluginAudioHandle,
    track: usize,
    entry: &PluginCatalogEntry,
    state: Option<&[u8]>,
) -> Result<InsertedInstrument, String> {
    prepare_instrument(handle, track, entry, state)?.insert(handle)
}

/// A plugin loaded and activated for engine slot `track` but not yet in the
/// mixer — a project load stages its plugins this way while the old project
/// keeps playing its own, then swaps them all in at once
/// ([`insert`](Self::insert)).
pub(crate) struct PreparedInstrument {
    /// The engine slot the voice goes into.
    track: usize,
    /// The audio-thread half, until the mixer takes it.
    voice: Box<dyn InstrumentVoice>,
    /// The rate the plugin was activated at.
    sample_rate: f64,
    /// The main-thread half.
    editor: Box<dyn InstrumentEditor>,
}

/// A plugin in the mixer, as the UI keeps it: its editor, and the rate it
/// was activated at, which the UI compares with the stream's to know when to
/// reactivate it ([`PluginAudioHandle::reactivate`]).
pub(crate) type InsertedInstrument = (Box<dyn InstrumentEditor>, f64);

impl PreparedInstrument {
    /// Sends the voice to the mixer for its slot and hands back the editor
    /// and its rate — the slot must be empty by then (its old voice's
    /// `Remove` sent first).
    pub(crate) fn insert(
        self,
        handle: &mut PluginAudioHandle,
    ) -> Result<InsertedInstrument, String> {
        let track = self.track;
        let (editor, ready) = self.into_ready();
        let sample_rate = handle.insert_ready(track, ready)?;
        Ok((editor, sample_rate))
    }

    /// Splits a plugin loaded again in an audio restart into its editor and
    /// the voice to hold until the restart's plugins go back together
    /// ([`PluginAudioHandle::insert_ready`]).
    pub(crate) fn into_ready(self) -> (Box<dyn InstrumentEditor>, ReadyVoice) {
        let PreparedInstrument {
            voice,
            sample_rate,
            editor,
            ..
        } = self;
        (editor, ReadyVoice { voice, sample_rate })
    }

    /// Drops a plugin that never reached the mixer, in the order a removed
    /// one goes: the voice first, then the deactivated instance.
    pub(crate) fn discard(self) {
        let PreparedInstrument {
            voice, mut editor, ..
        } = self;
        drop(voice);
        editor.deactivate();
    }
}

/// The loading half of [`load_instrument`]: loads and activates `entry`'s
/// plugin for `track`, with every note reset queued, without sending it to
/// the mixer.
///
/// This is the one place a plugin format is chosen; everything downstream of it
/// works through the two traits.
pub(crate) fn prepare_instrument(
    handle: &mut PluginAudioHandle,
    track: usize,
    entry: &PluginCatalogEntry,
    state: Option<&[u8]>,
) -> Result<PreparedInstrument, String> {
    // Read once, so the voice is known to run at the rate it was built at.
    let sample_rate = handle.sample_rate();
    let (mut voice, editor) = match entry.format {
        PluginFormat::Clap => clap::load(handle, track, entry, state, sample_rate)?,
        PluginFormat::Vst3 => vst3::load(handle, track, entry, state, sample_rate)?,
    };
    // Every format starts from silence, whatever its restored state believes
    // is held. Queued here, on the main thread, before `Insert`.
    voice.queue_note_reset();
    info!(
        "plugin host: loaded {} '{}' on track {}",
        entry.format.label(),
        entry.name,
        track + 1
    );
    Ok(PreparedInstrument {
        track,
        voice,
        sample_rate,
        editor,
    })
}
