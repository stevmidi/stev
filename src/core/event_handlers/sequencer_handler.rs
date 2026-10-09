//! `handle_sequencer_command` — the one `match` over every
//! [`SequencerCommand`]. Arms are
//! kept thin: undoable ops build a `SequencerEdit`, record it, and pass the
//! result to `handle_edit_result`; the rest delegate to a workflow file. See
//! `050-undo-redo.md`.

use std::time::Instant;

use undo::Record;

use crate::core::sequencer::{
    CommitClipEdit, DeleteSelectedEventsEdit, DeleteTimeEdit, DragEventsVelocityEdit,
    DragNotesEdit, DuplicateClipsEdit, DuplicateTimeEdit, InsertCaptureEdit, InsertNotesEdit,
    InsertSilenceEdit, MoveClipEdit, MoveRangeEdit, MuteInRangeEdit, MuteSelectedEventsEdit,
    NudgeSelectedEventsEdit, NudgeSelectedEventsLengthEdit, PasteClipsEdit, QuantizeEventsEdit,
    RenameTrackEdit, SequencerEdit, SetTempoEdit, SplitClipsEdit, TempoGesture,
    TransposeSelectedEventsEdit,
};
use crate::core::time::TapTempo;
use crate::models::clip::ClipEdge;

use super::*;

impl EventHandlers {
    /// Applies one [`SequencerCommand`]
    /// against `sequencer`, recording undoable ones on `undo_record`, keeping
    /// `tap_tempo` for tap-tempo and `saved` for the unsaved-changes
    /// check. Called from the `"sequencer"` thread's `select!` loop.
    pub(crate) fn handle_sequencer_command(
        &self,
        cmd: &SequencerCommand,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        tap_tempo: &mut TapTempo,
        saved: &mut SavedProject,
    ) {
        match cmd {
            SequencerCommand::Commit => {
                // Into the lead clip if there is one, otherwise a new clip:
                // the last pass while running, the detected phrase while
                // stopped.
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
                        InsertCaptureEdit::from_stopped_capture(sequencer).map(SequencerEdit::from)
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

            SequencerCommand::ToggleLiveRecording => {
                if sequencer.is_recording() {
                    self.end_live_recording_workflow(sequencer, undo_record);
                } else {
                    self.start_live_recording_workflow(sequencer);
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
                    Some(rect) => SplitClipsEdit::from_time_range_in_tracks(
                        sequencer,
                        rect.track_start,
                        rect.track_end,
                        rect.start,
                        rect.end,
                        cursor,
                    ),
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
                let edit = DragNotesEdit::from_sequencer(
                    sequencer,
                    undo_record,
                    event_ids.clone(),
                    *drag,
                    *drag_id,
                );
                self.record_edit(sequencer, undo_record, edit);
            }

            SequencerCommand::PreviewNote { note, velocity } => {
                sequencer.preview_pitch(*note, *velocity);
            }

            SequencerCommand::DuplicateSelectedEvents => {
                self.record_edit(
                    sequencer,
                    undo_record,
                    InsertNotesEdit::duplicating_selection(sequencer),
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
                    saved,
                    action,
                    *discard_changes,
                );
            }

            SequencerCommand::SaveProject { filename, folder } => {
                self.save_project_workflow(sequencer, saved, folder.as_deref(), filename);
            }

            SequencerCommand::ExportClip {
                time_bounds,
                folder,
                name,
            } => {
                self.export_clip_workflow(sequencer, *time_bounds, folder.as_deref(), name);
            }

            SequencerCommand::ImportMidiClip { clip, name, target } => {
                self.import_clip_workflow(sequencer, undo_record, clip, name, *target);
            }

            SequencerCommand::Undo => {
                if let Some(result) = undo_record.undo(sequencer) {
                    self.handle_edit_result(sequencer, result);
                }
            }

            SequencerCommand::Redo => {
                if let Some(result) = undo_record.redo(sequencer) {
                    self.handle_edit_result(sequencer, result);
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

            SequencerCommand::SetTrackOutput { track, output } => {
                sequencer.set_track_output(*track, output.clone());
                self.emit_tracks(sequencer, None);
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
                sequencer.set_slot_instrument_state(*slot, state.clone());
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

            SequencerCommand::FitTempo => {
                self.fit_first_clip_tempo_workflow(sequencer, undo_record);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    use undo::Record;
    use uuid::Uuid;

    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::midi::input::InputTicks;
    use crate::core::project::ProjectAction;
    use crate::core::sequencer::{SequencerCommand, SequencerEdit};
    use crate::core::view_state::ViewState;
    use crate::models::{
        clip::NoteDrag::{self, Move, ResizeEnd, ResizeStart},
        event::EventType,
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

    /// The header's BPM field sets the tempo as one undo step, and the
    /// change counts as unsaved.
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
            state: Vec::new(),
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

    /// The stopped `/` with a lead clip under the arranger cursor inserts the
    /// played phrase into it as one undoable edit: no pending view opens, the
    /// transport is not sent anything, and ⌘Z takes the notes back out.
    #[test]
    fn stopped_commit_into_the_lead_clip_is_one_undoable_insert() {
        let mut h = harness();
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Arranger);
        h.cursor_tick.store(480, Ordering::Relaxed);
        for (tick, message) in [(100, [0x90, 64, 100]), (400, [0x80, 64, 0])] {
            let ticks = InputTicks {
                position: tick,
                elapsed: tick,
            };
            h.sequencer.handle_midi_input_dispatch(&message, ticks);
        }
        while h.transport_commands.try_recv().is_ok() {}

        let mut record: Record<SequencerEdit> = Record::new();
        run_command(&mut h, &mut record, SequencerCommand::Commit);

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
        let mut record: Record<SequencerEdit> = Record::new();
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
        let mut record: Record<SequencerEdit> = Record::new();
        run_command(&mut h, &mut record, quit());
        assert_eq!(last_project_reply(&h), Some(None));
    }

    /// A change since the last load asks for the prompt instead of running
    /// the action, and leaves the project alone; Don't Save then runs it.
    #[test]
    fn a_changed_project_prompts_and_dont_save_goes_on() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();
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

    /// The check compares what would be saved, not whether anything
    /// happened: undoing back to the loaded state leaves nothing unsaved,
    /// and a change no undo step records (a track's volume) still counts.
    #[test]
    fn unsaved_means_it_would_save_differently() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();
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
        let mut record: Record<SequencerEdit> = Record::new();

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
        let mut record: Record<SequencerEdit> = Record::new();
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
        let mut record: Record<SequencerEdit> = Record::new();
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
        let mut record: Record<SequencerEdit> = Record::new();
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
        let mut record: Record<SequencerEdit> = Record::new();
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
        let undone = record.undo(&mut h.sequencer).unwrap();
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
}
