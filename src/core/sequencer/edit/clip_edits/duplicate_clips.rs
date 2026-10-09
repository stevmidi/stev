//! Undoable plain `⌘/Ctrl+D` (Duplicate Clips) — paste a copy of the marquee
//! rectangle flush after itself, carving whatever it lands on. A thin
//! anchor-picking newtype over [`PasteClipsEdit`].

use crate::core::input_event::TimeSelectionRect;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// DuplicateClips
// ---------------------------------------------------------------------------

/// Plain `⌘/Ctrl+D` in the Arranger — duplicate the marquee's *content*, not
/// its time. Builds a throwaway clipboard of the `[start, end)` slice of every
/// clip on the marqueed tracks (`Sequencer::copy_range_to_clipboard`, the same
/// region-windowed, phase-locked, region-detached pieces plain `⌘C` produces)
/// and pastes it at `end`, so each piece lands at `original + width` on its
/// own track. Nothing is shifted: whatever already sits in the destination
/// span on those tracks is carved out Ableton-style by `PasteClipsEdit`'s
/// per-target `DeleteInRangeEdit`, so the copy wins. Tracks outside the
/// marquee are untouched — contrast `DuplicateTimeEdit` (`Shift+⌘/Ctrl+D`),
/// which opens a gap across every track instead of overwriting.
///
/// All placement, destination carve-out, freeze-for-redo and undo behavior is
/// `PasteClipsEdit`'s; this only picks the anchor and reports the advanced
/// marquee. Same worked-example reuse shape as `InsertSilenceEdit` /
/// `DeleteInRangeEdit` composing `SplitClipsEdit`.
pub(crate) struct DuplicateClipsEdit {
    /// The paste that does the real work, its anchor already chosen.
    inner: PasteClipsEdit,
    /// The marquee this was built from, frozen at construction — never
    /// re-read from the (view-local) time selection on redo. `edit()` reports
    /// it slid right by its own width, `undo()` reports it as-is.
    rect: TimeSelectionRect,
}

impl DuplicateClipsEdit {
    /// Builds the edit from the marquee rectangle. `None` if the tick range is
    /// empty/backwards or overlaps no clip on tracks
    /// `track_start..=track_end`.
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

        let clipboard = sequencer.copy_range_to_clipboard((track_start, track_end), start, end)?;
        // Pieces are normalized to `start`; anchoring at `end` places each at
        // `original_pos + (end - start)`.
        let inner = PasteClipsEdit::from_clipboard_at(sequencer, &clipboard, end, None)?;

        Some(Self {
            inner,
            rect: TimeSelectionRect {
                start,
                end,
                track_start,
                track_end,
            },
        })
    }

    /// The marquee slid right by its own width — where the copy now sits.
    fn advanced_rect(&self) -> TimeSelectionRect {
        let width = self.rect.end - self.rect.start;
        TimeSelectionRect {
            start: self.rect.end,
            end: self.rect.end + width,
            ..self.rect
        }
    }

    /// Delegates to the inner paste, then rewraps its result as
    /// [`EditResult::ClipsDuplicated`] carrying the advanced marquee so
    /// repeated presses chain down the timeline.
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
            } => EditResult::ClipsDuplicated {
                carve_updated,
                carve_added,
                carve_removed,
                pasted,
                new_selection: self.advanced_rect(),
            },
            other => other,
        }
    }

    /// Delegates to the inner paste's undo, then rewraps its result as
    /// [`EditResult::ClipsUnduplicated`] carrying the original marquee.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        match self.inner.undo(sequencer) {
            EditResult::ClipsUnpasted {
                carve_updated,
                carve_added,
                carve_removed,
                unpasted,
                selected_track_idx,
                selected_clip_id,
            } => EditResult::ClipsUnduplicated {
                carve_updated,
                carve_added,
                carve_removed,
                unpasted,
                selected_track_idx,
                selected_clip_id,
                restored_selection: self.rect,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

    use super::*;

    fn starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    fn region(sequencer: &Sequencer) -> (i32, i32) {
        (sequencer.region_start(), sequencer.region_end())
    }

    #[test]
    fn is_none_on_empty_or_backwards_range() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        assert!(DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 100, 100).is_none());
        assert!(DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 200, 50).is_none());
    }

    #[test]
    fn is_none_when_marquee_overlaps_no_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(500, 100)); // outside the tick range
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 100)); // outside the track span
        assert!(DuplicateClipsEdit::from_track_span(&sequencer, 0, 1, 0, 100).is_none());
    }

    /// The defining difference from Duplicate Time: nothing after the copy
    /// moves. The `[200,300)` clip stays put on track 0, and track 1 (outside
    /// the marquee) is untouched even though it has a clip in the tick range.
    #[test]
    fn pastes_flush_after_the_marquee_without_shifting_anything() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 100)); // outside the track span

        let mut edit = DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 0, 100).unwrap();
        let EditResult::ClipsDuplicated {
            pasted,
            carve_updated,
            carve_added,
            carve_removed,
            new_selection,
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipsDuplicated");
        };

        assert_eq!(pasted.len(), 1);
        assert!(carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty());
        assert_eq!(
            new_selection,
            TimeSelectionRect {
                start: 100,
                end: 200,
                track_start: 0,
                track_end: 0,
            }
        );
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (200, 300)]
        );
        assert_eq!(starts(&sequencer, 1), vec![(0, 100)]);

        let EditResult::ClipsUnduplicated {
            unpasted,
            restored_selection,
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("expected ClipsUnduplicated");
        };
        assert_eq!(unpasted.len(), 1);
        assert_eq!(
            restored_selection,
            TimeSelectionRect {
                start: 0,
                end: 100,
                track_start: 0,
                track_end: 0,
            }
        );
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (200, 300)]);
    }

    /// The copy wins: existing material in the destination span is carved
    /// out (here a clip fully inside it is removed, a straddler is trimmed),
    /// and undo puts it all back.
    #[test]
    fn copy_overwrites_existing_destination_material_and_undo_restores_it() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(120, 50)); // fully inside [100,200) — removed
        sequencer.tracks_mut()[0].add_clip(&clip_at(180, 100)); // straddles 200 — trimmed to [200,280)

        let mut edit = DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 0, 100).unwrap();
        let EditResult::ClipsDuplicated {
            carve_added,
            carve_removed,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipsDuplicated");
        };

        // Unlike Duplicate Time, the carve buckets are real (exact bucket
        // shapes are `DeleteInRangeEdit`'s business — see its own tests).
        assert!(!carve_removed.is_empty());
        assert!(!carve_added.is_empty());
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (100, 200), (200, 280)]
        );

        edit.undo(&mut sequencer);
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (120, 170), (180, 280)]
        );
    }

    /// Only the marqueed tracks are duplicated, each onto itself.
    #[test]
    fn duplicates_every_marqueed_track_onto_itself() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100)); // outside the span
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[2].add_clip(&clip_at(50, 100)); // straddles `end`

        let mut edit = DuplicateClipsEdit::from_track_span(&sequencer, 1, 2, 0, 100).unwrap();
        let EditResult::ClipsDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsDuplicated");
        };

        assert_eq!(pasted.len(), 2);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 100), (100, 200)]);
        // Track 2's [50,150) slice inside the marquee is [50,100); its copy
        // lands at [150,200), carving that span out of the original.
        assert_eq!(starts(&sequencer, 2), vec![(50, 150), (150, 200)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 1), vec![(0, 100)]);
        assert_eq!(starts(&sequencer, 2), vec![(50, 150)]);
    }

    #[test]
    fn redo_reuses_frozen_ids() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));

        let mut edit = DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 0, 100).unwrap();
        let EditResult::ClipsDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsDuplicated");
        };
        let first_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100)]);

        let EditResult::ClipsDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsDuplicated on redo");
        };
        let redo_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first_ids, redo_ids, "redo must reuse the frozen clip ids");
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (100, 200)]);
    }

    /// Unlike Duplicate Time, no time is inserted, so the loop region never
    /// moves — even when it sits inside/after the duplicated span.
    #[test]
    fn loop_region_is_never_touched() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.set_global_region(20, 60);

        let mut edit = DuplicateClipsEdit::from_track_span(&sequencer, 0, 0, 0, 100).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (20, 60));

        edit.undo(&mut sequencer);
        assert_eq!(region(&sequencer), (20, 60));
    }
}
