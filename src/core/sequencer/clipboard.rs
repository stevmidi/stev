//! The Arranger clip clipboard, the clip view's note clipboard, and the copy
//! operations that fill them.
//!
//! Copying is not a data mutation, so none of this is undoable and none of it
//! is persisted — the clipboard is session-only in-memory state on
//! [`Sequencer`], like a selection. Paste itself *is* undoable and lives in
//! `edit/clip_edits/paste.rs`. Both Duplicate edits build a throwaway
//! clipboard here without disturbing the real one: `DuplicateTimeEdit`
//! (`Shift+⌘/Ctrl+D`) via [`clipboard_snapshot`](Sequencer::clipboard_snapshot),
//! `DuplicateClipsEdit` (plain `⌘/Ctrl+D`) via the track-scoped
//! [`copy_range_to_clipboard`](Sequencer::copy_range_to_clipboard).
//!
//! The note clipboard is a separate buffer, so a note copy never discards a
//! clip copy: each view's `⌘/Ctrl+V` pastes its own kind (`InsertNotesEdit`
//! in `edit/event_edits.rs` for notes).

use crate::core::input_event::TimeSelectionRect;
use crate::models::clip::{Clip, CopiedNote};
use crate::models::region::Region;

use super::Sequencer;

/// Session-only, in-memory clipboard for Arranger clip copy/paste. Holds the
/// copied clip pieces with their `start_tick` normalized to the copy anchor
/// (the time-selection start), so paste can re-anchor the whole set to the
/// cursor; each piece keeps the absolute track index it was copied from, and
/// the clipboard optionally records the marquee's top track as a *track
/// anchor* so paste can re-anchor the whole set onto the selected track the
/// same way. Never persisted, never part of the undo record — copying is not
/// a data mutation, exactly like a selection.
pub(crate) struct ClipClipboard {
    /// The copied pieces, anchor-normalized.
    clips: Vec<ClipboardClip>,
    /// The track the set is anchored on, if it was copied with a track
    /// shape: the marquee's `track_start` for plain `⌘/Ctrl+C`/`⌘/Ctrl+X`,
    /// so a paste lands the set on the selected track with the marquee's
    /// shape (a copy of tracks 3..=5 pasted with track 2 selected lands on
    /// tracks 2..=4). `None` for the all-track `Shift+⌘/Ctrl+C`/`X` forms
    /// and the Duplicate edits' throwaway clipboards — those keep every
    /// piece on its absolute source track, whatever is selected.
    track_anchor: Option<usize>,
}

impl ClipClipboard {
    /// The copied pieces.
    pub(crate) fn clips(&self) -> &[ClipboardClip] {
        &self.clips
    }

    /// The track `piece` should land on when pasted with `selected_track`
    /// as the paste target: `selected_track + (piece.track_idx - anchor)`
    /// when the clipboard has a track anchor and a track is selected, the
    /// piece's absolute source track otherwise.
    pub(crate) fn target_track(
        &self,
        piece: &ClipboardClip,
        selected_track: Option<usize>,
    ) -> usize {
        match (self.track_anchor, selected_track) {
            (Some(anchor), Some(selected)) => selected + piece.track_idx.saturating_sub(anchor),
            _ => piece.track_idx,
        }
    }
}

/// One clip piece on the clipboard.
pub(crate) struct ClipboardClip {
    /// Absolute source track index the piece was copied from.
    pub(crate) track_idx: usize,
    /// The piece: `start_tick` is normalized to the copy anchor, the region
    /// window is the intersection with the copied range (phase-locked, full
    /// event list retained — non-destructive, exactly like a split), and the
    /// region atomics are detached (fresh, not shared with the source clip).
    pub(crate) clip: Clip,
}

impl Sequencer {
    /// Copies the portion of every clip on every track that intersects
    /// `[start, end)` into `self.clip_clipboard` — `Shift+⌘/Ctrl+C`, which
    /// bypasses the marquee's track range. Marquee-only, like every other
    /// clipboard/delete binding: there is no selected-clip fallback. A no-op
    /// (leaving any existing clipboard content intact) when nothing overlaps.
    pub(crate) fn copy_clips_to_clipboard(&mut self, start: i32, end: i32) {
        if let Some(clipboard) = self.clipboard_snapshot(start, end) {
            self.clip_clipboard = Some(clipboard);
        }
    }

    /// The clip pieces `copy_clips_to_clipboard` would store, without storing
    /// them. `Shift+⌘/Ctrl+D`'s global form (`DuplicateTimeEdit::from_time_range`)
    /// uses this to build a throwaway clipboard it pastes at the selection's
    /// `end`, leaving the real `⌘C`/`⌘V` clipboard (`self.clip_clipboard`)
    /// untouched.
    pub(in crate::core::sequencer) fn clipboard_snapshot(
        &self,
        start: i32,
        end: i32,
    ) -> Option<ClipClipboard> {
        self.copy_range_to_clipboard((0, self.tracks.len().saturating_sub(1)), start, end)
    }

    /// The current clipboard contents, if anything has been copied this
    /// session.
    pub(crate) fn clip_clipboard(&self) -> Option<&ClipClipboard> {
        self.clip_clipboard.as_ref()
    }

    /// Builds a time-selection clipboard: the `[start, end)` intersection slice
    /// of every overlapping clip on tracks `tracks.0..=tracks.1`, each piece
    /// re-anchored to `start` and given detached region atoms. `None` if the
    /// range is empty or overlaps nothing. `pub(in crate::core::sequencer)`
    /// so `DuplicateClipsEdit::from_track_span` can build a track-scoped
    /// throwaway clipboard for plain `⌘D`, and plain `⌘C`/`⌘X` can use it
    /// directly for the marquee rectangle.
    pub(in crate::core::sequencer) fn copy_range_to_clipboard(
        &self,
        tracks: (usize, usize),
        start: i32,
        end: i32,
    ) -> Option<ClipClipboard> {
        if end <= start {
            return None;
        }

        let mut clips = Vec::new();
        for (track_idx, track) in self.tracks.iter().enumerate() {
            if track_idx < tracks.0 || track_idx > tracks.1 {
                continue;
            }
            for clip_id in track.find_clip_ids_in(start, end) {
                let Some(clip) = track.get_clip_by_id(clip_id) else {
                    continue;
                };

                let piece_start = clip.start_tick().max(start);
                let piece_end = clip.end_tick().min(end);
                if piece_end <= piece_start {
                    continue;
                }

                let delta = piece_start - clip.start_tick();
                let region_start = clip.region().start() + delta;
                let region_end = region_start + (piece_end - piece_start);

                let mut piece = clip.clone();
                piece.generate_new_id();
                piece.set_start_tick(piece_start - start);
                *piece.region_mut() = Region::new(region_start, region_end);

                clips.push(ClipboardClip {
                    track_idx,
                    clip: piece,
                });
            }
        }

        if clips.is_empty() {
            return None;
        }

        Some(ClipClipboard {
            clips,
            track_anchor: None,
        })
    }

    /// Copies every clip within the marquee rectangle into
    /// `self.clip_clipboard` — the plain `⌘/Ctrl+C` binding (distinct from
    /// `copy_clips_to_clipboard`, which backs `Shift+⌘/Ctrl+C`'s all-track
    /// copy that bypasses the marquee's track range). Copies the tick-range
    /// intersection of every clip on tracks
    /// `rect.track_start..=rect.track_end`, anchored on `rect.track_start`
    /// so `⌘/Ctrl+V` re-anchors the set onto the selected track (see
    /// [`ClipClipboard::target_track`]). A no-op (leaving any existing
    /// clipboard content intact) when the rect overlaps nothing — there is
    /// no whole-clip fallback of any kind.
    pub(crate) fn copy_clips_scoped_to_clipboard(&mut self, rect: TimeSelectionRect) {
        if let Some(mut clipboard) =
            self.copy_range_to_clipboard((rect.track_start, rect.track_end), rect.start, rect.end)
        {
            clipboard.track_anchor = Some(rect.track_start);
            self.clip_clipboard = Some(clipboard);
        }
    }

    // --- Note clipboard ---

    /// Copies the lead clip's selected notes into `self.note_clipboard` —
    /// `⌘/Ctrl+C` in the clip view ([`Clip::copy_selected_notes`]). Returns
    /// whether anything was copied; with nothing selected it is a no-op that
    /// leaves the clipboard as it was.
    pub(crate) fn copy_selected_notes_to_clipboard(&mut self) -> bool {
        let notes = self
            .selected_clip()
            .map(Clip::copy_selected_notes)
            .unwrap_or_default();
        if notes.is_empty() {
            return false;
        }
        self.note_clipboard = notes;
        true
    }

    /// The copied notes, empty if none have been copied this session.
    pub(crate) fn note_clipboard(&self) -> &[CopiedNote] {
        &self.note_clipboard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

    /// A range exactly covering one clip (what a band press marquees) copies
    /// that clip whole, re-anchored to the range start.
    #[test]
    fn range_exactly_covering_one_clip_copies_it_whole_and_detaches_region() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(1920, 960);
        let clip_id = clip.id();
        sequencer.tracks_mut()[2].add_clip(&clip);

        sequencer.copy_clips_to_clipboard(1920, 2880);

        let clipboard = sequencer.clip_clipboard().unwrap();
        assert_eq!(clipboard.clips().len(), 1);
        let piece = &clipboard.clips()[0];
        assert_eq!(piece.track_idx, 2);
        assert_eq!(piece.clip.start_tick(), 0);
        assert_eq!(piece.clip.region().end() - piece.clip.region().start(), 960);

        // Mutating the source region must not touch the clipboard copy.
        sequencer.tracks_mut()[2]
            .get_clip_by_id_mut(clip_id)
            .unwrap()
            .region_mut()
            .set_region(Some(0), Some(240));
        assert_eq!(
            sequencer.clip_clipboard().unwrap().clips()[0]
                .clip
                .region()
                .end(),
            960
        );
    }

    #[test]
    fn range_copy_produces_intersection_pieces_across_tracks() {
        let mut sequencer = test_sequencer();
        // Track 0: clip spanning the whole range -> piece is the middle window.
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920)); // [0, 1920)
        // Track 1: clip straddling the start edge -> right portion only.
        sequencer.tracks_mut()[1].add_clip(&clip_at(240, 960)); // [240, 1200)
        // Track 2: clip fully inside -> whole clip, re-anchored.
        sequencer.tracks_mut()[2].add_clip(&clip_at(600, 240)); // [600, 840)

        sequencer.copy_clips_to_clipboard(480, 960);

        let clipboard = sequencer.clip_clipboard().unwrap();
        assert_eq!(clipboard.clips().len(), 3);

        let by_track = |idx: usize| {
            clipboard
                .clips()
                .iter()
                .find(|c| c.track_idx == idx)
                .map(|c| {
                    (
                        c.clip.start_tick(),
                        c.clip.region().start(),
                        c.clip.region().end(),
                    )
                })
                .unwrap()
        };

        // Track 0: intersection [480, 960) -> normalized start 0, region [480, 960).
        assert_eq!(by_track(0), (0, 480, 960));
        // Track 1: intersection [480, 1200)->clamped to [480, 960) -> start 0,
        // region [0 + (480-240), ...) = [240, 720).
        assert_eq!(by_track(1), (0, 240, 720));
        // Track 2: fully inside -> start 600-480 = 120, region [0, 240).
        assert_eq!(by_track(2), (120, 0, 240));
    }

    #[test]
    fn copy_with_nothing_to_copy_is_a_noop() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));

        // Time range that overlaps nothing.
        sequencer.copy_clips_to_clipboard(2000, 3000);
        assert!(sequencer.clip_clipboard().is_none());

        // Backwards range.
        sequencer.copy_clips_to_clipboard(960, 480);
        assert!(sequencer.clip_clipboard().is_none());
    }

    #[test]
    fn copy_range_to_clipboard_track_filter_excludes_tracks_outside_the_span() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // outside the span
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960)); // in the span
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 960)); // in the span
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 960)); // outside the span

        let clipboard = sequencer.copy_range_to_clipboard((1, 2), 0, 960).unwrap();
        let track_idxs: Vec<usize> = clipboard.clips().iter().map(|c| c.track_idx).collect();
        assert_eq!(track_idxs, vec![1, 2]);
    }

    #[test]
    fn copy_clips_scoped_to_clipboard_copies_every_clip_in_the_marquee() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960)); // outside the marquee's track range
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960));
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 960));

        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 960,
            track_start: 1,
            track_end: 2,
        });

        let clipboard = sequencer.clip_clipboard().unwrap();
        let track_idxs: Vec<usize> = clipboard.clips().iter().map(|c| c.track_idx).collect();
        assert_eq!(track_idxs, vec![1, 2]);
    }

    /// The marquee-scoped copy anchors on the marquee's top track, so
    /// `target_track` shifts the set onto the selected track keeping the
    /// marquee's shape; the all-track copy has no anchor and always answers
    /// the absolute source track.
    #[test]
    fn target_track_re_anchors_scoped_copies_and_keeps_all_track_copies_absolute() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_count(6);
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 960));
        sequencer.tracks_mut()[5].add_clip(&clip_at(0, 960));

        // Marquee over tracks 3..=5 (track 4 empty) pasted with track 2
        // selected: 3 -> 2, 5 -> 4.
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 960,
            track_start: 3,
            track_end: 5,
        });
        let clipboard = sequencer.clip_clipboard().unwrap();
        let targets: Vec<usize> = clipboard
            .clips()
            .iter()
            .map(|c| clipboard.target_track(c, Some(2)))
            .collect();
        assert_eq!(targets, vec![2, 4]);
        // No selected track: back on the source tracks.
        let targets: Vec<usize> = clipboard
            .clips()
            .iter()
            .map(|c| clipboard.target_track(c, None))
            .collect();
        assert_eq!(targets, vec![3, 5]);

        // Shift+⌘C: absolute regardless of the selected track.
        sequencer.copy_clips_to_clipboard(0, 960);
        let clipboard = sequencer.clip_clipboard().unwrap();
        let targets: Vec<usize> = clipboard
            .clips()
            .iter()
            .map(|c| clipboard.target_track(c, Some(0)))
            .collect();
        assert_eq!(targets, vec![3, 5]);
    }

    #[test]
    fn copy_clips_scoped_to_clipboard_overlapping_nothing_is_a_noop() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));

        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 960,
            track_start: 1,
            track_end: 2,
        });

        assert!(sequencer.clip_clipboard().is_none());
    }
}
