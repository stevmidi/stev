//! Undoable Ableton-style "Deactivate Time Selection" — carve `[start, end)`
//! out of every overlapping clip, like `DeleteInRangeEdit`, but mute the
//! interior instead of removing it. Composed from the same [`EdgeSplits`] as
//! `DeleteInRangeEdit`.

use std::collections::HashSet;

use uuid::Uuid;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::split::EdgeSplits;
use super::{in_tracks, interior_clip_ids, metadata_for};

// ---------------------------------------------------------------------------
// MuteInRange
// ---------------------------------------------------------------------------

/// Ableton-style "mute the time selection": every clip overlapping the range
/// is split at `start` and `end` (exactly like `DeleteInRangeEdit`), and every
/// resulting piece that ends up fully inside `[start, end)` has its mute flag
/// flipped — a uniform target across the whole range: mute all if any
/// interior clip is unmuted, otherwise unmute all. This is the only way a
/// clip gets muted. Nothing is removed and nothing shifts; the splits
/// stay in place even when a second press unmutes the range.
pub(crate) struct MuteInRangeEdit {
    /// Low tick of the muted range.
    start: i32,
    /// High tick of the muted range.
    end: i32,
    /// The inclusive track span `(lo, hi)` the mute is restricted to — the
    /// arranger marquee's track-scoped `M`.
    tracks: (usize, usize),
    /// The splits at both edges; each replays its own frozen snapshot on redo.
    splits: EdgeSplits,
    /// `(track_idx, clip_id, was_muted)` for every clip that ended up fully
    /// inside `[start, end)` after both splits. Frozen on the first `edit()`
    /// (post-split, so the freshly-split interior pieces are visible) and
    /// replayed on redo — snapshot, not toggle, so undo restores each
    /// piece's exact prior state, and the uniform target derived from it
    /// stays the same on redo.
    mute_targets: Option<Vec<(usize, Uuid, bool)>>,
}

impl MuteInRangeEdit {
    /// Builds the edit for the arranger marquee's track-scoped `M`: `None` if
    /// the range is empty/backwards, or if no clip on tracks
    /// `track_start..=track_end` overlaps it.
    pub(crate) fn from_track_span(
        sequencer: &Sequencer,
        track_start: usize,
        track_end: usize,
        start: i32,
        end: i32,
    ) -> Option<Self> {
        if end <= start {
            return None;
        }

        let tracks = (track_start, track_end);
        let overlaps_any = sequencer
            .tracks()
            .iter()
            .enumerate()
            .any(|(track_idx, track)| {
                in_tracks(Some(tracks), track_idx) && !track.find_clip_ids_in(start, end).is_empty()
            });
        if !overlaps_any {
            return None;
        }

        Some(Self {
            start,
            end,
            tracks,
            splits: EdgeSplits::new(sequencer, Some(tracks), start, end),
            mute_targets: None,
        })
    }

    /// Runs the two splits, then flips the mute flag on every piece that
    /// ended up fully inside `[start, end)` (freezing the target set for redo
    /// on the first call). Returns [`EditResult::RangeMuted`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        // 1. Split every straddling clip at `end`, then at `start`.
        let (split_updated_ids, split_added_ids) = self.splits.edit(sequencer);
        let split_added_ids: HashSet<Uuid> = split_added_ids.into_iter().collect();

        // 2. Resolve the interior clips: computed only the first time, after
        //    both splits so the freshly-split middle pieces are visible, and
        //    replayed on redo.
        let (start, end, tracks) = (self.start, self.end, self.tracks);
        let mute_targets = self.mute_targets.get_or_insert_with(|| {
            interior_clip_ids(sequencer, Some(tracks), start, end)
                .into_iter()
                .filter_map(|(track_idx, id)| {
                    let clip = sequencer.clip_on(track_idx, id)?;
                    Some((track_idx, id, clip.is_muted()))
                })
                .collect()
        });

        // 3. Uniform toggle target — mute all if any interior clip was
        //    unmuted, otherwise unmute all.
        let target_muted = mute_targets.iter().any(|&(_, _, was_muted)| !was_muted);
        for &(track_idx, id, _) in mute_targets.iter() {
            if let Some(clip) = sequencer.clip_on_mut(track_idx, id) {
                clip.set_muted(target_muted);
            }
        }

        // 4. UI buckets — `added` is every split-created piece (whether or
        //    not it's also interior; only the splits create new pieces);
        //    `updated` is every trimmed original (bounds changed, id kept)
        //    plus every pre-existing interior clip that only had its mute
        //    flag flipped.
        let added = metadata_for(sequencer, &split_added_ids, &HashSet::new());
        let updated_ids: Vec<Uuid> = split_updated_ids
            .into_iter()
            .chain(mute_targets.iter().map(|&(_, id, _)| id))
            .collect();
        let updated = metadata_for(sequencer, &updated_ids, &split_added_ids);

        if updated.is_empty() && added.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::RangeMuted { updated, added }
    }

    /// Restores each clip's exact prior mute state, then un-splits both
    /// edges (start first, LIFO). Returns [`EditResult::RangeUnmuted`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let selected_track_idx = sequencer.selected_track_index();
        let selected_clip_id = sequencer.selected_clip_id();

        let Some(mute_targets) = self.mute_targets.as_ref() else {
            return EditResult::NoOp;
        };

        // 1. Restore each clip's exact prior mute state.
        for &(track_idx, id, was_muted) in mute_targets {
            if let Some(clip) = sequencer.clip_on_mut(track_idx, id) {
                clip.set_muted(was_muted);
            }
        }

        // 2. Undo both splits, mirroring `DeleteInRangeEdit::undo`.
        let (restored_ids, split_removed) = self.splits.undo(sequencer);

        // 3. UI buckets — `updated` mirrors `edit()`'s: every trimmed
        //    original restored to full bounds plus every interior clip whose
        //    mute flag was restored (a clip wholly inside the range is never
        //    split, so it is only reported here), minus the split pieces
        //    deleted again, which are `removed`.
        let removed_ids: HashSet<Uuid> = split_removed.iter().map(|m| m.clip_id).collect();
        let updated_ids: Vec<Uuid> = restored_ids
            .into_iter()
            .chain(mute_targets.iter().map(|&(_, id, _)| id))
            .collect();
        let updated = metadata_for(sequencer, &updated_ids, &removed_ids);

        if updated.is_empty() && split_removed.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::RangeUnmuted {
            updated,
            removed: split_removed,
            selected_track_idx,
            selected_clip_id,
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::core::config::MAX_TRACKS;

    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

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
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));
        assert!(
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 480).is_none()
        );
        assert!(
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 720, 240).is_none()
        );
    }

    #[test]
    fn returns_none_when_nothing_overlaps() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // [0, 480)
        assert!(
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 960, 1440).is_none()
        );
        // Touching the clip's end tick only is not an overlap (half-open).
        assert!(
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).is_none()
        );
    }

    #[test]
    fn clip_spanning_the_whole_range_is_split_into_three_and_only_the_middle_is_muted() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920); // [0, 1920)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        let EditResult::RangeMuted { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert_eq!(updated[0].start_tick, 0);
        assert_eq!(updated[0].end_tick, 480);
        assert!(!updated[0].muted);

        assert_eq!(added.len(), 2);
        let middle = added.iter().find(|m| m.start_tick == 480).unwrap();
        assert_eq!(middle.end_tick, 960);
        assert!(middle.muted);
        let right = added.iter().find(|m| m.start_tick == 960).unwrap();
        assert_eq!(right.end_tick, 1920);
        assert!(!right.muted);

        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 480), (480, 960), (960, 1920)]
        );
    }

    #[test]
    fn clip_straddling_start_is_split_once_and_the_interior_piece_is_muted() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 700); // [0, 700)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        // Range [480, 960): only `start` falls inside the clip.
        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        let EditResult::RangeMuted { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert_eq!(updated[0].start_tick, 0);
        assert_eq!(updated[0].end_tick, 480);
        assert!(!updated[0].muted);

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].start_tick, 480);
        assert_eq!(added[0].end_tick, 700);
        assert!(added[0].muted);

        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (480, 700)]);
    }

    #[test]
    fn clip_fully_inside_range_is_muted_with_no_split() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(480, 480); // [480, 960)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 0, 1920).unwrap();
        let EditResult::RangeMuted { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };

        assert!(added.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert!(updated[0].muted);
        assert_eq!(starts(&sequencer, 0), vec![(480, 960)]);
        assert!(sequencer.tracks()[0].get_clip_by_id(id).unwrap().is_muted());
    }

    #[test]
    fn mutes_matching_clips_across_multiple_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // spans range
        sequencer.tracks_mut()[1].add_clip(&clip_at(600, 240)); // fully inside [480, 960)
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 720)); // straddles start only

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        assert!(sequencer.tracks()[1].clips()[0].is_muted());
        let track2_interior = sequencer.tracks()[2]
            .clips()
            .iter()
            .find(|c| c.start_tick() == 480)
            .unwrap();
        assert!(track2_interior.is_muted());
        let track0_interior = sequencer.tracks()[0]
            .clips()
            .iter()
            .find(|c| c.start_tick() == 480)
            .unwrap();
        assert!(track0_interior.is_muted());
    }

    #[test]
    fn from_track_span_mutes_only_the_named_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // outside the span
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960)); // in the span

        let mut edit = MuteInRangeEdit::from_track_span(&sequencer, 1, 1, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        assert!(!sequencer.tracks()[0].clips()[0].is_muted());
        let track1_interior = sequencer.tracks()[1]
            .clips()
            .iter()
            .find(|c| c.start_tick() == 480)
            .unwrap();
        assert!(track1_interior.is_muted());
    }

    #[test]
    fn from_track_span_returns_none_when_only_a_track_outside_the_span_overlaps() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));

        assert!(MuteInRangeEdit::from_track_span(&sequencer, 1, 2, 480, 960).is_none());
    }

    #[test]
    fn undo_reports_an_unsplit_interior_clip_as_updated_and_unmuted() {
        // Regression: a clip wholly inside the range is never split, so undo
        // used to report nothing (a `NoOp`) and the view kept it muted.
        let mut sequencer = test_sequencer();
        let clip = clip_at(480, 480); // [480, 960)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        let EditResult::RangeUnmuted {
            updated, removed, ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected RangeUnmuted");
        };

        assert!(removed.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert!(!updated[0].muted);
    }

    #[test]
    fn undo_restores_mute_state_and_removes_split_pieces() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920);
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 3);

        let EditResult::RangeUnmuted {
            updated, removed, ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected RangeUnmuted");
        };

        assert_eq!(removed.len(), 2);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);

        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        let restored = sequencer.tracks()[0].get_clip_by_id(id).unwrap();
        assert!(!restored.is_muted());
    }

    #[test]
    fn redo_reuses_the_frozen_snapshot_instead_of_recomputing() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920);
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();

        let EditResult::RangeMuted { added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };
        let first_ids: HashSet<Uuid> = added.iter().map(|m| m.clip_id).collect();

        edit.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1);

        let EditResult::RangeMuted { added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };
        let second_ids: HashSet<Uuid> = added.iter().map(|m| m.clip_id).collect();

        assert_eq!(first_ids, second_ids);
        assert_eq!(sequencer.tracks()[0].clips().len(), 3);
    }

    #[test]
    fn toggle_rule_unmutes_when_every_interior_clip_is_already_muted() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920);
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        edit.edit(&mut sequencer);
        let interior_id = sequencer.tracks()[0]
            .clips()
            .iter()
            .find(|c| c.start_tick() == 480)
            .unwrap()
            .id();
        assert!(
            sequencer.tracks()[0]
                .get_clip_by_id(interior_id)
                .unwrap()
                .is_muted()
        );

        // A fresh edit over the same, already-split range should just flip
        // the flag back — nothing straddles the edges any more.
        let mut second =
            MuteInRangeEdit::from_track_span(&sequencer, 0, MAX_TRACKS - 1, 480, 960).unwrap();
        let EditResult::RangeMuted { added, updated } = second.edit(&mut sequencer) else {
            panic!("expected RangeMuted");
        };
        assert!(added.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(!updated[0].muted);
        assert!(
            !sequencer.tracks()[0]
                .get_clip_by_id(interior_id)
                .unwrap()
                .is_muted()
        );
    }
}
