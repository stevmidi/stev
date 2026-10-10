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
//! - [`voice`] / [`editor`] — the two traits a format implements: the `Send`
//!   audio-thread half and the `!Send` main-thread half.
//! - [`buffers`] — the declared audio layout and the `[port][channel]` buffer
//!   set a voice renders into.
//! - [`transport`] — the transport atomics and the per-block snapshot voices
//!   are handed.
//! - [`catalog`] — the merged installed-plugin list the Track modal offers.
//! - [`window`] / [`key_guard`] — the native editor window and the app-wide key
//!   reservation that survives a plugin taking OS keyboard focus.
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
//!
//! [`start_plugin_host`] builds the empty mixer, hands it to the running engine
//! via [`EngineHandle::add_source`], and spawns the reclaim thread.
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
mod key_guard;
mod mixer;
mod shutdown;
mod transport;
mod voice;
mod vst3;
mod window;

use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use rtrb::{Consumer, Producer, RingBuffer};

use catalog::{PluginCatalogEntry, PluginFormat};
use mixer::InstrumentMixer;
use transport::TransportState;
use voice::{InstrumentVoice, note_reset_messages};

use crate::core::audio::{AudioSource, EngineHandle};
use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::Midi3;
use crate::core::sequencer::ClipInstrumentEvent;
use crate::core::shared_atomics::SharedAtomics;

pub(crate) use catalog::scan_catalog;
pub(crate) use editor::InstrumentEditor;
pub(crate) use key_guard::{install_key_guard, take_toggle_editor_pending};
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
/// reclaim thread to drop off the audio thread. The worst realistic burst is
/// one full project reload — a `Remove` per instrument track, `MAX_TRACKS` at
/// most — before the reclaim thread's next drain; 2× that. A full ring (the
/// reclaim thread stalled through several plugin swaps — not reachable in
/// practice) falls back to dropping the voice on the audio thread.
const DEAD_VOICE_RING_CAPACITY: usize = 2 * MAX_TRACKS;

/// Handle to the running plugin host, kept by `Display`. Used to load plugins
/// ([`load_instrument`]), send runtime commands, and coordinate app-exit
/// shutdown.
pub(crate) struct PluginAudioHandle {
    /// Runtime commands to the mixer (insert / remove voices). A realtime-safe
    /// `rtrb` ring producer — the mixer pops from the other end on the audio
    /// callback thread, where a `crossbeam_channel`'s internal allocate/free
    /// isn't safe.
    pub(crate) cmd_tx: Producer<PluginHostCommand>,
    /// The `"plugin-host"` reclaim thread acks each removed track here once its
    /// voice has been dropped off the audio thread — the UI then drops the
    /// matching `!Send` instance.
    pub(crate) voice_dropped_rx: Receiver<usize>,
    /// App-exit coordination — see [`HostShutdown`].
    pub(crate) shutdown: Arc<HostShutdown>,
    /// Device sample rate the engine opened at — plugins are built against it.
    sample_rate: f64,
    /// Shared egui context slot, for a plugin to wake the reactive UI (CLAP's
    /// main-thread callback requests).
    repaint: Arc<OnceLock<egui::Context>>,
}

/// Builds the instrument mixer, hands it to the running audio engine as a
/// second source, and spawns the `"plugin-host"` thread that reclaims removed
/// plugin voices off the audio thread. Call once from `main`; `clip_midi_rx` /
/// `live_midi_rx` are the tagged MIDI feeds — `(track slot, [`Midi3`])` — for
/// per-track clip events and live keyboard notes respectively (two separate
/// `rtrb` rings, since `rtrb` is strict single-producer and the two feeds have
/// different producer threads). `repaint` is the app's shared egui context
/// slot, handed to every plugin loaded later. Any failure is returned as `Err`
/// and the whole feature is disabled.
pub(crate) fn start_plugin_host(
    engine: &mut EngineHandle,
    clip_midi_rx: Consumer<ClipInstrumentEvent>,
    live_midi_rx: Consumer<(usize, Midi3)>,
    atomics: &SharedAtomics,
    repaint: Arc<OnceLock<egui::Context>>,
) -> Result<PluginAudioHandle, String> {
    let shutdown = Arc::new(HostShutdown::default());
    let (cmd_tx, cmd_rx) = RingBuffer::new(CMD_RING_CAPACITY);
    let (dead_tx, dead_rx) = RingBuffer::new(DEAD_VOICE_RING_CAPACITY);
    let (voice_dropped_tx, voice_dropped_rx) = unbounded::<usize>();

    let transport = TransportState {
        running: Arc::clone(&atomics.running),
        tempo_us: Arc::clone(&atomics.tempo),
        meter: Arc::clone(&atomics.meter),
        playback_tick: Arc::clone(&atomics.playback_tick),
        region_start: Arc::clone(&atomics.region_start),
        region_end: Arc::clone(&atomics.region_end),
        loop_enabled: Arc::clone(&atomics.loop_enabled),
    };

    let mixer = InstrumentMixer::new(
        cmd_rx,
        dead_tx,
        clip_midi_rx,
        live_midi_rx,
        transport,
        Arc::clone(&atomics.track_mix),
        Arc::clone(&shutdown),
    );
    engine.add_source(Box::new(mixer) as Box<dyn AudioSource>)?;

    let reclaim_shutdown = Arc::clone(&shutdown);
    thread::Builder::new()
        .name("plugin-host".to_string())
        .spawn(move || reclaim_voices(dead_rx, voice_dropped_tx, reclaim_shutdown))
        .expect("failed to spawn plugin-host thread");

    dprintln!("plugin host: mixer attached to the audio engine");

    Ok(PluginAudioHandle {
        cmd_tx,
        voice_dropped_rx,
        shutdown,
        sample_rate: engine.sample_rate,
        repaint,
    })
}

/// The `"plugin-host"` thread: drops plugin voices the mixer removed (a plugin
/// teardown must not run in the audio callback) and acks the track back to the
/// UI so it can drop the matching `!Send` instance. Voices still loaded at app
/// exit are deliberately left to leak — dropping them races the plugin bundles'
/// static destructors that `exit()` runs (see [`HostShutdown`]).
fn reclaim_voices(
    mut dead_rx: Consumer<(usize, Box<dyn InstrumentVoice>)>,
    voice_dropped_tx: Sender<usize>,
    shutdown: Arc<HostShutdown>,
) {
    let drain = |rx: &mut Consumer<(usize, Box<dyn InstrumentVoice>)>| {
        while let Ok((track, voice)) = rx.pop() {
            drop(voice);
            voice_dropped_tx.send(track).ok();
        }
    };

    shutdown.register_thread();
    while !shutdown.is_requested() {
        drain(&mut dead_rx);
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
) -> Result<Box<dyn InstrumentEditor>, String> {
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
    /// The main-thread half.
    editor: Box<dyn InstrumentEditor>,
}

impl PreparedInstrument {
    /// Sends the voice to the mixer for its slot and hands back the editor —
    /// the slot must be empty by then (its old voice's `Remove` sent first).
    pub(crate) fn insert(
        self,
        handle: &mut PluginAudioHandle,
    ) -> Result<Box<dyn InstrumentEditor>, String> {
        let PreparedInstrument {
            track,
            voice,
            editor,
        } = self;
        handle
            .cmd_tx
            .push(PluginHostCommand::Insert { track, voice })
            .map_err(|_| "plugin host audio thread is gone (command ring full)".to_string())?;
        Ok(editor)
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
    let (mut voice, editor) = match entry.format {
        PluginFormat::Clap => clap::load(handle, track, entry, state)?,
        PluginFormat::Vst3 => vst3::load(handle, track, entry, state)?,
    };
    // Every format starts from silence, whatever its restored state believes
    // is held. Queued here, on the main thread, before `Insert`.
    for msg in note_reset_messages() {
        voice.queue_midi(msg, 0);
    }
    dprintln!(
        "plugin host: loaded {} '{}' on track {}",
        entry.format.label(),
        entry.name,
        track + 1
    );
    Ok(PreparedInstrument {
        track,
        voice,
        editor,
    })
}
