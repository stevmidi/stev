//! Test-only harness for the `EventHandlers` workflows: real handlers over
//! unbounded channels, with the `UiEvent`, transport and sequencer-command
//! streams kept for inspection, and a sequencer with track 0 selected whose shared atomics the
//! test can read and set. Shared by the workflow test modules so the
//! 14-argument `Sequencer::new` setup lives in one place.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16},
};

use crossbeam_channel::{Receiver, unbounded};
use rtrb::RingBuffer;
use undo::Record;
use uuid::Uuid;

use crate::core::project::{LibraryChange, ProjectAction, StorageJob};
use crate::core::sequencer::test_support::{clip_at, note_off, note_on};
use crate::core::sequencer::{Sequencer, SequencerCommand, SequencerEdit};
use crate::core::shared_atomics::{AudioExportAtomics, TrackMixAtomics};
use crate::core::time::{Meter, TapTempo};
use crate::core::transport::TransportCommand;
use crate::view::display::UiEvent;

use super::{EventHandlers, OpenProject, RecoveryKeeper, SavedProject, Storage};

/// The handlers plus what a test inspects or sets up: the `UiEvent` and
/// transport-command streams they send, the published selection mirror, and
/// the transport state the sequencer shares — cursor, playback position,
/// running flag and loop region — which a test sets directly, since there is
/// no transport thread.
pub(crate) struct Harness {
    /// The handlers under test.
    pub(crate) handlers: EventHandlers,
    /// Everything they sent to the view.
    pub(crate) ui_events: Receiver<UiEvent>,
    /// Everything they sent to the transport.
    pub(crate) transport_commands: Receiver<TransportCommand>,
    /// Everything they sent to the `"sequencer"` thread.
    pub(crate) sequencer_commands: Receiver<SequencerCommand>,
    /// `SharedAtomics::has_event_selection`.
    pub(crate) has_event_selection: Arc<AtomicBool>,
    /// `SharedAtomics::audio_export`.
    pub(crate) audio_export: Arc<AudioExportAtomics>,
    /// The transport cursor, shared with the sequencer.
    pub(crate) cursor_tick: Arc<AtomicI32>,
    /// The playback position, shared with the sequencer.
    pub(crate) playback_tick: Arc<AtomicI32>,
    /// Whether the transport is running, shared with the sequencer.
    pub(crate) running: Arc<AtomicBool>,
    /// The loop region, shared with the sequencer: `(start, end)`.
    pub(crate) region: (Arc<AtomicI32>, Arc<AtomicI32>),
    /// A sequencer with track 0 selected.
    pub(crate) sequencer: Sequencer,
    /// The project as a file: as last saved or loaded (the sequencer's empty
    /// one to start with), and the crash recovery check, with no `"storage"`
    /// thread behind it.
    pub(crate) open: OpenProject,
    /// Everything `open.recovery` sent to the `"storage"` thread, in order.
    pub(crate) storage_jobs: Receiver<StorageJob>,
    /// The flag a failed write sets, for a test to set.
    pub(crate) recovery_write_failed: Arc<AtomicBool>,
}

/// Handlers over unbounded channels (every receiver but the `UiEvent`,
/// transport and sequencer-command ones dropped, so sends are silent
/// no-ops) and a
/// sequencer with track 0 selected.
pub(crate) fn harness() -> Harness {
    let (ui_event_tx, ui_events) = unbounded();
    let (transport_command_tx, transport_commands) = unbounded();
    let (sequencer_command_tx, sequencer_commands) = unbounded();
    let has_event_selection = Arc::new(AtomicBool::new(false));
    let audio_export = Arc::new(AudioExportAtomics::default());
    let handlers = EventHandlers::new(
        ui_event_tx,
        transport_command_tx,
        sequencer_command_tx,
        unbounded().0,
        unbounded().0,
        Arc::new(AtomicU8::new(0)),
        has_event_selection.clone(),
        Arc::new(AtomicI32::new(0)),
        audio_export.clone(),
        Arc::new(OnceLock::new()),
    );

    let cursor_tick = Arc::new(AtomicI32::new(0));
    let playback_tick = Arc::new(AtomicI32::new(0));
    let running = Arc::new(AtomicBool::new(false));
    let region = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(3840)));
    let (plugin_midi_tx, _plugin_midi_rx) = RingBuffer::new(64);
    let mut sequencer = Sequencer::new(
        unbounded().0,
        plugin_midi_tx,
        RingBuffer::new(8).0,
        playback_tick.clone(),
        cursor_tick.clone(),
        region.0.clone(),
        region.1.clone(),
        Arc::new(AtomicBool::new(true)), // loop_enabled
        running.clone(),
        Arc::new(AtomicI32::new(500_000)),
        Arc::new(AtomicU16::new(Meter::FOUR_FOUR.to_bits())),
        Arc::new(AtomicU8::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(TrackMixAtomics::new()),
        audio_export.clone(),
    );
    let track_id = sequencer.track_id_by_index(0).unwrap();
    sequencer.select_track(Some(track_id));

    let saved = SavedProject::of(&sequencer, None);
    let (storage_job_tx, storage_jobs) = unbounded();
    let recovery_write_failed = Arc::new(AtomicBool::new(false));
    let recovery = RecoveryKeeper::new(
        Some(storage_job_tx.clone()),
        recovery_write_failed.clone(),
        None,
    );
    let storage = Storage::new(storage_job_tx, None);
    Harness {
        handlers,
        ui_events,
        transport_commands,
        sequencer_commands,
        has_event_selection,
        audio_export,
        cursor_tick,
        playback_tick,
        running,
        region,
        sequencer,
        open: OpenProject::new(saved, recovery, storage),
        storage_jobs,
        recovery_write_failed,
    }
}

/// Adds a half-bar clip (a 1920-tick window) at `start_tick` on track 0 (the
/// selected one) holding one note; returns the clip's and the note's ids.
pub(crate) fn add_clip(sequencer: &mut Sequencer, start_tick: i32) -> (Uuid, Uuid) {
    let mut clip = clip_at(start_tick, 1920);
    let note = note_on(0);
    let note_id = note.id();
    clip.add_event(note);
    clip.add_event(note_off(240));
    clip.calculate_note_lengths();
    let clip_id = clip.id();
    sequencer.tracks_mut()[0].add_clip(&clip);
    (clip_id, note_id)
}

/// Runs `cmd` through the handlers, recording on `record`.
pub(crate) fn run_command(
    h: &mut Harness,
    record: &mut Record<SequencerEdit>,
    cmd: SequencerCommand,
) {
    h.handlers.handle_sequencer_command(
        &cmd,
        &mut h.sequencer,
        record,
        &mut TapTempo::default(),
        &mut h.open,
    );
}

/// Runs `cmd` with an undo record of its own.
pub(crate) fn run_once(h: &mut Harness, cmd: SequencerCommand) {
    let mut record: Record<SequencerEdit> = Record::new();
    run_command(h, &mut record, cmd);
}

/// Asks to quit, past the prompt when `discard_changes`.
pub(crate) fn quit(h: &mut Harness, discard_changes: bool) {
    let cmd = SequencerCommand::ProjectAction {
        action: ProjectAction::Quit,
        discard_changes,
    };
    run_once(h, cmd);
}

/// Saves as `name` at the projects root: the handover alone, the outcome
/// still to come ([`written`]).
pub(crate) fn save_as(h: &mut Harness, name: &str) {
    let cmd = SequencerCommand::SaveProject {
        filename: name.to_owned(),
        folder: None,
    };
    run_once(h, cmd);
}

/// The `"storage"` thread reports on the oldest save on its way: written,
/// or failed with `error`.
pub(crate) fn written(h: &mut Harness, error: Option<&str>) {
    let outcome = error.map_or(Ok(()), |e| Err(e.to_owned()));
    run_once(h, SequencerCommand::ProjectWritten(outcome));
}

/// Runs the browser's `change` to the library through the sequencer
/// (`SequencerCommand::LibraryChanged`).
pub(crate) fn library_changed(h: &mut Harness, change: LibraryChange) {
    run_once(h, SequencerCommand::LibraryChanged(change));
}

/// The footer lines sent since the last call; every other `UiEvent` is
/// dropped.
pub(crate) fn statuses(h: &Harness) -> Vec<String> {
    h.ui_events
        .try_iter()
        .filter_map(|event| match event {
            UiEvent::Status { message } => Some(message),
            _ => None,
        })
        .collect()
}
