//! Undoable ⌥-drag copy — the arranger band drag released with ⌥ held. Drops
//! a copy of the grabbed block (a whole clip or a marquee rect) shifted by
//! one tick/track delta and leaves the source where it was. A thin newtype
//! over [`PasteClipsEdit`], in the shape of `DuplicateClipsEdit`.

use crate::core::input_event::TimeSelectionRect;

use super::super::super::Sequencer;
use super::super::{EditResult, PasteLead};
use super::detached_clone;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// CopyRange
// ---------------------------------------------------------------------------

/// ⌥ + the arranger band drag (`460-copy-drag.md`): copy everything inside
/// `rect` — the `[start, end)` slice of every clip it overlaps on its tracks,
/// region-windowed with the full event list kept, as `⌘C` makes it — to
/// `start + delta_ticks` on `track + delta_tracks`. A whole-clip drag sends
/// the clip's own span and track as the rect, so one edit serves both kinds
/// of drag: a copy keeps no clip id, unlike a move.
///
/// The slice is taken before anything is carved, so the copy is always the
/// whole of what was grabbed; the destination is then carved Ableton-style
/// by the paste, the source included where the copy overlaps it (the copy
/// wins). All placement, carving, freezing for redo and undo are
/// `PasteClipsEdit`'s; this only shifts the pieces and reports where the
/// copy sits.
pub(crate) struct CopyRangeEdit {
    /// The paste that does the real work, its pieces already placed.
    inner: PasteClipsEdit,
    /// Where the copy sits: the grabbed rect shifted by the drag, frozen at
    /// construction.
    copy_rect: TimeSelectionRect,
}

impl CopyRangeEdit {
    /// Builds the edit from the drag's frozen release delta. `None` when the
    /// rect has no tick width, the delta is zero (a copy on its own source is
    /// pointless), the shift would push the block before tick 0 or off the
    /// lanes, or no clip on the rect's tracks overlaps it — nothing then
    /// enters the undo record. The same refusals as `MoveRangeEdit::new`.
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

        let clipboard = sequencer.copy_range_to_clipboard(
            (rect.track_start, rect.track_end),
            rect.start,
            rect.end,
        )?;
        // Pieces are normalized to `rect.start`, on their absolute source
        // tracks.
        let anchor = rect.start + delta_ticks;
        let targets = clipboard
            .clips()
            .iter()
            .map(|piece| {
                let mut clip = detached_clone(&piece.clip);
                clip.generate_new_id();
                clip.set_start_tick(anchor + piece.clip.start_tick());
                let track_idx = (piece.track_idx as i32 + delta_tracks) as usize;
                (track_idx, clip)
            })
            .collect();
        let inner = PasteClipsEdit::from_targets(sequencer, targets, PasteLead::CursorRule)?;

        Some(Self {
            inner,
            copy_rect: TimeSelectionRect {
                start: rect.start + delta_ticks,
                end: rect.end + delta_ticks,
                track_start: (rect.track_start as i32 + delta_tracks) as usize,
                track_end: (rect.track_end as i32 + delta_tracks) as usize,
            },
        })
    }

    /// Delegates to the inner paste, then rewraps its result as
    /// [`EditResult::RangeCopied`] carrying the copy's rect, so the
    /// selection follows the copy and drags chain.
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        match self.inner.edit(sequencer) {
            EditResult::ClipsPasted {
                carve_updated,
                carve_added,
                carve_removed,
                pasted,
                lead: _,
            } => EditResult::RangeCopied {
                carve_updated,
                carve_added,
                carve_removed,
                pasted,
                new_selection: self.copy_rect,
            },
            other => other,
        }
    }

    /// The inner paste's undo, as it is: [`EditResult::ClipsUnpasted`]. The
    /// selection comes back with the undo step (`UndoStep`).
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.inner.undo(sequencer)
    }
}

#[cfg(test)]
mod tests {

    use uuid::Uuid;

    use crate::models::clip::Clip;
    use crate::models::event::Event;

    use crate::core::sequencer::test_support::{clip_at, rect, test_sequencer};

    use super::*;

    /// A clip with a note every 500 ticks, so a piece can be checked to keep
    /// the full event list behind its region window.
    fn clip_with_marker_notes(start_tick: i32, length: i32) -> Clip {
        let mut clip = clip_at(start_tick, length);
        for offset in (0..length).step_by(500) {
            clip.add_event(Event::new(offset, 0, vec![0x90, 60, 100]));
        }
        clip
    }

    fn spans(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    /// Runs the copy, returning its `pasted` ids and `new_selection`.
    fn run(edit: &mut CopyRangeEdit, sequencer: &mut Sequencer) -> (Vec<Uuid>, TimeSelectionRect) {
        let EditResult::RangeCopied {
            pasted,
            new_selection,
            ..
        } = edit.edit(sequencer)
        else {
            panic!("expected RangeCopied");
        };
        (pasted.iter().map(|m| m.clip_id).collect(), new_selection)
    }

    #[test]
    fn is_none_for_a_no_op_empty_or_off_lane_copy() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 4000));
        let track_count = sequencer.tracks().len();

        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 0, 0).is_none()); // on itself
        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 1000, 0, 0), 500, 0).is_none()); // no width
        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), -1500, 0).is_none()); // before 0
        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 0, -1).is_none()); // above lanes
        let past = track_count as i32;
        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 0, past).is_none()); // below
        assert!(CopyRangeEdit::new(&sequencer, rect(5000, 6000, 0, 0), 500, 0).is_none()); // nothing there
        assert!(CopyRangeEdit::new(&sequencer, rect(1000, 2000, 1, 1), 500, 0).is_none()); // other track
    }

    #[test]
    fn copies_a_whole_clip_later_on_its_own_track() {
        let mut sequencer = test_sequencer();
        let source = clip_with_marker_notes(0, 1000);
        let source_id = source.id();
        sequencer.tracks_mut()[0].add_clip(&source);

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 2000, 0).unwrap();
        let (pasted, new_selection) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 1000), (2000, 3000)]);
        assert_eq!(new_selection, rect(2000, 3000, 0, 0));
        assert_eq!(pasted.len(), 1);
        assert_ne!(pasted[0], source_id, "a copy has an id of its own");
        let copy = sequencer.tracks()[0].get_clip_by_id(pasted[0]).unwrap();
        assert_eq!(copy.events().len(), 2);
    }

    #[test]
    fn copies_a_whole_clip_onto_another_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1000));

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 500, 2).unwrap();
        let (_, new_selection) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
        assert_eq!(spans(&sequencer, 2), vec![(500, 1500)]);
        assert_eq!(new_selection, rect(500, 1500, 2, 2));
    }

    /// A marquee over part of a clip copies just that piece, region-windowed
    /// with the full event list kept, and leaves the source whole.
    #[test]
    fn a_partial_marquee_copies_only_its_piece() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 4000));

        let mut edit = CopyRangeEdit::new(&sequencer, rect(1000, 2000, 0, 0), 4000, 1).unwrap();
        let (pasted, _) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 4000)]);
        assert_eq!(spans(&sequencer, 1), vec![(5000, 6000)]);
        let piece = sequencer.tracks()[1].get_clip_by_id(pasted[0]).unwrap();
        assert_eq!((piece.region().start(), piece.region().end()), (1000, 2000));
        assert_eq!(piece.events().len(), 8);
    }

    #[test]
    fn a_multi_track_block_keeps_its_track_spacing() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1000));
        sequencer.tracks_mut()[2].add_clip(&clip_at(500, 1000));

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 2000, 0, 2), 4000, 1).unwrap();
        let (pasted, new_selection) = run(&mut edit, &mut sequencer);

        assert_eq!(pasted.len(), 2);
        assert_eq!(spans(&sequencer, 1), vec![(4000, 5000)]);
        assert_eq!(spans(&sequencer, 3), vec![(4500, 5500)]);
        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
        assert_eq!(spans(&sequencer, 2), vec![(500, 1500)]);
        assert_eq!(new_selection, rect(4000, 6000, 1, 3));
    }

    /// Dropped over part of itself, the copy is still the whole clip and
    /// trims the source where it lands: the copy wins.
    #[test]
    fn a_copy_over_its_own_source_trims_the_source_and_wins() {
        let mut sequencer = test_sequencer();
        let source = clip_at(0, 1000);
        let source_id = source.id();
        sequencer.tracks_mut()[0].add_clip(&source);

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 500, 0).unwrap();
        let (pasted, _) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 500), (500, 1500)]);
        let trimmed = sequencer.tracks()[0].get_clip_by_id(source_id).unwrap();
        assert_eq!(trimmed.end_tick(), 500);
        let copy = sequencer.tracks()[0].get_clip_by_id(pasted[0]).unwrap();
        assert_eq!((copy.start_tick(), copy.end_tick()), (500, 1500));

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
    }

    #[test]
    fn landing_on_another_clip_carves_it_and_undo_puts_everything_back() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1000));
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 4000));

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 1500, 1).unwrap();
        let EditResult::RangeCopied {
            carve_updated,
            carve_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected RangeCopied");
        };

        assert!(!carve_updated.is_empty() && !carve_added.is_empty());
        assert_eq!(
            spans(&sequencer, 1),
            vec![(0, 1500), (1500, 2500), (2500, 4000)]
        );

        let EditResult::ClipsUnpasted { unpasted, .. } = edit.undo(&mut sequencer) else {
            panic!("expected ClipsUnpasted");
        };
        assert_eq!(unpasted.len(), 1);
        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
        assert_eq!(spans(&sequencer, 1), vec![(0, 4000)]);
    }

    #[test]
    fn redo_lands_the_same_ids() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1000));

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 2000, 1).unwrap();
        let (first, _) = run(&mut edit, &mut sequencer);
        edit.undo(&mut sequencer);
        assert!(spans(&sequencer, 1).is_empty());

        let (redone, _) = run(&mut edit, &mut sequencer);
        assert_eq!(first, redone, "redo must reuse the frozen clip ids");
        assert_eq!(spans(&sequencer, 1), vec![(2000, 3000)]);
    }

    #[test]
    fn a_muted_clip_copies_muted() {
        let mut sequencer = test_sequencer();
        let mut source = clip_at(0, 1000);
        source.set_muted(true);
        sequencer.tracks_mut()[0].add_clip(&source);

        let mut edit = CopyRangeEdit::new(&sequencer, rect(0, 1000, 0, 0), 2000, 0).unwrap();
        let (pasted, _) = run(&mut edit, &mut sequencer);
        assert!(
            sequencer.tracks()[0]
                .get_clip_by_id(pasted[0])
                .unwrap()
                .is_muted()
        );
    }
}
