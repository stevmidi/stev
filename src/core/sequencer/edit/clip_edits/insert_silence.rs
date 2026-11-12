//! Undoable "Insert Silence" — open a gap of `amount` ticks at `insert_tick`
//! across all tracks, splitting any straddling clip. Reuses [`SplitClipsEdit`].

use std::collections::HashSet;

use uuid::Uuid;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::partition_metadata;
use super::split::SplitClipsEdit;

// ---------------------------------------------------------------------------
// InsertSilence
// ---------------------------------------------------------------------------

/// Ableton/Bitwig-style "Insert Silence": opens a gap of `amount` ticks at
/// `insert_tick`, across all tracks. Every clip whose bounds strictly
/// contain `insert_tick` is split there first — reusing `SplitClipsEdit`
/// verbatim rather than reimplementing split logic — so its right-hand half
/// starts exactly at `insert_tick` and moves along with everything else.
/// Every clip (pre-existing or freshly split) with `start_tick() >=
/// insert_tick` is then shifted forward by `amount`.
///
/// Shifting is a raw `Clip::set_start_tick` mutation, not a
/// `remove_clip_by_id`/`add_clip` round-trip: a uniform forward shift of an
/// already-non-overlapping set of clips can never create a new overlap,
/// either among themselves (relative order and spacing is preserved) or
/// with the untouched clips before `insert_tick` (every shifted clip lands
/// at or past `insert_tick + amount`, strictly after every untouched one).
/// Going through `Track::add_clip`'s overlap check one clip at a time would
/// risk spurious rejection, since that check has no awareness of sibling
/// clips that are mid-shift in the same edit.
pub(crate) struct InsertSilenceEdit {
    /// Tick the gap opens at.
    insert_tick: i32,
    /// Gap width, in ticks (an amount).
    amount: i32,
    /// Clips straddling `insert_tick` at construction time, if any. `None`
    /// when nothing needs splitting (the common case: the insertion point
    /// falls in a gap or exactly on an existing clip boundary).
    split: Option<SplitClipsEdit>,
    /// Frozen after the first `edit()` call (post-split), so redo replays
    /// the exact same set rather than rescanning — same pattern as
    /// `SplitClipsEdit`/`PasteClipsEdit`.
    shift_targets: Option<Vec<(usize, Uuid)>>,
}

impl InsertSilenceEdit {
    /// Builds the edit from the Arranger's time selection bounds. `None` if
    /// the range is empty/backwards, or if there is truly nothing to do (no
    /// clip straddles `start` and no clip starts at or after it).
    pub(crate) fn from_time_range(sequencer: &Sequencer, start: i32, end: i32) -> Option<Self> {
        if end <= start {
            return None;
        }

        let split = SplitClipsEdit::from_time_range(sequencer, start, end, start);

        let has_shift_target =
            split.is_some() || !sequencer.clips_starting_at_or_after(start).is_empty();

        if !has_shift_target {
            return None;
        }

        Some(Self {
            insert_tick: start,
            amount: end - start,
            split,
            shift_targets: None,
        })
    }

    /// Splits any straddling clip, then shifts every clip at or after
    /// `insert_tick` right by `amount`. Returns [`EditResult::SilenceInserted`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let (split_updated, split_added_ids) = match self.split.as_mut().map(|s| s.edit(sequencer))
        {
            Some(EditResult::ClipsSplit { updated, added }) => {
                (updated, added.into_iter().map(|m| m.clip_id).collect())
            }
            _ => (Vec::new(), HashSet::new()),
        };

        let insert_tick = self.insert_tick;
        let targets = self
            .shift_targets
            .get_or_insert_with(|| sequencer.clips_starting_at_or_after(insert_tick));
        sequencer.shift_clips(targets, self.amount);

        let (split_added, shifted) = partition_metadata(sequencer, targets, &split_added_ids);

        if shifted.is_empty() && split_added.is_empty() && split_updated.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::SilenceInserted {
            shifted,
            split_updated,
            split_added,
        }
    }

    /// Shifts the moved clips back left, then un-splits. Returns
    /// [`EditResult::SilenceRemoved`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some(targets) = self.shift_targets.as_ref() else {
            return EditResult::NoOp;
        };
        sequencer.shift_clips(targets, -self.amount);

        let (split_updated, split_removed) = match self.split.as_mut().map(|s| s.undo(sequencer)) {
            Some(EditResult::ClipsUnsplit {
                updated, removed, ..
            }) => (updated, removed),
            _ => (Vec::new(), Vec::new()),
        };

        let removed_ids: HashSet<Uuid> = split_removed.iter().map(|m| m.clip_id).collect();
        let (_, shifted) = partition_metadata(sequencer, targets, &removed_ids);

        EditResult::SilenceRemoved {
            shifted,
            split_updated,
            split_removed,
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

    #[test]
    fn returns_none_on_empty_or_backwards_range() {
        let sequencer = test_sequencer();
        assert!(InsertSilenceEdit::from_time_range(&sequencer, 500, 500).is_none());
        assert!(InsertSilenceEdit::from_time_range(&sequencer, 500, 100).is_none());
    }

    #[test]
    fn returns_none_when_nothing_to_move() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // entirely before insert point
        assert!(InsertSilenceEdit::from_time_range(&sequencer, 960, 1440).is_none());
    }

    #[test]
    fn pure_shift_with_no_straddling_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // [0, 480), untouched
        sequencer.tracks_mut()[0].add_clip(&clip_at(960, 480)); // [960, 1440), shifts

        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::SilenceInserted {
            shifted,
            split_updated,
            split_added,
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected SilenceInserted");
        };

        assert!(split_updated.is_empty());
        assert!(split_added.is_empty());
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 1440);
        assert_eq!(shifted[0].end_tick, 1920);

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].start_tick(), 0);
        assert_eq!(clips[0].end_tick(), 480);
        assert_eq!(clips[1].start_tick(), 1440);
        assert_eq!(clips[1].end_tick(), 1920);
    }

    #[test]
    fn splits_straddling_clip_and_shifts_the_right_half_to_final_position() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // [0, 960), straddles 480

        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 480, 720).unwrap();
        let EditResult::SilenceInserted {
            shifted,
            split_updated,
            split_added,
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected SilenceInserted");
        };

        assert_eq!(split_updated.len(), 1);
        assert_eq!(split_updated[0].start_tick, 0);
        assert_eq!(split_updated[0].end_tick, 480);

        // The right-hand piece must be reported at its *final* (post-shift)
        // position, not the intermediate split boundary.
        assert_eq!(split_added.len(), 1);
        assert_eq!(split_added[0].start_tick, 480 + 240);
        assert_eq!(split_added[0].end_tick, 960 + 240);
        assert!(shifted.is_empty());

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].start_tick(), 0);
        assert_eq!(clips[0].end_tick(), 480);
        assert_eq!(clips[1].start_tick(), 720);
        assert_eq!(clips[1].end_tick(), 1200);
    }

    #[test]
    fn undo_restores_original_positions_and_removes_split_piece() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let original_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.tracks_mut()[0].add_clip(&clip_at(2000, 480));

        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 480, 720).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 3);

        let EditResult::SilenceRemoved {
            shifted,
            split_updated,
            split_removed,
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected SilenceRemoved");
        };

        assert_eq!(split_removed.len(), 1);
        assert_eq!(split_updated.len(), 1);
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].start_tick, 2000);

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 2);
        let restored = sequencer.tracks()[0].get_clip_by_id(original_id).unwrap();
        assert_eq!(restored.start_tick(), 0);
        assert_eq!(restored.end_tick(), 960);
    }

    #[test]
    fn redo_reuses_frozen_targets_without_recomputation() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));

        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 480, 720).unwrap();

        let EditResult::SilenceInserted { split_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected SilenceInserted");
        };
        let first_right_id = split_added[0].clip_id;

        edit.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1);

        let EditResult::SilenceInserted { split_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected SilenceInserted");
        };
        assert_eq!(split_added[0].clip_id, first_right_id);
        assert_eq!(sequencer.tracks()[0].clips().len(), 2);
    }

    #[test]
    fn shifts_matching_clips_across_multiple_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(960, 480)); // shifts
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 480)); // untouched
        sequencer.tracks_mut()[2].add_clip(&clip_at(1200, 480)); // shifts

        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::SilenceInserted { shifted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected SilenceInserted");
        };

        assert_eq!(shifted.len(), 2);
        assert_eq!(sequencer.tracks()[0].clips()[0].start_tick(), 1440);
        assert_eq!(sequencer.tracks()[1].clips()[0].start_tick(), 0);
        assert_eq!(sequencer.tracks()[2].clips()[0].start_tick(), 1680);
    }

    fn clip_with_open_note() -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), Some(960));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: shifting the currently-playing clip forward moves it out
    /// from under the playhead exactly like a removal would — its open note
    /// must still get a real note-off instead of hanging.
    #[test]
    fn shifting_the_playing_clip_releases_its_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);

        let clip = clip_with_open_note();
        sequencer.tracks_mut()[0].add_clip(&clip);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);

        // One tick sounds the note-on.
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        // Insert silence right at the clip's start, shifting it forward —
        // no straddling clip here, so this is a pure shift.
        let mut edit = InsertSilenceEdit::from_time_range(&sequencer, 0, 240).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        // The stranded note gets a real note-off.
        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
