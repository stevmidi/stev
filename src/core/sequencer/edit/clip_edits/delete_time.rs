//! Undoable "Delete Time" — the exact opposite of Insert Silence: closes a
//! `[start, end)` gap across all tracks, carving that span out non-rippling
//! (reusing [`DeleteInRangeEdit`] verbatim) and then rippling everything at
//! or after `end` back left by the gap width to remove the gap it left behind.

use std::collections::HashSet;

use uuid::Uuid;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::delete_in_range::DeleteInRangeEdit;
use super::partition_metadata;

// ---------------------------------------------------------------------------
// DeleteTime
// ---------------------------------------------------------------------------

/// Opposite of [`InsertSilenceEdit`](super::InsertSilenceEdit): closes a
/// `[start, end)` gap of `end - start` ticks, across every track.
/// Every clip overlapping the range is trimmed or split exactly like
/// `DeleteInRangeEdit` — reused verbatim rather than reimplementing the carve
/// — so nothing survives inside `[start, end)`. Every clip (pre-existing or
/// freshly split) with `start_tick() >= end` is then shifted *back* by
/// `end - start`, closing the gap left by the carve.
///
/// Shifting is a raw `Clip::set_start_tick` mutation, not a
/// `remove_clip_by_id`/`add_clip` round-trip — same reasoning as
/// `InsertSilenceEdit`: a uniform backward shift of an already-non-overlapping
/// set of clips can never create a new overlap, either among themselves or
/// with the untouched clips before `start` (every shifted clip lands at or
/// past `start`, and the carve already guarantees nothing occupies
/// `[start, end)`).
pub(crate) struct DeleteTimeEdit {
    /// Low tick of the deleted range — the selection undo restores.
    start: i32,
    /// High tick of the deleted range — every shift target's threshold.
    end: i32,
    /// The non-rippling carve of `[start, end)`. `None` when nothing
    /// overlaps the range at construction time (the common case: the range
    /// falls entirely in an already-empty span).
    delete: Option<DeleteInRangeEdit>,
    /// Clips (post-carve) with `start_tick() >= end` at the first `edit()`
    /// call — every one of these shifts left by `end - start`. Frozen after that
    /// first call so redo replays the exact same set rather than
    /// rescanning — same pattern as `InsertSilenceEdit`.
    shift_targets: Option<Vec<(usize, Uuid)>>,
}

impl DeleteTimeEdit {
    /// Builds the edit from the Arranger's time selection bounds. `None` if
    /// the range is empty/backwards, or if there is truly nothing to do (no
    /// clip overlaps `[start, end)` and no clip starts at or after `end`).
    pub(crate) fn from_time_range(sequencer: &Sequencer, start: i32, end: i32) -> Option<Self> {
        if end <= start {
            return None;
        }

        let delete = DeleteInRangeEdit::from_time_range(sequencer, start, end);

        let has_shift_target =
            delete.is_some() || !sequencer.clips_starting_at_or_after(end).is_empty();

        if !has_shift_target {
            return None;
        }

        Some(Self {
            start,
            end,
            delete,
            shift_targets: None,
        })
    }

    /// Carves `[start, end)` out, then shifts every clip at or after `end`
    /// left by the gap width. Returns [`EditResult::TimeDeleted`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let (carve_updated, carve_removed, carve_added_ids, selected_track_idx, selected_clip_id) =
            match self.delete.as_mut().map(|d| d.edit(sequencer)) {
                Some(EditResult::RangeDeleted {
                    updated,
                    added,
                    removed,
                    selected_track_idx,
                    selected_clip_id,
                }) => (
                    updated,
                    removed,
                    added.into_iter().map(|m| m.clip_id).collect(),
                    selected_track_idx,
                    selected_clip_id,
                ),
                _ => (Vec::new(), Vec::new(), HashSet::new(), None, None),
            };

        let end = self.end;
        let targets = self
            .shift_targets
            .get_or_insert_with(|| sequencer.clips_starting_at_or_after(end));
        sequencer.shift_clips(targets, self.start - self.end);

        let (carve_added, shifted) = partition_metadata(sequencer, targets, &carve_added_ids);

        if shifted.is_empty()
            && carve_added.is_empty()
            && carve_updated.is_empty()
            && carve_removed.is_empty()
        {
            return EditResult::NoOp;
        }

        EditResult::TimeDeleted {
            shifted,
            carve_updated,
            carve_added,
            carve_removed,
            selected_track_idx,
            selected_clip_id,
        }
    }

    /// Shifts the moved clips back right, then un-carves. Returns
    /// [`EditResult::TimeUndeleted`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some(targets) = self.shift_targets.as_ref() else {
            return EditResult::NoOp;
        };

        // Shift back right first — same ordering rationale as
        // `InsertSilenceEdit::undo`: this must happen before `delete.undo()`
        // reinstates the carve boundary piece this shift also covers, so it
        // lands back at its pre-shift (post-carve) position before the carve
        // itself unwinds.
        sequencer.shift_clips(targets, self.end - self.start);

        let (carve_updated, carve_added, carve_removed, selected_track_idx, selected_clip_id) =
            match self.delete.as_mut().map(|d| d.undo(sequencer)) {
                Some(EditResult::RangeRestored {
                    updated,
                    added,
                    removed,
                    selected_track_idx,
                    selected_clip_id,
                }) => (
                    updated,
                    added,
                    removed,
                    selected_track_idx,
                    selected_clip_id,
                ),
                _ => (Vec::new(), Vec::new(), Vec::new(), None, None),
            };

        let removed_ids: HashSet<Uuid> = carve_removed.iter().map(|m| m.clip_id).collect();
        let (_, shifted) = partition_metadata(sequencer, targets, &removed_ids);

        EditResult::TimeUndeleted {
            shifted,
            carve_updated,
            carve_added,
            carve_removed,
            selected_track_idx,
            selected_clip_id,
            restored_selection: (self.start, self.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use crate::models::clip::Clip;
    use crate::models::event::Event;

    use crate::core::sequencer::test_support::{
        clip_at, drain, instrument_track_0, sequencer_with, test_sequencer,
    };

    use super::*;

    fn starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    #[test]
    fn returns_none_on_empty_or_backwards_range() {
        let sequencer = test_sequencer();
        assert!(DeleteTimeEdit::from_time_range(&sequencer, 500, 500).is_none());
        assert!(DeleteTimeEdit::from_time_range(&sequencer, 500, 100).is_none());
    }

    #[test]
    fn returns_none_when_nothing_to_do() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // entirely before the range
        assert!(DeleteTimeEdit::from_time_range(&sequencer, 960, 1440).is_none());
    }

    #[test]
    fn pure_shift_with_no_overlapping_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // [0, 480), untouched
        sequencer.tracks_mut()[0].add_clip(&clip_at(1440, 480)); // [1440, 1920), shifts

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::TimeDeleted {
            shifted,
            carve_updated,
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDeleted");
        };

        assert!(carve_updated.is_empty());
        assert!(carve_added.is_empty());
        assert!(carve_removed.is_empty());
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 960);
        assert_eq!(shifted[0].end_tick, 1440);

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].start_tick(), 0);
        assert_eq!(clips[0].end_tick(), 480);
        assert_eq!(clips[1].start_tick(), 960);
        assert_eq!(clips[1].end_tick(), 1440);

        let EditResult::TimeUndeleted { shifted, .. } = edit.undo(&mut sequencer) else {
            panic!("expected TimeUndeleted");
        };
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 1440);
        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (1440, 1920)]);
    }

    /// A clip spanning the whole deleted range is carved down to its two
    /// non-overlapping remainders (like `DeleteInRangeEdit`), and the
    /// right-hand remainder is then reported at its final, post-shift
    /// position — not the intermediate carve boundary.
    #[test]
    fn clip_spanning_the_range_is_carved_then_its_remainder_shifts_to_final_position() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // [0, 1920), spans [480, 720)

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 720).unwrap();
        let EditResult::TimeDeleted {
            shifted,
            carve_updated,
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDeleted");
        };

        assert_eq!(carve_updated.len(), 1);
        assert_eq!(carve_updated[0].start_tick, 0);
        assert_eq!(carve_updated[0].end_tick, 480);
        assert!(carve_removed.is_empty());
        assert!(shifted.is_empty());

        // The right-hand remainder was carved at 720 then shifted left by
        // 240 (the deleted width) — its final position is 480, directly
        // against the left half.
        assert_eq!(carve_added.len(), 1);
        assert_eq!(carve_added[0].start_tick, 480);
        assert_eq!(carve_added[0].end_tick, 1680);

        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (480, 1680)]);
    }

    #[test]
    fn clip_fully_inside_the_range_is_removed_and_later_clips_ripple_back() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(480, 240)); // [480, 720), fully inside
        sequencer.tracks_mut()[0].add_clip(&clip_at(960, 480)); // [960, 1440), shifts back

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 0, 960).unwrap();
        let EditResult::TimeDeleted {
            shifted,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDeleted");
        };

        assert_eq!(carve_removed.len(), 1);
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 0);
        assert_eq!(shifted[0].end_tick, 480);
        assert_eq!(starts(&sequencer, 0), vec![(0, 480)]);
    }

    #[test]
    fn undo_restores_original_positions_and_the_carved_clip() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920);
        let original_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.tracks_mut()[0].add_clip(&clip_at(2000, 480));

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 720).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 3);

        let EditResult::TimeUndeleted {
            shifted,
            carve_updated,
            carve_removed,
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected TimeUndeleted");
        };

        assert_eq!(carve_removed.len(), 1);
        assert_eq!(carve_updated.len(), 1);
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 2000);

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 2);
        let restored = sequencer.tracks()[0].get_clip_by_id(original_id).unwrap();
        assert_eq!(restored.start_tick(), 0);
        assert_eq!(restored.end_tick(), 1920);
    }

    /// Undo reports the original `[start, end)` bounds so the handler can put
    /// the Arranger time selection back where the deleted material reappears.
    #[test]
    fn undo_reports_the_original_selection_bounds() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920));

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 720).unwrap();
        edit.edit(&mut sequencer);

        let EditResult::TimeUndeleted {
            restored_selection, ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected TimeUndeleted");
        };
        assert_eq!(restored_selection, (480, 720));
    }

    #[test]
    fn redo_reuses_frozen_targets_without_recomputation() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920));

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 720).unwrap();

        let EditResult::TimeDeleted { carve_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDeleted");
        };
        let first_right_id = carve_added[0].clip_id;

        edit.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1);

        let EditResult::TimeDeleted { carve_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDeleted");
        };
        assert_eq!(carve_added[0].clip_id, first_right_id);
        assert_eq!(sequencer.tracks()[0].clips().len(), 2);
    }

    #[test]
    fn shifts_matching_clips_across_multiple_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(960, 480)); // shifts
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 480)); // untouched
        sequencer.tracks_mut()[2].add_clip(&clip_at(1200, 480)); // shifts

        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::TimeDeleted { shifted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDeleted");
        };

        assert_eq!(shifted.len(), 2);
        assert_eq!(sequencer.tracks()[0].clips()[0].start_tick(), 480);
        assert_eq!(sequencer.tracks()[1].clips()[0].start_tick(), 0);
        assert_eq!(sequencer.tracks()[2].clips()[0].start_tick(), 720);
    }

    fn clip_with_open_note(start_tick: i32) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut().set_region(Some(0), Some(960));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: shifting the currently-playing clip backward moves it out
    /// from under the playhead exactly like a removal would — its open note
    /// must still get a real note-off instead of hanging.
    #[test]
    fn shifting_the_playing_clip_releases_its_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);

        let clip = clip_with_open_note(960);
        sequencer.tracks_mut()[0].add_clip(&clip);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(960);

        // One tick sounds the note-on.
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        // Delete time right before the clip's start, shifting it backward —
        // no overlap with the deleted range here, so this is a pure shift.
        let mut edit = DeleteTimeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        // The stranded note gets a real note-off.
        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
