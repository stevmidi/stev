//! `EventHandlers` workflows for the take session (`290-framing-a-take.md`
//! § Phase 2, `sequencer/take_framing.rs`): the stopped `/` into a lead clip
//! opens it, and while it is open [`take_framing_command`](EventHandlers::take_framing_command)
//! stands in front of every [`SequencerCommand`]. The session has no keys of
//! its own: Enter inserts, Esc cancels, and `[`/`]`, the window band and the
//! cursor keys act on the take as on any clip; leaving the clip view cancels
//! it; transport, zoom, the mixer and note previews go through; everything
//! else that edits is blocked.

use undo::Record;

use crate::core::project::ProjectAction;
use crate::core::sequencer::{InsertCaptureEdit, SequencerEdit};
use crate::models::clip::Clip;

use super::*;

/// The footer's answer to a project command the session blocks.
const FINISH_TAKE_FIRST: &str = "Finish the take first: Enter inserts it, Esc cancels";

impl EventHandlers {
    /// The stopped `/` with a lead clip: opens a take session
    /// (`Sequencer::open_take_framing`). The clip view shows the take and
    /// takes the keyboard, the panel shown if it was hidden, as `Shift+Tab`
    /// does; the lead clip already holds the framed notes. Nothing opens with
    /// nothing to frame.
    pub(super) fn open_take_framing_workflow(&self, sequencer: &mut Sequencer) {
        if !sequencer.open_take_framing(self.view_state()) {
            return;
        }
        self.set_view_state(ViewState::Clip);
        self.publish_event_selection(sequencer);
        let start_tick = sequencer.selected_clip().map_or(0, Clip::start_tick);
        self.ui_event_tx
            .send(UiEvent::TakeFramingStarted {
                clip_view: sequencer.current_clip_view(),
                start_tick,
            })
            .ok();
        self.send_selected_clip_updated_ui_event(sequencer);
    }

    /// The session's hold on `cmd` (`290` § Session rules). `true` when it
    /// was handled here — done, or blocked — and the ordinary arm must not
    /// run; `false` with no session open, for the commands that go through,
    /// and for those that end the session first (it is cancelled here).
    pub(super) fn take_framing_command(
        &self,
        cmd: &SequencerCommand,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
    ) -> bool {
        if !sequencer.is_framing_take() {
            return false;
        }
        match cmd {
            // Enter inserts; Esc, ⌘Z and ⇧⌘Z cancel (the edge moves inside a
            // session are not undo steps of their own).
            SequencerCommand::FitTempo => self.finish_take_framing_workflow(sequencer, undo_record),
            SequencerCommand::ClearEventSelection
            | SequencerCommand::Undo
            | SequencerCommand::Redo => self.cancel_take_framing_workflow(sequencer),

            // The take's window, as a clip's: `[`/`]` at the cursor, the
            // band released on the grid. Not `ResizeClipEdit`s — the take
            // isn't in the arrangement.
            SequencerCommand::SetClipEdgeToCursor(edge) => {
                if sequencer.set_take_edge_to_cursor(*edge) {
                    self.send_selected_clip_updated_ui_event(sequencer);
                }
            }
            SequencerCommand::SetClipWindowEdge { edge, tick } => {
                if sequencer.set_take_edge_at(*edge, *tick) {
                    self.send_selected_clip_updated_ui_event(sequencer);
                }
            }

            // Leaving the clip view, another clip or track, and quitting
            // (so the unsaved-changes check sees the clip without the
            // temporary notes): cancel, then carry on. An arranger cursor
            // move cancels where it re-syncs the lead clip
            // (`sync_clip_selection_to_cursor_workflow`).
            SequencerCommand::ExitClip
            | SequencerCommand::FocusPane(Pane::Arranger)
            | SequencerCommand::SelectNextTrack
            | SequencerCommand::SelectPrevTrack
            | SequencerCommand::SelectTrackAt(_)
            | SequencerCommand::SelectPerformanceLane
            | SequencerCommand::SelectClipSpan { .. }
            | SequencerCommand::ProjectAction {
                action: ProjectAction::Quit,
                ..
            } => {
                self.cancel_take_framing_workflow(sequencer);
                return false;
            }

            // The cursor moves on the take as on any clip (`selected_clip`
            // is the take). Playing, auditioning, mixing and re-routing a
            // track go through (`Display` swaps a plugin before it asks);
            // `⌥Space` plays from where the take's cursor sounds in place.
            SequencerCommand::NudgeClipCursor(_)
            | SequencerCommand::NudgeClipCursorByGrid { .. }
            | SequencerCommand::CommitClipClickByTicks(_)
            | SequencerCommand::MoveClipCursorToStart
            | SequencerCommand::MoveClipCursorToEnd
            | SequencerCommand::PlayFromClipCursor
            | SequencerCommand::PreviewNote { .. }
            | SequencerCommand::FocusPane(Pane::Clip)
            | SequencerCommand::SetTrackVolume { .. }
            | SequencerCommand::SetTrackPan { .. }
            | SequencerCommand::ToggleTrackMute { .. }
            | SequencerCommand::ToggleTrackSolo { .. }
            | SequencerCommand::SetTrackOutput { .. }
            | SequencerCommand::SetTrackInstrumentState { .. } => return false,

            // The project commands say why they did nothing.
            SequencerCommand::ProjectAction { .. }
            | SequencerCommand::SaveProject { .. }
            | SequencerCommand::ExportClip { .. }
            | SequencerCommand::ImportMidiClip { .. } => {
                self.send_status_ui_event(FINISH_TAKE_FIRST.to_string());
            }

            // Everything else that edits — another `/` among them — is
            // blocked: the take only chooses a window.
            _ => {}
        }
        true
    }

    /// Esc, and everything that ends a session without inserting: the lead
    /// clip put back as it was, the capture buffer kept for `/` to reopen.
    /// A no-op with no session open.
    pub(super) fn cancel_take_framing_workflow(&self, sequencer: &mut Sequencer) {
        if let Some(return_to) = sequencer.cancel_take_framing() {
            self.end_take_framing(sequencer, return_to);
        }
    }

    /// Enter: the framed notes inserted at the lead clip's cursor as one
    /// `InsertCaptureEdit`, whose undo state is the clip from before the
    /// session; it consumes the capture buffer.
    fn finish_take_framing_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
    ) {
        let Some((return_to, insert)) = sequencer.finish_take_framing() else {
            return;
        };
        // The clip view goes back to the lead clip before the edit's own
        // fan-out refreshes its notes.
        self.end_take_framing(sequencer, return_to);
        let edit = insert.and_then(|insert| InsertCaptureEdit::from_insert(sequencer, insert));
        self.record_edit(sequencer, undo_record, edit);
    }

    /// A closed session's fan-out: the keyboard back to `return_to`, the
    /// lead clip's selection published again, the clip view back on it, and
    /// its arranger thumbnail without the temporary notes.
    fn end_take_framing(&self, sequencer: &Sequencer, return_to: ViewState) {
        self.set_view_state(return_to);
        self.publish_event_selection(sequencer);
        self.ui_event_tx
            .send(UiEvent::TakeFramingEnded {
                clip_view: sequencer.current_clip_view(),
            })
            .ok();
        self.send_selected_clip_updated_ui_event(sequencer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::midi::input::InputTicks;
    use crate::core::project::ProjectAction;
    use crate::core::sequencer::SequencerCommand;
    use crate::core::sequencer::test_support::stub_instrument;
    use crate::core::time::Meter;
    use crate::core::view_state::{Pane, ViewState};
    use crate::models::{clip::ClipEdge, event::EventType};
    use crate::view::display::UiEvent;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// A lead clip at bar 1 under the arranger cursor (holding a note 60 at
    /// its start), the arranger focused with the cursor an eighth into it,
    /// two notes played (64 then 67, a quarter apart) and the transport
    /// stopped. Returns the clip's event count before any take.
    fn played_into_a_lead_clip(h: &mut Harness) -> usize {
        add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Arranger);
        h.cursor_tick.store(BAR / 8, Ordering::Relaxed);
        for (tick, message) in [
            (100, [0x90, 64, 100]),
            (400, [0x80, 64, 0]),
            (1060, [0x90, 67, 100]),
            (1300, [0x80, 67, 0]),
        ] {
            let ticks = InputTicks {
                position: tick,
                elapsed: tick,
            };
            h.sequencer.handle_midi_input_dispatch(&message, ticks);
        }
        lead_events(h)
    }

    /// The lead clip's event count — the clip in the arrangement.
    fn lead_events(h: &Harness) -> usize {
        h.sequencer.lead_clip().unwrap().events().len()
    }

    /// The lead clip's note pitches, in tick order.
    fn lead_pitches(h: &Harness) -> Vec<u8> {
        h.sequencer
            .lead_clip()
            .unwrap()
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .filter_map(|e| e.note_number())
            .collect()
    }

    /// Every UI event sent so far, drained.
    fn ui_events(h: &Harness) -> Vec<UiEvent> {
        h.ui_events.try_iter().collect()
    }

    /// The stopped `/` from the arranger opens the session: the clip view
    /// takes the keyboard and shows the take, nothing enters the undo
    /// record, and Enter inserts the take as one undo step and hands the
    /// keyboard back to the arranger.
    #[test]
    fn the_stopped_slash_opens_a_session_and_enter_inserts_one_undo_step() {
        let mut h = harness();
        let mut record = Record::new();
        let before = played_into_a_lead_clip(&mut h);

        run_command(&mut h, &mut record, SequencerCommand::Commit);
        assert!(h.sequencer.is_framing_take());
        assert_eq!(h.handlers.view_state(), ViewState::Clip);
        assert!(record.is_empty());
        assert!(ui_events(&h).iter().any(|e| matches!(
            e,
            UiEvent::TakeFramingStarted {
                clip_view: Some(_),
                ..
            }
        )));
        assert_eq!(lead_pitches(&h), vec![60, 64, 67], "heard in place");

        run_command(&mut h, &mut record, SequencerCommand::FitTempo);
        assert!(!h.sequencer.is_framing_take());
        assert_eq!(h.handlers.view_state(), ViewState::Arranger);
        assert_eq!(record.len(), 1);
        assert_eq!(lead_pitches(&h), vec![60, 64, 67]);
        assert!(
            ui_events(&h)
                .iter()
                .any(|e| matches!(e, UiEvent::TakeFramingEnded { .. }))
        );

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(lead_events(&h), before);
    }

    /// `]` with the cursor between the two notes frames just the first; Esc
    /// puts the clip back and keeps the buffer, so `/` reopens the take
    /// whole.
    #[test]
    fn esc_cancels_and_slash_reopens_the_take() {
        let mut h = harness();
        let mut record = Record::new();
        let before = played_into_a_lead_clip(&mut h);
        run_command(&mut h, &mut record, SequencerCommand::Commit);

        // The cursor a quarter into the take: between its two notes.
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::NudgeClipCursorByGrid { step_ticks: 960 },
        );
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetClipEdgeToCursor(ClipEdge::End),
        );
        assert_eq!(lead_pitches(&h), vec![60, 64]);

        run_command(&mut h, &mut record, SequencerCommand::ClearEventSelection);
        assert!(!h.sequencer.is_framing_take());
        assert_eq!(lead_events(&h), before);
        assert!(record.is_empty());

        run_command(&mut h, &mut record, SequencerCommand::Commit);
        assert_eq!(lead_pitches(&h), vec![60, 64, 67]);
    }

    /// ⌘Z cancels rather than undoing an earlier edit, and an edit command
    /// is a no-op while the session is open.
    #[test]
    fn undo_cancels_and_edits_are_blocked() {
        let mut h = harness();
        let mut record = Record::new();
        let before = played_into_a_lead_clip(&mut h);
        run_command(&mut h, &mut record, SequencerCommand::Commit);

        run_command(&mut h, &mut record, SequencerCommand::QuantizeEvents);
        run_command(&mut h, &mut record, SequencerCommand::Commit);
        assert!(record.is_empty());
        assert!(h.sequencer.is_framing_take());

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(!h.sequencer.is_framing_take());
        assert_eq!(lead_events(&h), before);
    }

    /// Re-routing a track goes through an open session, which stays open:
    /// `Display` has already swapped the plugin by the time it asks, so a
    /// block would leave the sequencer routing to a plugin no longer loaded.
    #[test]
    fn an_output_change_goes_through_a_session() {
        let mut h = harness();
        let mut record = Record::new();
        played_into_a_lead_clip(&mut h);
        run_command(&mut h, &mut record, SequencerCommand::Commit);

        let synth = stub_instrument(Vec::new());
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetTrackOutput {
                track: 1,
                output: synth.clone(),
            },
        );
        assert_eq!(*h.sequencer.tracks()[1].output(), synth);
        assert!(h.sequencer.is_framing_take());
    }

    /// Leaving the clip view cancels, and so does quitting — before the
    /// unsaved-changes check, which then sees the project untouched.
    #[test]
    fn leaving_the_clip_view_or_quitting_cancels() {
        let quit = || SequencerCommand::ProjectAction {
            action: ProjectAction::Quit,
            discard_changes: false,
        };
        let quit_reply = |h: &Harness| {
            ui_events(h)
                .into_iter()
                .rev()
                .find(|e| matches!(e, UiEvent::UnsavedChanges { .. } | UiEvent::QuitApproved))
                .map(|e| matches!(e, UiEvent::QuitApproved))
        };
        let mut h = harness();
        let mut record = Record::new();
        let before = played_into_a_lead_clip(&mut h);
        while h.ui_events.try_recv().is_ok() {}
        run_command(&mut h, &mut record, quit());
        let reply_without_a_session = quit_reply(&h);
        assert!(reply_without_a_session.is_some());

        run_command(&mut h, &mut record, SequencerCommand::Commit);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::FocusPane(Pane::Arranger),
        );
        assert!(!h.sequencer.is_framing_take());
        assert_eq!(h.handlers.view_state(), ViewState::Arranger);
        assert_eq!(lead_events(&h), before);

        run_command(&mut h, &mut record, SequencerCommand::Commit);
        assert!(h.sequencer.is_framing_take());
        while h.ui_events.try_recv().is_ok() {}
        run_command(&mut h, &mut record, quit());
        assert!(!h.sequencer.is_framing_take());
        assert_eq!(lead_events(&h), before);
        assert_eq!(quit_reply(&h), reply_without_a_session);
    }
}
