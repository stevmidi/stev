//! Undoable `Shift+⌘/Ctrl+V` (Paste Time): `⌘/Ctrl+V`'s paste, inserting
//! instead of overwriting. Composed from [`InsertSilenceEdit`] +
//! [`PasteClipsEdit`], the same pair as `DuplicateTimeEdit` next door.

use super::super::super::Sequencer;
use super::super::EditResult;
use super::insert_silence::InsertSilenceEdit;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// PasteTime
// ---------------------------------------------------------------------------

/// `Shift+⌘/Ctrl+V` in the Arranger — Ableton-style "Paste Time". The paste
/// is `⌘/Ctrl+V`'s exactly (the same clipboard, the cursor as anchor, the
/// same tracks: the selected one for a scoped copy, the source ones for an
/// all-track copy), but room is made for it first: everything at or after
/// the cursor moves right by the copied length, on every track, so nothing
/// is overwritten. Every track, whichever copy filled the clipboard, as for
/// every edit that inserts time — a partial shift would slide some tracks
/// out of line with the rest.
///
/// 1. `InsertSilenceEdit` opens a `ClipClipboard::span` tick gap at the
///    cursor (splitting any clip straddling it). The span is the copied
///    range's length, so a copy ending in an empty stretch keeps it.
/// 2. `PasteClipsEdit` drops the clipboard into the freed span. Every piece
///    lies within `[0, span)` of its anchor, so the carve is always a no-op.
///
/// Like Duplicate Time, the loop region is *moved* (not resized) when it
/// starts at or after the cursor, so it stays on the material that moved.
pub(crate) struct PasteTimeEdit {
    /// The cursor tick the gap opens at, frozen at construction.
    anchor: i32,
    /// Gap width: the clipboard's span.
    span: i32,
    /// Opens the gap. `None` when nothing sits at or after the cursor.
    insert: Option<InsertSilenceEdit>,
    /// The paste into the gap, its targets frozen at construction.
    paste: PasteClipsEdit,
    /// Whether the loop region was slid right by `span`, frozen on the
    /// first `edit()` so redo replays the same decision.
    region_shifted: Option<bool>,
}

impl PasteTimeEdit {
    /// Builds the edit from the session clipboard, the cursor and the
    /// selected track. `None` whenever `⌘/Ctrl+V` would paste nothing (an
    /// empty clipboard, a scoped copy with no selected track).
    pub(crate) fn from_sequencer(sequencer: &Sequencer) -> Option<Self> {
        let clipboard = sequencer.clip_clipboard()?;
        let anchor = sequencer.cursor_tick();
        let span = clipboard.span();
        let paste = PasteClipsEdit::from_clipboard_at(
            sequencer,
            clipboard,
            anchor,
            sequencer.selected_track_index(),
        )?;
        let insert = InsertSilenceEdit::from_time_range(sequencer, anchor, anchor + span);

        Some(Self {
            anchor,
            span,
            insert,
            paste,
            region_shifted: None,
        })
    }

    /// Opens the gap, pastes into it, then slides the loop region if it
    /// starts at or after the cursor. Returns [`EditResult::TimePasted`].
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

        let anchor = self.anchor;
        let region_shifted = *self
            .region_shifted
            .get_or_insert_with(|| sequencer.region_start() >= anchor);
        if region_shifted {
            sequencer.shift_region(self.span);
        }

        EditResult::TimePasted {
            shifted,
            split_updated,
            split_added,
            pasted,
        }
    }

    /// Un-pastes, removes the gap, and slides the loop region back. Returns
    /// [`EditResult::TimeUnduplicated`], Duplicate Time's undo, whose shape
    /// it shares.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        if self.region_shifted == Some(true) {
            sequencer.shift_region(-self.span);
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

    use crate::core::input_event::TimeSelectionRect;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

    use super::*;

    fn starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    fn select(sequencer: &mut Sequencer, track_idx: usize, clip_id: Option<Uuid>) {
        let track_id = sequencer.track_id_by_index(track_idx).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(clip_id);
    }

    fn region(sequencer: &Sequencer) -> (i32, i32) {
        (sequencer.region_start(), sequencer.region_end())
    }

    #[test]
    fn is_none_with_an_empty_clipboard() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        assert!(PasteTimeEdit::from_sequencer(&sequencer).is_none());
    }

    #[test]
    fn a_scoped_copy_with_no_selected_track_pastes_nothing() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect::spanning(0, 100, 0, 0));
        sequencer.select_track(None);
        assert!(PasteTimeEdit::from_sequencer(&sequencer).is_none());
    }

    /// An all-track copy (`⇧⌘C`) lands on its source tracks at the cursor,
    /// and everything from the cursor on moves right on every track.
    #[test]
    fn an_all_track_copy_inserts_at_the_cursor_on_every_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(500, 100));
        sequencer.tracks_mut()[2].add_clip(&clip_at(400, 100)); // not copied, moves
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(400);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::TimePasted {
            shifted, pasted, ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected TimePasted");
        };

        assert_eq!(pasted.len(), 1);
        assert_eq!(shifted.len(), 2);
        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (400, 500)]);
        assert_eq!(starts(&sequencer, 1), vec![(600, 700)]);
        assert_eq!(starts(&sequencer, 2), vec![(500, 600)]);

        let EditResult::TimeUnduplicated { .. } = edit.undo(&mut sequencer) else {
            panic!("expected TimeUnduplicated");
        };
        assert_eq!(starts(&sequencer, 0), vec![(0, 100)]);
        assert_eq!(starts(&sequencer, 1), vec![(500, 600)]);
        assert_eq!(starts(&sequencer, 2), vec![(400, 500)]);
    }

    /// A scoped copy (`⌘C`) lands on the selected track in the marquee's
    /// shape, as `⌘V` does, but the gap still opens on every track.
    #[test]
    fn a_scoped_copy_lands_on_the_selected_track_and_moves_every_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(200, 100));
        sequencer.tracks_mut()[3].add_clip(&clip_at(200, 100));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect::spanning(0, 100, 0, 0));
        select(&mut sequencer, 1, None);
        sequencer.set_cursor_tick(200);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 100)]);
        assert_eq!(starts(&sequencer, 1), vec![(200, 300), (300, 400)]);
        assert_eq!(starts(&sequencer, 3), vec![(300, 400)]);
    }

    /// The gap is the copied length, not the pieces' extent: an empty
    /// stretch at the end of the copy is inserted too.
    #[test]
    fn inserts_the_copied_length_including_trailing_silence() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(1000, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 400);
        sequencer.set_cursor_tick(1000);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (1000, 1100), (1400, 1500)]
        );
    }

    #[test]
    fn splits_a_clip_straddling_the_cursor_and_pastes_between_its_halves() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 400));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 100), (200, 300)]);
        assert_eq!(
            starts(&sequencer, 1),
            vec![(0, 200), (200, 300), (300, 500)]
        );

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 1), vec![(0, 400)]);
    }

    #[test]
    fn redo_reuses_the_frozen_clip_ids() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        sequencer.tracks_mut()[0].add_clip(&clip_at(200, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::TimePasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimePasted");
        };
        let first: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        edit.undo(&mut sequencer);
        let EditResult::TimePasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected TimePasted on redo");
        };
        let redo: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first, redo);
        assert_eq!(
            starts(&sequencer, 0),
            vec![(0, 100), (200, 300), (300, 400)]
        );
    }

    #[test]
    fn a_loop_region_at_or_after_the_cursor_slides_along() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);
        sequencer.set_global_region(200, 600);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (300, 700));

        edit.undo(&mut sequencer);
        assert_eq!(region(&sequencer), (200, 600));
    }

    #[test]
    fn a_loop_region_before_the_cursor_is_left_alone() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 100));
        select(&mut sequencer, 0, None);
        sequencer.copy_clips_to_clipboard(0, 100);
        sequencer.set_cursor_tick(200);
        sequencer.set_global_region(0, 400);

        let mut edit = PasteTimeEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(region(&sequencer), (0, 400));
    }
}
