//! Undoable marquee move — the arranger band drag started *inside* an active
//! marquee. Splits the marquee's tick range out of every clip it overlaps on
//! the marqueed tracks, lifts every resulting piece, carves each piece's
//! destination span out of its destination track via [`DeleteInRangeEdit`],
//! and drops the pieces there shifted by one tick/track delta, ids kept.

use std::collections::HashSet;

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::delete_in_range::DeleteInRangeEdit;
use super::split::EdgeSplits;
use super::{CarveBuckets, interior_clip_ids, metadata_for};

// ---------------------------------------------------------------------------
// MoveRange
// ---------------------------------------------------------------------------

/// Moves everything inside a marquee rectangle — tick range × track range —
/// by the same tick and track delta, Ableton-style: the moved block moves as
/// one unit and whatever it lands on that is *not* part of the block is
/// carved out (trimmed / split / removed). Backs the arranger band drag that
/// starts inside an active marquee (`020-views-and-state.md` § "Clip Band
/// Press & Move Drag"); a marquee covering one track and part of one clip is
/// simply its one-piece case, so this is also how part of a clip is moved.
///
/// Three phases, each composed from the existing primitives:
/// 1. **Split** every overlapping clip on the marqueed tracks at `rect.end`,
///    then at `rect.start` — track-scoped [`EdgeSplits`], the exact
///    composition `DeleteInRangeEdit` uses. Non-destructive: every piece
///    keeps the full event list.
/// 2. **Lift** every clip now fully inside the rect — the pieces. All of
///    them, before any carve, so a piece never carves a sibling that is
///    about to move out of the way.
/// 3. For each piece, **carve** its destination span out of its destination
///    track (`DeleteInRangeEdit::from_track_range`, built lazily one at a
///    time against the live state like `PasteClipsEdit`'s) and **drop** it
///    there with its id and region untouched.
///
/// Undo reverses LIFO: lift the pieces off their destinations, un-carve in
/// reverse order, put the pieces back, un-split — re-merging every source
/// clip to its pre-move bounds.
pub(crate) struct MoveRangeEdit {
    /// The marquee, frozen at press time.
    rect: TimeSelectionRect,
    /// Tick shift applied to every piece.
    delta_ticks: i32,
    /// Lane shift applied to every piece.
    delta_tracks: i32,
    /// `(track, id)` of every clip overlapping the rect at construction —
    /// the clips undo re-merges, reported as `restored`.
    source_ids: Vec<(usize, Uuid)>,
    /// The splits at the rect's edges, on the marqueed tracks.
    splits: EdgeSplits,
    /// `(source track, id)` of every piece, frozen on the first `edit()` in
    /// move order — the ids are stable across undo/redo because the splits
    /// freeze theirs. `None` until then.
    pieces: Option<Vec<(usize, Uuid)>>,
    /// One carve per piece, parallel to `pieces`; the inner `None` means the
    /// destination span was already clear. Redo replays the frozen carves.
    carves: Option<Vec<Option<DeleteInRangeEdit>>>,
}

impl MoveRangeEdit {
    /// Builds the edit from the drag's frozen release delta. `None` when the
    /// marquee has no tick width, the delta is zero, the shift would push
    /// the block before tick 0 or off the lanes, or no clip on the marqueed
    /// tracks overlaps the marquee — nothing then enters the undo record.
    pub(crate) fn new(
        sequencer: &Sequencer,
        rect: TimeSelectionRect,
        delta_ticks: i32,
        delta_tracks: i32,
    ) -> Option<Self> {
        let track_count = sequencer.tracks().len();
        if !rect.has_tick_range()
            || (delta_ticks == 0 && delta_tracks == 0)
            || rect.start + delta_ticks < 0
            || rect.track_start > rect.track_end
            || rect.track_end >= track_count
            || rect.track_start as i32 + delta_tracks < 0
            || rect.track_end as i32 + delta_tracks >= track_count as i32
        {
            return None;
        }

        let source_ids: Vec<(usize, Uuid)> = sequencer.clip_ids_in(rect).collect();
        if source_ids.is_empty() {
            return None;
        }

        Some(Self {
            rect,
            delta_ticks,
            delta_tracks,
            source_ids,
            splits: EdgeSplits::new(
                sequencer,
                Some((rect.track_start, rect.track_end)),
                rect.start,
                rect.end,
            ),
            pieces: None,
            carves: None,
        })
    }

    /// The rect the marquee should sit on after the move.
    fn shifted_rect(&self) -> TimeSelectionRect {
        TimeSelectionRect {
            start: self.rect.start + self.delta_ticks,
            end: self.rect.end + self.delta_ticks,
            track_start: (self.rect.track_start as i32 + self.delta_tracks) as usize,
            track_end: (self.rect.track_end as i32 + self.delta_tracks) as usize,
        }
    }

    /// Splits the edges, lifts every piece, then carves + drops each at its
    /// destination (freezing pieces and carves on the first call). Returns
    /// [`EditResult::RangeMoved`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let (split_updated_ids, split_added_ids) = self.splits.edit(sequencer);

        // The pieces: every clip on the marqueed tracks now fully inside the
        // rect, resolved after the splits.
        let building = self.pieces.is_none();
        let pieces = self.pieces.take().unwrap_or_else(|| {
            let rect = self.rect;
            interior_clip_ids(
                sequencer,
                Some((rect.track_start, rect.track_end)),
                rect.start,
                rect.end,
            )
        });
        if pieces.is_empty() {
            self.splits.undo(sequencer);
            return EditResult::NoOp;
        }

        // Lift every piece before carving anything.
        let lifted: Vec<Option<(ClipMetadata, Clip)>> = pieces
            .iter()
            .map(|&(track_idx, id)| sequencer.lift_clip(track_idx, id))
            .collect();

        let mut carves = self
            .carves
            .take()
            .unwrap_or_else(|| pieces.iter().map(|_| None).collect());

        let mut moved_from = Vec::new();
        let mut moved_to = Vec::new();
        let mut carved = CarveBuckets::default();

        for (i, ((source_track, _), lifted)) in pieces.iter().zip(lifted).enumerate() {
            let Some((from, mut clip)) = lifted else {
                continue;
            };
            let dest_track = (*source_track as i32 + self.delta_tracks) as usize;
            clip.set_start_tick(clip.start_tick() + self.delta_ticks);

            if building {
                carves[i] = DeleteInRangeEdit::from_track_range(
                    sequencer,
                    dest_track,
                    clip.start_tick(),
                    clip.end_tick(),
                );
            }
            if let Some(carve) = carves.get_mut(i).and_then(|c| c.as_mut()) {
                carved.absorb(carve.edit(sequencer));
            }

            let added = sequencer
                .tracks_mut()
                .get_mut(dest_track)
                .is_some_and(|track| track.add_clip(&clip));
            if !added {
                // Should not happen: the carve just cleared exactly this
                // span. Put the piece back where it came from — the rect
                // span on its source track was vacated by the lifts — so it
                // is never lost; it simply doesn't move.
                clip.set_start_tick(clip.start_tick() - self.delta_ticks);
                if let Some(track) = sequencer.tracks_mut().get_mut(*source_track) {
                    track.add_clip(&clip);
                }
                continue;
            }

            moved_from.push(from);
            moved_to.push(ClipMetadata::from_clip(dest_track, &clip));
        }

        self.pieces = Some(pieces);
        self.carves = Some(carves);

        // The split buckets exclude the pieces themselves — they are
        // `moved_from`/`moved_to` — and are read back from the live state so
        // a leftover the carve then touched reports its final bounds.
        let piece_ids: HashSet<Uuid> = moved_to.iter().map(|m| m.clip_id).collect();
        let split_updated = metadata_for(sequencer, &split_updated_ids, &piece_ids);
        let split_added = metadata_for(sequencer, &split_added_ids, &piece_ids);

        EditResult::RangeMoved {
            moved_from,
            moved_to,
            split_updated,
            split_added,
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
            new_selection: self.shifted_rect(),
        }
    }

    /// Exact reverse: lifts every piece off its destination, un-carves in
    /// reverse order, puts the pieces back on their source tracks, then
    /// un-splits the edges so every source clip is re-merged. Returns
    /// [`EditResult::RangeUnmoved`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let (Some(pieces), Some(carves)) = (self.pieces.as_ref(), self.carves.as_mut()) else {
            return EditResult::NoOp;
        };

        // 1. Lift every piece off its destination first, so each carve's
        //    undo finds the span it must restore into vacated.
        let mut unmoved = Vec::new();
        let mut lifted: Vec<Option<(usize, Clip)>> = Vec::with_capacity(pieces.len());
        for &(source_track, id) in pieces {
            let dest_track = (source_track as i32 + self.delta_tracks) as usize;
            match sequencer.lift_clip(dest_track, id) {
                Some((from, clip)) => {
                    unmoved.push(from);
                    lifted.push(Some((source_track, clip)));
                }
                None => lifted.push(None),
            }
        }

        // 2. Un-carve in reverse order (LIFO).
        let mut carved = CarveBuckets::default();
        for carve in carves.iter_mut().rev().flatten() {
            carved.absorb(carve.undo(sequencer));
        }

        // 3. Put every piece back. The rect span on each source track was
        //    vacated by `edit()`'s lifts and the carves only ever touched
        //    other clips, so these adds always succeed.
        let piece_ids: HashSet<Uuid> = pieces.iter().map(|&(_, id)| id).collect();
        for (source_track, mut clip) in lifted.into_iter().flatten() {
            clip.set_start_tick(clip.start_tick() - self.delta_ticks);
            if let Some(track) = sequencer.tracks_mut().get_mut(source_track) {
                track.add_clip(&clip);
            }
        }

        // 4. Un-split. A piece that was itself a split product is removed
        //    again here; the UI never had it at the source (its shape was
        //    keyed to the destination and `unmoved` drops that), so it is
        //    left out of `split_removed`.
        let (_, split_removed) = self.splits.undo(sequencer);
        let split_removed: Vec<ClipMetadata> = split_removed
            .into_iter()
            .filter(|m| !piece_ids.contains(&m.clip_id))
            .collect();

        // 5. Every source clip is back at its pre-move bounds.
        let restored: Vec<ClipMetadata> = self
            .source_ids
            .iter()
            .filter_map(|&(track_idx, id)| {
                let clip = sequencer.clip_on(track_idx, id)?;
                Some(ClipMetadata::from_clip(track_idx, clip))
            })
            .collect();

        EditResult::RangeUnmoved {
            unmoved,
            restored,
            split_removed,
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
            restored_selection: self.rect,
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::models::event::Event;

    use crate::core::sequencer::test_support::{clip_at, rect, test_sequencer};

    use super::*;

    /// A clip with a note every 500 ticks, so a piece's region window can be
    /// checked to still frame exactly its own material.
    fn clip_with_marker_notes(start_tick: i32, length: i32) -> Clip {
        let mut clip = clip_at(start_tick, length);
        for offset in (0..length).step_by(500) {
            clip.add_event(Event::new(offset, 0, vec![0x90, 60, 100]));
        }
        clip
    }

    fn starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    fn add(sequencer: &mut Sequencer, track_idx: usize, clip: Clip) -> Uuid {
        let id = clip.id();
        assert!(sequencer.tracks_mut()[track_idx].add_clip(&clip));
        id
    }

    fn ids(metadata: &[ClipMetadata]) -> Vec<Uuid> {
        metadata.iter().map(|m| m.clip_id).collect()
    }

    #[test]
    fn new_returns_none_for_a_no_op_empty_or_off_lane_move() {
        let mut sequencer = test_sequencer();
        add(&mut sequencer, 0, clip_at(0, 4000));
        let track_count = sequencer.tracks().len();

        assert!(MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 0, 0).is_none()); // no-op
        assert!(MoveRangeEdit::new(&sequencer, rect(1000, 1000, 0, 0), 500, 0).is_none()); // no width
        assert!(MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), -1500, 0).is_none()); // before 0
        assert!(MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 0, -1).is_none()); // above lanes
        assert!(
            MoveRangeEdit::new(
                &sequencer,
                rect(1000, 2000, 0, 1),
                0,
                track_count as i32 - 1
            )
            .is_none()
        ); // below lanes
        assert!(MoveRangeEdit::new(&sequencer, rect(5000, 6000, 0, 0), 500, 0).is_none()); // nothing there
        assert!(MoveRangeEdit::new(&sequencer, rect(1000, 2000, 1, 1), 500, 0).is_none()); // other track
    }

    #[test]
    fn one_piece_from_the_middle_of_a_clip_is_split_out_and_moved() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_with_marker_notes(0, 4000));

        let mut edit = MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 4000, 1).unwrap();
        let EditResult::RangeMoved {
            moved_from,
            moved_to,
            split_updated,
            split_added,
            carve_updated,
            carve_added,
            carve_removed,
            new_selection,
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };

        assert_eq!(starts(&sequencer, 0), vec![(0, 1000), (2000, 4000)]);
        assert_eq!(starts(&sequencer, 1), vec![(5000, 6000)]);
        assert_eq!(ids(&split_updated), vec![id]);
        assert_eq!(split_added.len(), 1);
        assert_eq!(
            (split_added[0].start_tick, split_added[0].end_tick),
            (2000, 4000)
        );
        assert!(carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty());

        let piece_id = moved_to[0].clip_id;
        assert_ne!(piece_id, id);
        assert_eq!(ids(&moved_from), vec![piece_id]);
        assert_eq!(
            (moved_from[0].track_idx, moved_from[0].start_tick),
            (0, 1000)
        );
        assert_eq!((moved_to[0].track_idx, moved_to[0].start_tick), (1, 5000));
        assert_eq!(
            (
                new_selection.start,
                new_selection.end,
                new_selection.track_start,
                new_selection.track_end
            ),
            (5000, 6000, 1, 1)
        );

        // Non-destructive: the piece's region window still frames its own
        // material, phase-locked.
        let piece = sequencer.tracks()[1].get_clip_by_id(piece_id).unwrap();
        assert_eq!((piece.region().start(), piece.region().end()), (1000, 2000));
        assert_eq!(piece.events().len(), 8);
    }

    #[test]
    fn a_piece_at_a_clip_head_keeps_the_clip_id() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(0, 4000));

        let mut edit = MoveRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 6000, 0).unwrap();
        let EditResult::RangeMoved {
            moved_to,
            split_updated,
            split_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };

        assert_eq!(ids(&moved_to), vec![id]);
        assert!(split_updated.is_empty()); // the shrunk original *is* the piece
        assert_eq!(split_added.len(), 1);
        assert_eq!(starts(&sequencer, 0), vec![(1000, 4000), (6000, 7000)]);
    }

    #[test]
    fn moves_every_piece_across_tracks_and_clips_as_one_block() {
        let mut sequencer = test_sequencer();
        // Track 0: two adjacent clips straddling the rect; track 1: one clip
        // fully inside; track 2: outside the track range.
        let a = add(&mut sequencer, 0, clip_at(0, 1500)); // [0, 1500)
        let b = add(&mut sequencer, 0, clip_at(1500, 1500)); // [1500, 3000)
        let c = add(&mut sequencer, 1, clip_at(1200, 500)); // [1200, 1700)
        let d = add(&mut sequencer, 2, clip_at(1000, 1000)); // untouched

        let mut edit = MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 1), 4000, 1).unwrap();
        let EditResult::RangeMoved {
            moved_to,
            split_updated,
            split_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };

        // a's head and b's tail stay; the rect's contents land one lane down.
        assert_eq!(starts(&sequencer, 0), vec![(0, 1000), (2000, 3000)]);
        assert_eq!(starts(&sequencer, 1), vec![(5000, 5500), (5500, 6000)]);
        assert_eq!(starts(&sequencer, 2), vec![(1000, 2000), (5200, 5700)]);
        assert_eq!(
            sequencer.tracks()[2]
                .get_clip_by_id(d)
                .unwrap()
                .start_tick(),
            1000
        );

        assert_eq!(ids(&split_updated), vec![a]); // b's id went with its head piece
        assert_eq!(split_added.len(), 1); // b's tail [2000, 3000)
        assert_eq!(moved_to.len(), 3);
        assert!(ids(&moved_to).contains(&b));
        assert!(ids(&moved_to).contains(&c));
    }

    #[test]
    fn a_shifted_block_never_carves_its_own_pieces() {
        let mut sequencer = test_sequencer();
        let a = add(&mut sequencer, 0, clip_at(0, 1000));
        let b = add(&mut sequencer, 0, clip_at(1000, 1000));

        // Shift both right by half a clip: a lands on b's old span.
        let mut edit = MoveRangeEdit::new(&sequencer, rect(0, 2000, 0, 0), 500, 0).unwrap();
        let EditResult::RangeMoved {
            carve_updated,
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };

        assert!(carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(500, 1500), (1500, 2500)]);
        assert_eq!(
            sequencer.tracks()[0]
                .get_clip_by_id(a)
                .unwrap()
                .start_tick(),
            500
        );
        assert_eq!(
            sequencer.tracks()[0]
                .get_clip_by_id(b)
                .unwrap()
                .start_tick(),
            1500
        );
    }

    #[test]
    fn carves_what_the_block_lands_on() {
        let mut sequencer = test_sequencer();
        add(&mut sequencer, 0, clip_at(0, 1000));
        let big = add(&mut sequencer, 1, clip_at(0, 4000));

        let mut edit = MoveRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 1500, 1).unwrap();
        let EditResult::RangeMoved {
            carve_updated,
            carve_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };

        assert_eq!(ids(&carve_updated), vec![big]);
        assert_eq!(carve_added.len(), 1);
        assert!(starts(&sequencer, 0).is_empty());
        assert_eq!(
            starts(&sequencer, 1),
            vec![(0, 1500), (1500, 2500), (2500, 4000)]
        );
    }

    #[test]
    fn undo_re_merges_every_source_clip_and_restores_the_carved_material() {
        let mut sequencer = test_sequencer();
        let a = add(&mut sequencer, 0, clip_with_marker_notes(0, 4000));
        let c = add(&mut sequencer, 1, clip_at(1200, 500));
        let victim = add(&mut sequencer, 2, clip_at(5200, 480));

        let mut edit = MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 1), 4000, 1).unwrap();
        let EditResult::RangeMoved {
            moved_to,
            split_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };
        let tail_id = split_added[0].clip_id;

        let EditResult::RangeUnmoved {
            unmoved,
            restored,
            split_removed,
            carve_added,
            restored_selection,
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected RangeUnmoved");
        };

        assert_eq!(ids(&unmoved), ids(&moved_to));
        assert_eq!(ids(&split_removed), vec![tail_id]);
        assert_eq!(ids(&carve_added), vec![victim]);
        assert_eq!(restored.len(), 2);
        assert!(ids(&restored).contains(&a) && ids(&restored).contains(&c));
        assert_eq!(
            (restored_selection.start, restored_selection.end),
            (1000, 2000)
        );

        assert_eq!(starts(&sequencer, 0), vec![(0, 4000)]);
        assert_eq!(starts(&sequencer, 1), vec![(1200, 1700)]);
        assert_eq!(starts(&sequencer, 2), vec![(5200, 5680)]);
        let original = sequencer.tracks()[0].get_clip_by_id(a).unwrap();
        assert_eq!(
            (original.region().start(), original.region().end()),
            (0, 4000)
        );
        assert_eq!(original.events().len(), 8);
    }

    #[test]
    fn redo_replays_with_stable_piece_and_leftover_ids() {
        let mut sequencer = test_sequencer();
        add(&mut sequencer, 0, clip_at(0, 4000));
        add(&mut sequencer, 1, clip_at(0, 4000));

        let mut edit = MoveRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 1500, 1).unwrap();
        let EditResult::RangeMoved {
            moved_to,
            split_added,
            carve_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };
        let first = (ids(&moved_to), ids(&split_added), ids(&carve_added));

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 4000)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 4000)]);

        let EditResult::RangeMoved {
            moved_to,
            split_added,
            carve_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeMoved");
        };
        assert_eq!(
            (ids(&moved_to), ids(&split_added), ids(&carve_added)),
            first
        );
        assert_eq!(starts(&sequencer, 0), vec![(0, 1000), (2000, 4000)]);
        assert_eq!(
            starts(&sequencer, 1),
            vec![(0, 2500), (2500, 3500), (3500, 4000)]
        );
    }
}
