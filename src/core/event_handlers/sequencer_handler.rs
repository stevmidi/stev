//! `handle_sequencer_command` — the one `match` over every
//! [`SequencerCommand`]. Arms are
//! kept thin: undoable ops build a `SequencerEdit`, record it, and pass the
//! result to `handle_edit_result`; the rest delegate to a workflow file. See
//! `050-undo-redo.md`.

use std::time::Instant;

use undo::Record;

use crate::core::agent::{AgentAnswer, AgentReply, answer};
use crate::core::sequencer::{
    ClearControllerSpanEdit, CommitClipEdit, DeleteSelectedEventsEdit, DeleteTimeEdit,
    DragEventsVelocityEdit, DragNotesEdit, DuplicateClipsEdit, DuplicateTimeEdit,
    InsertCaptureEdit, InsertNotesEdit, InsertSilenceEdit, MoveClipEdit, MoveRangeEdit,
    MuteInRangeEdit, MuteSelectedEventsEdit, NudgeSelectedEventsEdit,
    NudgeSelectedEventsLengthEdit, PasteClipsEdit, QuantizeEventsEdit, RenameTrackEdit,
    SequencerEdit, SetMeterEdit, SetTempoEdit, SetTrackOutputEdit, SplitClipsEdit, TempoGesture,
    TransposeSelectedEventsEdit, UndoStep, top_step,
};
use crate::core::time::TapTempo;
use crate::models::clip::ClipEdge;

use super::*;

impl EventHandlers {
    /// Applies one [`SequencerCommand`]
    /// against `sequencer`, recording undoable ones on `undo_record`, keeping
    /// `tap_tempo` for tap-tempo and `open` for what it means to the project
    /// as a file: the unsaved-changes check, a project waiting on the view to
    /// load its plugins, the crash recovery copy. Called from the
    /// `"sequencer"` thread's `select!` loop.
    pub(crate) fn handle_sequencer_command(
        &self,
        cmd: &SequencerCommand,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        tap_tempo: &mut TapTempo,
        open: &mut OpenProject,
    ) {
        // The agent bridge's marquee mirror and calls go ahead of every
        // hold: none applies to them, and a write that can't run now says
        // why itself (`360-mcp-server.md` decision 13).
        match cmd {
            SequencerCommand::SetMarquee(marquee) => {
                return self.mirror_marquee_workflow(sequencer, *marquee);
            }
            SequencerCommand::SetModalOpen(open) => return sequencer.set_modal_open(*open),
            SequencerCommand::Agent { request, reply } => {
                let answer = match answer(request, sequencer, open.saved.name()) {
                    AgentAnswer::Reply(answer) => answer,
                    AgentAnswer::Write(write) => {
                        self.record_edit(sequencer, undo_record, Some(write.edit));
                        self.send_status_ui_event(write.footer);
                        AgentReply::ok(write.reply)
                    }
                };
                reply.send(answer).ok();
                return;
            }
            // Done on disk already: the origin has to follow, whatever the
            // project is in the middle of, and the view's copy of it with it.
            SequencerCommand::LibraryChanged(change) => {
                if open.saved.follow(change) {
                    let origin = open.saved.origin().cloned();
                    self.ui_event_tx
                        .send(UiEvent::OpenProjectFile { origin })
                        .ok();
                    // Woken now, the view takes it before the next key: a
                    // frame handles input ahead of these events, and an idle
                    // UI would otherwise hold them until a ⌘S came in and
                    // saved to the old file.
                    self.request_repaint();
                }
                return;
            }
            // So is a save; the actions held behind it then run as if they
            // had just come (`420-large-projects.md` decision 6).
            SequencerCommand::ProjectWritten(outcome) => {
                for (action, discard_changes) in self.project_written_workflow(open, outcome) {
                    let cmd = SequencerCommand::ProjectAction {
                        action,
                        discard_changes,
                    };
                    self.handle_sequencer_command(&cmd, sequencer, undo_record, tap_tempo, open);
                }
                return;
            }
            _ => {}
        }
        // An open take session handles or blocks most commands itself
        // (`take_framing.rs`).
        if self.take_framing_command(cmd, sequencer, undo_record) {
            return;
        }
        // A running audio export blocks nearly everything (`audio_export.rs`).
        if self.audio_export_command(cmd, sequencer) {
            return;
        }
        match cmd {
            SequencerCommand::Commit => {
                // Into the lead clip if there is one, otherwise a new clip:
                // the last pass while running, the detected phrase while
                // stopped. Stopped into the lead clip opens a take session
                // instead, which inserts on Enter.
                let into_lead_clip = sequencer.selected_clip_id().is_some();
                let running = sequencer.is_running();
                let edit = match (into_lead_clip, running) {
                    (true, true) => {
                        InsertCaptureEdit::from_running_capture(sequencer).map(SequencerEdit::from)
                    }
                    (true, false) => {
                        if self.view_state() == ViewState::Arranger {
                            sequencer.sync_selected_clip_cursor_with_arranger();
                        }
                        self.open_take_framing_workflow(sequencer);
                        None
                    }
                    (false, true) => {
                        CommitClipEdit::from_running_capture(sequencer).map(SequencerEdit::from)
                    }
                    (false, false) => {
                        #[cfg(debug_assertions)]
                        sequencer.write_stopped_capture_fixture();
                        CommitClipEdit::from_stopped_capture(sequencer).map(SequencerEdit::from)
                    }
                };
                if self.record_edit(sequencer, undo_record, edit) && !into_lead_clip && !running {
                    // The project's first clip: loop over it.
                    self.loop_sole_clip_workflow(sequencer, true);
                }
            }

            SequencerCommand::InsertEmptyClip { time_bounds } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    CommitClipEdit::from_empty_clip(sequencer, *time_bounds),
                );
            }

            SequencerCommand::RemoveClips { rect } => {
                // Marquee-only, with no selected-clip fallback.
                self.delete_rect_workflow(sequencer, undo_record, *rect);
            }

            SequencerCommand::SplitClips { time_bounds } => {
                let cursor = sequencer.cursor_tick();
                let edit = match time_bounds {
                    Some(rect) => SplitClipsEdit::from_marquee(sequencer, *rect, cursor),
                    None => SplitClipsEdit::from_selected_clip(sequencer, cursor),
                };
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::InsertSilenceInRange { start, end } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    InsertSilenceEdit::from_time_range(sequencer, *start, *end),
                );
            }

            SequencerCommand::DeleteTimeInRange { start, end } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    DeleteTimeEdit::from_time_range(sequencer, *start, *end),
                );
            }

            SequencerCommand::DuplicateTimeInRange { start, end } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    DuplicateTimeEdit::from_time_range(sequencer, *start, *end),
                );
            }

            SequencerCommand::DuplicateClips { rect } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    DuplicateClipsEdit::from_track_span(
                        sequencer,
                        rect.track_start,
                        rect.track_end,
                        rect.start,
                        rect.end,
                    ),
                );
            }

            SequencerCommand::MergeClips { rect } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    PasteClipsEdit::merging(sequencer, *rect),
                );
            }

            SequencerCommand::CopyClips { start, end } => {
                sequencer.copy_clips_to_clipboard(*start, *end);
                self.prime_os_clipboard_for_clips(sequencer);
            }

            SequencerCommand::CutClips { start, end } => {
                self.cut_clips_workflow(sequencer, undo_record, *start, *end);
            }

            SequencerCommand::CopyClipsScoped { rect } => {
                sequencer.copy_clips_scoped_to_clipboard(*rect);
                self.prime_os_clipboard_for_clips(sequencer);
            }

            SequencerCommand::CutClipsScoped { rect } => {
                self.cut_clips_scoped_workflow(sequencer, undo_record, *rect);
            }

            SequencerCommand::SelectClipSpan { track_idx, clip_id } => {
                self.select_clip_span_workflow(sequencer, *track_idx, *clip_id);
            }

            SequencerCommand::MoveClip {
                track_idx,
                clip_id,
                to_track_idx,
                to_start_tick,
            } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    MoveClipEdit::new(
                        sequencer,
                        *track_idx,
                        *clip_id,
                        *to_track_idx,
                        *to_start_tick,
                    ),
                );
            }

            SequencerCommand::MoveRange {
                rect,
                delta_ticks,
                delta_tracks,
            } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    MoveRangeEdit::new(sequencer, *rect, *delta_ticks, *delta_tracks),
                );
            }

            SequencerCommand::CopyRange {
                rect,
                delta_ticks,
                delta_tracks,
            } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    PasteClipsEdit::copying(sequencer, *rect, *delta_ticks, *delta_tracks),
                );
            }

            SequencerCommand::PasteClips => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    PasteClipsEdit::from_sequencer(sequencer),
                );
            }

            SequencerCommand::MuteClipsInRange { rect } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    MuteInRangeEdit::from_track_span(
                        sequencer,
                        rect.track_start,
                        rect.track_end,
                        rect.start,
                        rect.end,
                    ),
                );
            }

            SequencerCommand::EnterClip => {
                self.enter_clip_workflow(sequencer);
            }

            SequencerCommand::ExitClip => {
                self.exit_clip_workflow(sequencer);
            }

            SequencerCommand::FocusPane(pane) => {
                self.focus_pane_workflow(sequencer, *pane);
            }

            SequencerCommand::CommitClipClickByTicks(ticks) => {
                self.set_clip_cursor_workflow(sequencer, *ticks);
            }

            SequencerCommand::RescaleSelectedClipTempo(direction) => {
                self.rescale_selected_clip_workflow(sequencer, undo_record, *direction);
            }

            SequencerCommand::MoveClipCursorToStart => {
                if let Some(tick) = sequencer.selected_clip_region_start() {
                    self.set_clip_cursor_workflow(sequencer, tick);
                }
            }

            SequencerCommand::MoveClipCursorToEnd => {
                if let Some(tick) = sequencer.selected_clip_region_end() {
                    self.set_clip_cursor_workflow(sequencer, tick);
                }
            }

            SequencerCommand::PlayFromClipCursor => {
                if let Some(tick) = sequencer.selected_clip_play_from_tick() {
                    self.send_transport(TransportCommand::PlayFromTick { tick });
                }
            }

            SequencerCommand::NudgeClipCursor(nudge) => {
                if let Some(current) = sequencer.selected_clip_cursor_tick() {
                    let new_tick = current + nudge;
                    self.set_clip_cursor_workflow(sequencer, new_tick);
                }
            }

            SequencerCommand::NudgeClipCursorByGrid { step_ticks } => {
                if let Some(tick) = sequencer.grid_snapped_selected_clip_cursor_tick(*step_ticks) {
                    self.set_clip_cursor_workflow(sequencer, tick);
                }
            }

            SequencerCommand::NudgeSelectedEvents(nudge) => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    NudgeSelectedEventsEdit::from_sequencer(sequencer, *nudge),
                );
            }

            SequencerCommand::NudgeSelectedEventsLength(nudge) => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    NudgeSelectedEventsLengthEdit::from_sequencer(sequencer, *nudge),
                );
            }

            SequencerCommand::TransposeSelectedEvents(semitones) => {
                let edit = TransposeSelectedEventsEdit::from_sequencer(sequencer, *semitones);
                if self.record_edit(sequencer, undo_record, edit) {
                    sequencer.preview_note(sequencer.first_selected_note_on());
                }
            }

            SequencerCommand::MuteSelectedEvents => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    MuteSelectedEventsEdit::from_sequencer(sequencer),
                );
            }

            SequencerCommand::QuantizeEvents => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    QuantizeEventsEdit::from_sequencer(sequencer),
                );
            }

            SequencerCommand::ClearControllerSpan {
                controller,
                start,
                end,
            } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    ClearControllerSpanEdit::from_sequencer(sequencer, *controller, *start, *end),
                );
            }

            SequencerCommand::DragEventsVelocity {
                event_ids,
                nudge,
                drag_id,
            } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    DragEventsVelocityEdit::from_sequencer(
                        sequencer,
                        event_ids.clone(),
                        *nudge,
                        *drag_id,
                    ),
                );
            }

            SequencerCommand::InsertNote {
                tick,
                length,
                pitch,
                velocity,
            } => {
                let edit =
                    InsertNotesEdit::from_sequencer(sequencer, *tick, *length, *pitch, *velocity);
                if self.record_edit(sequencer, undo_record, edit) {
                    sequencer.preview_note(sequencer.first_selected_note_on());
                }
            }

            SequencerCommand::DragNotes {
                event_ids,
                drag,
                drag_id,
            } => {
                let top = top_step(undo_record).map(UndoStep::sequencer_edit);
                let edit = DragNotesEdit::from_sequencer(
                    sequencer,
                    top,
                    event_ids.clone(),
                    *drag,
                    *drag_id,
                );
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::PreviewNote { note, velocity } => {
                sequencer.preview_pitch(*note, *velocity);
            }

            SequencerCommand::DuplicateSelectedEvents { span } => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    InsertNotesEdit::duplicating_selection(sequencer, *span),
                );
            }

            SequencerCommand::DeleteSelectedEvents => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    DeleteSelectedEventsEdit::from_sequencer(sequencer),
                );
            }

            SequencerCommand::CopyNotes => {
                if sequencer.copy_selected_notes_to_clipboard() {
                    self.prime_os_clipboard();
                }
            }

            SequencerCommand::CutNotes => {
                if sequencer.copy_selected_notes_to_clipboard() {
                    self.prime_os_clipboard();
                    self.record_edit(
                        sequencer,
                        undo_record,
                        DeleteSelectedEventsEdit::from_sequencer(sequencer),
                    );
                }
            }

            SequencerCommand::PasteNotes => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    InsertNotesEdit::from_clipboard(sequencer),
                );
            }

            SequencerCommand::SelectClipEvent(event_id) => {
                self.select_clip_event_workflow(sequencer, *event_id);
                sequencer.preview_note(sequencer.selected_note_on());
            }

            SequencerCommand::SelectEventsInRect {
                tick_min,
                tick_max,
                note_min,
                note_max,
            } => {
                self.select_events_in_rect_workflow(
                    sequencer, *tick_min, *tick_max, *note_min, *note_max,
                );
            }

            SequencerCommand::SelectAllEvents => {
                self.select_all_events_workflow(sequencer);
            }

            SequencerCommand::ClearEventSelection => {
                self.clear_event_selection_workflow(sequencer);
            }

            SequencerCommand::SelectNextTrack => {
                self.cycle_selected_track_workflow(sequencer, 1);
            }

            SequencerCommand::SelectPrevTrack => {
                self.cycle_selected_track_workflow(sequencer, -1);
            }

            SequencerCommand::SelectTrackAt(idx) => {
                self.select_track_workflow(sequencer, *idx);
            }

            SequencerCommand::SelectPerformanceLane => {
                self.select_performance_lane_workflow(sequencer);
            }

            SequencerCommand::ResizeSelectedClipRegionEnd {
                target_tick,
                drag_id,
            } => {
                self.drag_clip_edge_workflow(
                    sequencer,
                    undo_record,
                    ClipEdge::End,
                    *target_tick,
                    *drag_id,
                );
            }

            SequencerCommand::ResizeSelectedClipRegionStart {
                target_tick,
                drag_id,
            } => {
                self.drag_clip_edge_workflow(
                    sequencer,
                    undo_record,
                    ClipEdge::Start,
                    *target_tick,
                    *drag_id,
                );
            }

            SequencerCommand::SetClipWindowEdge { edge, tick } => {
                self.window_edge_workflow(sequencer, undo_record, *edge, *tick);
            }

            SequencerCommand::SetClipEdgeToCursor(edge) => {
                self.clip_edge_to_cursor_workflow(sequencer, undo_record, *edge);
            }

            SequencerCommand::ProjectAction {
                action,
                discard_changes,
            } => {
                self.project_action_workflow(
                    sequencer,
                    undo_record,
                    open,
                    action,
                    *discard_changes,
                );
            }

            SequencerCommand::ApplyStagedProject => {
                self.apply_staged_project_workflow(sequencer, undo_record, open);
            }

            SequencerCommand::AnswerRecovery { restore } => {
                self.answer_recovery_workflow(sequencer, undo_record, open, *restore);
            }

            SequencerCommand::SaveProject { filename, folder } => {
                self.save_project_workflow(sequencer, open, folder.as_deref(), filename);
            }

            SequencerCommand::SaveTemplate { name } => {
                self.save_template_workflow(sequencer, open, name);
            }

            SequencerCommand::ExportClip { time_bounds, name } => {
                self.export_clip_workflow(sequencer, *time_bounds, name);
            }

            SequencerCommand::ExportAudio { time_bounds, name } => {
                self.export_audio_workflow(sequencer, *time_bounds, name);
            }

            SequencerCommand::CancelAudioExport => {
                self.cancel_audio_export_workflow(sequencer);
            }

            SequencerCommand::AudioExportWritten(outcome) => {
                self.finish_audio_export_workflow(sequencer, outcome);
            }

            SequencerCommand::ImportMidiClip { clip, name, target } => {
                self.import_clip_workflow(sequencer, undo_record, clip, name, *target);
            }

            // The step's view goes back after its clips: the view before
            // the edit on an undo, after it on a redo (`UndoStep`).
            SequencerCommand::Undo => {
                if let Some(step) = undo_record.undo(sequencer) {
                    self.handle_step_output(sequencer, step);
                }
            }

            SequencerCommand::Redo => {
                if let Some(step) = undo_record.redo(sequencer) {
                    self.handle_step_output(sequencer, step);
                }
            }

            SequencerCommand::TapTempo => {
                if let Some((tempo_us, burst)) = tap_tempo.tap(Instant::now()) {
                    let gesture = Some(TempoGesture::Taps(burst));
                    let edit = SetTempoEdit::new(sequencer, tempo_us, gesture);
                    self.record_edit(sequencer, undo_record, edit);
                }
            }

            SequencerCommand::SetTempo { tempo_us, gesture } => {
                let edit = SetTempoEdit::new(sequencer, *tempo_us, *gesture);
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::SetMeter { meter } => {
                let edit = SetMeterEdit::new(sequencer, *meter);
                self.record_edit(sequencer, undo_record, edit);
            }

            // Answered above, ahead of the holds.
            SequencerCommand::SetMarquee(_)
            | SequencerCommand::SetModalOpen(_)
            | SequencerCommand::LibraryChanged(_)
            | SequencerCommand::ProjectWritten(_)
            | SequencerCommand::Agent { .. } => {}

            SequencerCommand::SetTrackOutput { track, output } => {
                let edit = SetTrackOutputEdit::new(sequencer, *track, output.clone());
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::SetTrackVolume {
                track_idx,
                volume_db,
            } => {
                sequencer.set_track_volume(*track_idx, *volume_db);
            }

            SequencerCommand::SetTrackPan { track_idx, pan } => {
                sequencer.set_track_pan(*track_idx, *pan);
            }

            SequencerCommand::ToggleTrackMute { track_idx } => {
                sequencer.toggle_track_mute(*track_idx);
            }

            SequencerCommand::ToggleTrackSolo { track_idx } => {
                sequencer.toggle_track_solo(*track_idx);
            }

            SequencerCommand::SetTrackInstrumentState { slot, state } => {
                if sequencer.set_slot_instrument_state(*slot, state) {
                    open.recovery.state_captured();
                }
            }

            SequencerCommand::AddTrack { track_idx } => {
                self.add_track_workflow(sequencer, undo_record, *track_idx);
            }

            SequencerCommand::RemoveTrack { track_idx } => {
                self.remove_track_workflow(sequencer, undo_record, *track_idx);
            }

            SequencerCommand::RenameTrack { track_idx, name } => {
                let edit = RenameTrackEdit::new(sequencer, *track_idx, name.clone());
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::SetKnobParam {
                track_idx,
                knob,
                param,
            } => {
                self.set_knob_param_workflow(sequencer, undo_record, *track_idx, *knob, *param);
            }

            SequencerCommand::HoldKnob {
                track_idx,
                knob,
                start,
                tick,
            } => {
                sequencer.hold_knob(*track_idx, *knob, *start, *tick);
            }

            SequencerCommand::ReleaseKnob { track_idx, knob } => {
                sequencer.release_knob(*track_idx, *knob);
            }

            SequencerCommand::SetKnobValue {
                track_idx,
                knob,
                value,
                tick,
            } => {
                sequencer.turn_knob(*track_idx, *knob, *value, *tick);
            }

            SequencerCommand::PluginStateChanged { track_idx } => {
                sequencer.count_plugin_change(*track_idx);
            }

            SequencerCommand::FitTempo => {
                self.fit_first_clip_tempo_workflow(sequencer, undo_record);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::knobs::test_param;
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    use crossbeam_channel::bounded;
    use serde_json::{Value, json};
    use undo::Record;
    use uuid::Uuid;

    use crate::core::agent::{AgentReply, AgentRequest};
    use crate::core::event_handlers::SavedProject;
    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::midi::message::Controller;
    use crate::core::paths::project_dir;
    use crate::core::project::{ProjectAction, ProjectData, ProjectOrigin};
    use crate::core::sequencer::test_support::{clip_at, param_at, rect, stub_instrument};
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::view_state::ViewState;
    use crate::models::{
        clip::NoteDrag::{self, Move, ResizeEnd, ResizeStart},
        event::{Event, EventType},
        knobs::UNASSIGNED_KNOBS,
        track::{InstrumentRef, TrackOutput},
    };
    use crate::view::display::{TrackLane, TrackRoute, UiEvent};

    /// `action` through the unsaved-changes check, or past it.
    fn project_action(action: ProjectAction, discard_changes: bool) -> SequencerCommand {
        SequencerCommand::ProjectAction {
            action,
            discard_changes,
        }
    }

    /// Quitting, through the unsaved-changes check.
    fn quit() -> SequencerCommand {
        project_action(ProjectAction::Quit, false)
    }

    /// ⌘/Ctrl+N, checked or (the prompt's Don't Save) not.
    fn new_project(discard_changes: bool) -> SequencerCommand {
        project_action(ProjectAction::New, discard_changes)
    }

    /// What the handlers answered a project action with: the prompt's
    /// action, or `None` for a quit approved — the last such reply sent.
    fn last_project_reply(h: &Harness) -> Option<Option<ProjectAction>> {
        h.ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::UnsavedChanges { action } => Some(Some(action)),
                UiEvent::QuitApproved => Some(None),
                _ => None,
            })
            .last()
    }

    /// The routes of the last `TracksChanged` the handlers sent, if any.
    fn last_track_routes(h: &Harness) -> Option<Vec<TrackRoute>> {
        h.ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::TracksChanged { tracks, .. } => {
                    Some(tracks.into_iter().map(|lane| lane.route).collect())
                }
                _ => None,
            })
            .last()
    }

    /// Removing a mapping tells the view what went, for the footer, moves
    /// included; a knob with nothing recorded says so too, and an undo sends
    /// the lead clip back so its lane redraws.
    #[test]
    fn a_removed_mapping_reports_whether_moves_went_with_it() {
        let mut h = harness();
        let mut record = Record::new();
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[0] = Some(test_param(714));
        knobs[1] = Some(test_param(9));
        h.sequencer.set_track_knobs(0, knobs);
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.sequencer
            .clip_on_mut(0, clip_id)
            .unwrap()
            .restore_events(vec![param_at(10, 714, 0.5)]);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);

        for (knob, expected) in [(0, true), (1, false)] {
            while h.ui_events.try_recv().is_ok() {}
            run_command(
                &mut h,
                &mut record,
                SequencerCommand::SetKnobParam {
                    track_idx: 0,
                    knob,
                    param: None,
                },
            );
            assert!(h.ui_events.try_iter().any(|event| matches!(
                event,
                UiEvent::KnobMappingRemoved { knob: k, with_moves, .. }
                    if k == knob && with_moves == expected
            )));
        }

        while h.ui_events.try_recv().is_ok() {}
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(
            h.sequencer.tracks()[0].knobs()[0].map(|knob| knob.id),
            Some(714)
        );
        assert!(
            h.ui_events
                .try_iter()
                .any(|event| matches!(event, UiEvent::EventsUpdated { .. }))
        );
    }

    #[test]
    fn set_tempo_is_undoable_and_unsaved() {
        let mut h = harness();
        let mut record = Record::new();
        let start = h.sequencer.tempo_us();
        let set_tempo = SequencerCommand::SetTempo {
            tempo_us: 400_000,
            gesture: None,
        };
        assert_ne!(start, 400_000);
        run_command(&mut h, &mut record, set_tempo);
        assert_eq!(h.sequencer.tempo_us(), 400_000);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(Some(ProjectAction::Quit)));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.tempo_us(), start);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
    }

    /// The header's meter field sets the meter as one undo step, and the
    /// change counts as unsaved.
    #[test]
    fn set_meter_is_undoable_and_unsaved() {
        let mut h = harness();
        let mut record = Record::new();
        let six_eight = Meter::new(6, 8).unwrap();
        let set_meter = SequencerCommand::SetMeter { meter: six_eight };
        run_command(&mut h, &mut record, set_meter);
        assert_eq!(h.sequencer.meter(), six_eight);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(Some(ProjectAction::Quit)));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.meter(), Meter::FOUR_FOUR);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
    }

    /// Every output change sends the view the whole mirror back — the track
    /// header's chip reads it, so it must follow a plugin on and a channel
    /// pick back off.
    #[test]
    fn set_track_output_mirrors_every_track_route_to_the_view() {
        let mut h = harness();
        let mut record = Record::new();
        let instrument = InstrumentRef {
            bundle_path: "/Library/Audio/Plug-Ins/VST3/Diva.vst3".into(),
            plugin_id: "diva".into(),
            display_name: "Diva".into(),
            state: Vec::new().into(),
        };
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetTrackOutput {
                track: 2,
                output: TrackOutput::Instrument(instrument),
            },
        );
        let mut want: Vec<_> = TrackLane::defaults()
            .into_iter()
            .map(|lane| lane.route)
            .collect();
        want[2] = TrackRoute::Instrument {
            name: "Diva".into(),
        };
        assert_eq!(last_track_routes(&h), Some(want.clone()));

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetTrackOutput {
                track: 2,
                output: TrackOutput::MidiOut { channel: 9 },
            },
        );
        want[2] = TrackRoute::MidiOut { channel: 9 };
        assert_eq!(last_track_routes(&h), Some(want));
    }

    /// An output change is one undo step, and its undo tells the view which
    /// plugin the track's slot holds again, so `Display` can swap it back.
    #[test]
    fn set_track_output_is_undoable_and_the_undo_names_the_plugin_to_reload() {
        let mut h = harness();
        let mut record = Record::new();
        let synth = stub_instrument(vec![7]);
        let set = |output| SequencerCommand::SetTrackOutput { track: 2, output };
        run_command(&mut h, &mut record, set(synth.clone()));
        run_command(
            &mut h,
            &mut record,
            set(TrackOutput::MidiOut { channel: 9 }),
        );
        while h.ui_events.try_recv().is_ok() {}

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(*h.sequencer.tracks()[2].output(), synth);
        let slot = h.sequencer.tracks()[2].slot();
        let reload = h.ui_events.try_iter().find_map(|event| match event {
            UiEvent::SlotInstrument {
                slot: s,
                instrument,
                ..
            } if s == slot => Some(instrument),
            _ => None,
        });
        assert!(reload.flatten().is_some_and(|i| *i.state == [7]));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(matches!(
            h.sequencer.tracks()[2].output(),
            TrackOutput::MidiOut { channel: 2 }
        ));
    }

    /// The stopped `/` with a lead clip under the arranger cursor opens a take
    /// session rather than editing (`archive/290-framing-a-take.md`), and Enter
    /// inserts the played phrase as one undoable edit: the transport is sent
    /// nothing either way, and ⌘Z takes the notes back out.
    #[test]
    fn stopped_commit_into_the_lead_clip_is_one_undoable_insert() {
        let mut h = harness();
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Arranger);
        h.cursor_tick.store(480, Ordering::Relaxed);
        for (tick, message) in [(100, [0x90, 64, 100]), (400, [0x80, 64, 0])] {
            h.sequencer.handle_midi_input(&message, tick);
        }
        while h.transport_commands.try_recv().is_ok() {}

        let mut record: Record<UndoStep> = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::Commit);
        assert_eq!(h.handlers.view_state(), ViewState::Clip);
        assert!(record.is_empty());

        run_command(&mut h, &mut record, SequencerCommand::FitTempo);
        assert_eq!(h.handlers.view_state(), ViewState::Arranger);
        assert_eq!(record.len(), 1);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60), (480, 64)]);
        assert!(h.transport_commands.try_recv().is_err());

        record.undo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60)]);
    }

    /// A new project (and a load, through the same workflow) clears the undo
    /// record: its edits name tracks of the project it replaced, so ⌘Z would
    /// replay them by position against the new one — here, remove a track.
    #[test]
    fn new_project_clears_the_undo_record() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        assert_eq!(record.len(), 1);

        run_command(&mut h, &mut record, new_project(true));
        let tracks = h.sequencer.tracks().len();
        assert!(!record.can_undo() && !record.can_redo());

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.tracks().len(), tracks);
    }

    /// Nothing changed since the project was loaded (here: the startup
    /// one): quitting is approved at once, no prompt.
    #[test]
    fn an_untouched_project_quits_without_a_prompt() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
    }

    /// A change since the last load asks for the prompt instead of running
    /// the action, and leaves the project alone; Don't Save then runs it.
    #[test]
    fn a_changed_project_prompts_and_dont_save_goes_on() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        let tracks = h.sequencer.tracks().len();

        run_command(&mut h, &mut record, new_project(false));
        assert_eq!(last_project_reply(&h), Some(Some(ProjectAction::New)));
        assert_eq!(h.sequencer.tracks().len(), tracks);
        assert!(record.can_undo());

        run_command(&mut h, &mut record, new_project(true));
        assert!(h.sequencer.tracks().len() < tracks);
        assert!(!record.can_undo());

        // The new project is the saved state now.
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
    }

    /// A template opens like ⌘N, checked first: the open project is
    /// replaced by the template's setup, untitled — no name, no folder, no
    /// clips even when the file holds some, an empty undo record — and taken
    /// as saved, so closing it untouched asks nothing. The template file is
    /// left as it was.
    #[test]
    fn a_template_opens_as_an_untitled_project_without_clips() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        let mut source = harness();
        add_clip(&mut source.sequencer, 0);
        let mut template = ProjectData::from_sequencer(&source.sequencer);
        template.tempo_us = 666_667;
        template.tracks.truncate(2);
        assert!(!template.tracks[0].clips.is_empty());
        let json = serde_json::to_string_pretty(&template).unwrap();
        let path = std::env::temp_dir().join(format!("stev-template-{}.stev", std::process::id()));
        std::fs::write(&path, &json).unwrap();
        let open = |discard_changes| {
            project_action(
                ProjectAction::NewFromTemplate { path: path.clone() },
                discard_changes,
            )
        };
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );

        run_command(&mut h, &mut record, open(false));
        assert_eq!(
            last_project_reply(&h),
            Some(Some(ProjectAction::NewFromTemplate { path: path.clone() }))
        );
        assert_ne!(h.sequencer.tracks().len(), 2);

        run_command(&mut h, &mut record, open(true));
        let loaded = h.ui_events.try_iter().find_map(|event| match event {
            UiEvent::ProjectLoaded { clips, origin, .. } => Some((clips.is_empty(), origin)),
            _ => None,
        });
        assert_eq!(loaded, Some((true, None)));
        assert!(h.sequencer.tracks().iter().all(|t| t.clips().is_empty()));
        assert_eq!(h.sequencer.tracks().len(), 2);
        assert_eq!(h.sequencer.tempo_us(), 666_667);
        assert!(!record.can_undo());

        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), json);
        let _ = std::fs::remove_file(&path);
    }

    /// A template that won't load says why in the footer and keeps the open
    /// project, its changes and its undo history.
    #[test]
    fn a_failed_template_load_keeps_the_open_project() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        let path =
            std::env::temp_dir().join(format!("stev-no-template-{}.stev", std::process::id()));
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        let tracks = h.sequencer.tracks().len();

        run_command(
            &mut h,
            &mut record,
            project_action(ProjectAction::NewFromTemplate { path }, true),
        );
        let status = h
            .ui_events
            .try_iter()
            .find_map(|event| match event {
                UiEvent::Status { message } => Some(message),
                UiEvent::ProjectLoaded { .. } => {
                    panic!("a failed template load replaced the project")
                }
                _ => None,
            })
            .expect("a failed template load says why");
        assert!(
            status.starts_with("Could not open stev-no-template-"),
            "{status}"
        );
        assert_eq!(h.sequencer.tracks().len(), tracks);
        assert!(record.can_undo());
    }

    /// Finder opening the project already open leaves it as it is while it
    /// has unsaved changes: no prompt, no reload. Another project in the
    /// library still prompts, and with no changes the open project reloads
    /// (here a file that isn't there, so the footer says so).
    #[test]
    fn opening_the_open_project_from_finder_keeps_its_changes() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        let name = format!("stev-open-{}", std::process::id());
        let origin = ProjectOrigin::new(None, &name);
        h.open.saved = SavedProject::of(&h.sequencer, Some(origin));
        h.sequencer.set_tempo(400_000);
        let open = |name: &str| {
            let path = project_dir().join(format!("{name}.stev"));
            project_action(ProjectAction::OpenFile { path }, false)
        };

        run_command(&mut h, &mut record, open(&name));
        assert_eq!(h.ui_events.try_iter().count(), 0);
        assert_eq!(h.sequencer.tempo_us(), 400_000);

        run_command(&mut h, &mut record, open(&format!("{name}-other")));
        assert!(matches!(last_project_reply(&h), Some(Some(_))));

        h.open.saved = SavedProject::of(&h.sequencer, Some(ProjectOrigin::new(None, &name)));
        run_command(&mut h, &mut record, open(&name));
        let reloaded = h
            .ui_events
            .try_iter()
            .any(|event| matches!(event, UiEvent::Status { message } if message.starts_with("Could not open")));
        assert!(reloaded);
    }

    /// A project opened from Finder goes through the same check: a changed
    /// project prompts first and stays as it is; an unchanged one goes
    /// ahead, and a file that won't load says so and keeps the project.
    #[test]
    fn opening_from_finder_is_checked_first() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        let path = std::env::temp_dir().join(format!("stev-missing-{}.stev", std::process::id()));
        let open = || project_action(ProjectAction::OpenFile { path: path.clone() }, false);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        let tracks = h.sequencer.tracks().len();

        run_command(&mut h, &mut record, open());
        assert_eq!(
            last_project_reply(&h),
            Some(Some(ProjectAction::OpenFile { path: path.clone() }))
        );
        assert_eq!(h.sequencer.tracks().len(), tracks);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        run_command(&mut h, &mut record, open());
        let status = h
            .ui_events
            .try_iter()
            .find_map(|event| match event {
                UiEvent::Status { message } => Some(message),
                UiEvent::UnsavedChanges { .. } => panic!("an unchanged project prompted"),
                _ => None,
            })
            .expect("a failed open says why");
        assert!(
            status.starts_with("Could not open stev-missing-"),
            "{status}"
        );
        assert_eq!(h.sequencer.tracks().len(), tracks - 1);
    }

    /// The check compares what would be saved, not whether anything
    /// happened: undoing back to the loaded state leaves nothing unsaved,
    /// and a change no undo step records (a track's volume) still counts.
    #[test]
    fn unsaved_means_it_would_save_differently() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));

        h.sequencer.set_track_volume(0, -6.0);
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(Some(ProjectAction::Quit)));
    }

    /// The piano roll's mouse edits: a double-clicked note is one undoable
    /// step that selects it, redo brings back the same note, and a drag is
    /// one more step however many live steps it sends — each measured from
    /// where the gesture began (a first step that changes nothing records
    /// none).
    #[test]
    fn inserted_and_dragged_notes_are_one_undo_step_each() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertNote {
                tick: 480,
                length: 480,
                pitch: 64,
                velocity: 100,
            },
        );
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60), (480, 64)]);
        let inserted = selected_events(&h, clip_id);
        assert_eq!(inserted.len(), 1);

        for (delta_ticks, delta_pitch) in [(0, 0), (240, 1), (480, 0)] {
            run_command(
                &mut h,
                &mut record,
                drag_step(
                    &inserted,
                    Move {
                        delta_ticks,
                        delta_pitch,
                    },
                ),
            );
        }
        assert_eq!(record.len(), 2);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60), (960, 64)]);
        assert_eq!(selected_events(&h, clip_id), inserted);

        record.undo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60), (480, 64)]);
        record.undo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60)]);

        record.redo(&mut h.sequencer);
        let clip = h.sequencer.tracks()[0].get_clip_by_id(clip_id).unwrap();
        assert!(clip.events().iter().any(|e| e.id() == inserted[0]));
    }

    /// Copy, move the clip cursor, paste: the earliest copied note lands on
    /// the cursor, the paste is one undo step that selects the pasted notes,
    /// and copying records nothing.
    #[test]
    fn copied_notes_paste_at_the_clip_cursor_as_one_undo_step() {
        let (mut h, clip_id, note_id) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertNote {
                tick: 480,
                length: 240,
                pitch: 64,
                velocity: 100,
            },
        );
        run_command(&mut h, &mut record, SequencerCommand::SelectAllEvents);
        run_command(&mut h, &mut record, SequencerCommand::CopyNotes);
        assert_eq!(record.len(), 1);

        h.sequencer.set_selected_clip_cursor_tick(960);
        run_command(&mut h, &mut record, SequencerCommand::PasteNotes);
        assert_eq!(record.len(), 2);
        assert_eq!(
            note_ons(&h, clip_id),
            vec![(0, 60), (480, 64), (960, 60), (1440, 64)]
        );
        let pasted = selected_events(&h, clip_id);
        assert_eq!(pasted.len(), 2);
        assert!(!pasted.contains(&note_id));

        record.undo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60), (480, 64)]);
        record.redo(&mut h.sequencer);
        assert_eq!(
            note_ons(&h, clip_id),
            vec![(0, 60), (480, 64), (960, 60), (1440, 64)]
        );
    }

    /// Cut copies and deletes the selection in one undo step; with nothing
    /// selected it neither records a step nor empties the clipboard, and a
    /// paste with an empty clipboard records nothing.
    #[test]
    fn cut_notes_is_one_undo_step_and_needs_a_selection() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::PasteNotes);
        assert_eq!(record.len(), 0);

        run_command(&mut h, &mut record, SequencerCommand::SelectAllEvents);
        run_command(&mut h, &mut record, SequencerCommand::CutNotes);
        assert_eq!(record.len(), 1);
        assert!(note_ons(&h, clip_id).is_empty());

        run_command(&mut h, &mut record, SequencerCommand::CutNotes);
        assert_eq!(record.len(), 1);

        h.sequencer.set_selected_clip_cursor_tick(480);
        run_command(&mut h, &mut record, SequencerCommand::PasteNotes);
        assert_eq!(note_ons(&h, clip_id), vec![(480, 60)]);
    }

    /// A live drag that passes over a same-pitch note trims it only while it
    /// overlaps — each step starts from the notes as the gesture found them —
    /// and a drag back to where it began (what Esc sends) leaves no undo step.
    #[test]
    fn a_live_note_drag_trims_only_where_it_lands_and_back_to_start_annuls() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertNote {
                tick: 960,
                length: 240,
                pitch: 60,
                velocity: 100,
            },
        );
        let inserted = selected_events(&h, clip_id);
        // `(tick, is_on)` of every note edge, in event order.
        let edges = |h: &Harness| {
            let clip = h.sequencer.tracks()[0].get_clip_by_id(clip_id).unwrap();
            clip.events()
                .iter()
                .map(|e| (e.tick(), e.event_type() == Some(EventType::NoteOn)))
                .collect::<Vec<_>>()
        };
        let at_origin = vec![(0, true), (240, false), (960, true), (1200, false)];
        assert_eq!(edges(&h), at_origin);

        // Over the first note, which is trimmed to end where it lands …
        run_command(
            &mut h,
            &mut record,
            drag_step(
                &inserted,
                Move {
                    delta_ticks: -840,
                    delta_pitch: 0,
                },
            ),
        );
        assert_eq!(
            edges(&h),
            vec![(0, true), (120, false), (120, true), (360, false)]
        );
        // … and on past it, which gives the first note its length back.
        run_command(
            &mut h,
            &mut record,
            drag_step(
                &inserted,
                Move {
                    delta_ticks: 480,
                    delta_pitch: 0,
                },
            ),
        );
        assert_eq!(
            edges(&h),
            vec![(0, true), (240, false), (1440, true), (1680, false)]
        );
        assert_eq!(record.len(), 2);

        run_command(
            &mut h,
            &mut record,
            drag_step(
                &inserted,
                Move {
                    delta_ticks: 0,
                    delta_pitch: 0,
                },
            ),
        );
        assert_eq!(edges(&h), at_origin);
        assert_eq!(record.len(), 1, "back to start leaves only the insert");
    }

    /// A step that can't merge — the record was saved mid-drag — is its own
    /// undo step, still applied to the notes as the gesture found them, and
    /// undoing it goes back to before the gesture.
    #[test]
    fn a_note_drag_step_after_a_save_still_starts_from_where_the_gesture_began() {
        let (mut h, clip_id, note_id) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        let step = |delta_ticks| {
            drag_step(
                &[note_id],
                Move {
                    delta_ticks,
                    delta_pitch: 0,
                },
            )
        };

        run_command(&mut h, &mut record, step(240));
        record.set_saved();
        run_command(&mut h, &mut record, step(480));
        assert_eq!(record.len(), 2);
        assert_eq!(note_ons(&h, clip_id), vec![(480, 60)]);

        record.undo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(0, 60)]);
        record.redo(&mut h.sequencer);
        assert_eq!(note_ons(&h, clip_id), vec![(480, 60)]);
    }

    /// A harness whose track 0 holds the selected clip of [`add_clip`] (one
    /// note, 60, ticks 0–240). Returns the clip's and the note's ids.
    fn harness_with_clip() -> (Harness, Uuid, Uuid) {
        let mut h = harness();
        let (clip_id, note_id) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        (h, clip_id, note_id)
    }

    /// The wheel lane's `Delete`: one undo step that returns the span to
    /// rest, undone and redone whole; a span already at rest records none.
    #[test]
    fn clearing_a_wheel_span_is_one_undo_step() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        let curve = vec![
            Event::new(0, 0, vec![0xB0, 0x01, 40]),
            Event::new(480, 0, vec![0xB0, 0x01, 80]),
        ];
        h.sequencer.tracks_mut()[0].edit_clip_events(clip_id, |clip| {
            for event in curve {
                clip.add_event(event);
            }
            clip.sort_events_by_tick();
        });
        let mod_moves = |h: &Harness| -> Vec<(i32, u8)> {
            h.sequencer.tracks()[0]
                .get_clip_by_id(clip_id)
                .unwrap()
                .events()
                .iter()
                .filter(|e| e.midi_message()[0] == 0xB0)
                .map(|e| (e.tick(), e.midi_message()[2]))
                .collect()
        };
        let rest = |start, end| SequencerCommand::ClearControllerSpan {
            controller: Controller::MOD_WHEEL,
            start,
            end,
        };

        run_command(&mut h, &mut record, rest(240, 720));
        assert_eq!(record.len(), 1);
        assert_eq!(mod_moves(&h), vec![(0, 40), (240, 0), (720, 80)]);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(mod_moves(&h), vec![(0, 40), (480, 80)]);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(mod_moves(&h), vec![(0, 40), (240, 0), (720, 80)]);

        run_command(&mut h, &mut record, rest(300, 600));
        assert_eq!(record.len(), 1, "a span at rest records nothing");
    }

    /// `(tick, pitch)` of each `NoteOn` in `clip_id` on track 0.
    fn note_ons(h: &Harness, clip_id: Uuid) -> Vec<(i32, u8)> {
        let clip = h.sequencer.tracks()[0].get_clip_by_id(clip_id).unwrap();
        clip.events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.note_number().unwrap()))
            .collect()
    }

    /// The event selection of `clip_id` on track 0.
    fn selected_events(h: &Harness, clip_id: Uuid) -> Vec<Uuid> {
        h.sequencer.tracks()[0]
            .get_clip_by_id(clip_id)
            .unwrap()
            .selected_event_ids()
    }

    /// One step of note drag gesture 1 on `event_ids`.
    fn drag_step(event_ids: &[Uuid], drag: NoteDrag) -> SequencerCommand {
        SequencerCommand::DragNotes {
            event_ids: event_ids.to_vec(),
            drag,
            drag_id: 1,
        }
    }

    /// Plays track 0 for `ticks` ticks from the playback position, advancing
    /// it as the clock would, and returns each note edge the track emits as
    /// `(tick, pitch, is_on)`.
    fn play(h: &mut Harness, ticks: i32) -> Vec<(i32, u8, bool)> {
        let mut edges = Vec::new();
        for _ in 0..ticks {
            let tick = h.playback_tick.load(Ordering::Relaxed);
            while let Some(event) = h.sequencer.tracks_mut()[0].tick() {
                // Dropped here as `Sequencer::tick` drops it.
                if event.is_muted() {
                    continue;
                }
                let is_on = match event.event_type() {
                    Some(EventType::NoteOn) => true,
                    Some(EventType::NoteOff) => false,
                    None => continue,
                };
                edges.push((tick, event.note_number().unwrap(), is_on));
            }
            h.playback_tick.fetch_add(1, Ordering::Relaxed);
        }
        edges
    }

    /// Asserts no `NoteOn` in `edges` lands on a pitch already sounding and
    /// every note is released by the end.
    fn assert_no_hung_or_doubled_notes(edges: &[(i32, u8, bool)], case: &str) {
        let mut sounding = HashSet::new();
        for &(tick, pitch, is_on) in edges {
            if is_on {
                assert!(
                    sounding.insert(pitch),
                    "{case}: second NoteOn for {pitch} at {tick}: {edges:?}"
                );
            } else {
                sounding.remove(&pitch);
            }
        }
        assert!(
            sounding.is_empty(),
            "{case}: {sounding:?} left hanging: {edges:?}"
        );
    }

    /// A harness playing a clip at 0 whose one note (60, ticks 0–240) is
    /// sounding under the playhead at tick 120 (its `NoteOn` played). Returns
    /// the note's id.
    fn harness_mid_note() -> (Harness, Uuid) {
        let (mut h, _, note_id) = harness_with_clip();
        h.running.store(true, Ordering::Relaxed);
        h.sequencer.reset_to_tick(0);
        assert_eq!(play(&mut h, 120), vec![(0, 60, true)]);
        (h, note_id)
    }

    /// Dragging a note while it sounds during playback never hangs it: a note
    /// moved away in time or pitch, or cut short behind the playhead, is
    /// released when the edit lands, and one whose start moves ahead of the
    /// playhead is released before it sounds again.
    #[test]
    fn dragging_a_sounding_note_away_from_the_playhead_releases_it() {
        let cases = [
            (
                Move {
                    delta_ticks: 480,
                    delta_pitch: 0,
                },
                "moved later",
            ),
            (
                Move {
                    delta_ticks: 0,
                    delta_pitch: 2,
                },
                "moved to a new pitch",
            ),
            (ResizeEnd { delta_ticks: -180 }, "end behind the playhead"),
            (ResizeStart { delta_ticks: 180 }, "start past the playhead"),
        ];
        for (drag, case) in cases {
            let (mut h, note_id) = harness_mid_note();
            run_command(&mut h, &mut Record::new(), drag_step(&[note_id], drag));
            let edges = [vec![(0, 60, true)], play(&mut h, 1800)].concat();
            assert_eq!(edges[1], (120, 60, false), "{case}: released at once");
            assert_no_hung_or_doubled_notes(&edges, case);
        }
    }

    /// Issue #19: muting a note while it sounds releases it. The mute flag
    /// goes on its `NoteOff` too, so the off the walk would have met is
    /// dropped: without its own release the note hung until the wrap. A redo
    /// releases it the same way; the undo between them only brings the note
    /// back for the next pass.
    #[test]
    fn muting_a_sounding_note_releases_it_and_redo_does_too() {
        let selected_mid_note = || {
            let (mut h, note_id) = harness_mid_note();
            let clip_id = h.sequencer.selected_clip_id().unwrap();
            h.sequencer
                .clip_on_mut(0, clip_id)
                .unwrap()
                .select_event(Some(note_id));
            h
        };

        let mut h = selected_mid_note();
        let mut record = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::MuteSelectedEvents);
        assert_eq!(play(&mut h, 1800), vec![(120, 60, false)]);

        let mut h = selected_mid_note();
        let mut record = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::MuteSelectedEvents);
        assert_eq!(play(&mut h, 2), vec![(120, 60, false)]);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(play(&mut h, 2), vec![(122, 60, false)]);
    }

    /// Issue #19: `M` on a marquee holding the whole clip splits nothing, so
    /// the mute itself has to release what sounds. The workflow's `reset`
    /// seeks off the muted clip before the next tick, so `Track::tick` can't
    /// find its notes any more. Undoing an unmute mutes the clip the same way.
    #[test]
    fn muting_a_sounding_clip_with_the_marquee_releases_its_notes() {
        let mute = || SequencerCommand::MuteClipsInRange {
            rect: rect(0, 1920, 0, 0),
        };

        let (mut h, _) = harness_mid_note();
        run_command(&mut h, &mut Record::new(), mute());
        assert_eq!(play(&mut h, 1800), vec![(120, 60, false)]);

        // Muted before playback, then unmuted and re-muted by undo mid-note.
        let (mut h, _, _) = harness_with_clip();
        let mut record = Record::new();
        run_command(&mut h, &mut record, mute());
        run_command(&mut h, &mut record, mute());
        h.running.store(true, Ordering::Relaxed);
        h.sequencer.reset_to_tick(0);
        assert_eq!(play(&mut h, 120), vec![(0, 60, true)]);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(play(&mut h, 1800), vec![(120, 60, false)]);
    }

    /// Stretching a sounding note keeps it sounding to its new end — no
    /// second `NoteOn`, no early release — and undoing the stretch once the
    /// playhead is past the old end releases it then.
    #[test]
    fn stretching_a_sounding_note_holds_it_to_the_new_end_and_undo_releases_it() {
        let stretch = |note_id| drag_step(&[note_id], ResizeEnd { delta_ticks: 720 });

        let (mut h, note_id) = harness_mid_note();
        run_command(&mut h, &mut Record::new(), stretch(note_id));
        assert_eq!(play(&mut h, 1800), vec![(960, 60, false)]);

        let (mut h, note_id) = harness_mid_note();
        let mut record = Record::new();
        run_command(&mut h, &mut record, stretch(note_id));
        assert!(play(&mut h, 480).is_empty());
        let (undone, _) = record.undo(&mut h.sequencer).unwrap();
        h.handlers.handle_edit_result(&mut h.sequencer, undone);
        assert_eq!(play(&mut h, 1320), vec![(600, 60, false)]);
    }

    /// A live stretch keeps a sounding note sounding through every step —
    /// each step goes back to the gesture's origin, where the note has
    /// already ended, but within one clip edit — and a later step that pulls
    /// the end back behind the playhead releases it there.
    #[test]
    fn live_stretch_steps_hold_a_sounding_note_until_one_ends_it() {
        let (mut h, note_id) = harness_mid_note();
        let mut record = Record::new();
        // Each step's end, then how far to play and the edges heard: ends
        // at 600, 960, then 600 again with the playhead at 700.
        for (delta_ticks, play_ticks, heard) in [
            (360, 380, vec![]),
            (720, 200, vec![]),
            (360, 100, vec![(700, 60, false)]),
        ] {
            run_command(
                &mut h,
                &mut record,
                drag_step(&[note_id], ResizeEnd { delta_ticks }),
            );
            assert_eq!(play(&mut h, play_ticks), heard, "end at +{delta_ticks}");
        }
        assert_eq!(record.len(), 1);
    }

    /// One agent-bridge call through the handlers, answered.
    fn agent_call(
        h: &mut Harness,
        record: &mut Record<UndoStep>,
        tool: &str,
        arguments: Value,
    ) -> AgentReply {
        let (reply, answer) = bounded(1);
        let request = AgentRequest {
            tool: tool.to_owned(),
            arguments,
        };
        run_command(h, record, SequencerCommand::Agent { request, reply });
        answer.try_recv().unwrap()
    }

    /// Decision 15 (`360-mcp-server.md`): a write leaves the lead clip, its
    /// event selection, the selected track, the cursor and the marquee as
    /// the user left them, its own new clips included, and says what it did
    /// in the footer. A clip that isn't the lead gets its thumbnail redrawn.
    #[test]
    fn agent_writes_leave_the_selection_alone_and_show_the_footer() {
        let mut h = harness();
        let (lead_id, note_id) = add_clip(&mut h.sequencer, 0);
        h.sequencer.select_clip(Some(lead_id));
        h.sequencer
            .clip_on_mut(0, lead_id)
            .unwrap()
            .restore_event_selection(&[note_id]);
        h.cursor_tick.store(480, Ordering::Relaxed);
        let marquee = rect(0, 1920, 0, 0);
        h.sequencer.set_marquee(Some(marquee));
        let mut record = Record::new();

        let created = agent_call(
            &mut h,
            &mut record,
            "create_clips",
            json!({ "clips": [{ "track": 2, "start": "3", "length": 4, "notes": "C3 0 1 90" }] }),
        );
        assert!(!created.is_error, "{}", created.text);
        let other = h.sequencer.tracks()[1].clips()[0].id();
        let other_id = &other.simple().to_string()[..8];
        let replaced = agent_call(
            &mut h,
            &mut record,
            "replace_clip_notes",
            json!({ "clip": other_id, "notes": "E3 0 2 90" }),
        );
        assert!(!replaced.is_error, "{}", replaced.text);
        let tempo = agent_call(&mut h, &mut record, "set_tempo", json!({ "bpm": 100 }));
        assert!(!tempo.is_error, "{}", tempo.text);
        assert_eq!(record.len(), 3, "one undo step a call");

        assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id));
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(
            h.sequencer
                .clip_on(0, lead_id)
                .unwrap()
                .selected_event_ids(),
            vec![note_id]
        );
        assert_eq!(h.cursor_tick.load(Ordering::Relaxed), 480);
        assert_eq!(h.sequencer.marquee(), Some(marquee));

        let events: Vec<UiEvent> = h.ui_events.try_iter().collect();
        let footers: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                UiEvent::Status { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            footers,
            vec![
                "Agent: 1 new clip, bar 3",
                "Agent: new notes on track 2, bar 3",
                "Agent: tempo 100.0 BPM",
            ]
        );
        assert!(
            events.iter().any(
                |event| matches!(event, UiEvent::ClipUpdated { clip } if clip.clip_id == other)
            )
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, UiEvent::LeadClipChanged { .. }))
        );
    }

    /// Undoing a note edit after the lead moved to another clip puts back
    /// the clip it was made in as the lead, as every clip and note edit's
    /// undo puts back the view before it (`440-undo-view-state.md`
    /// decision 5): the clip view shows the notes as they were. It used to
    /// stay on the new lead and only redraw the edited clip's thumbnail.
    #[test]
    fn undoing_a_note_edit_off_the_lead_puts_its_clip_back_as_the_lead() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertNote {
                tick: 480,
                length: 480,
                pitch: 64,
                velocity: 100,
            },
        );
        let (other_id, _) = add_clip(&mut h.sequencer, 3840 * 4);
        h.cursor_tick.store(3840 * 4, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        assert_eq!(h.sequencer.selected_clip_id(), Some(other_id));
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert_eq!(note_ons(&h, clip_id), vec![(0, 60)]);
        assert_eq!(h.sequencer.selected_clip_id(), Some(clip_id));
        assert_eq!(h.sequencer.cursor_tick(), 0);
        let events: Vec<UiEvent> = h.ui_events.try_iter().collect();
        assert!(events.iter().any(|event| matches!(
            event,
            UiEvent::LeadClipChanged { clip_view: Some(view) } if view.clip_id == clip_id
        )));
    }

    /// Issue #3: undoing a marquee delete puts back the state before it,
    /// whatever was done since — the clips, the lead clip (so the clip view
    /// shows it again) and the marquee — and moves no focus: from a clip
    /// view opened on another clip, the view stays and shows the deleted
    /// one. Read at undo time, the dropped lead stayed dropped and the clip
    /// view stayed on the other clip or empty.
    #[test]
    fn undoing_a_range_delete_restores_the_lead_clip_and_the_marquee() {
        let (mut h, lead_id, _) = harness_with_clip();
        add_clip(&mut h.sequencer, 1920);
        let (other_id, _) = add_clip(&mut h.sequencer, 3840 * 2);
        let mut record: Record<UndoStep> = Record::new();
        let marquee = rect(0, 3840, 0, 0);
        h.sequencer.set_marquee(Some(marquee));
        let carve = SequencerCommand::RemoveClips { rect: marquee };
        run_command(&mut h, &mut record, carve);
        assert_eq!(h.sequencer.selected_clip_id(), None);
        h.sequencer.select_clip(Some(other_id));
        h.cursor_tick.store(3840 * 2, Ordering::Relaxed);
        run_command(&mut h, &mut record, SequencerCommand::EnterClip);
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert_eq!(h.sequencer.cursor_tick(), 0);
        assert_eq!(h.handlers.view_state(), ViewState::Clip);
        assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id));
        let events: Vec<UiEvent> = h.ui_events.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, UiEvent::ClipExited))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            UiEvent::LeadClipChanged { clip_view: Some(view) } if view.clip_id == lead_id
        )));
        assert!(matches!(
            events.last(),
            Some(UiEvent::TimeSelectionSet { rect }) if *rect == marquee
        ));
    }

    /// Issue #3: the same undo after moving to a clip on another track
    /// selects the deleted clip's track again, since the lead clip lives on
    /// the selected track. Without it the clip was the lead in name only and
    /// the clip view said "No clip at the cursor".
    #[test]
    fn undoing_a_range_delete_from_another_track_selects_the_clips_track() {
        let (mut h, lead_id, _) = harness_with_clip();
        add_clip(&mut h.sequencer, 1920);
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));
        let mut record: Record<UndoStep> = Record::new();
        let carve = SequencerCommand::RemoveClips {
            rect: rect(0, 3840, 0, 0),
        };
        run_command(&mut h, &mut record, carve);
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        assert!(
            h.sequencer
                .selected_clip_id()
                .is_some_and(|id| id != lead_id)
        );
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id));
        let lead = h.ui_events.try_iter().filter_map(|event| match event {
            UiEvent::LeadClipChanged { clip_view } => Some(clip_view.map(|view| view.clip_id)),
            _ => None,
        });
        assert_eq!(lead.last(), Some(Some(lead_id)));
    }

    /// Range mute (`M`), a marquee split (`⌘E`) and Insert Silence (`⌘I`)
    /// undo like the range delete: the marquee comes back last (Insert
    /// Silence, global, on the tracks it had) at the cursor it was anchored
    /// to, and the lead clip and its track as they were before the edit,
    /// from another track and tick by now. Without the cursor, a zero-width
    /// marquee came back as the cursor line wherever it had gone.
    #[test]
    fn undoing_a_marquee_edit_restores_the_marquee_and_the_lead_clip() {
        let marquee = rect(480, 960, 0, 0);
        let cases = [
            ("mute", SequencerCommand::MuteClipsInRange { rect: marquee }),
            (
                "split",
                SequencerCommand::SplitClips {
                    time_bounds: Some(marquee),
                },
            ),
            (
                "insert silence",
                SequencerCommand::InsertSilenceInRange {
                    start: marquee.start,
                    end: marquee.end,
                },
            ),
        ];
        for (name, command) in cases {
            let (mut h, lead_id, _) = harness_with_clip();
            h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));
            h.cursor_tick.store(720, Ordering::Relaxed);
            h.sequencer.set_marquee(Some(marquee));
            let mut record: Record<UndoStep> = Record::new();
            run_command(&mut h, &mut record, command);
            assert_eq!(record.len(), 1);
            h.handlers.select_track_workflow(&mut h.sequencer, 1);
            h.cursor_tick.store(3840 * 2, Ordering::Relaxed);
            h.ui_events.try_iter().for_each(drop);

            run_command(&mut h, &mut record, SequencerCommand::Undo);

            assert_eq!(h.sequencer.cursor_tick(), 720, "{name}");
            assert_eq!(h.sequencer.selected_track_index(), Some(0), "{name}");
            assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id), "{name}");
            let events: Vec<UiEvent> = h.ui_events.try_iter().collect();
            assert!(
                matches!(
                    events.last(),
                    Some(UiEvent::TimeSelectionSet { rect }) if *rect == marquee
                ),
                "{name}"
            );
        }
    }

    /// An edit made with no lead clip undoes to its track with no lead,
    /// whatever the user selected since: the state before the edit, not
    /// the lead picked now cleared on the track moved to.
    #[test]
    fn undoing_an_edit_made_with_no_lead_restores_its_track_without_one() {
        let mut h = harness();
        add_clip(&mut h.sequencer, 1920);
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));
        assert_eq!(h.sequencer.selected_clip_id(), None);
        let mut record: Record<UndoStep> = Record::new();
        let mute = SequencerCommand::MuteClipsInRange {
            rect: rect(1920, 3840, 0, 0),
        };
        run_command(&mut h, &mut record, mute);
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        assert!(h.sequencer.selected_clip_id().is_some());

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), None);
    }

    /// Selecting the performance lane leaves the track: no selected track,
    /// no lead clip and no marquee while it is armed.
    #[test]
    fn selecting_the_performance_lane_drops_the_track_the_lead_and_the_marquee() {
        let (mut h, _, _) = harness_with_clip();
        h.handlers
            .set_marquee_workflow(&mut h.sequencer, rect(0, 1920, 0, 0));
        assert!(h.sequencer.selected_clip_id().is_some());

        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);

        assert!(h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), None);
        assert_eq!(h.sequencer.selected_clip_id(), None);
        assert_eq!(h.sequencer.marquee(), None);
    }

    /// No marquee exists while the lane is armed, whatever asks for one:
    /// the view's mirror and a workflow's are both refused, and the view is
    /// told to drop its own.
    #[test]
    fn a_marquee_is_refused_while_the_performance_lane_is_armed() {
        let (mut h, _, _) = harness_with_clip();
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        while h.ui_events.try_recv().is_ok() {}

        h.handlers
            .mirror_marquee_workflow(&mut h.sequencer, Some(rect(0, 1920, 0, 0)));
        assert_eq!(h.sequencer.marquee(), None);
        h.handlers
            .set_marquee_workflow(&mut h.sequencer, rect(0, 1920, 0, 0));
        assert_eq!(h.sequencer.marquee(), None);

        let events: Vec<_> = h.ui_events.try_iter().collect();
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, UiEvent::TimeSelectionCleared))
        );

        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        h.handlers
            .mirror_marquee_workflow(&mut h.sequencer, Some(rect(0, 1920, 0, 0)));
        assert_eq!(h.sequencer.marquee(), Some(rect(0, 1920, 0, 0)));
    }

    /// A track selected from the lane disarms it and takes the clip under
    /// the cursor as its lead, as any track selection does.
    #[test]
    fn selecting_a_track_from_the_lane_selects_the_clip_under_the_cursor() {
        let (mut h, clip_id, _) = harness_with_clip();
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);

        h.handlers.select_track_workflow(&mut h.sequencer, 0);

        assert!(!h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(clip_id));
    }

    /// An undo that puts a track back leaves the performance lane: the
    /// track is selected the ordinary way, which disarms, and the lead from
    /// before the edit comes back with it.
    #[test]
    fn an_undo_that_puts_a_track_back_leaves_the_performance_lane() {
        let (mut h, lead_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        let carve = SequencerCommand::RemoveClips {
            rect: rect(0, 3840, 0, 0),
        };
        run_command(&mut h, &mut record, carve);
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert!(!h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id));
    }

    /// A step recorded while the lane was armed has no track to put back:
    /// undoing it changes no selection, and arming is never restored, so
    /// the track selected since stays.
    #[test]
    fn an_undo_of_a_step_recorded_while_armed_changes_no_selection() {
        let (mut h, clip_id, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        let carve = SequencerCommand::RemoveClips {
            rect: rect(0, 3840, 0, 0),
        };
        run_command(&mut h, &mut record, carve);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert!(h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), None);
        assert_eq!(h.sequencer.selected_clip_id(), None);
        assert!(h.sequencer.clip_on(0, clip_id).is_some());

        run_command(&mut h, &mut record, SequencerCommand::Redo);
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert!(!h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
    }

    /// An undo or redo of a note edit leaves the clip cursor where the
    /// edit's result put it (the duplicate span's start), rather than
    /// resetting it from the arranger cursor, which the step didn't move.
    #[test]
    fn undoing_a_span_duplicate_keeps_the_clip_cursor_on_the_span() {
        let (mut h, _, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        h.cursor_tick.store(1200, Ordering::Relaxed);
        h.handlers.select_all_events_workflow(&mut h.sequencer);
        let clip_cursor = |h: &Harness| h.sequencer.selected_clip().unwrap().cursor_tick();
        let duplicate = SequencerCommand::DuplicateSelectedEvents {
            span: Some((0, 480)),
        };

        run_command(&mut h, &mut record, duplicate);
        assert_eq!(clip_cursor(&h), 480);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(clip_cursor(&h), 0);

        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(clip_cursor(&h), 480);
    }

    /// Decision 7 (`440-undo-view-state.md`): ⌘Z after an agent write undoes
    /// it to what the user was looking at when it ran — the lead clip, its
    /// track, the cursor and the marquee the write read from.
    #[test]
    fn undoing_an_agent_write_restores_the_view_it_ran_in() {
        let (mut h, lead_id, _) = harness_with_clip();
        h.cursor_tick.store(480, Ordering::Relaxed);
        let marquee = rect(0, 1920, 0, 0);
        h.sequencer.set_marquee(Some(marquee));
        let mut record = Record::new();
        let created = agent_call(
            &mut h,
            &mut record,
            "create_clips",
            json!({ "clips": [{ "track": 3, "start": "3", "length": 4, "notes": "C3 0 1 90" }] }),
        );
        assert!(!created.is_error, "{}", created.text);
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        h.cursor_tick.store(3840 * 8, Ordering::Relaxed);
        h.sequencer.set_marquee(None);

        run_command(&mut h, &mut record, SequencerCommand::Undo);

        assert!(h.sequencer.tracks()[2].clips().is_empty());
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(lead_id));
        assert_eq!(h.sequencer.cursor_tick(), 480);
        assert_eq!(h.sequencer.marquee(), Some(marquee));
    }

    /// A drag merged into one step undoes to where its first step started
    /// and redoes to where its last ended (`440-undo-view-state.md`
    /// decision 2).
    #[test]
    fn a_merged_drag_undoes_to_its_start_and_redoes_to_its_end() {
        let (mut h, _, note_id) = harness_with_clip();
        let mut record = Record::new();
        let nudge = |nudge| SequencerCommand::DragEventsVelocity {
            event_ids: vec![note_id],
            nudge,
            drag_id: 7,
        };
        run_command(&mut h, &mut record, nudge(5));
        h.cursor_tick.store(240, Ordering::Relaxed);
        run_command(&mut h, &mut record, nudge(5));
        assert_eq!(record.len(), 1);
        h.cursor_tick.store(960, Ordering::Relaxed);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.cursor_tick(), 0);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.cursor_tick(), 240);
    }

    /// A drag back to where it began annuls its step (`Merged::Annul`), and
    /// the step below it keeps its own `after`: its redo doesn't land where
    /// the annulled drag was made.
    #[test]
    fn an_annulled_drag_leaves_the_step_below_its_view() {
        let (mut h, _, note_id) = harness_with_clip();
        h.cursor_tick.store(120, Ordering::Relaxed);
        let mut record = Record::new();
        let insert = SequencerCommand::InsertNote {
            tick: 480,
            length: 480,
            pitch: 64,
            velocity: 100,
        };
        run_command(&mut h, &mut record, insert);
        h.cursor_tick.store(600, Ordering::Relaxed);
        let moved = Move {
            delta_ticks: 240,
            delta_pitch: 0,
        };
        let back = Move {
            delta_ticks: 0,
            delta_pitch: 0,
        };
        run_command(&mut h, &mut record, drag_step(&[note_id], moved));
        run_command(&mut h, &mut record, drag_step(&[note_id], back));
        assert_eq!(record.len(), 1);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.cursor_tick(), 120);
        h.cursor_tick.store(1500, Ordering::Relaxed);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.cursor_tick(), 120);
    }

    /// A project that fails to open leaves the open project as it is, the
    /// focus with it: from the clip view, the clip view keeps the keyboard
    /// (`440-undo-view-state.md` decision 6). It used to move it to the
    /// arranger, left over from when loading was a view state of its own.
    #[test]
    fn a_failed_open_keeps_the_focus() {
        let (mut h, _, _) = harness_with_clip();
        let mut record: Record<UndoStep> = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::EnterClip);
        let path = std::env::temp_dir().join(format!("stev-gone-{}.stev", std::process::id()));

        run_command(
            &mut h,
            &mut record,
            project_action(ProjectAction::OpenFile { path }, true),
        );

        assert_eq!(h.handlers.view_state(), ViewState::Clip);
        let status = h.ui_events.try_iter().find_map(|event| match event {
            UiEvent::Status { message } => Some(message),
            _ => None,
        });
        assert!(status.is_some_and(|status| status.starts_with("Could not open stev-gone-")));
    }

    /// Decision 13: a write while a modal is up says why and changes
    /// nothing, while a read still answers.
    #[test]
    fn agent_writes_wait_for_an_open_modal_but_reads_answer() {
        let mut h = harness();
        h.sequencer.set_modal_open(true);
        let mut record = Record::new();
        let refused = agent_call(&mut h, &mut record, "set_tempo", json!({ "bpm": 100 }));
        assert!(
            refused.is_error && refused.text.contains("dialog"),
            "{}",
            refused.text
        );
        assert_eq!(record.len(), 0);
        let read = agent_call(&mut h, &mut record, "get_context", json!({}));
        assert!(!read.is_error, "{}", read.text);
    }
}
