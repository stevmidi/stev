//! The ⌥-drag copy — the arranger band drag released with ⌥ held. Drops a
//! copy of the grabbed block (a whole clip or a marquee rect) shifted by one
//! tick/track delta and leaves the source where it was. Not an edit type of
//! its own: it builds a [`PasteClipsEdit`] of the shifted pieces, as Merge
//! Clips does of the merged ones.

use crate::core::input_event::TimeSelectionRect;

use super::super::super::Sequencer;
use super::super::PasteLead;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// CopyRange
// ---------------------------------------------------------------------------

impl PasteClipsEdit {
    /// ⌥ + the arranger band drag (`460-copy-drag.md`): copy everything
    /// inside `rect` — the `[start, end)` slice of every clip it overlaps on
    /// its tracks, region-windowed with the full event list kept, as `⌘C`
    /// makes it — to `start + delta_ticks` on `track + delta_tracks`. A
    /// whole-clip drag sends the clip's own span and track as the rect, so
    /// one paste serves both kinds of drag: a copy keeps no clip id, unlike a
    /// move.
    ///
    /// The slice is taken before anything is carved, so the copy is always
    /// the whole of what was grabbed; the paste then carves the destination
    /// Ableton-style, the source included where the copy overlaps it (the
    /// copy wins). The lead is `PasteLead::Range` on the shifted rect: the
    /// selection follows the copy as it follows a moved block, so ⌥-drags
    /// chain.
    ///
    /// `None` for the refusals a marquee move has
    /// (`TimeSelectionRect::shifted_on_lanes`: no tick width, a zero delta —
    /// a copy on its own source is pointless — before tick 0 or off the
    /// lanes) or when no clip on the rect's tracks overlaps it: nothing then
    /// enters the undo record.
    pub(crate) fn copying(
        sequencer: &Sequencer,
        rect: TimeSelectionRect,
        delta_ticks: i32,
        delta_tracks: i32,
    ) -> Option<Self> {
        let to = rect.shifted_on_lanes(delta_ticks, delta_tracks, sequencer.tracks().len())?;
        let clipboard = sequencer.copy_range_to_clipboard(
            (rect.track_start, rect.track_end),
            rect.start,
            rect.end,
        )?;
        // The pieces are fresh (detached, ids of their own) and normalized
        // to `rect.start` on their absolute source tracks.
        let targets = clipboard
            .into_clips()
            .into_iter()
            .map(|mut piece| {
                piece
                    .clip
                    .set_start_tick(to.start + piece.clip.start_tick());
                let track_idx = (piece.track_idx as i32 + delta_tracks) as usize;
                (track_idx, piece.clip)
            })
            .collect();
        Self::from_targets(sequencer, targets, PasteLead::Range(to))
    }
}

#[cfg(test)]
mod tests {

    use uuid::Uuid;

    use crate::core::sequencer::test_support::{
        clip_at, clip_with_marker_notes, rect, spans, test_sequencer,
    };

    use super::super::super::EditResult;
    use super::*;

    /// Runs the copy, returning its `pasted` ids and the rect its lead
    /// selects.
    fn run(edit: &mut PasteClipsEdit, sequencer: &mut Sequencer) -> (Vec<Uuid>, TimeSelectionRect) {
        let EditResult::ClipsPasted {
            pasted,
            lead: PasteLead::Range(to),
            ..
        } = edit.edit(sequencer)
        else {
            panic!("expected ClipsPasted with a range lead");
        };
        (pasted.iter().map(|m| m.clip_id).collect(), to)
    }

    #[test]
    fn is_none_when_the_rect_holds_no_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 4000));

        assert!(PasteClipsEdit::copying(&sequencer, rect(5000, 6000, 0, 0), 500, 0).is_none());
        assert!(PasteClipsEdit::copying(&sequencer, rect(1000, 2000, 1, 1), 500, 0).is_none());
        // The shift's own refusals are `shifted_on_lanes`'s, tested there.
        assert!(PasteClipsEdit::copying(&sequencer, rect(1000, 2000, 0, 0), 0, 0).is_none());
    }

    #[test]
    fn copies_a_whole_clip_later_on_its_own_track() {
        let mut sequencer = test_sequencer();
        let source = clip_with_marker_notes(0, 1000);
        let source_id = source.id();
        sequencer.tracks_mut()[0].add_clip(&source);

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 2000, 0).unwrap();
        let (pasted, to) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 1000), (2000, 3000)]);
        assert_eq!(to, rect(2000, 3000, 0, 0));
        assert_eq!(pasted.len(), 1);
        assert_ne!(pasted[0], source_id, "a copy has an id of its own");
        let copy = sequencer.tracks()[0].get_clip_by_id(pasted[0]).unwrap();
        assert_eq!(copy.events().len(), 2);
    }

    #[test]
    fn copies_a_whole_clip_onto_another_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1000));

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 500, 2).unwrap();
        let (_, to) = run(&mut edit, &mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
        assert_eq!(spans(&sequencer, 2), vec![(500, 1500)]);
        assert_eq!(to, rect(500, 1500, 2, 2));
    }

    /// A marquee over part of a clip copies just that piece, region-windowed
    /// with the full event list kept, and leaves the source whole.
    #[test]
    fn a_partial_marquee_copies_only_its_piece() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 4000));

        let mut edit =
            PasteClipsEdit::copying(&sequencer, rect(1000, 2000, 0, 0), 4000, 1).unwrap();
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

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 2000, 0, 2), 4000, 1).unwrap();
        let (pasted, to) = run(&mut edit, &mut sequencer);

        assert_eq!(pasted.len(), 2);
        assert_eq!(spans(&sequencer, 1), vec![(4000, 5000)]);
        assert_eq!(spans(&sequencer, 3), vec![(4500, 5500)]);
        assert_eq!(spans(&sequencer, 0), vec![(0, 1000)]);
        assert_eq!(spans(&sequencer, 2), vec![(500, 1500)]);
        assert_eq!(to, rect(4000, 6000, 1, 3));
    }

    /// Dropped over part of itself, the copy is still the whole clip and
    /// trims the source where it lands: the copy wins.
    #[test]
    fn a_copy_over_its_own_source_trims_the_source_and_wins() {
        let mut sequencer = test_sequencer();
        let source = clip_at(0, 1000);
        let source_id = source.id();
        sequencer.tracks_mut()[0].add_clip(&source);

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 500, 0).unwrap();
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

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 1500, 1).unwrap();
        let EditResult::ClipsPasted {
            carve_updated,
            carve_added,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipsPasted");
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

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 2000, 1).unwrap();
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

        let mut edit = PasteClipsEdit::copying(&sequencer, rect(0, 1000, 0, 0), 2000, 0).unwrap();
        let (pasted, _) = run(&mut edit, &mut sequencer);
        assert!(
            sequencer.tracks()[0]
                .get_clip_by_id(pasted[0])
                .unwrap()
                .is_muted()
        );
    }
}
