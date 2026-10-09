//! [`EventHandlers`] — the stateless bridge from input to the `"sequencer"`
//! thread.
//!
//! It holds only channel senders and `Arc` refs (`080-conventions.md`); all
//! business logic lives in `sequencer/` or `transport.rs`. `handle_*_command`
//! in `input_handler.rs` / `transport_handler.rs` / `sequencer_handler.rs`
//! matches every command; anything that grows past a line or is shared between
//! arms is extracted to a per-concern file here — `selection.rs`,
//! `clip_edges.rs`, `clip_lifecycle.rs`, `clip_range_edits.rs`, `clip_view.rs`,
//! `project.rs`, `tracks.rs`, `live_recording.rs`,
//! `command_helpers.rs`, `edit_result_handler.rs`, … There is no single
//! `workflows.rs`; this per-concern split *is* the DRY mechanism.
//!
//! The one non-channel/atomic field is `repaint_ctx` (see its doc) — the
//! deliberate exception that lets a background thread wake the reactive UI.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam_channel::Sender;
use uuid::Uuid;

use crate::{
    core::{
        metronome::Metronome,
        note_logger::NoteLoggerCommand,
        sequencer::{Sequencer, SequencerCommand},
        time::sixteenth_straight_ticks,
        transport::{Transport, TransportCommand, TransportEvent},
        view_state::{Pane, ViewState},
    },
    metadata::clip_metadata::ClipMetadata,
    view::display::UiEvent,
};

mod clip_edges;
mod clip_lifecycle;
mod clip_range_edits;
mod clip_view;
mod command_helpers;
mod edit_result_handler;
mod input_handler;
mod live_recording;
mod performance_lane_handler;
mod project;
mod selection;
mod sequencer_handler;
#[cfg(test)]
mod test_harness;
mod tracks;
mod transport_handler;
mod ui_helpers;

pub(crate) use project::SavedProject;

/// Stateless senders + `Arc` refs, moved into the `"sequencer"` thread at
/// startup. See the module docs.
pub(crate) struct EventHandlers {
    // --- Channels ---
    /// Change notifications to `Display`.
    ui_event_tx: Sender<UiEvent>,
    /// Transport-only commands to the tick pump.
    transport_command_tx: Sender<TransportCommand>,
    /// The main command stream to the `"sequencer"` thread.
    sequencer_command_tx: Sender<SequencerCommand>,
    /// Commands to the `"midiout"` thread's `NoteLogger`.
    note_logger_command_tx: Sender<NoteLoggerCommand>,

    // --- Shared atomics ---
    /// The active [`ViewState`] discriminant.
    view_state: Arc<AtomicU8>,
    /// Whether the open clip has selected events — see
    /// `SharedAtomics::has_event_selection`.
    has_event_selection: Arc<AtomicBool>,
    /// Live mirror of the persisted MIDI-output offset, published for the
    /// `"midiout"` thread when the MIDI settings modal is confirmed. See
    /// `160-midi-out-offset.md`.
    midi_out_offset_ms: Arc<AtomicI32>,

    /// Handle to the real `egui::Context`, filled in once at startup (see
    /// `main.rs`) after `eframe::run_native` hands it over — `EventHandlers`
    /// is constructed and moved into the sequencer thread before that
    /// happens, hence `OnceLock` rather than a plain field. Lets background
    /// threads (e.g. a performance-lane trigger arriving from a physical
    /// MIDI device, which never touches the OS window) wake the reactive
    /// UI exactly when something changed, instead of polling.
    repaint_ctx: Arc<OnceLock<egui::Context>>,
}

impl EventHandlers {
    // --- Constructor ---
    /// Wires up the senders and atomics. `repaint_ctx` starts empty and is
    /// filled once from `main.rs`. Parameter order: channels, then atomics
    /// (`080-conventions.md`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ui_event_tx: Sender<UiEvent>,
        transport_command_tx: Sender<TransportCommand>,
        sequencer_command_tx: Sender<SequencerCommand>,
        note_logger_command_tx: Sender<NoteLoggerCommand>,
        view_state: Arc<AtomicU8>,
        has_event_selection: Arc<AtomicBool>,
        midi_out_offset_ms: Arc<AtomicI32>,
        repaint_ctx: Arc<OnceLock<egui::Context>>,
    ) -> Self {
        EventHandlers {
            ui_event_tx,
            transport_command_tx,
            sequencer_command_tx,
            note_logger_command_tx,
            view_state,
            has_event_selection,
            midi_out_offset_ms,
            repaint_ctx,
        }
    }

    /// Wakes the reactive UI thread. See `repaint_ctx` doc — a no-op if
    /// called before `eframe::run_native` has handed over its context
    /// (can't happen in practice: nothing that calls this runs before the
    /// window exists).
    fn request_repaint(&self) {
        if let Some(ctx) = self.repaint_ctx.get() {
            ctx.request_repaint();
        }
    }

    /// Sends a transport command, ignoring a closed channel.
    fn send_transport(&self, cmd: TransportCommand) {
        self.transport_command_tx.send(cmd).ok();
    }

    /// Sends a sequencer command, ignoring a closed channel.
    fn send_sequencer(&self, cmd: SequencerCommand) {
        self.sequencer_command_tx.send(cmd).ok();
    }

    /// The active view.
    fn view_state(&self) -> ViewState {
        ViewState::from_u8(self.view_state.load(Ordering::Relaxed))
    }

    /// Switches the active view.
    fn set_view_state(&self, state: ViewState) {
        self.view_state.store(state as u8, Ordering::Relaxed);
    }

    /// Whether the open clip has selected events (the mirror
    /// `publish_event_selection` keeps).
    fn has_event_selection(&self) -> bool {
        self.has_event_selection.load(Ordering::Relaxed)
    }
}
