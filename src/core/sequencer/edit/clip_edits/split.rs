//! Undoable clip split at a fixed tick — the primitive the compound clip edits
//! (`InsertSilenceEdit`, `DeleteInRangeEdit`, …) reuse. Non-destructive: both
//! halves keep the full event list, only their region windows differ.

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::{detached_clone, in_tracks};

// ---------------------------------------------------------------------------
// Private Sequencer helpers — used exclusively by edit structs in this file
// ---------------------------------------------------------------------------

/// One successful split, frozen for undo/redo.
struct FrozenSplit {
    /// Track both halves are on.
    track_idx: usize,
    /// The clip that was cut — it keeps its id as the left half.
    original_id: Uuid,
    /// The original's region end before the split, for `undo()`.
    pre_split_region_end: i32,
    /// The right half, re-added verbatim on redo so its id stays stable.
    new_clip: Clip,
}

impl Sequencer {
    /// For each `(track_idx, clip_id)` target whose current bounds strictly
    /// contain `split_tick`, shrinks that clip's region to end at the split
    /// point and adds a new clip carrying the remainder. Non-destructive,
    /// like the clip edge drag-resize: both pieces keep the *full* original
    /// event list, only `start_tick`/`region` differ, so dragging an edge
    /// back out later can reveal the other half's content again. The new
    /// clip's `start_tick`/`region().start()` are shifted by the same delta
    /// (phase-locked), exactly like `resize_selected_clip_region_start_to_tick`'s
    /// left-edge trim, so the split doesn't shift playback content at all.
    /// Targets that no longer exist or no longer contain `split_tick` are
    /// silently skipped. Returns one [`FrozenSplit`] per successful split.
    fn split_clips_at_tick(
        &mut self,
        targets: &[(usize, Uuid)],
        split_tick: i32,
    ) -> Vec<FrozenSplit> {
        let running = self.is_running();
        let mut results = Vec::new();

        for &(track_idx, clip_id) in targets {
            let Some(track) = self.tracks.get_mut(track_idx) else {
                continue;
            };
            let Some(original) = track.get_clip_by_id(clip_id) else {
                continue;
            };

            if split_tick <= original.start_tick() || split_tick >= original.end_tick() {
                continue;
            }

            let region_start = original.region().start();
            let region_end = original.region().end();
            let delta = split_tick - original.start_tick();

            // Detached: a plain clone would share the original's region
            // atomics, and the whole point is for the two halves' regions
            // to diverge.
            let mut right = detached_clone(original);
            right.generate_new_id();
            right.set_start_tick(split_tick);
            right
                .region_mut()
                .set_region(Some(region_start + delta), None);

            // Shrinking the original's region end away from a currently-sounding
            // note would strand its note-off — `Clip::tick` never advances past a
            // shrunk `end_tick` — so queue it now, exactly like `DeleteInRangeEdit`.
            if running {
                track.release_sounding_notes_for_clip(clip_id);
            }

            let Some(original_mut) = track.get_clip_by_id_mut(clip_id) else {
                continue;
            };
            original_mut
                .region_mut()
                .set_region(None, Some(region_start + delta));

            if !track.add_clip(&right) {
                // Should not happen: the original clip occupied this whole
                // span contiguously, so the trailing half it just gave up
                // cannot collide with anything else. Revert defensively.
                if let Some(original_mut) = track.get_clip_by_id_mut(clip_id) {
                    original_mut.region_mut().set_region(None, Some(region_end));
                }
                continue;
            }

            results.push(FrozenSplit {
                track_idx,
                original_id: clip_id,
                pre_split_region_end: region_end,
                new_clip: right,
            });
        }

        results
    }

    /// Redo path for a previously-frozen split: reapplies the same region
    /// shrink to the original (recomputed from its current bounds, which
    /// undo has already restored to what produced `new_clip` the first
    /// time) and re-adds the exact frozen `new_clip`, keeping its id stable
    /// across repeated undo/redo.
    fn reapply_split(&mut self, split: &FrozenSplit, split_tick: i32) -> bool {
        let running = self.is_running();
        let Some(track) = self.tracks.get_mut(split.track_idx) else {
            return false;
        };
        let Some(original) = track.get_clip_by_id(split.original_id) else {
            return false;
        };

        let region_start = original.region().start();
        let new_end = region_start + (split_tick - original.start_tick());

        if running {
            track.release_sounding_notes_for_clip(split.original_id);
        }

        let Some(original_mut) = track.get_clip_by_id_mut(split.original_id) else {
            return false;
        };
        original_mut.region_mut().set_region(None, Some(new_end));

        track.add_clip(&split.new_clip)
    }

    /// Undoes one split: removes the right-hand clip and restores the
    /// original's region end to what it was before the split.
    fn unsplit_clip(&mut self, split: &FrozenSplit) -> bool {
        let running = self.is_running();
        let Some(track) = self.tracks.get_mut(split.track_idx) else {
            return false;
        };
        let new_clip_id = split.new_clip.id();
        if running {
            track.release_sounding_notes_for_clip(new_clip_id);
        }
        if track.remove_clip_by_id(new_clip_id).is_none() {
            return false;
        }
        let Some(original) = track.get_clip_by_id_mut(split.original_id) else {
            return false;
        };
        original
            .region_mut()
            .set_region(None, Some(split.pre_split_region_end));
        true
    }

    /// Current metadata of a split's left half (the original, id kept).
    fn original_metadata(&self, split: &FrozenSplit) -> Option<ClipMetadata> {
        self.clip_on(split.track_idx, split.original_id)
            .map(|clip| ClipMetadata::from_clip(split.track_idx, clip))
    }
}

// ---------------------------------------------------------------------------
// SplitClips
// ---------------------------------------------------------------------------

/// Splits one or more clips at a fixed tick, Ableton-style: each target clip
/// is shrunk in place (keeping its id) and a new clip is added carrying the
/// remainder. Backs both the single-clip `E` binding (no time selection) and
/// the multi-clip one (an active time selection) — the only difference
/// between them is how `targets` gets built; the edit/undo mechanics are
/// identical either way.
pub(crate) struct SplitClipsEdit {
    /// Tick every target is cut at.
    split_tick: i32,
    /// `(track index, clip id)` of each clip to split, resolved at construction.
    targets: Vec<(usize, Uuid)>,
    /// Every split that succeeded, frozen after the first `edit()` call.
    /// Reused on redo so ids and content stay identical across repeated
    /// undo/redo, same pattern as `PasteClipsEdit`'s frozen `targets`.
    frozen: Option<Vec<FrozenSplit>>,
}

impl SplitClipsEdit {
    /// Single-clip binding (`E` with no active time selection): splits only
    /// the selected clip. `None` when nothing is selected or `split_tick`
    /// doesn't fall strictly inside the selected clip's bounds.
    pub(crate) fn from_selected_clip(sequencer: &Sequencer, split_tick: i32) -> Option<Self> {
        let clip_id = sequencer.selected_clip_id()?;
        let track_idx = sequencer.selected_track_index()?;
        Self::from_clip(sequencer, track_idx, clip_id, split_tick)
    }

    /// Splits exactly one named clip. `None` when the clip isn't on
    /// `track_idx` or `split_tick` doesn't fall strictly inside its bounds.
    /// Backs `from_selected_clip` and the partial band-drag move
    /// (`MoveClipEdit`'s pre-move split of the marqueed piece).
    pub(crate) fn from_clip(
        sequencer: &Sequencer,
        track_idx: usize,
        clip_id: Uuid,
        split_tick: i32,
    ) -> Option<Self> {
        let clip = sequencer.clip_on(track_idx, clip_id)?;

        if split_tick <= clip.start_tick() || split_tick >= clip.end_tick() {
            return None;
        }

        Some(Self {
            split_tick,
            targets: vec![(track_idx, clip_id)],
            frozen: None,
        })
    }

    /// Time-selection binding (`E` with an active time selection): splits
    /// every clip across all tracks that overlaps `[start, end)` and whose
    /// bounds actually contain `split_tick`. `None` when no such clip
    /// exists.
    pub(crate) fn from_time_range(
        sequencer: &Sequencer,
        start: i32,
        end: i32,
        split_tick: i32,
    ) -> Option<Self> {
        Self::from_time_range_filtered(sequencer, None, start, end, split_tick)
    }

    /// Like `from_time_range`, but restricted to an inclusive track span —
    /// the arranger marquee's track-scoped `E`.
    pub(crate) fn from_time_range_in_tracks(
        sequencer: &Sequencer,
        track_start: usize,
        track_end: usize,
        start: i32,
        end: i32,
        split_tick: i32,
    ) -> Option<Self> {
        Self::from_time_range_filtered(
            sequencer,
            Some((track_start, track_end)),
            start,
            end,
            split_tick,
        )
    }

    /// Shared constructor: the targets are every clip (optionally restricted
    /// to an inclusive track span) overlapping `[start, end)` that strictly
    /// contains `split_tick`. Also backs [`EdgeSplits`].
    fn from_time_range_filtered(
        sequencer: &Sequencer,
        track_filter: Option<(usize, usize)>,
        start: i32,
        end: i32,
        split_tick: i32,
    ) -> Option<Self> {
        let mut targets = Vec::new();

        for (track_idx, track) in sequencer.tracks().iter().enumerate() {
            if !in_tracks(track_filter, track_idx) {
                continue;
            }
            for clip_id in track.find_clip_ids_in(start, end) {
                if let Some(clip) = track.get_clip_by_id(clip_id)
                    && split_tick > clip.start_tick()
                    && split_tick < clip.end_tick()
                {
                    targets.push((track_idx, clip_id));
                }
            }
        }

        if targets.is_empty() {
            return None;
        }

        Some(Self {
            split_tick,
            targets,
            frozen: None,
        })
    }

    /// Splits every valid target (freezing the results for redo on the first
    /// call). Returns [`EditResult::ClipsSplit`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let mut updated = Vec::new();
        let mut added = Vec::new();

        if let Some(frozen) = self.frozen.as_ref() {
            // Redo: reapply the frozen shrink and re-add the frozen
            // right-hand clips rather than recomputing from scratch.
            for split in frozen {
                if !sequencer.reapply_split(split, self.split_tick) {
                    continue;
                }
                updated.extend(sequencer.original_metadata(split));
                added.push(ClipMetadata::from_clip(split.track_idx, &split.new_clip));
            }
        } else {
            let frozen = sequencer.split_clips_at_tick(&self.targets, self.split_tick);
            if frozen.is_empty() {
                return EditResult::NoOp;
            }
            for split in &frozen {
                updated.extend(sequencer.original_metadata(split));
                added.push(ClipMetadata::from_clip(split.track_idx, &split.new_clip));
            }
            self.frozen = Some(frozen);
        }

        if added.is_empty() {
            EditResult::NoOp
        } else {
            EditResult::ClipsSplit { updated, added }
        }
    }

    /// Removes each remainder clip and restores each original's region end.
    /// Returns [`EditResult::ClipsUnsplit`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some(frozen) = self.frozen.as_ref() else {
            return EditResult::NoOp;
        };

        let selected_track_idx = sequencer.selected_track_index();
        let selected_clip_id = sequencer.selected_clip_id();

        let mut updated = Vec::new();
        let mut removed = Vec::new();

        for split in frozen {
            if !sequencer.unsplit_clip(split) {
                continue;
            }
            updated.extend(sequencer.original_metadata(split));
            removed.push(ClipMetadata::from_clip(split.track_idx, &split.new_clip));
        }

        if removed.is_empty() {
            EditResult::NoOp
        } else {
            EditResult::ClipsUnsplit {
                updated,
                removed,
                selected_track_idx,
                selected_clip_id,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// EdgeSplits
// ---------------------------------------------------------------------------

/// The two splits a range edit (`DeleteInRangeEdit`, `MuteInRangeEdit`,
/// `MoveRangeEdit`) makes at the edges of `[start, end)`: every straddling
/// clip is cut at `end`, then at `start`. Splitting at `end` *first* is
/// deliberate: `split_clips_at_tick` keeps the original clip id on the left
/// piece, and for a range-spanning clip that left piece is exactly the one
/// that still straddles `start`, so the `start` split's targets (frozen at
/// construction against the original ids) stay valid without any lazy
/// re-resolution. Undo runs LIFO, `start` first.
pub(super) struct EdgeSplits {
    /// Split at `end`; `None` when nothing straddles it.
    end: Option<SplitClipsEdit>,
    /// Split at `start`; `None` when nothing straddles it.
    start: Option<SplitClipsEdit>,
}

impl EdgeSplits {
    /// Resolves the clips on the filtered tracks straddling each edge.
    pub(super) fn new(
        sequencer: &Sequencer,
        track_filter: Option<(usize, usize)>,
        start: i32,
        end: i32,
    ) -> Self {
        let split_at = |split_tick| {
            SplitClipsEdit::from_time_range_filtered(
                sequencer,
                track_filter,
                start,
                end,
                split_tick,
            )
        };
        Self {
            end: split_at(end),
            start: split_at(start),
        }
    }

    /// Runs both splits (each replaying its frozen snapshot on redo).
    /// Returns the ids they reported as `(updated, added)`.
    pub(super) fn edit(&mut self, sequencer: &mut Sequencer) -> (Vec<Uuid>, Vec<Uuid>) {
        let mut updated_ids = Vec::new();
        let mut added_ids = Vec::new();
        for split in [self.end.as_mut(), self.start.as_mut()]
            .into_iter()
            .flatten()
        {
            if let EditResult::ClipsSplit { updated, added } = split.edit(sequencer) {
                updated_ids.extend(updated.iter().map(|m| m.clip_id));
                added_ids.extend(added.iter().map(|m| m.clip_id));
            }
        }
        (updated_ids, added_ids)
    }

    /// Reverses both splits. Requires every right-hand piece to be back on
    /// its track — `SplitClipsEdit::undo` skips a split whose piece is
    /// missing. Returns the ids of the originals restored to full bounds and
    /// every right-hand piece removed.
    pub(super) fn undo(&mut self, sequencer: &mut Sequencer) -> (Vec<Uuid>, Vec<ClipMetadata>) {
        let mut restored_ids = Vec::new();
        let mut removed = Vec::new();
        for split in [self.start.as_mut(), self.end.as_mut()]
            .into_iter()
            .flatten()
        {
            if let EditResult::ClipsUnsplit {
                updated,
                removed: pieces,
                ..
            } = split.undo(sequencer)
            {
                restored_ids.extend(updated.iter().map(|m| m.clip_id));
                removed.extend(pieces);
            }
        }
        (restored_ids, removed)
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

    fn select_clip(sequencer: &mut Sequencer, track_idx: usize, clip_id: Uuid) {
        let track_id = sequencer.track_id_by_index(track_idx).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(Some(clip_id));
    }

    #[test]
    fn split_from_selected_clip_returns_none_when_nothing_selected() {
        let sequencer = test_sequencer();
        assert!(SplitClipsEdit::from_selected_clip(&sequencer, 480).is_none());
    }

    #[test]
    fn split_from_selected_clip_returns_none_when_split_tick_outside_clip_bounds() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select_clip(&mut sequencer, 0, clip_id);

        assert!(SplitClipsEdit::from_selected_clip(&sequencer, 0).is_none()); // at start
        assert!(SplitClipsEdit::from_selected_clip(&sequencer, 960).is_none()); // at end
        assert!(SplitClipsEdit::from_selected_clip(&sequencer, 1200).is_none()); // past end
    }

    #[test]
    fn split_from_clip_needs_the_clip_on_the_named_track_and_the_tick_inside_it() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        assert!(SplitClipsEdit::from_clip(&sequencer, 0, clip_id, 480).is_some());
        assert!(SplitClipsEdit::from_clip(&sequencer, 1, clip_id, 480).is_none()); // wrong track
        assert!(SplitClipsEdit::from_clip(&sequencer, 0, clip_id, 0).is_none()); // at start
        assert!(SplitClipsEdit::from_clip(&sequencer, 0, clip_id, 960).is_none()); // at end
    }

    #[test]
    fn split_edit_shrinks_original_and_adds_phase_locked_remainder() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let original_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select_clip(&mut sequencer, 0, original_id);

        let mut edit = SplitClipsEdit::from_selected_clip(&sequencer, 480).unwrap();
        let EditResult::ClipsSplit { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsSplit");
        };

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].clip_id, original_id);
        assert_eq!(updated[0].start_tick, 0);
        assert_eq!(updated[0].end_tick, 480);

        assert_eq!(added.len(), 1);
        assert_ne!(added[0].clip_id, original_id);
        assert_eq!(added[0].start_tick, 480);
        assert_eq!(added[0].end_tick, 960);

        assert_eq!(sequencer.tracks()[0].clips().len(), 2);
        let right_id = added[0].clip_id;
        let right = sequencer.tracks()[0].get_clip_by_id(right_id).unwrap();
        assert_eq!(right.region().start(), 480);
        assert_eq!(right.region().end(), 960);
    }

    #[test]
    fn split_undo_removes_added_clip_and_restores_original_bounds() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let original_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select_clip(&mut sequencer, 0, original_id);

        let mut edit = SplitClipsEdit::from_selected_clip(&sequencer, 480).unwrap();
        edit.edit(&mut sequencer);

        let EditResult::ClipsUnsplit {
            updated, removed, ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected ClipsUnsplit");
        };

        assert_eq!(removed.len(), 1);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].start_tick, 0);
        assert_eq!(updated[0].end_tick, 960);

        assert_eq!(sequencer.tracks()[0].clips().len(), 1);
        let restored = sequencer.tracks()[0].get_clip_by_id(original_id).unwrap();
        assert_eq!(restored.region().start(), 0);
        assert_eq!(restored.region().end(), 960);
    }

    #[test]
    fn split_redo_reuses_the_frozen_snapshot_instead_of_recomputing() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let original_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select_clip(&mut sequencer, 0, original_id);

        let mut edit = SplitClipsEdit::from_selected_clip(&sequencer, 480).unwrap();

        let EditResult::ClipsSplit { added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsSplit");
        };
        let first_right_id = added[0].clip_id;

        edit.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1);

        let EditResult::ClipsSplit { added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsSplit");
        };
        assert_eq!(added[0].clip_id, first_right_id);
        assert_eq!(sequencer.tracks()[0].clips().len(), 2);
    }

    #[test]
    fn split_from_time_range_returns_none_when_no_clip_contains_split_tick() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(500, 100)); // [500, 600)

        // Range overlaps the clip, but the split tick doesn't fall inside it.
        assert!(SplitClipsEdit::from_time_range(&sequencer, 0, 1000, 700).is_none());
    }

    #[test]
    fn split_from_time_range_splits_every_overlapping_clip_across_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // split tick 480 inside
        sequencer.tracks_mut()[0].add_clip(&clip_at(2000, 960)); // outside range, untouched
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960)); // split tick 480 inside

        let mut edit = SplitClipsEdit::from_time_range(&sequencer, 0, 960, 480)
            .expect("a clip on each of two tracks contains the split tick");
        let EditResult::ClipsSplit { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsSplit");
        };

        assert_eq!(updated.len(), 2);
        assert_eq!(added.len(), 2);

        assert_eq!(sequencer.tracks()[0].clips().len(), 3); // 2 split halves + untouched clip
        assert_eq!(sequencer.tracks()[1].clips().len(), 2);
    }

    #[test]
    fn split_from_time_range_in_tracks_only_touches_the_named_span() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // outside the span
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960)); // in the span
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 960)); // in the span
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 960)); // outside the span

        let mut edit = SplitClipsEdit::from_time_range_in_tracks(&sequencer, 1, 2, 0, 960, 480)
            .expect("clips on tracks 1 and 2 contain the split tick");
        let EditResult::ClipsSplit { updated, added } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsSplit");
        };

        assert_eq!(updated.len(), 2);
        assert_eq!(added.len(), 2);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1); // untouched
        assert_eq!(sequencer.tracks()[1].clips().len(), 2);
        assert_eq!(sequencer.tracks()[2].clips().len(), 2);
        assert_eq!(sequencer.tracks()[3].clips().len(), 1); // untouched
    }

    #[test]
    fn split_from_time_range_in_tracks_ignores_a_clip_just_outside_the_span() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 960));

        assert!(SplitClipsEdit::from_time_range_in_tracks(&sequencer, 1, 2, 0, 960, 480).is_none());
    }

    fn clip_with_open_note() -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), Some(960));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: splitting the currently-playing clip mid-note must not
    /// strand the open note — shrinking the original's region end away from
    /// its note-off (still ahead of the split point) would otherwise mean
    /// `Track::tick` never reaches it.
    #[test]
    fn splitting_the_playing_clip_releases_its_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);

        let clip = clip_with_open_note();
        sequencer.tracks_mut()[0].add_clip(&clip);
        let clip_id = sequencer.tracks()[0].clips()[0].id();
        select_clip(&mut sequencer, 0, clip_id);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);

        // One tick sounds the note-on.
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        // Split mid-note, well before the note-off at 480.
        let mut edit = SplitClipsEdit::from_selected_clip(&sequencer, 240).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        // The stranded note gets a real note-off.
        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
