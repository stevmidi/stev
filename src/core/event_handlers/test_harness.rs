//! Test-only harness for the `EventHandlers` workflows: real handlers over
//! unbounded channels, with the `UiEvent`, transport and sequencer-command
//! streams kept for inspection, and a sequencer with track 0 selected whose shared atomics the
//! test can read and set. Shared by the workflow test modules so the
//! 14-argument `Sequencer::new` setup lives in one place.

use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16},
};

use crossbeam_channel::{Receiver, unbounded};
use rtrb::RingBuffer;
use undo::Record;
use uuid::Uuid;

use crate::core::sequencer::test_support::{clip_at, note_off, note_on};
use crate::core::sequencer::{Sequencer, SequencerCommand, SequencerEdit};
use crate::core::shared_atomics::{LiveRecState, TrackMixAtomics};
use crate::core::time::{Meter, TapTempo};
use crate::core::transport::TransportCommand;
use crate::view::display::UiEvent;

use super::{EventHandlers, SavedProject};

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
    /// The project as last saved or loaded — the sequencer's empty one to
    /// start with.
    pub(crate) saved: SavedProject,
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
    let handlers = EventHandlers::new(
        ui_event_tx,
        transport_command_tx,
        sequencer_command_tx,
        unbounded().0,
        Arc::new(AtomicU8::new(0)),
        has_event_selection.clone(),
        Arc::new(AtomicI32::new(0)),
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
        playback_tick.clone(),
        Arc::new(AtomicI32::new(0)), // elapsed_ticks
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
        LiveRecState {
            last_note_on: Arc::new(AtomicU8::new(0)),
            last_note_velocity: Arc::new(AtomicU8::new(0)),
            elapsed_start_tick: Arc::new(AtomicI32::new(0)),
            thumbnail_snapshot: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let track_id = sequencer.track_id_by_index(0).unwrap();
    sequencer.select_track(Some(track_id));

    let saved = SavedProject::of(&sequencer);
    Harness {
        handlers,
        ui_events,
        transport_commands,
        sequencer_commands,
        has_event_selection,
        cursor_tick,
        playback_tick,
        running,
        region,
        sequencer,
        saved,
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
        &mut h.saved,
        &mut None,
    );
}
