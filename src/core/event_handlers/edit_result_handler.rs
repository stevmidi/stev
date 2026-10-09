//! The single place an [`EditResult`] is
//! turned into `UiEvent`s — called from every `undo_record.edit/undo/redo`
//! arm. One match on the result's variant fans out clip-added / -updated /
//! -removed and event-updated notifications. See `050-undo-redo.md`.

use crate::core::sequencer::{EditResult, PasteLead};

use super::clip_range_edits::CarveResult;
use super::*;

impl EventHandlers {
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
            EditResult::ClipsUnsplit {
                updated,
                removed,
                selected_track_idx,
                selected_clip_id,
            } => {
                self.unsplit_clips_workflow(
                    sequencer,
                    &updated,
                    &removed,
                    selected_track_idx,
                    selected_clip_id,
                );
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
                self.ui_event_tx.send(UiEvent::TimeSelectionCleared).ok();
            }
            EditResult::TimeUndeleted {
                shifted,
                carve_updated,
                carve_added,
                carve_removed,
                selected_track_idx,
                selected_clip_id,
                restored_selection,
            } => {
                self.restore_time_workflow(
                    sequencer,
                    &shifted,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    selected_track_idx,
                    selected_clip_id,
                );
                self.send_time_selection_set_ui_event(
                    restored_selection.0,
                    restored_selection.1,
                    None,
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
                selected_track_idx,
                selected_clip_id,
            } => {
                self.restore_range_workflow(
                    sequencer,
                    CarveResult::new(&updated, &added, &removed),
                    selected_track_idx,
                    selected_clip_id,
                );
            }
            EditResult::RangeMuted { updated, added } => {
                self.mute_in_range_workflow(sequencer, &updated, &added);
            }
            EditResult::RangeUnmuted {
                updated,
                removed,
                selected_track_idx,
                selected_clip_id,
            } => {
                self.unmute_in_range_workflow(
                    sequencer,
                    &updated,
                    &removed,
                    selected_track_idx,
                    selected_clip_id,
                );
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
                selected_track_idx,
                selected_clip_id,
            } => {
                self.unpaste_clips_workflow(
                    sequencer,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    &unpasted,
                    selected_track_idx,
                    selected_clip_id,
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
                self.send_time_selection_set_ui_event(
                    new_selection.start,
                    new_selection.end,
                    Some((new_selection.track_start, new_selection.track_end)),
                );
            }
            EditResult::ClipsUnduplicated {
                carve_updated,
                carve_added,
                carve_removed,
                unpasted,
                selected_track_idx,
                selected_clip_id,
                restored_selection,
            } => {
                self.unpaste_clips_workflow(
                    sequencer,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    &unpasted,
                    selected_track_idx,
                    selected_clip_id,
                );
                self.send_time_selection_set_ui_event(
                    restored_selection.start,
                    restored_selection.end,
                    Some((restored_selection.track_start, restored_selection.track_end)),
                );
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
                restored_selection,
            } => {
                self.unmove_range_workflow(
                    sequencer,
                    &unmoved,
                    &restored,
                    &split_removed,
                    CarveResult::new(&carve_updated, &carve_added, &carve_removed),
                    restored_selection,
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
                // then advance the Arranger time selection onto the copy. The
                // paste carve buckets are always empty — the gap is pre-cleared.
                self.insert_silence_workflow(sequencer, &shifted, &split_updated, &split_added);
                self.paste_clips_workflow(
                    sequencer,
                    CarveResult::EMPTY,
                    &pasted,
                    PasteLead::CursorRule,
                );
                self.send_time_selection_set_ui_event(new_selection.0, new_selection.1, None);
            }
            EditResult::TimeUnduplicated {
                unpasted,
                shifted,
                split_updated,
                split_removed,
                selected_track_idx,
                selected_clip_id,
                restored_selection,
            } => {
                self.unpaste_clips_workflow(
                    sequencer,
                    CarveResult::EMPTY,
                    &unpasted,
                    selected_track_idx,
                    selected_clip_id,
                );
                self.remove_silence_workflow(sequencer, &shifted, &split_updated, &split_removed);
                self.send_time_selection_set_ui_event(
                    restored_selection.0,
                    restored_selection.1,
                    None,
                );
            }
            EditResult::ClipResized { clip, retime } => {
                self.clip_resized_workflow(sequencer, clip, retime);
            }
            EditResult::EventsModified {
                track_idx,
                clip_id,
                selected_event_ids,
                inserted_take,
            } => {
                sequencer.reset();

                if let Some(clip) = sequencer.clip_on_mut(track_idx, clip_id) {
                    clip.restore_event_selection(&selected_event_ids);
                }

                self.sync_clip_post_edit_workflow(sequencer);
                self.send_events_selected_ui_event(selected_event_ids);

                // The only `EditResult` that can change which events are
                // selected (delete, duplicate, or undoing either) — this is
                // the one place in this function the selection mirror needs
                // republishing.
                self.publish_event_selection(sequencer);

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
            EditResult::TempoChanged | EditResult::MeterChanged => self.request_repaint(),
            EditResult::NoOp => {}
        }
    }
}
