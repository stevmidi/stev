//! Undoable `Shift+⌘/Ctrl+D` (Duplicate Time) and `Shift+⌘/Ctrl+V` (Paste
//! Time) — insert a copy and push everything after it right. Composed from
//! [`InsertSilenceEdit`] + [`PasteClipsEdit`]. Plain `⌘/Ctrl+D` is the
//! non-shifting `DuplicateClipsEdit` next door.

use crate::core::input_event::TimeSelectionRect;

use super::super::super::Sequencer;
use super::super::super::clipboard::ClipClipboard;
use super::super::EditResult;
use super::insert_silence::InsertSilenceEdit;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// DuplicateTime
// ---------------------------------------------------------------------------

/// Ableton-style "Duplicate Time" (`Shift+⌘/Ctrl+D`) and "Paste Time"
/// (`Shift+⌘/Ctrl+V`): a paste that *inserts* time. The space for the copy
/// is opened by pushing every clip at or after the anchor to the right by
/// the copy's width, on every track, so nothing is destroyed. Always global —
/// a partial shift would desync the tracks, the same alignment concern
/// `InsertSilenceEdit` guards against. They differ only in what they paste
/// and where:
///
/// - **Duplicate Time** ([`from_time_range`](Self::from_time_range)) pastes
///   a throwaway every-track copy of `[start, end)` at `end`, and moves the
///   marquee on to the copy so presses chain. The marquee-scoped,
///   overwriting sibling is `DuplicateClipsEdit`.
/// - **Paste Time** ([`pasting`](Self::pasting)) is `⌘/Ctrl+V`'s paste
///   exactly (the session clipboard at the cursor, on the tracks `⌘V` would
///   use), inserting `ClipClipboard::span` ticks first — the copied range's
///   length, so a copy ending in silence keeps it. The marquee stays.
/// - **A performance capture** ([`capturing`](Self::capturing), `/` on the
///   lane) is Paste Time with the lane run's render in place of the session
///   clipboard (`archive/520-performance-capture.md` decision 1).
///
/// Each is the composition of the two edits that already do each
/// half — same worked-example reuse shape as `InsertSilenceEdit`/
/// `DeleteInRangeEdit` (composing `SplitClipsEdit`):
///
/// 1. `InsertSilenceEdit` at the anchor opens a `width` tick gap on every
///    track (splitting any clip straddling the anchor, shifting every clip at
///    or after it right by the width).
/// 2. `PasteClipsEdit` drops the copy into the freed span. Every piece lies
///    within `[0, width)` of the anchor, so the paste's carve is a no-op.
///
/// `edit()` runs them in that order; `undo()` reverses it (un-paste, then
/// remove the silence). Everything is frozen at construction — never re-read
/// from the (view-local) time selection or the cursor on redo.
///
/// The markers at or after the anchor move on with the clips, through the
/// `InsertSilenceEdit`; none is copied (`600-markers.md` decision 9).
///
/// The loop region is *moved* (not resized) if it starts at or after
/// `region_from` — the duplicated span's start, or the cursor for a paste:
/// it slides right by the width so it keeps the same position relative to
/// the material that moved. A region starting earlier is left alone.
pub(crate) struct DuplicateTimeEdit {
    /// The insertion point: the duplicated span's end, or the cursor.
    anchor: i32,
    /// Gap width, in ticks.
    width: i32,
    /// A loop region starting at or after this tick slides along.
    region_from: i32,
    /// The tracks the marquee covers once it moves on to the copy: the
    /// marquee's as the sequencer mirrors it when the command arrives, else
    /// the selected track. The edit itself spans every track, so its command
    /// names none; frozen here so a redo moves the marquee onto the same
    /// tracks. `None` for Paste Time, which leaves the marquee alone.
    marquee_tracks: Option<(usize, usize)>,
    /// Opens the gap at `anchor`. `None` when `InsertSilenceEdit::from_time_range`
    /// finds nothing at or after `anchor` to move — the paste still happens,
    /// into the already-empty span.
    insert: Option<InsertSilenceEdit>,
    /// Places the copy at `anchor`. Its absolute target positions are frozen
    /// at construction and are unaffected by the shift `insert` performs.
    paste: PasteClipsEdit,
    /// Whether the loop region was slid right by `width`. Frozen on the
    /// first `edit()` so redo replays the same decision; the region is not
    /// resized, only moved. See `Sequencer::shift_region`.
    region_shifted: Option<bool>,
}

impl DuplicateTimeEdit {
    /// Duplicate Time, from the Arranger's time selection bounds. `None` if
    /// the range is empty/backwards or overlaps no clip on any track.
    pub(crate) fn from_time_range(sequencer: &Sequencer, start: i32, end: i32) -> Option<Self> {
        if end <= start {
            return None;
        }

        // Frozen pre-insert: the copied pieces and their absolute destinations.
        let clipboard = sequencer.clipboard_snapshot(start, end)?;
        let paste = PasteClipsEdit::from_clipboard_at(sequencer, &clipboard, end, None)?;

        let marquee_tracks = sequencer
            .marquee()
            .map(|rect| (rect.track_start, rect.track_end))
            .or_else(|| sequencer.selected_track_index().map(|idx| (idx, idx)))
            .unwrap_or_default();

        Some(Self::inserting(
            sequencer,
            end,
            end - start,
            start,
            Some(marquee_tracks),
            paste,
        ))
    }

    /// Paste Time, from the session clipboard, the cursor and the selected
    /// track. `None` whenever `⌘/Ctrl+V` would paste nothing (an empty
    /// clipboard, a scoped copy with no selected track).
    pub(crate) fn pasting(sequencer: &Sequencer) -> Option<Self> {
        let clipboard = sequencer.clip_clipboard()?;
        Self::paste_time(sequencer, clipboard, sequencer.selected_track_index())
    }

    /// `/` on the performance lane (`archive/520-performance-capture.md` decision
    /// 1): Paste Time's edit with the run's render
    /// (`Sequencer::render_performance`) in place of the session clipboard,
    /// on the tracks it was rendered from (it has no track anchor). The
    /// session clipboard is untouched and the marquee stays. `None` when the
    /// render places nothing.
    pub(crate) fn capturing(sequencer: &Sequencer, render: &ClipClipboard) -> Option<Self> {
        Self::paste_time(sequencer, render, None)
    }

    /// Paste Time's edit for `clipboard`: pasted at the cursor, aimed at
    /// `target_track` as `⌘V` aims it, after inserting its span.
    fn paste_time(
        sequencer: &Sequencer,
        clipboard: &ClipClipboard,
        target_track: Option<usize>,
    ) -> Option<Self> {
        let anchor = sequencer.cursor_tick();
        let paste = PasteClipsEdit::from_clipboard_at(sequencer, clipboard, anchor, target_track)?;
        Some(Self::inserting(
            sequencer,
            anchor,
            clipboard.span(),
            anchor,
            None,
            paste,
        ))
    }

    /// The shared tail of the constructors: the gap of `width` at `anchor`.
    fn inserting(
        sequencer: &Sequencer,
        anchor: i32,
        width: i32,
        region_from: i32,
        marquee_tracks: Option<(usize, usize)>,
        paste: PasteClipsEdit,
    ) -> Self {
        // `from_time_range(s, a, b)` inserts `b - a` ticks at `a`.
        let insert = InsertSilenceEdit::from_time_range(sequencer, anchor, anchor + width);
        Self {
            anchor,
            width,
            region_from,
            marquee_tracks,
            insert,
            paste,
            region_shifted: None,
        }
    }

    /// Inserts the silence, then pastes the copy into the freed span, then
    /// slides the loop region if needed. Returns [`EditResult::TimeDuplicated`].
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

        // Slide the loop region along with the inserted time. Frozen on the
        // first call so redo replays the same decision. Not resized — only
        // moved.
        let region_from = self.region_from;
        let region_shifted = *self
            .region_shifted
            .get_or_insert_with(|| sequencer.region_start() >= region_from);
        if region_shifted {
            sequencer.shift_region(self.width);
        }

        EditResult::TimeDuplicated {
            shifted,
            split_updated,
            split_added,
            pasted,
            new_selection: self.marquee_tracks.map(|(track_start, track_end)| {
                TimeSelectionRect::spanning(
                    self.anchor,
                    self.anchor + self.width,
                    track_start,
                    track_end,
                )
            }),
        }
    }

    /// Un-pastes the copy, removes the silence, and slides the loop region
    /// back. Returns [`EditResult::TimeUnduplicated`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        if self.region_shifted == Some(true) {
            sequencer.shift_region(-self.width);
        }

        let mut unpasted = Vec::new();

        if let EditResult::ClipsUnpasted { unpasted: u, .. } = self.paste.undo(sequencer) {
            unpasted = u;
        }

        let mut shifted = Vec::new();
        let mut split_updated = Vec::new();
        let mut split_removed = Vec::new();

        if let Some(insert) = self.insert.as_mut()
            && let EditResult::SilenceRemoved {
                shifted: s,
                split_updated: su,
                split_removed: sr,
                ..
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
        }
    }
}

#[cfg(test)]
mod tests {

    use uuid::Uuid;

    use crate::core::sequencer::test_support::{clip_at, spans, test_sequencer};

    use super::*;

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
        assert_eq!(
            new_selection,
            Some(TimeSelectionRect::spanning(100, 200, 0, 0))
        );
        // Unlike plain `⌘D` (`DuplicateClipsEdit`), the [200,300) clip is
        // pushed to [300,400) rather than overwritten.
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (100, 200), (300, 400)]);
        assert_eq!(spans(&sequencer, 1), vec![(0, 100), (100, 200)]);

        let EditResult::TimeUnduplicated { .. } = edit.undo(&mut sequencer) else {
            panic!("expected TimeUnduplicated");
        };
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (200, 300)]);
        assert_eq!(spans(&sequencer, 1), vec![(0, 100)]);
    }

    /// Duplicate Time moves the markers at or after the insertion point
    /// on by the copy's length and copies none from the source span: a
    /// number is one slot (`600-markers.md` decision 9). The undo puts them
    /// back.
    #[test]
    fn later_markers_move_on_and_none_is_copied() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.markers_mut().set(1, 0);
        sequencer.markers_mut().set(2, 100);
        sequencer.markers_mut().set(3, 250);
        let placed = |sequencer: &Sequencer| -> Vec<(u8, i32)> {
            sequencer
                .markers()
                .as_slice()
                .iter()
                .map(|marker| (marker.number, marker.tick))
                .collect()
        };

        let mut edit = DuplicateTimeEdit::from_time_range(&sequencer, 0, 100).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(placed(&sequencer), vec![(1, 0), (2, 200), (3, 350)]);

        edit.undo(&mut sequencer);
        assert_eq!(placed(&sequencer), vec![(1, 0), (2, 100), (3, 250)]);
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
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (100, 200), (200, 500)]);

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, 400)]);
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
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (100, 200)]);

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, 100)]);
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
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (100, 200), (500, 600)]);
        assert_eq!(spans(&sequencer, 2), vec![(700, 800)]);

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (400, 500)]);
        assert_eq!(spans(&sequencer, 2), vec![(600, 700)]);
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
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (200, 300)]);

        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated on redo");
        };
        let redo_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first_ids, redo_ids, "redo must reuse the frozen clip ids");
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (100, 200), (300, 400)]);
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

    #[test]
    fn paste_time_is_none_with_an_empty_clipboard() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        assert!(DuplicateTimeEdit::pasting(&sequencer).is_none());
    }

    #[test]
    fn paste_time_of_a_scoped_copy_with_no_selected_track_pastes_nothing() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect::spanning(0, 100, 0, 0));
        sequencer.select_track(None);
        assert!(DuplicateTimeEdit::pasting(&sequencer).is_none());
    }

    /// An all-track copy (`⇧⌘C`) lands on its source tracks at the cursor,
    /// and everything from the cursor on moves right on every track.
    #[test]
    fn paste_time_of_an_all_track_copy_inserts_on_every_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(500, 100));
        sequencer.tracks_mut()[2].add_clip(&clip_at(400, 100)); // not copied, moves
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(400);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        let EditResult::TimeDuplicated {
            shifted, pasted, ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimeDuplicated");
        };

        assert_eq!(pasted.len(), 1);
        assert_eq!(shifted.len(), 2);
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (400, 500)]);
        assert_eq!(spans(&sequencer, 1), vec![(600, 700)]);
        assert_eq!(spans(&sequencer, 2), vec![(500, 600)]);

        let EditResult::TimeUnduplicated { .. } = edit.undo(&mut sequencer) else {
            panic!("expected TimeUnduplicated");
        };
        assert_eq!(spans(&sequencer, 0), vec![(0, 100)]);
        assert_eq!(spans(&sequencer, 1), vec![(500, 600)]);
        assert_eq!(spans(&sequencer, 2), vec![(400, 500)]);
    }

    /// A scoped copy (`⌘C`) lands on the selected track in the marquee's
    /// shape, as `⌘V` does, but the gap still opens on every track.
    #[test]
    fn paste_time_of_a_scoped_copy_lands_on_the_selected_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(200, 100));
        sequencer.tracks_mut()[3].add_clip(&clip_at(200, 100));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect::spanning(0, 100, 0, 0));
        select(&mut sequencer, 1, None);
        sequencer.set_cursor_tick(200);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 100)]);
        assert_eq!(spans(&sequencer, 1), vec![(200, 300), (300, 400)]);
        assert_eq!(spans(&sequencer, 3), vec![(300, 400)]);
    }

    /// The gap is the copied length, not the pieces' extent: an empty
    /// stretch at the end of the copy is inserted too.
    #[test]
    fn paste_time_inserts_the_copied_length_including_trailing_silence() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(1000, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 400);
        sequencer.set_cursor_tick(1000);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(
            spans(&sequencer, 0),
            vec![(0, 100), (1000, 1100), (1400, 1500)]
        );
    }

    #[test]
    fn paste_time_splits_a_clip_straddling_the_cursor() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 400));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (200, 300)]);
        assert_eq!(spans(&sequencer, 1), vec![(0, 200), (200, 300), (300, 500)]);

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 1), vec![(0, 400)]);
    }

    #[test]
    fn paste_time_redo_reuses_the_frozen_clip_ids() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated");
        };
        let first: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        edit.undo(&mut sequencer);
        let EditResult::TimeDuplicated { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated on redo");
        };
        let redo: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first, redo);
        assert_eq!(spans(&sequencer, 0), vec![(0, 100), (200, 300), (300, 400)]);
    }

    #[test]
    fn paste_time_slides_a_loop_region_at_or_after_the_cursor() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);
        sequencer.set_global_region(200, 600);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (300, 700));

        edit.undo(&mut sequencer);
        assert_eq!(region(&sequencer), (200, 600));
    }

    #[test]
    fn paste_time_leaves_a_loop_region_before_the_cursor_alone() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);
        sequencer.set_global_region(0, 400);

        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (0, 400));
    }

    /// Paste Time leaves the marquee where it was: no selection to advance.
    #[test]
    fn paste_time_moves_no_marquee() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        let mut edit = DuplicateTimeEdit::pasting(&sequencer).unwrap();
        let EditResult::TimeDuplicated { new_selection, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimeDuplicated");
        };
        assert_eq!(new_selection, None);
    }
}
