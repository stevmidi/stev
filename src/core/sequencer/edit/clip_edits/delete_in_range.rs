//! Undoable Ableton-style range delete — carve `[start, end)` out of every
//! overlapping clip without rippling. Composed from `SplitClipsEdit`s.

use std::collections::HashSet;

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::split::EdgeSplits;
use super::{detach, detached_clone, in_tracks, interior_clip_ids, metadata_for};

// ---------------------------------------------------------------------------
// DeleteInRange
// ---------------------------------------------------------------------------

/// Ableton-style "carve out the time selection": clears `[start, end)` across
/// all tracks *without rippling*. Every clip overlapping the range is trimmed
/// or split so only the parts outside the range survive; a clip whose middle
/// is selected becomes two clips with a gap between them. Material after the
/// range keeps its timeline position — nothing shifts.
///
/// Non-destructive, exactly like `SplitClipsEdit` and the clip edge
/// drag-resize: the trimmed-away material stays in each piece's event list,
/// just outside its region window, so dragging an edge back out later reveals
/// it again.
///
/// Composed from the [`EdgeSplits`] at `start` and `end` plus a removal of
/// every resulting piece that ends up fully inside `[start, end)`.
pub(crate) struct DeleteInRangeEdit {
    /// Low tick of the carved range.
    start: i32,
    /// High tick of the carved range.
    end: i32,
    /// `Some((lo, hi))` restricts the whole carve to tracks `lo..=hi` —
    /// `lo == hi` backs `PasteClipsEdit`'s per-target overwrite, a wider span
    /// backs the arranger marquee's track-scoped `Delete`/`Backspace`. `None`
    /// carves every track (Shift+⌘/Ctrl+X's all-track binding).
    track_filter: Option<(usize, usize)>,
    /// The splits at both edges; each replays its own frozen snapshot on redo.
    splits: EdgeSplits,
    /// Every clip removed because it (or a freshly-split piece of it) ended up
    /// fully inside `[start, end)`. Frozen (with detached regions) on the
    /// first `edit()`; re-removed on redo, re-added on undo — the same
    /// frozen-snapshot pattern `SplitClipsEdit`/`PasteClipsEdit` use.
    removed: Option<Vec<(usize, Clip)>>,
}

impl DeleteInRangeEdit {
    /// Builds the edit from the Arranger's time selection bounds. `None` if
    /// the range is empty/backwards, or if no clip on any track overlaps it.
    pub(crate) fn from_time_range(sequencer: &Sequencer, start: i32, end: i32) -> Option<Self> {
        Self::build(sequencer, None, start, end)
    }

    /// Like `from_time_range`, but carves only `track_idx`. Backs
    /// `PasteClipsEdit`'s per-target overwrite.
    pub(crate) fn from_track_range(
        sequencer: &Sequencer,
        track_idx: usize,
        start: i32,
        end: i32,
    ) -> Option<Self> {
        Self::build(sequencer, Some((track_idx, track_idx)), start, end)
    }

    /// Like `from_time_range`, but restricted to an inclusive track span —
    /// backs the arranger marquee's track-scoped `Delete`/`Backspace` and
    /// plain `⌘/Ctrl+X`.
    pub(crate) fn from_track_span(
        sequencer: &Sequencer,
        track_start: usize,
        track_end: usize,
        start: i32,
        end: i32,
    ) -> Option<Self> {
        Self::build(sequencer, Some((track_start, track_end)), start, end)
    }

    /// Shared constructor: resolves which clips straddle each edge, or `None`
    /// if the range is empty or nothing overlaps it.
    fn build(
        sequencer: &Sequencer,
        track_filter: Option<(usize, usize)>,
        start: i32,
        end: i32,
    ) -> Option<Self> {
        if end <= start {
            return None;
        }

        let overlaps_any = sequencer
            .tracks()
            .iter()
            .enumerate()
            .any(|(track_idx, track)| {
                in_tracks(track_filter, track_idx) && !track.find_clip_ids_in(start, end).is_empty()
            });
        if !overlaps_any {
            return None;
        }

        Some(Self {
            start,
            end,
            track_filter,
            splits: EdgeSplits::new(sequencer, track_filter, start, end),
            removed: None,
        })
    }

    /// Runs the two splits, then removes every piece that ended up fully
    /// inside `[start, end)` (freezing them for redo on the first call).
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let selected_track_idx = sequencer.selected_track_index();
        let selected_clip_id = sequencer.selected_clip_id();

        // 1. Split every straddling clip at `end`, then at `start`.
        let (split_updated_ids, split_added_ids) = self.splits.edit(sequencer);
        let split_added_ids: HashSet<Uuid> = split_added_ids.into_iter().collect();

        // 2. Remove every clip (pre-existing or freshly split) now fully
        //    inside `[start, end)`.
        let removed_clips = self.take_interior(sequencer);
        let removed_ids: HashSet<Uuid> = removed_clips.iter().map(|(id, _)| *id).collect();

        // 3. Classify survivors / removals into UI buckets. The split results
        //    already say which surviving ids are brand new, so knowing the
        //    pre-edit id set is unnecessary.
        //    - a removed piece the UI never saw (a freshly-split interior
        //      half) is dropped from `removed`.
        let removed: Vec<ClipMetadata> = removed_clips
            .into_iter()
            .filter(|(id, _)| !split_added_ids.contains(id))
            .map(|(_, metadata)| metadata)
            .collect();

        //    - a new split piece that survived (the `[end, oe]` remainder of
        //      a clip that reached past the range) is `added`.
        let added = metadata_for(sequencer, &split_added_ids, &removed_ids);

        //    - a trimmed original that kept its id is `updated`.
        let updated = metadata_for(sequencer, &split_updated_ids, &removed_ids);

        if updated.is_empty() && added.is_empty() && removed.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::RangeDeleted {
            updated,
            added,
            removed,
            selected_track_idx,
            selected_clip_id,
        }
    }

    /// Re-adds the removed pieces, then un-splits both edges.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let selected_track_idx = sequencer.selected_track_index();
        let selected_clip_id = sequencer.selected_clip_id();

        let Some(frozen) = self.removed.as_ref() else {
            return EditResult::NoOp;
        };

        // 1. Re-add every interior-removed clip. Detached regions: a later
        //    split undo restoring a straddle-`end` original's region end
        //    must not reach back and mutate the frozen snapshot (`Region`'s
        //    `Clone` shares its `Arc<AtomicI32>`s — see `050-undo-redo.md`).
        //    The interior `[start, end)` is empty here and the pieces tile it
        //    without overlap, so every add succeeds.
        let mut readded_ids: Vec<Uuid> = Vec::new();
        for (track_idx, clip) in frozen {
            if let Some(track) = sequencer.tracks_mut().get_mut(*track_idx)
                && track.add_clip(&detached_clone(clip))
            {
                readded_ids.push(clip.id());
            }
        }

        // 2. Undo both splits — only after step 1: `SplitClipsEdit::undo`
        //    bails without restoring the region end if its added clip is
        //    missing, and step 2 of `edit()` removed some of those as
        //    "interior".
        let (restored_ids, split_removed) = self.splits.undo(sequencer);

        // 3. UI buckets.
        //    - a re-added clip still present is `added` (the freshly-split
        //      interior halves get removed again by the split undo).
        let added = metadata_for(sequencer, &readded_ids, &HashSet::new());
        let readded_ids: HashSet<Uuid> = readded_ids.into_iter().collect();

        //    - a right-hand split piece deleted again is `removed`, unless it
        //      is one we just re-added (a freshly-split interior half).
        let removed: Vec<ClipMetadata> = split_removed
            .into_iter()
            .filter(|m| !readded_ids.contains(&m.clip_id))
            .collect();

        //    - a trimmed original restored to full bounds is `updated`, unless
        //      it reappeared via `added` (a straddle-`end` original).
        let updated = metadata_for(sequencer, &restored_ids, &readded_ids);

        if updated.is_empty() && added.is_empty() && removed.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::RangeRestored {
            updated,
            added,
            removed,
            selected_track_idx,
            selected_clip_id,
        }
    }

    /// Lifts every clip fully inside `[start, end)` and returns each one's
    /// id and metadata. The first call computes the set and freezes the
    /// clips (with detached regions) into `self.removed`; redo replays that
    /// exact set.
    fn take_interior(&mut self, sequencer: &mut Sequencer) -> Vec<(Uuid, ClipMetadata)> {
        let targets: Vec<(usize, Uuid)> = match self.removed.as_ref() {
            Some(frozen) => frozen.iter().map(|(t, clip)| (*t, clip.id())).collect(),
            None => interior_clip_ids(sequencer, self.track_filter, self.start, self.end),
        };

        let mut out = Vec::with_capacity(targets.len());
        let mut lifted = Vec::with_capacity(targets.len());
        for (track_idx, id) in targets {
            if let Some((metadata, clip)) = sequencer.lift_clip(track_idx, id) {
                out.push((id, metadata));
                lifted.push((track_idx, detach(clip)));
            }
        }

        if self.removed.is_none() {
            self.removed = Some(lifted);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use crate::models::event::Event;

    use crate::core::sequencer::test_support::{
        clip_at, drain, instrument_track_0, sequencer_with, test_sequencer,
    };

    use super::*;

    /// An instrument-output track holding one clip `[0, 960)` with a note
    /// that is still sounding at tick 0 (on at 0, off at 480).
    fn instrument_track_with_open_note(sequencer: &mut Sequencer) {
        instrument_track_0(sequencer);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        sequencer.tracks_mut()[0].add_clip(&clip);
    }

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
        assert!(DeleteInRangeEdit::from_time_range(&sequencer, 480, 480).is_none());
        assert!(DeleteInRangeEdit::from_time_range(&sequencer, 720, 240).is_none());
    }

    #[test]
    fn returns_none_when_nothing_overlaps() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // [0, 480)
        // Range sits entirely in the gap after the clip.
        assert!(DeleteInRangeEdit::from_time_range(&sequencer, 960, 1440).is_none());
        // Touching the clip's end tick only is not an overlap (half-open).
        assert!(DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).is_none());
    }

    #[test]
    fn clip_fully_inside_range_is_removed_whole_and_restored_on_undo() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(480, 480)); // [480, 960)

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 0, 1920).unwrap();
        let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeDeleted");
        };
        assert!(updated.is_empty());
        assert!(added.is_empty());
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].start_tick, 480);
        assert!(sequencer.tracks()[0].clips().is_empty());

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(480, 960)]);
    }

    #[test]
    fn clip_straddling_start_is_trimmed_on_the_right_keeping_its_id() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960); // [0, 960)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        // Range [480, 1440): only `start` (480) falls inside the clip.
        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 1440).unwrap();
        let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeDeleted");
        };
        assert!(added.is_empty());
        assert!(removed.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert_eq!(starts(&sequencer, 0), vec![(0, 480)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 960)]);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), id);
    }

    #[test]
    fn clip_straddling_end_keeps_its_remainder_at_the_same_timeline_position() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(480, 960)); // [480, 1440)

        // Range [0, 960): only `end` (960) falls inside the clip.
        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 0, 960).unwrap();
        let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeDeleted");
        };
        assert!(updated.is_empty());
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].start_tick, 960);
        assert_eq!(added[0].end_tick, 1440);
        assert_eq!(removed.len(), 1);
        // The remainder stays put — no ripple.
        assert_eq!(starts(&sequencer, 0), vec![(960, 1440)]);

        let EditResult::RangeRestored {
            updated,
            added,
            removed,
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected RangeRestored");
        };
        // The original clip "reappears" (its id was on the cleared left half),
        // and the remainder is deleted again.
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].start_tick, 480);
        assert_eq!(added[0].end_tick, 1440);
        assert!(updated.is_empty());
        assert_eq!(removed.len(), 1);
        assert_eq!(starts(&sequencer, 0), vec![(480, 1440)]);
    }

    #[test]
    fn clip_spanning_the_whole_range_is_split_into_two_with_a_gap() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920); // [0, 1920)
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeDeleted");
        };
        assert!(removed.is_empty(), "the interior half was never UI-visible");
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert_eq!(updated[0].start_tick, 0);
        assert_eq!(updated[0].end_tick, 480);
        assert_eq!(added.len(), 1);
        assert_ne!(added[0].clip_id, id);
        assert_eq!(added[0].start_tick, 960);
        assert_eq!(added[0].end_tick, 1920);

        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (960, 1920)]);

        let EditResult::RangeRestored {
            updated,
            added,
            removed,
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected RangeRestored");
        };
        // The left half (id kept) is restored to full bounds; the right half
        // is deleted again; nothing "reappears" (the interior half stayed
        // internal both ways).
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, id);
        assert_eq!(removed.len(), 1);
        assert!(added.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), id);
    }

    #[test]
    fn carve_is_non_destructive_events_are_preserved_under_the_edges() {
        use crate::models::event::Event;

        let mut sequencer = test_sequencer();
        let mut clip = clip_at(0, 1920);
        // Notes at 240, 720 (inside the carved range), 1200.
        clip.add_event(Event::new(240, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(260, 0, vec![0x80, 60, 0]));
        clip.add_event(Event::new(720, 0, vec![0x90, 62, 100]));
        clip.add_event(Event::new(740, 0, vec![0x80, 62, 0]));
        clip.add_event(Event::new(1200, 0, vec![0x90, 64, 100]));
        clip.add_event(Event::new(1220, 0, vec![0x80, 64, 0]));
        let event_count = clip.events().len();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        // Both halves still own the full event list — only their region
        // windows changed.
        for clip in sequencer.tracks()[0].clips() {
            assert_eq!(clip.events().len(), event_count);
        }
    }

    #[test]
    fn carves_clips_across_multiple_tracks_in_one_edit() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // spans range
        sequencer.tracks_mut()[1].add_clip(&clip_at(600, 240)); // fully inside [480, 960)
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 720)); // straddles start

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (960, 1920)]);
        assert!(sequencer.tracks()[1].clips().is_empty());
        assert_eq!(starts(&sequencer, 2), vec![(0, 480)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(600, 840)]);
        assert_eq!(starts(&sequencer, 2), vec![(0, 720)]);
    }

    #[test]
    fn redo_after_undo_reuses_frozen_snapshots_and_is_stable_across_cycles() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 1920);
        let id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.tracks_mut()[1].add_clip(&clip_at(480, 960)); // straddles end

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).unwrap();

        let carved = |seq: &Sequencer| (starts(seq, 0), starts(seq, 1));

        edit.edit(&mut sequencer);
        let after_first = carved(&sequencer);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), id);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(480, 1440)]);

        edit.edit(&mut sequencer);
        assert_eq!(carved(&sequencer), after_first);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), id);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(480, 1440)]);
    }

    #[test]
    fn from_track_range_carves_only_the_named_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // [0, 1920)
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920)); // untouched

        let mut edit = DeleteInRangeEdit::from_track_range(&sequencer, 0, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (960, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 1920)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 1920)]);
    }

    #[test]
    fn from_track_range_returns_none_when_only_other_tracks_overlap() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920));

        assert!(DeleteInRangeEdit::from_track_range(&sequencer, 0, 480, 960).is_none());
    }

    #[test]
    fn from_track_span_carves_every_track_in_the_span_but_not_outside_it() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // outside the span
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 1920)); // in the span
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 1920)); // in the span
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 1920)); // outside the span

        let mut edit = DeleteInRangeEdit::from_track_span(&sequencer, 1, 2, 480, 960).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 480), (960, 1920)]);
        assert_eq!(starts(&sequencer, 2), vec![(0, 480), (960, 1920)]);
        assert_eq!(starts(&sequencer, 3), vec![(0, 1920)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 1), vec![(0, 1920)]);
        assert_eq!(starts(&sequencer, 2), vec![(0, 1920)]);
    }

    #[test]
    fn from_track_span_returns_none_when_only_a_track_outside_the_span_overlaps() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 1920));

        assert!(DeleteInRangeEdit::from_track_span(&sequencer, 1, 2, 480, 960).is_none());
    }

    #[test]
    fn selection_exactly_covering_one_clip_removes_it_whole() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(480, 480)); // [480, 960)

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 480, 960).unwrap();
        let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeDeleted");
        };
        assert!(updated.is_empty());
        assert!(added.is_empty());
        assert_eq!(removed.len(), 1);
        assert!(sequencer.tracks()[0].clips().is_empty());
    }

    /// Removing an instrument-track clip whole while its note is sounding must
    /// send that note's note-off — the clip is gone before `tick()` could emit
    /// it, and the CLAP route has no `NoteLogger` safety net.
    #[test]
    fn removing_a_playing_clip_releases_its_instrument_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_with_open_note(&mut sequencer);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);

        // One tick sounds the note-on.
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        // Delete the clip mid-playback, then tick once more.
        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 0, 960).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        // The stranded note gets a real note-off.
        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }

    /// The release is gated on playback: deleting a clip while stopped must not
    /// queue phantom note-offs.
    #[test]
    fn removing_a_clip_while_stopped_sends_nothing() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_with_open_note(&mut sequencer);

        let mut edit = DeleteInRangeEdit::from_time_range(&sequencer, 0, 960).unwrap();
        edit.edit(&mut sequencer);

        assert!(plugin_rx.pop().is_err());
    }
}
