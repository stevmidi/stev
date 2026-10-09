//! macOS instrument plugin host — the `Display`-side glue: per-track plugin
//! load / teardown, editor visibility (`v`), and the per-frame editor pump.
//!
//! All the `Display`-side plugin state is grouped into [`InstrumentHost`], held
//! as `Display::instruments`. The `!Send` plugin instances live behind the
//! boxed [`InstrumentEditor`]s in `self.instruments.track_instruments` and
//! never leave this (the eframe main) thread. That array, like the mixer's
//! voices, is indexed by engine slot (`TrackLane::slot`), so adding or
//! removing a track moves no plugin; the methods here take a track's
//! position and look its slot up in the `tracks` mirror. The matching audio-thread voices
//! run in the shared audio engine's mixer; `Display` talks to it through
//! [`PluginAudioHandle`].
//!
//! Nothing here names a plugin format: `load_instrument` picks the format from
//! the catalog entry and hands back a boxed editor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};

use crate::core::config::MAX_TRACKS;
use uuid::Uuid;

use crate::core::input_event::InputEvent;
use crate::core::plugin_host::catalog::PluginCatalogEntry;
use crate::core::plugin_host::{
    InstrumentEditor, PluginAudioHandle, PluginHostCommand, load_instrument,
    take_toggle_editor_pending,
};
use crate::models::track::{InstrumentRef, TrackOutput};

use super::browser::BrowserPlugin;
use super::status_message::StatusMessage;
use super::{Display, ViewState};

/// The macOS CLAP instrument host state, grouped out of [`Display`] — the
/// running audio-thread handle, the per-track `!Send` editor instances, the
/// pending-teardown queue and the background plugin-catalog scan. `Display` holds exactly one of these behind
/// `#[cfg(target_os = "macos")]`; every method that drives it stays on
/// `Display` (in this module) and reaches in through `self.instruments`.
pub(super) struct InstrumentHost {
    /// Handle to the running plugin host. `Some` once `main` has started it via
    /// [`Display::attach_instrument_audio`]. Used to load plugins and
    /// coordinate shutdown.
    pub(super) audio: Option<PluginAudioHandle>,
    /// Mirrors `SharedAtomics.live_instrument_target` — written whenever the
    /// selected/plugin-loaded track changes, read by `MidiInputForwarder` (a
    /// different thread) to tag live-thru MIDI for the mixer. Set once from
    /// `attach_instrument_audio`; a fresh, unshared placeholder until then (never
    /// written to in that state, since every writer is gated on `audio`
    /// being loaded).
    pub(super) live_instrument_target: Arc<AtomicI32>,
    /// The plugin of each track that currently hosts one, by engine slot.
    /// Each editor holds a `!Send` plugin handle, so this must never leave
    /// the eframe main thread. Pumped once per frame from `update`.
    pub(super) track_instruments: [Option<TrackInstrument>; MAX_TRACKS],
    /// The live state of each plugin whose track left the arrangement, by
    /// track id — what an undo bringing the track back reloads it from
    /// (`restore_slot_instrument`). Session-only; cleared on project load.
    pub(super) parked_states: HashMap<Uuid, Vec<u8>>,
    /// Editors whose plugin was removed, by the slot it was in, awaiting the
    /// mixer's voice-dropped ack before the instance can be deactivated and
    /// dropped.
    pub(super) pending_instance_drop: Vec<(usize, Box<dyn InstrumentEditor>)>,
    /// Catalog of installed instrument plugins for the browser's Plugins
    /// category, `None`
    /// until the background scan (`plugin_catalog_rx`) delivers it.
    pub(super) plugin_catalog: Option<Vec<PluginCatalogEntry>>,
    /// Receives the catalog from the background `"plugin-catalog-scan"` thread
    /// — once per plugin format, each message carrying the whole catalog so far
    /// (VST3 discovery is slow enough that waiting for it would hide the CLAP
    /// plugins too). Polled every frame in `pump_instrument_editors` and
    /// dropped once the channel disconnects, which is what marks the scan
    /// complete. `Some` therefore also means "still scanning". `None` if
    /// `attach_plugin_catalog_rx` was never called, e.g. `start_plugin_host`
    /// failed.
    pub(super) plugin_catalog_rx: Option<Receiver<Vec<PluginCatalogEntry>>>,
}

/// A track's loaded plugin: its editor and the catalog entry it came from.
pub(super) struct TrackInstrument {
    /// The plugin instance's `Display`-side half. Boxed because the concrete
    /// type depends on the plugin's format.
    pub(super) editor: Box<dyn InstrumentEditor>,
    /// What it was loaded from — what the browser compares a pick against,
    /// and names when it replaces it.
    pub(super) entry: PluginCatalogEntry,
}

impl InstrumentHost {
    /// An idle host: no audio handle, no editors, empty catalog.
    pub(super) fn new() -> Self {
        InstrumentHost {
            audio: None,
            live_instrument_target: Arc::new(AtomicI32::new(-1)),
            track_instruments: std::array::from_fn(|_| None),
            parked_states: HashMap::new(),
            pending_instance_drop: Vec::new(),
            plugin_catalog: None,
            plugin_catalog_rx: None,
        }
    }
}

impl Display {
    /// Per-frame: finish any pending per-track teardown, service a `v` the
    /// key guard (`key_guard`) intercepted while an editor window had OS
    /// keyboard focus, then pump every editor and schedule the next repaint
    /// at the shortest plugin timer cadence.
    pub(super) fn pump_instrument_editors(&mut self, ctx: &egui::Context) {
        // Drain the background catalog scan. It reports once per plugin
        // format, each message carrying the whole catalog so far, so keep the
        // last one and hold the receiver until the scan thread drops its
        // sender — a disconnected channel is what marks the scan complete.
        if let Some(rx) = &self.instruments.plugin_catalog_rx {
            let mut disconnected = false;
            let mut changed = false;
            loop {
                match rx.try_recv() {
                    Ok(catalog) => {
                        self.instruments.plugin_catalog = Some(catalog);
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected {
                self.instruments.plugin_catalog_rx = None;
            }
            // The browser's Plugins category lists the catalog as it grows,
            // and drops its "Scanning…" row once the scan is done.
            if changed || disconnected {
                self.sync_browser_plugins();
            }
        }

        // Mirrors the in-focus `v` binding's own guard (`forward_input_event`)
        // — the key guard has no access to `view_state`, so it always
        // swallows a bare `v` aimed at one of our editor windows and leaves
        // this check to us.
        if take_toggle_editor_pending()
            && self.view_state() == ViewState::Arranger
            && self.overlay.is_none()
        {
            self.toggle_instrument_editor(self.selected_track_idx);
        }

        // Complete teardown for slots whose voice the mixer has now dropped.
        if let Some(handle) = &self.instruments.audio {
            while let Ok(slot) = handle.voice_dropped_rx.try_recv() {
                if let Some(pos) = self
                    .instruments
                    .pending_instance_drop
                    .iter()
                    .position(|(s, _)| *s == slot)
                {
                    let (_, mut editor) = self.instruments.pending_instance_drop.remove(pos);
                    editor.deactivate();
                    // `editor` drops here: GUI already torn down, instance is now
                    // the sole owner → clean destroy.
                }
            }
        }

        // An open project dialog must not sit behind a floating editor.
        let yield_to_main = self.project_dialog.is_some();
        let mut next: Option<Duration> = None;
        for slot in &mut self.instruments.track_instruments {
            if let Some(TrackInstrument { editor, .. }) = slot
                && let Some(interval) = editor.pump(yield_to_main)
            {
                next = Some(next.map_or(interval, |n| n.min(interval)));
            }
        }
        if let Some(interval) = next {
            ctx.request_repaint_after(interval);
        }
    }

    /// The installed instrument plugins, as scanned by the background
    /// `"plugin-catalog-scan"` thread — empty until it delivers
    /// (`pump_instrument_editors` polls it every frame). Whether the scan is
    /// still running is [`plugin_catalog_scanning`](Self::plugin_catalog_scanning).
    pub(super) fn plugin_catalog(&self) -> &[PluginCatalogEntry] {
        self.instruments.plugin_catalog.as_deref().unwrap_or(&[])
    }

    /// Where the plugin `plugin_id` in the bundle at `bundle_path` sits in
    /// the catalog, if it is installed.
    fn catalog_index_of(&self, bundle_path: &Path, plugin_id: &str) -> Option<usize> {
        self.plugin_catalog()
            .iter()
            .position(|e| e.is(bundle_path, plugin_id))
    }

    /// The plugin loaded for the track at `track_idx`, if any.
    pub(super) fn track_instrument(&self, track_idx: usize) -> Option<&TrackInstrument> {
        self.instruments
            .track_instruments
            .get(self.track_slot(track_idx)?)?
            .as_ref()
    }

    /// Whether the background catalog scan is still running. The catalog is
    /// delivered per format, so it can be non-empty and still growing; the
    /// browser shows a "Scanning…" row while this is true so a plugin that
    /// hasn't been found *yet* doesn't read as not installed.
    pub(super) fn plugin_catalog_scanning(&self) -> bool {
        self.instruments.plugin_catalog_rx.is_some()
    }

    /// Loads `catalog_index`'s plugin into engine slot `slot`, applying
    /// `state` (the persisted preset blob; empty on a pick) before
    /// activation. Any plugin already in the slot is torn down first.
    ///
    /// `picked` — a browser pick, naming the track's position — opens the
    /// editor now and sends `SetTrackOutput` to route that track's output to
    /// the plugin. A restore (project load, an undo bringing a plugin track
    /// back) passes `None`: no editor until the user presses `v`, and no
    /// announce, since the sequencer already holds the right `TrackOutput`
    /// (with its state) and re-announcing would clobber it.
    pub(super) fn load_slot_instrument(
        &mut self,
        slot: usize,
        catalog_index: usize,
        state: &[u8],
        picked: Option<usize>,
    ) {
        let Some(entry) = self.plugin_catalog().get(catalog_index).cloned() else {
            return;
        };
        if self.instruments.audio.is_none() || slot >= self.instruments.track_instruments.len() {
            return;
        }

        self.remove_slot_instrument(slot);

        let loaded = {
            let handle = self.instruments.audio.as_mut().expect("checked above");
            load_instrument(handle, slot, &entry, Some(state))
        };
        match loaded {
            Ok(mut editor) => {
                if let Some(track_idx) = picked {
                    editor.show();
                    let instrument = InstrumentRef {
                        bundle_path: entry.bundle_path.clone(),
                        plugin_id: entry.plugin_id.clone(),
                        display_name: entry.name.clone(),
                        state: Vec::new(),
                    };
                    self.input_event_tx
                        .send(InputEvent::SetTrackOutput {
                            track: track_idx,
                            output: TrackOutput::Instrument(instrument),
                        })
                        .ok();
                }
                self.instruments.track_instruments[slot] = Some(TrackInstrument { editor, entry });
                self.sync_live_instrument_target();
            }
            Err(e) => {
                eprintln!("plugin host: failed to load '{}': {e}", entry.name);
            }
        }
    }

    /// Puts `plugin`, picked in the browser's Plugins category (Enter on
    /// the selected track, or a drop), on `track_idx`: loads it and opens its
    /// editor, replacing any other plugin — not undoable, the footer says so.
    /// A no-op when the track already has it.
    pub(super) fn put_plugin_on_track(&mut self, track_idx: usize, plugin: &BrowserPlugin) {
        let (bundle_path, plugin_id) = (&plugin.bundle_path, plugin.plugin_id.as_str());
        let current = self.track_instrument(track_idx).map(|loaded| &loaded.entry);
        if current.is_some_and(|entry| entry.is(bundle_path, plugin_id)) {
            return;
        }
        let replaced = current.map(|entry| entry.name.clone());
        let (Some(idx), Some(slot)) = (
            self.catalog_index_of(bundle_path, plugin_id),
            self.track_slot(track_idx),
        ) else {
            return;
        };
        self.load_slot_instrument(slot, idx, &[], Some(track_idx));
        let track = track_idx + 1;
        let loaded = self.track_instrument(track_idx).is_some();
        let message = match (replaced, loaded) {
            (_, false) => format!("Could not load {} on track {track}", plugin.name),
            (Some(old), true) => format!("Replaced {old} with {} on track {track}", plugin.name),
            (None, true) => return,
        };
        self.render.status = Some(StatusMessage::new(message));
    }

    /// Captures every loaded plugin's current preset and hands it to the
    /// sequencer, so the imminent project save persists it. Called from the
    /// ⌘S handler just before `ConfirmFilename`; the events queue ahead of the
    /// save on the same channel.
    pub(super) fn capture_instrument_states(&mut self) {
        for slot in 0..self.instruments.track_instruments.len() {
            // Skip a plugin with no `state` extension or a failed save — better
            // to keep the last good blob than overwrite it with nothing.
            let Some(state) = self.save_slot_instrument_state(slot) else {
                continue;
            };
            self.input_event_tx
                .send(InputEvent::CaptureTrackInstrumentState { slot, state })
                .ok();
        }
    }

    /// The live state of the plugin in `slot`, `None` with no plugin there, no
    /// `state` extension or a failed save.
    fn save_slot_instrument_state(&mut self, slot: usize) -> Option<Vec<u8>> {
        let TrackInstrument { editor, .. } =
            self.instruments.track_instruments.get_mut(slot)?.as_mut()?;
        editor.save_state()
    }

    /// Removes `track_idx`'s plugin — see
    /// [`remove_slot_instrument`](Self::remove_slot_instrument).
    pub(super) fn remove_track_instrument(&mut self, track_idx: usize) {
        if let Some(slot) = self.track_slot(track_idx) {
            self.remove_slot_instrument(slot);
        }
    }

    /// Removes the plugin in `slot`: tears down its GUI, tells the mixer to
    /// drop the voice, and stashes the editor until the voice-dropped ack
    /// arrives.
    fn remove_slot_instrument(&mut self, slot: usize) {
        let Some(TrackInstrument { mut editor, .. }) = self
            .instruments
            .track_instruments
            .get_mut(slot)
            .and_then(Option::take)
        else {
            return;
        };
        editor.teardown_gui();
        if let Some(handle) = &mut self.instruments.audio {
            handle
                .cmd_tx
                .push(PluginHostCommand::Remove { track: slot })
                .ok();
        }
        self.instruments.pending_instance_drop.push((slot, editor));
        self.sync_live_instrument_target();
    }

    /// A track left the arrangement (`UiEvent::TrackInstrumentRemoved`): if
    /// `slot` holds a plugin, keeps its live state under `track_id` (so an
    /// undo reloads it as it was, not as of the last save) and tears it down.
    pub(super) fn park_track_instrument(&mut self, slot: usize, track_id: Uuid) {
        if let Some(state) = self.save_slot_instrument_state(slot) {
            self.instruments.parked_states.insert(track_id, state);
        }
        self.remove_slot_instrument(slot);
    }

    /// Reloads `want` into engine slot `slot`, editor closed — a project load
    /// (`track_id` `None`), or a plugin track coming back into the
    /// arrangement (`UiEvent::TrackInstrumentRestored`), which prefers the
    /// live state parked for `track_id` over `want`'s blob.
    pub(super) fn restore_slot_instrument(
        &mut self,
        slot: usize,
        track_id: Option<Uuid>,
        want: &InstrumentRef,
    ) {
        let parked = track_id.and_then(|id| self.instruments.parked_states.remove(&id));
        match self.catalog_index_of(&want.bundle_path, &want.plugin_id) {
            Some(idx) => {
                let state = parked.as_deref().unwrap_or(&want.state);
                self.load_slot_instrument(slot, idx, state, None);
            }
            None => eprintln!("plugin host: '{}' is not installed", want.display_name),
        }
    }

    /// Opens or closes the selected track's plugin editor (`v` in the
    /// Arranger). Closing destroys the plugin's GUI rather than hiding it — see
    /// [`InstrumentEditor::close`].
    pub(super) fn toggle_instrument_editor(&mut self, track_idx: usize) {
        if let Some(TrackInstrument { editor, .. }) = self
            .track_slot(track_idx)
            .and_then(|slot| self.instruments.track_instruments.get_mut(slot))
            .and_then(Option::as_mut)
        {
            editor.toggle();
        }
    }

    /// Points the live keyboard MIDI feed at the plugin in engine slot
    /// `slot` (or `None`) by writing `live_instrument_target`, which
    /// `MidiInputForwarder` (a different thread) reads to tag each live-thru
    /// event before sending it to the mixer on `instrument_midi_tx`.
    pub(super) fn set_live_instrument_target(&self, slot: Option<usize>) {
        let value = slot.map_or(-1, |s| s as i32);
        self.instruments
            .live_instrument_target
            .store(value, Ordering::Relaxed);
    }

    /// Re-points the live target when the selected track changes.
    pub(super) fn sync_live_instrument_target(&self) {
        let target = self
            .track_instrument(self.selected_track_idx)
            .and(self.track_slot(self.selected_track_idx));
        self.set_live_instrument_target(target);
    }

    /// Rebuilds the loaded editors for a freshly loaded/created project: every
    /// current editor is torn down and every parked state dropped, then an
    /// editor-less plugin is loaded into the slot of each `Instrument` track
    /// named in `specs`.
    pub(super) fn sync_instruments_to_tracks(&mut self, specs: &[(usize, InstrumentRef)]) {
        for slot in 0..self.instruments.track_instruments.len() {
            self.remove_slot_instrument(slot);
        }
        self.instruments.parked_states.clear();
        for (slot, want) in specs {
            self.restore_slot_instrument(*slot, None, want);
        }
    }
}
