//! Undoable `Shift+⌘/Ctrl+D` (Duplicate Time) — insert a copy of the
//! selection and push everything after it right. Composed from
//! [`InsertSilenceEdit`] + [`PasteClipsEdit`]. Plain `⌘/Ctrl+D` is the
//! non-shifting `DuplicateClipsEdit` next door.

use uuid::Uuid;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::insert_silence::InsertSilenceEdit;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// DuplicateTime
// ---------------------------------------------------------------------------

/// `Shift+⌘/Ctrl+D` in the Arranger — Ableton-style "Duplicate Time". This
/// *inserts* time: the space for the copy is opened by pushing every clip
/// after the selection to the right by the selection width, on every track,
/// so nothing is destroyed. Always global — a partial shift would desync the
/// tracks, the same alignment concern `InsertSilenceEdit` guards against.
/// The marquee-scoped, overwriting sibling is `DuplicateClipsEdit`.
///
/// It is exactly the composition of the two edits that already do each half —
/// same worked-example reuse shape as `InsertSilenceEdit`/`DeleteInRangeEdit`
/// (composing `SplitClipsEdit`):
///
/// 1. `InsertSilenceEdit` anchored at `end` opens a `end - start` tick gap on
///    every track (splitting any clip straddling `end`, shifting every clip at
///    or after `end` right by the width).
/// 2. `PasteClipsEdit` drops a copy of the `[start, end)` slice of every
///    overlapping clip into the freed `[end, end + width)` span. Because step 1
///    pre-clears that span, the paste's per-target carve is always a no-op.
///
/// `edit()` runs them in that order; `undo()` reverses it (un-paste, then
/// remove the silence). The operand bounds are frozen at construction — never
/// re-read from the (view-local) time selection on redo.
///
/// The loop region is *moved* (not resized) if its start sits within the
/// duplicated span or after it (`region_start >= start`): it slides right by
/// the selection width so it keeps the same position relative to the material
/// that moved. A region starting before the selection is left alone.
pub(crate) struct DuplicateTimeEdit {
    /// Low tick of the duplicated span.
    start: i32,
    /// High tick of the duplicated span — also the insertion point.
    end: i32,
    /// Opens the gap at `end`. `None` when `InsertSilenceEdit::from_time_range`
    /// finds nothing at or after `end` to move — the paste still happens, into
    /// the already-empty `[end, end + width)` span.
    insert: Option<InsertSilenceEdit>,
    /// Copies the `[start, end)` slice and places it at anchor `end`. Its
    /// absolute target positions are frozen at construction and are unaffected
    /// by the shift `insert` performs.
    paste: PasteClipsEdit,
    /// Whether the loop region was slid right by the selection width — `true`
    /// when its start sat at or after `start` (within the duplicated span or
    /// after it), so it keeps its position relative to the material that moved.
    /// Frozen on the first `edit()` so redo replays the same decision; the
    /// region is not resized, only moved. See `Sequencer::shift_region`.
    region_shifted: Option<bool>,
}

impl DuplicateTimeEdit {
    /// Builds the edit from the Arranger's time selection bounds. `None` if the
    /// range is empty/backwards or overlaps no clip on any track.
    pub(crate) fn from_time_range(sequencer: &Sequencer, start: i32, end: i32) -> Option<Self> {
        if end <= start {
            return None;
        }

        let width = end - start;

        // Frozen pre-insert: the copied pieces and their absolute destinations.
        let clipboard = sequencer.clipboard_snapshot(start, end)?;
        let paste = PasteClipsEdit::from_clipboard_at(sequencer, &clipboard, end, None)?;

        // `from_time_range(s, a, b)` inserts `b - a` ticks at `a`, so this
        // inserts `width` ticks at `end`.
        let insert = InsertSilenceEdit::from_time_range(sequencer, end, end + width);

        Some(Self {
            start,
            end,
            insert,
            paste,
            region_shifted: None,
        })
    }

    /// Inserts the silence, then pastes the copied slice into the freed span,
    /// then slides the loop region if needed. Returns [`EditResult::TimeDuplicated`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let mut shifted = Vec::new();
        let mut split_updated = Vec::new();
        let mut split_added = Vec::new();

        if let Some(insert) = self.insert.as_mut()
            && let EditResult::SilenceInserted {
                shifted: s,
                split_updated: su,
                split_added: sa,
            } = insert.edit(sequencer)
        {
            shifted = s;
            split_updated = su;
            split_added = sa;
        }

        let mut pasted = Vec::new();
        if let EditResult::ClipsPasted { pasted: p, .. } = self.paste.edit(sequencer) {
            pasted = p;
        }

        if shifted.is_empty()
            && split_updated.is_empty()
            && split_added.is_empty()
            && pasted.is_empty()
        {
            return EditResult::NoOp;
        }

        // Slide the loop region along with the inserted time if it sits within
        // the duplicated span or after it. Frozen on the first call so redo
        // replays the same decision. Not resized — only moved.
        let start = self.start;
        let region_shifted = *self
            .region_shifted
            .get_or_insert_with(|| sequencer.region_start() >= start);
        if region_shifted {
            sequencer.shift_region(self.end - self.start);
        }

        EditResult::TimeDuplicated {
            shifted,
            split_updated,
            split_added,
            pasted,
            new_selection: (self.end, self.end + (self.end - self.start)),
        }
    }

    /// Un-pastes the copy, removes the silence, and slides the loop region
    /// back. Returns [`EditResult::TimeUnduplicated`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        if self.region_shifted == Some(true) {
            sequencer.shift_region(-(self.end - self.start));
        }

        let mut unpasted = Vec::new();
        let mut selected_track_idx = None;
        let mut selected_clip_id: Option<Uuid> = None;

        if let EditResult::ClipsUnpasted {
            unpasted: u,
            selected_track_idx: sti,
            selected_clip_id: sci,
            ..
        } = self.paste.undo(sequencer)
        {
            unpasted = u;
            selected_track_idx = sti;
            selected_clip_id = sci;
        }

        let mut shifted = Vec::new();
        let mut split_updated = Vec::new();
        let mut split_removed = Vec::new();

        if let Some(insert) = self.insert.as_mut()
            && let EditResult::SilenceRemoved {
                shifted: s,
                split_updated: su,
                split_removed: sr,
            } = insert.undo(sequencer)
        {
            shifted = s;
            split_updated = su;
            split_removed = sr;
        }

        EditResult::TimeUnduplicated {
            unpasted,
            shifted,
            split_updated,
            split_removed,
            selected_track_idx,
            selected_clip_id,
            restored_selection: (self.start, self.end),
        }
    }
}

#[cfg(test)]
mod tests {

    use uuid::Uuid;

    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

    use super::*;

    fn starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    fn select(sequencer: &mut Sequencer, track_idx: usize, clip_id: Option<Uuid>) {
        let track_id = sequencer.track_id_by_index(track_idx).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(clip_id);
    }

    fn region(sequencer: &Sequencer) -> (i32, i32) {
        (sequencer.region_start(), sequencer.region_end())
    }

    #[test]
    fn is_none_on_empty_or_backwards_range() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        assert!(DuplicateTimeEdit::from_time_range(&sequencer, 100, 100).is_none());
        assert!(DuplicateTimeEdit::from_time_range(&sequencer, 200, 50).is_none());
    }

    #[test]
    fn is_none_when_range_overlaps_no_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(500, 100));
        assert!(DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).is_none());
    }

    #[test]
    fn inserts_time_pushing_later_clips_and_places_the_copy_in_the_gap() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100)); // outside [0,100), must shift
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        let EditResult::TimeDuplicated {
            shifted,
            pasted,
            new_selection,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDuplicated");
        };

        assert_eq!(pasted.len(), 2);
        assert_eq!(shifted.len(), 1); // the [200,300) clip on track 0
        assert_eq!(new_selection, (100, 200));
        // Unlike plain `⌘D` (`DuplicateClipsEdit`), the [200,300) clip is
        // pushed to [300,400) rather than overwritten.
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (300, 400)]
        );
        assert_eq!(starts(&sequencer, 1), vec![(0, 100), (100, 200)]);

        let EditResult::TimeUnduplicated {
            restored_selection, ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected TimeUnduplicated");
        };
        assert_eq!(restored_selection, (0, 100));
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (200, 300)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 100)]);
    }

    #[test]
    fn splits_clip_straddling_the_selection_end_and_shifts_its_right_half() {
        let mut sequencer = test_sequencer();
        // Clip [0, 400); selection [0, 100). The clip straddles `end` (100), so
        // it is split there: left [0,100) stays, right half shifts by 100.
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 400));
        select(&mut sequencer, 0, None);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated");
        };

        assert_eq!(pasted.len(), 1);
        // [0,100) original left, [100,200) the pasted copy, [200,500) the shifted right half.
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (200, 500)]
        );

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 400)]);
    }

    #[test]
    fn nothing_after_end_still_pastes_the_copy() {
        let mut sequencer = test_sequencer();
        // Only a clip fully inside the selection; nothing at or after `end`.
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        assert!(edit.insert.is_none());

        let EditResult::TimeDuplicated {
            pasted, shifted, ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDuplicated");
        };
        assert_eq!(pasted.len(), 1);
        assert!(shifted.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (100, 200)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100)]);
    }

    #[test]
    fn shifts_matching_clips_across_multiple_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(400, 100));
        sequencer.tracks_mut()[2].add_clip(&clip_at(600, 100)); // no clip in selection here
        select(&mut sequencer, 0, None);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        edit.edit(&mut sequencer);

        // Every clip at/after `end` (100) moved right by 100 on every track.
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (500, 600)]
        );
        assert_eq!(starts(&sequencer, 2), vec![(700, 800)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (400, 500)]);
        assert_eq!(starts(&sequencer, 2), vec![(600, 700)]);
    }

    #[test]
    fn redo_reuses_frozen_ids() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100));
        select(&mut sequencer, 0, None);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated");
        };
        let first_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (200, 300)]);

        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated on redo");
        };
        let redo_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first_ids, redo_ids, "redo must reuse the frozen clip ids");
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (300, 400)]
        );
    }

    #[test]
    fn loop_region_within_or_after_the_selection_is_moved_not_resized() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);

        // Region sits inside the selection, at +20..+60. Width duplicated is 100.
        sequencer.set_global_region(20, 60);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        edit.edit(&mut sequencer);
        // Slid right by the full width, same length (40 ticks).
        assert_eq!(region(&sequencer), (120, 160));

        edit.undo(&mut sequencer);
        assert_eq!(region(&sequencer), (20, 60));

        // Redo replays the same slide.
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (120, 160));
    }

    #[test]
    fn loop_region_before_the_selection_is_left_alone() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100)); // overlaps the selection
        sequencer.tracks_mut()[0].add_clip(&clip_at(500, 100));
        select(&mut sequencer, 0, None);

        // Region starts before the selection start (200) — untouched even though
        // its end reaches past it (moving only the end would be a resize).
        sequencer.set_global_region(0, 400);

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 200, 300).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (0, 400));

        edit.undo(&mut sequencer);
        assert_eq!(region(&sequencer), (0, 400));
    }
}
