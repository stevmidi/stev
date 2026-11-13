//! The single place an [`EditResult`] is
//! turned into `UiEvent`s — called from every `undo_record.edit/undo/redo`
//! arm. One match on the result's variant fans out clip-added / -updated /
//! -removed and event-updated notifications. What an undo or redo puts back
//! of the view is not in the result but in the step (`handle_step_output`,
//! `selection.rs`'s `restore_view_workflow`). See `050-undo-redo.md`.

use crate::core::sequencer::{EditResult, PasteLead, StepOutput};

use super::clip_range_edits::CarveResult;
use super::*;

impl EventHandlers {
    /// An undo or redo: the clips first (`handle_edit_result`), then the
    /// view the step puts back, if any (`restore_view_workflow`).
    pub(super) fn handle_step_output(&self, sequencer: &mut Sequencer, step: StepOutput) {
        let (result, view) = step;
        self.handle_edit_result(sequencer, result);
        if let Some(view) = view {
            self.restore_view_workflow(sequencer, view);
        }
    }

    /// Dispatch UI side-effects based on what an edit/undo/redo did. Called
    /// from every `undo_record.edit/undo/redo` arm in
    /// `handle_sequencer_command`; the clip-level workflows it fans out to
    /// live in `clip_lifecycle.rs` and `clip_range_edits.rs`. See
    /// `050-undo-redo.md`.
    pub(super) fn handle_edit_result(&self, sequencer: &mut Sequencer, result: EditResult) {
        match result {
            EditResult::ClipsSplit { updated, added } => {
                self.split_clips_workflow(sequencer, &updated, &added);
            }
            EditResult::ClipsUnsplit { updated, removed } => {
                self.unsplit_clips_workflow(sequencer, &updated, &removed);
            }
            EditResult::SilenceInserted {
                shifted,
                split_updated,
                split_added,
            } => {
                self.insert_silence_workflow(sequencer, &shifted, &split_updated, &split_added);
            }
            EditResult::SilenceRemoved {
                shifted,
                split_updated,
                split_removed,
            } => {
                self.remove_silence_workflow(sequencer, &shifted, &split_updated, &split_removed);
            }
            EditResult::TimeDeleted {
                shifted,
                carve_updated,
                carve_added,
                carve_removed,
                selected_track_idx,
                selected_clip_id,
            } => {
                self.delete_time_workflow(
                    sequencer,
                    &shifted,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    selected_track_idx,
                    selected_clip_id,
                );
                // The deleted span is gone, so the marquee no longer refers
                // to any real content — collapse it back to the cursor.
                self.clear_marquee_workflow(sequencer);
            }
            EditResult::TimeUndeleted {
                shifted,
                carve_updated,
                carve_added,
                carve_removed,
            } => {
                self.restore_time_workflow(
                    sequencer,
                    &shifted,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                );
            }
            EditResult::RangeDeleted {
                updated,
                added,
                removed,
                selected_track_idx,
                selected_clip_id,
            } => {
                self.delete_in_range_workflow(
                    sequencer,
                    CarveResult::new(&updated, &added, &removed),
                    selected_track_idx,
                    selected_clip_id,
                );
            }
            EditResult::RangeRestored {
                updated,
                added,
                removed,
            } => {
                self.restore_range_workflow(
                    sequencer,
                    CarveResult::new(&updated, &added, &removed),
                );
            }
            EditResult::RangeMuted { updated, added } => {
                self.mute_in_range_workflow(sequencer, &updated, &added);
            }
            EditResult::RangeUnmuted { updated, removed } => {
                self.unmute_in_range_workflow(sequencer, &updated, &removed);
            }
            EditResult::ClipCommitted { clip } => {
                self.commit_clip_workflow(sequencer, &clip);
            }
            EditResult::ClipUncommitted { clip } => {
                self.uncommit_clip_workflow(sequencer, &clip);
            }
            EditResult::ClipsPasted {
                carve_updated,
                carve_added,
                carve_removed,
                pasted,
                lead,
            } => {
                self.paste_clips_workflow(
                    sequencer,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    &pasted,
                    lead,
                );
            }
            EditResult::ClipsUnpasted {
                carve_updated,
                carve_added,
                carve_removed,
                unpasted,
            } => {
                self.unpaste_clips_workflow(
                    sequencer,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    &unpasted,
                );
            }
            EditResult::ClipsDuplicated {
                carve_updated,
                carve_added,
                carve_removed,
                pasted,
                new_selection,
            } => {
                // A paste in every respect — reuse its workflow verbatim —
                // then slide the marquee onto the copy so repeated presses
                // chain. The edit knows its own track span, so the rect is
                // named in full rather than recovered view-side.
                self.paste_clips_workflow(
                    sequencer,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    &pasted,
                    PasteLead::CursorRule,
                );
                self.set_marquee_workflow(sequencer, new_selection);
            }
            EditResult::ClipMoved {
                from,
                to,
                carve_updated,
                carve_added,
                carve_removed,
            } => {
                self.move_clip_workflow(
                    sequencer,
                    &from,
                    &to,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                );
            }
            EditResult::ClipUnmoved {
                from,
                to,
                carve_updated,
                carve_added,
                carve_removed,
            } => {
                self.unmove_clip_workflow(
                    sequencer,
                    &from,
                    &to,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                );
            }
            EditResult::RangeMoved {
                moved_from,
                moved_to,
                split_updated,
                split_added,
                carve_updated,
                carve_added,
                carve_removed,
                new_selection,
            } => {
                self.move_range_workflow(
                    sequencer,
                    &moved_from,
                    &moved_to,
                    &split_updated,
                    &split_added,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    new_selection,
                );
            }
            EditResult::RangeUnmoved {
                unmoved,
                restored,
                split_removed,
                carve_updated,
                carve_added,
                carve_removed,
            } => {
                self.unmove_range_workflow(
                    sequencer,
                    &unmoved,
                    &restored,
                    &split_removed,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                );
            }
            EditResult::TimeDuplicated {
                shifted,
                split_updated,
                split_added,
                pasted,
                new_selection,
            } => {
                // Reuse both halves' workflows verbatim: the insert-silence half
                // first (shifted / split clips), then the paste half (the copy),
                // then advance the Arranger time selection onto the copy, on
                // the tracks the edit froze. The paste carve buckets are
                // always empty — the gap is pre-cleared.
                self.insert_silence_workflow(sequencer, &shifted, &split_updated, &split_added);
                self.paste_clips_workflow(
                    sequencer,
                    CarveResult::EMPTY,
                    &pasted,
                    PasteLead::CursorRule,
                );
                self.set_marquee_workflow(sequencer, new_selection);
            }
            EditResult::TimeUnduplicated {
                unpasted,
                shifted,
                split_updated,
                split_removed,
            } => {
                self.unpaste_clips_workflow(sequencer, CarveResult::EMPTY, &unpasted);
                self.remove_silence_workflow(sequencer, &shifted, &split_updated, &split_removed);
            }
            EditResult::ClipResized { clip, retime } => {
                self.clip_resized_workflow(sequencer, clip, retime);
            }
            EditResult::EventsModified {
                track_idx,
                clip_id,
                selected_event_ids,
                inserted_take,
                duplicate_span,
            } => {
                sequencer.reset();

                if let Some(clip) = sequencer.clip_on_mut(track_idx, clip_id) {
                    clip.restore_event_selection(&selected_event_ids);
                }

                // An event edit, or its undo or redo, on a clip that isn't
                // the lead (the agent bridge's `replace_clip_notes`, or ⌘Z
                // after the lead moved on): the clip panel shows another
                // clip, so only the arranger's thumbnail of this one changes.
                if sequencer.selected_clip_id() != Some(clip_id) {
                    if let Some(clip) = sequencer.clip_on(track_idx, clip_id) {
                        let clip = ClipMetadata::from_clip(track_idx, clip);
                        self.ui_event_tx.send(UiEvent::ClipUpdated { clip }).ok();
                    }
                    return;
                }

                self.sync_clip_post_edit_workflow(sequencer);
                self.send_events_selected_ui_event(selected_event_ids);

                // The only `EditResult` that can change which events are
                // selected (delete, duplicate, or undoing either) — this is
                // the one place in this function the selection mirror needs
                // republishing.
                self.publish_event_selection(sequencer);

                // ⌘/Ctrl+D over the piano roll's duplicate span, or its undo
                // or redo: the cursor and span follow the copy, as Duplicate
                // Clips' marquee does.
                if let Some((start, end)) = duplicate_span {
                    self.set_clip_cursor_workflow(sequencer, start);
                    self.ui_event_tx
                        .send(UiEvent::DuplicateSpanSet { start, end })
                        .ok();
                }

                // After the sync's `EventsUpdated`, so the view re-frames over
                // the notes as they now are.
                if let Some(pitch_range) = inserted_take {
                    self.ui_event_tx
                        .send(UiEvent::CaptureInserted {
                            clip_id,
                            pitch_range,
                        })
                        .ok();
                }
            }
            EditResult::TrackAdded {
                track_idx,
                track_id,
                slot,
                instrument,
            } => {
                self.track_added_workflow(sequencer, track_idx, track_id, slot, instrument);
            }
            EditResult::TrackRemoved {
                track_idx,
                track_id,
                slot,
            } => {
                self.track_removed_workflow(sequencer, track_idx, track_id, slot);
            }
            EditResult::TrackRenamed => self.emit_tracks(sequencer, None),
            EditResult::KnobsChanged => {
                self.emit_tracks(sequencer, None);
                // The lead clip's moves may have changed with the knob.
                self.sync_clip_post_edit_workflow(sequencer);
            }
            EditResult::TrackOutputChanged {
                track_id,
                slot,
                instrument,
            } => {
                self.emit_tracks(sequencer, None);
                self.sync_clip_post_edit_workflow(sequencer);
                self.send_slot_instrument(slot, track_id, instrument);
            }
            EditResult::TempoChanged | EditResult::MeterChanged => self.request_repaint(),
            EditResult::NoOp => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;
    use uuid::Uuid;

    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::input_event::TimeSelectionRect;
    use crate::core::sequencer::test_support::{clip_at, rect};
    use crate::core::sequencer::{SequencerCommand, UndoStep, ViewSnapshot};
    use crate::view::display::UiEvent;

    /// A clip at the start of track 0, the lead, with the cursor at
    /// `cursor_tick` and the view's marquee `marquee` mirrored, as the view
    /// publishes it before any command (`440-undo-view-state.md`
    /// decision 4).
    fn harness_with(cursor_tick: i32, marquee: TimeSelectionRect) -> (Harness, Uuid) {
        let mut h = harness();
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.cursor_tick.store(cursor_tick, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.sequencer.set_marquee(Some(marquee));
        (h, clip_id)
    }

    /// The user moves on: another track, the cursor far off, no marquee.
    fn move_on(h: &mut Harness) {
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        h.cursor_tick.store(3840 * 8, Ordering::Relaxed);
        h.sequencer.set_marquee(None);
        h.ui_events.try_iter().for_each(drop);
    }

    /// The last marquee change sent to the view: `Some(None)` for a
    /// cleared one, `None` for none sent.
    fn marquee_sent(h: &Harness) -> Option<Option<TimeSelectionRect>> {
        h.ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::TimeSelectionSet { rect } => Some(Some(rect)),
                UiEvent::TimeSelectionCleared => Some(None),
                _ => None,
            })
            .last()
    }

    /// Delete Time's undo puts back its marquee and the cursor it was
    /// anchored to, as well as the lead clip; its redo the state after it,
    /// with the marquee gone. The undo used to put the marquee back
    /// wherever the cursor had gone, and the redo kept the one the user had.
    #[test]
    fn delete_time_undoes_to_its_marquee_and_redoes_without_one() {
        let marquee = rect(480, 960, 0, 0);
        let (mut h, clip_id) = harness_with(480, marquee);
        let mut record: Record<UndoStep> = Record::new();
        let delete = SequencerCommand::DeleteTimeInRange {
            start: 480,
            end: 960,
        };
        run_command(&mut h, &mut record, delete);
        assert_eq!(h.sequencer.marquee(), None, "the mirror follows at once");
        move_on(&mut h);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.cursor_tick(), 480);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(clip_id));
        assert_eq!(marquee_sent(&h), Some(Some(marquee)));
        assert_eq!(h.sequencer.marquee(), Some(marquee));

        move_on(&mut h);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.cursor_tick(), 480);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(marquee_sent(&h), Some(None));
    }

    /// Duplicate Time (`⇧⌘D`) and Duplicate Clips (`⌘D`) undo to the
    /// marquee with its cursor and track span, and redo onto the copy
    /// again. Duplicate Time spans every track, so the marquee it moves on
    /// keeps the mirror's span.
    #[test]
    fn the_duplicates_undo_to_their_marquee_and_redo_onto_the_copy() {
        let marquee = rect(0, 1920, 0, 1);
        let cases = [
            (
                "duplicate time",
                SequencerCommand::DuplicateTimeInRange {
                    start: 0,
                    end: 1920,
                },
            ),
            (
                "duplicate clips",
                SequencerCommand::DuplicateClips { rect: marquee },
            ),
        ];
        for (name, command) in cases {
            let (mut h, _) = harness_with(0, marquee);
            h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));
            let mut record: Record<UndoStep> = Record::new();
            run_command(&mut h, &mut record, command);
            let copy = rect(1920, 3840, 0, 1);
            assert_eq!(h.sequencer.marquee(), Some(copy), "{name}");
            move_on(&mut h);

            run_command(&mut h, &mut record, SequencerCommand::Undo);
            assert_eq!(h.sequencer.cursor_tick(), 0, "{name}");
            assert_eq!(marquee_sent(&h), Some(Some(marquee)), "{name}");

            move_on(&mut h);
            run_command(&mut h, &mut record, SequencerCommand::Redo);
            assert_eq!(h.sequencer.cursor_tick(), 0, "{name}");
            assert_eq!(h.sequencer.selected_track_index(), Some(0), "{name}");
            assert_eq!(marquee_sent(&h), Some(Some(copy)), "{name}");
        }
    }

    /// A ⌥-drag copy leaves the selection on the copy — cursor on its
    /// start, its track, the copy as lead, the marquee shifted onto it —
    /// so drags chain; `⌘Z` puts back the selection from before the drag
    /// and redo the one after (`460-copy-drag.md` decision 4).
    #[test]
    fn a_copy_drag_selects_the_copy_and_undoes_to_the_source() {
        let marquee = rect(0, 1920, 0, 0);
        let (mut h, source) = harness_with(0, marquee);
        let mut record: Record<UndoStep> = Record::new();
        let drag = SequencerCommand::CopyRange {
            rect: marquee,
            delta_ticks: 3840,
            delta_tracks: 1,
        };
        run_command(&mut h, &mut record, drag);
        let copy_rect = rect(3840, 5760, 1, 1);
        assert_eq!(h.sequencer.marquee(), Some(copy_rect));
        assert_eq!(h.sequencer.cursor_tick(), 3840);
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
        let copy = h.sequencer.selected_clip_id().unwrap();
        assert_ne!(copy, source);
        assert!(h.sequencer.tracks()[0].get_clip_by_id(source).is_some());
        assert!(h.sequencer.tracks()[1].get_clip_by_id(copy).is_some());
        move_on(&mut h);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.cursor_tick(), 0);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(source));
        assert_eq!(marquee_sent(&h), Some(Some(marquee)));
        assert!(h.sequencer.tracks()[1].clips().is_empty());

        move_on(&mut h);
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.cursor_tick(), 3840);
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
        assert_eq!(h.sequencer.selected_clip_id(), Some(copy));
        assert_eq!(marquee_sent(&h), Some(Some(copy_rect)));
    }

    /// A marquee move's undo puts the cursor back where the move found it,
    /// here on the marquee's end; it used to land on the marquee's start.
    #[test]
    fn a_marquee_move_undoes_to_its_cursor() {
        let marquee = rect(0, 1920, 0, 0);
        let (mut h, _) = harness_with(1920, marquee);
        let mut record: Record<UndoStep> = Record::new();
        let drag = SequencerCommand::MoveRange {
            rect: marquee,
            delta_ticks: 3840,
            delta_tracks: 0,
        };
        run_command(&mut h, &mut record, drag);
        assert_eq!(h.sequencer.cursor_tick(), 3840);
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.cursor_tick(), 1920);
        assert_eq!(marquee_sent(&h), Some(Some(marquee)));
    }

    /// A split's redo selects the right-hand piece the split selected, not
    /// the piece under the cursor as it is by then.
    #[test]
    fn a_split_redoes_to_the_piece_it_selected() {
        let (mut h, clip_id) = harness_with(960, rect(960, 960, 0, 0));
        let mut record: Record<UndoStep> = Record::new();
        let split = SequencerCommand::SplitClips { time_bounds: None };
        run_command(&mut h, &mut record, split);
        let right = h.sequencer.selected_clip_id().unwrap();
        assert_ne!(right, clip_id);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.selected_clip_id(), Some(clip_id));
        h.cursor_tick.store(0, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);

        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.selected_clip_id(), Some(right));
        assert_eq!(h.sequencer.cursor_tick(), 960);
    }

    /// The all-track cut (`⇧⌘X`) names only its ticks, yet its undo puts
    /// the marquee back on the tracks it had, from the mirror.
    #[test]
    fn the_all_track_cut_undoes_onto_its_tracks() {
        let marquee = rect(480, 960, 1, 2);
        let (mut h, _) = harness_with(480, marquee);
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));
        h.sequencer.tracks_mut()[2].add_clip(&clip_at(0, 1920));
        h.handlers.select_track_workflow(&mut h.sequencer, 1);
        let mut record: Record<UndoStep> = Record::new();
        let cut = SequencerCommand::CutClips {
            start: 480,
            end: 960,
        };
        run_command(&mut h, &mut record, cut);
        h.handlers.select_track_workflow(&mut h.sequencer, 3);
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
        assert_eq!(marquee_sent(&h), Some(Some(marquee)));
    }

    /// A tempo change undoes with the view as it is, as in Ableton: the
    /// marquee drawn since, and the cursor, stay.
    #[test]
    fn a_tempo_undo_leaves_the_marquee_alone() {
        let (mut h, _) = harness_with(0, rect(0, 1920, 0, 0));
        let tempo = h.sequencer.tempo_us();
        let mut record: Record<UndoStep> = Record::new();
        let set_tempo = SequencerCommand::SetTempo {
            tempo_us: 400_000,
            gesture: None,
        };
        run_command(&mut h, &mut record, set_tempo);
        let drawn = rect(3840, 7680, 1, 1);
        h.sequencer.set_marquee(Some(drawn));
        h.cursor_tick.store(3840, Ordering::Relaxed);
        h.ui_events.try_iter().for_each(drop);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.tempo_us(), tempo);
        assert_eq!(marquee_sent(&h), None);
        assert_eq!(h.sequencer.marquee(), Some(drawn));
        assert_eq!(h.sequencer.cursor_tick(), 3840);
    }

    /// A view naming a track that is gone leaves the selection as it is,
    /// and still puts back the cursor and the marquee.
    #[test]
    fn a_view_on_a_gone_track_leaves_the_selection() {
        let (mut h, clip_id) = harness_with(0, rect(0, 1920, 0, 0));
        let view = ViewSnapshot {
            track: Some(Uuid::new_v4()),
            lead_clip: Some(Uuid::new_v4()),
            cursor_tick: 960,
            marquee: None,
        };

        h.handlers.restore_view_workflow(&mut h.sequencer, view);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert_eq!(h.sequencer.selected_clip_id(), Some(clip_id));
        assert_eq!(h.sequencer.cursor_tick(), 960);
        assert_eq!(h.sequencer.marquee(), None);
    }
}
