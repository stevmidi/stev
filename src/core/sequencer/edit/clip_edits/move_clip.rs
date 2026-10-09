//! Undoable clip move — the arranger band drag. Lifts one clip off its track,
//! carves the span it lands on out of the destination track via
//! [`DeleteInRangeEdit`], and puts the clip back down there with the same id.

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::CarveBuckets;
use super::delete_in_range::DeleteInRangeEdit;

// ---------------------------------------------------------------------------
// MoveClip
// ---------------------------------------------------------------------------

/// Moves one clip to a new start tick and/or another track, Ableton-style:
/// whatever it lands on is carved out (trimmed / split / removed) so the moved
/// clip wins. Backs the arranger's band drag (`020-views-and-state.md`
/// § "Clip Band Press & Move Drag") — the drag itself is a purely view-local
/// ghost; this edit is the single mutation sent on release.
///
/// Composes one `DeleteInRangeEdit::from_track_range` for the destination
/// span, exactly like `PasteClipsEdit`'s per-target carve, but built *after*
/// the clip has been lifted off its source track — that is what stops a
/// same-track move over its own old span from carving itself. The clip keeps
/// its id and its region throughout (a move never trims), so nothing needs a
/// frozen snapshot: the clip object itself travels through `edit()`/`undo()`.
pub(crate) struct MoveClipEdit {
    /// The clip being moved.
    clip_id: Uuid,
    /// Track it was lifted from.
    from_track_idx: usize,
    /// Its start tick before the move.
    from_start_tick: i32,
    /// Track it lands on.
    to_track_idx: usize,
    /// Its start tick after the move.
    to_start_tick: i32,
    /// `None` until the first `edit()`; then `Some(carve)` where the inner
    /// `Option` is `None` when the destination span was already clear. Redo
    /// replays the frozen carve rather than rebuilding it.
    carve: Option<Option<DeleteInRangeEdit>>,
}

impl MoveClipEdit {
    /// Builds the edit from the drag's frozen release target. `None` when the
    /// clip isn't on `from_track_idx`, the destination is out of range or
    /// negative, or the target equals the current position (a no-op that must
    /// not enter the undo record).
    pub(crate) fn new(
        sequencer: &Sequencer,
        from_track_idx: usize,
        clip_id: Uuid,
        to_track_idx: usize,
        to_start_tick: i32,
    ) -> Option<Self> {
        if to_track_idx >= sequencer.tracks().len() || to_start_tick < 0 {
            return None;
        }
        let clip = sequencer.clip_on(from_track_idx, clip_id)?;
        let from_start_tick = clip.start_tick();
        if from_track_idx == to_track_idx && from_start_tick == to_start_tick {
            return None;
        }

        Some(Self {
            clip_id,
            from_track_idx,
            from_start_tick,
            to_track_idx,
            to_start_tick,
            carve: None,
        })
    }

    /// Lifts the clip, carves its destination span (building the carve on the
    /// first call), and drops it there. Returns [`EditResult::ClipMoved`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some((from, mut clip)) = sequencer.lift_clip(self.from_track_idx, self.clip_id) else {
            return EditResult::NoOp;
        };
        clip.set_start_tick(self.to_start_tick);

        // The carve is built against the state *after* the lift, so on a
        // same-track move the clip's own vacated span is already empty and
        // only genuinely other clips are targeted.
        if self.carve.is_none() {
            self.carve = Some(DeleteInRangeEdit::from_track_range(
                sequencer,
                self.to_track_idx,
                clip.start_tick(),
                clip.end_tick(),
            ));
        }

        let mut carved = CarveBuckets::default();
        if let Some(carve) = self.carve.as_mut().and_then(|c| c.as_mut()) {
            carved.absorb(carve.edit(sequencer));
        }

        let Some(track) = sequencer.tracks_mut().get_mut(self.to_track_idx) else {
            return EditResult::NoOp;
        };
        if !track.add_clip(&clip) {
            // Should not happen: the carve just cleared exactly this span.
            // Revert defensively so the clip is never lost.
            if let Some(carve) = self.carve.as_mut().and_then(|c| c.as_mut()) {
                carve.undo(sequencer);
            }
            clip.set_start_tick(self.from_start_tick);
            if let Some(track) = sequencer.tracks_mut().get_mut(self.from_track_idx) {
                track.add_clip(&clip);
            }
            return EditResult::NoOp;
        }

        EditResult::ClipMoved {
            from,
            to: ClipMetadata::from_clip(self.to_track_idx, &clip),
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
        }
    }

    /// Exact reverse: lifts the clip off its destination, un-carves the span,
    /// and drops the clip back where it came from. Returns
    /// [`EditResult::ClipUnmoved`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some((from, mut clip)) = sequencer.lift_clip(self.to_track_idx, self.clip_id) else {
            return EditResult::NoOp;
        };

        let mut carved = CarveBuckets::default();
        if let Some(carve) = self.carve.as_mut().and_then(|c| c.as_mut()) {
            carved.absorb(carve.undo(sequencer));
        }

        // The source span was vacated by `edit()` and the carve only ever
        // touched other clips, none of which overlapped it, so this add
        // always succeeds.
        clip.set_start_tick(self.from_start_tick);
        let Some(track) = sequencer.tracks_mut().get_mut(self.from_track_idx) else {
            return EditResult::NoOp;
        };
        if !track.add_clip(&clip) {
            return EditResult::NoOp;
        }

        EditResult::ClipUnmoved {
            from,
            to: ClipMetadata::from_clip(self.from_track_idx, &clip),
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
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

    fn add(sequencer: &mut Sequencer, track_idx: usize, clip: Clip) -> Uuid {
        let id = clip.id();
        assert!(sequencer.tracks_mut()[track_idx].add_clip(&clip));
        id
    }

    #[test]
    fn new_returns_none_for_a_no_op_bad_track_or_negative_start() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(960, 960));
        let track_count = sequencer.tracks().len();

        assert!(MoveClipEdit::new(&sequencer, 0, id, 0, 960).is_none()); // no-op
        assert!(MoveClipEdit::new(&sequencer, 0, id, track_count, 0).is_none());
        assert!(MoveClipEdit::new(&sequencer, 0, id, 0, -1).is_none());
        assert!(MoveClipEdit::new(&sequencer, 1, id, 0, 0).is_none()); // wrong track
    }

    #[test]
    fn move_into_empty_space_keeps_id_and_vacates_the_source_span() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(0, 960));

        let mut edit = MoveClipEdit::new(&sequencer, 0, id, 0, 4000).unwrap();
        let EditResult::ClipMoved {
            from,
            to,
            carve_updated,
            carve_added,
            carve_removed,
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipMoved");
        };

        assert_eq!((from.start_tick, from.end_tick), (0, 960));
        assert_eq!((to.start_tick, to.end_tick), (4000, 4960));
        assert_eq!(to.clip_id, id);
        assert!(carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(4000, 4960)]);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), id);
    }

    #[test]
    fn move_onto_a_fully_covered_clip_removes_it() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(0, 960));
        let victim = add(&mut sequencer, 0, clip_at(2000, 480)); // fully inside [1800, 2760)

        let mut edit = MoveClipEdit::new(&sequencer, 0, id, 0, 1800).unwrap();
        let EditResult::ClipMoved { carve_removed, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipMoved");
        };

        assert_eq!(carve_removed.len(), 1);
        assert_eq!(carve_removed[0].clip_id, victim);
        assert_eq!(starts(&sequencer, 0), vec![(1800, 2760)]);
    }

    #[test]
    fn move_onto_a_straddling_clip_trims_it() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(0, 960));
        let neighbour = add(&mut sequencer, 0, clip_at(2000, 960)); // [2000, 2960)

        // Land on [1500, 2460): the neighbour's head is trimmed to start at 2460.
        let mut edit = MoveClipEdit::new(&sequencer, 0, id, 0, 1500).unwrap();
        let EditResult::ClipMoved {
            carve_added,
            carve_updated,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipMoved");
        };

        // A straddle-`end` clip: the original (id-keeping) left piece is
        // interior and removed, its right remainder survives as a new clip —
        // the same bucketing `DeleteInRangeEdit` documents.
        assert_eq!(
            carve_removed.iter().map(|m| m.clip_id).collect::<Vec<_>>(),
            vec![neighbour]
        );
        assert_eq!(carve_added.len(), 1);
        assert!(carve_updated.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(1500, 2460), (2460, 2960)]);
    }

    #[test]
    fn move_into_the_middle_of_a_clip_splits_it_around_the_mover() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 1, clip_at(0, 480));
        let big = add(&mut sequencer, 0, clip_at(1000, 3000)); // [1000, 4000)

        let mut edit = MoveClipEdit::new(&sequencer, 1, id, 0, 2000).unwrap();
        let EditResult::ClipMoved {
            carve_updated,
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipMoved");
        };

        assert_eq!(
            carve_updated.iter().map(|m| m.clip_id).collect::<Vec<_>>(),
            vec![big]
        );
        assert_eq!(carve_added.len(), 1);
        assert!(carve_removed.is_empty());
        assert_eq!(
            starts(&sequencer, 0),
            vec![(1000, 2000), (2000, 2480), (2480, 4000)]
        );
        assert!(starts(&sequencer, 1).is_empty());
    }

    #[test]
    fn same_track_move_over_its_own_old_span_never_carves_itself() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 0, clip_at(0, 960));

        // Shift by less than the clip length: new [480, 1440) overlaps old [0, 960).
        let mut edit = MoveClipEdit::new(&sequencer, 0, id, 0, 480).unwrap();
        let EditResult::ClipMoved {
            carve_updated,
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipMoved");
        };

        assert!(carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty());
        assert_eq!(starts(&sequencer, 0), vec![(480, 1440)]);
        let clip = sequencer.tracks()[0].get_clip_by_id(id).unwrap();
        assert_eq!((clip.region().start(), clip.region().end()), (0, 960));
    }

    #[test]
    fn undo_restores_positions_ids_and_carved_originals() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 1, clip_at(0, 480));
        let big = add(&mut sequencer, 0, clip_at(1000, 3000));

        let mut edit = MoveClipEdit::new(&sequencer, 1, id, 0, 2000).unwrap();
        edit.edit(&mut sequencer);

        let EditResult::ClipUnmoved { from, to, .. } = edit.undo(&mut sequencer) else {
            panic!("expected ClipUnmoved");
        };
        assert_eq!((from.track_idx, from.start_tick), (0, 2000));
        assert_eq!((to.track_idx, to.start_tick, to.clip_id), (1, 0, id));

        assert_eq!(starts(&sequencer, 0), vec![(1000, 4000)]);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), big);
        assert_eq!(starts(&sequencer, 1), vec![(0, 480)]);
        assert_eq!(sequencer.tracks()[1].clips()[0].id(), id);
    }

    #[test]
    fn redo_replays_the_frozen_carve_with_stable_ids() {
        let mut sequencer = test_sequencer();
        let id = add(&mut sequencer, 1, clip_at(0, 480));
        add(&mut sequencer, 0, clip_at(1000, 3000));

        let mut edit = MoveClipEdit::new(&sequencer, 1, id, 0, 2000).unwrap();
        let EditResult::ClipMoved { carve_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipMoved");
        };
        let first_right_id = carve_added[0].clip_id;

        edit.undo(&mut sequencer);
        let EditResult::ClipMoved { carve_added, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipMoved");
        };

        assert_eq!(carve_added[0].clip_id, first_right_id);
        assert_eq!(
            starts(&sequencer, 0),
            vec![(1000, 2000), (2000, 2480), (2480, 4000)]
        );
        assert!(starts(&sequencer, 1).is_empty());
    }

    fn clip_with_open_note() -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), Some(960));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: moving the currently-playing clip mid-note must release the
    /// open note rather than strand it — lifting the clip takes its note-off
    /// with it.
    #[test]
    fn moving_the_playing_clip_releases_its_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);
        let id = add(&mut sequencer, 0, clip_with_open_note());

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);

        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        let mut edit = MoveClipEdit::new(&sequencer, 0, id, 0, 4000).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
