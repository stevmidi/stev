//! Undoable clip paste — drop the session clipboard at the cursor, carving out
//! overlapping material per target track via [`DeleteInRangeEdit`].

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::super::clipboard::ClipClipboard;
use super::super::{EditResult, PasteLead};
use super::delete_in_range::DeleteInRangeEdit;
use super::{CarveBuckets, detach, detached_clone};

// ---------------------------------------------------------------------------
// PasteClips
// ---------------------------------------------------------------------------

/// Pastes the session clipboard (see `sequencer/clipboard.rs`) at the cursor
/// tick, Ableton-style. Each clipboard piece is re-anchored to the cursor and,
/// when the clipboard carries a track anchor (plain `⌘/Ctrl+C`/`X`) and a
/// track is selected, onto the selected track with the marquee's shape
/// (`ClipClipboard::target_track`); otherwise it goes back on the absolute
/// track it was copied from. Pieces that would land past the last track are
/// dropped. Where a pasted clip
/// overlaps existing material, that material is carved out exactly like
/// `Delete` — by composing one `DeleteInRangeEdit` per pasted clip, scoped to
/// that clip's target track, rather than re-deriving the split/trim logic.
/// Same worked-example shape as `InsertSilenceEdit`/`DeleteInRangeEdit`
/// composing `SplitClipsEdit`.
pub(crate) struct PasteClipsEdit {
    /// Final positioned clips to add: absolute `start_tick`, detached region,
    /// stable ids. Built at construction; from the first `undo()` on each
    /// slot holds the clip *as lifted* — so a redo re-adds a pasted clip with
    /// any non-undoable change made to it since (an edge trim), the same
    /// "the clip travels through the edit" rule as `CommitClipEdit`.
    targets: Vec<(usize, Clip)>,
    /// Parallel to `targets` once populated: the carve of the exact span each
    /// pasted clip lands on, scoped to that clip's target track. `None` per
    /// slot when that span is already clear. Built on the **first** `edit()`
    /// call — one carve at a time, each against the state the previous
    /// carve+add left behind, so a single existing clip spanning several
    /// pasted clips is handled correctly. `Some(_)` afterwards: redo replays
    /// each carve's own frozen snapshot.
    carves: Option<Vec<Option<DeleteInRangeEdit>>>,
    /// Which clip becomes the lead after the paste (Merge Clips and the
    /// MIDI clip import name one). Reported as `ClipsPasted::lead`.
    lead: PasteLead,
    /// Selection at construction time, so undo can restore it.
    selected_track_idx: Option<usize>,
    /// Selection at construction time, so undo can restore it.
    selected_clip_id: Option<Uuid>,
}

impl PasteClipsEdit {
    /// `⌘/Ctrl+V`: paste the session clipboard at the cursor tick, on the
    /// selected track when the clipboard is track-anchored.
    pub(crate) fn from_sequencer(sequencer: &Sequencer) -> Option<Self> {
        Self::from_clipboard_at(
            sequencer,
            sequencer.clip_clipboard()?,
            sequencer.cursor_tick(),
            sequencer.selected_track_index(),
        )
    }

    /// Core of a paste, decoupled from *which* clipboard and *where*. Each piece
    /// lands at `anchor + piece.start_tick()` (clipboard pieces are normalized
    /// to their copy anchor) on `clipboard.target_track(piece, target_track)`
    /// — its absolute source track unless both the clipboard is track-anchored
    /// and `target_track` is `Some`. Shared by `⌘V` (`from_sequencer`, real
    /// clipboard + cursor + selected track), `DuplicateTimeEdit`
    /// (`Shift+⌘/Ctrl+D`, a throwaway `Sequencer::clipboard_snapshot` + the
    /// selection's `end` as anchor) and `DuplicateClipsEdit` (plain
    /// `⌘/Ctrl+D`, a throwaway track-scoped `copy_range_to_clipboard` + the
    /// same anchor); both Duplicates pass `target_track: None` so their copy
    /// stays on the source tracks regardless of the selection.
    pub(in crate::core::sequencer::edit) fn from_clipboard_at(
        sequencer: &Sequencer,
        clipboard: &ClipClipboard,
        anchor: i32,
        target_track: Option<usize>,
    ) -> Option<Self> {
        let targets = clipboard
            .clips()
            .iter()
            .map(|piece| {
                let mut clip = detached_clone(&piece.clip);
                clip.generate_new_id();
                clip.set_start_tick(anchor + piece.clip.start_tick());
                (clipboard.target_track(piece, target_track), clip)
            })
            .collect();
        Self::from_targets(sequencer, targets, PasteLead::CursorRule)
    }

    /// A paste of ready-positioned clips: each `(track index, clip)` lands
    /// as it is (absolute `start_tick`, its own id and detached region),
    /// carving what it overlaps, then `lead` picks the lead clip. Targets
    /// past the last track are dropped; `None` if none is left. Merge Clips
    /// (`merge_clips.rs`) and the MIDI clip import (`import_clip.rs`) paste
    /// their clips through this.
    pub(in crate::core::sequencer::edit) fn from_targets(
        sequencer: &Sequencer,
        mut targets: Vec<(usize, Clip)>,
        lead: PasteLead,
    ) -> Option<Self> {
        let track_count = sequencer.tracks().len();
        targets.retain(|(track_idx, _)| *track_idx < track_count);
        if targets.is_empty() {
            return None;
        }

        Some(Self {
            targets,
            carves: None,
            lead,
            selected_track_idx: sequencer.selected_track_index(),
            selected_clip_id: sequencer.selected_clip_id(),
        })
    }

    /// Carves the span each target lands on (building the carves on the first
    /// call) and adds the pasted clips. Returns [`EditResult::ClipsPasted`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let mut carved = CarveBuckets::default();
        let mut pasted = Vec::new();

        let building = self.carves.is_none();
        let mut carves = self
            .carves
            .take()
            .unwrap_or_else(|| self.targets.iter().map(|_| None).collect());

        // Interleave carve + add per target: on the first call each carve is
        // constructed against the live state (post previous carve+add); on
        // redo each replays its own frozen snapshot.
        for (i, (track_idx, clip)) in self.targets.iter().enumerate() {
            if building {
                carves[i] = DeleteInRangeEdit::from_track_range(
                    sequencer,
                    *track_idx,
                    clip.start_tick(),
                    clip.end_tick(),
                );
            }

            if let Some(carve) = carves.get_mut(i).and_then(|c| c.as_mut()) {
                carved.absorb(carve.edit(sequencer));
            }

            if let Some(track) = sequencer.tracks_mut().get_mut(*track_idx)
                && track.add_clip(clip)
            {
                pasted.push(ClipMetadata::from_clip(*track_idx, clip));
            }
        }

        self.carves = Some(carves);

        if pasted.is_empty() && carved.is_empty() {
            return EditResult::NoOp;
        }

        EditResult::ClipsPasted {
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
            pasted,
            lead: self.lead,
        }
    }

    /// Removes the pasted clips (keeping each as lifted, for redo), un-carves
    /// each span, and restores the selection. Returns
    /// [`EditResult::ClipsUnpasted`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let mut carved = CarveBuckets::default();
        let mut unpasted = Vec::new();

        let Some(carves) = self.carves.as_mut() else {
            return EditResult::NoOp;
        };

        // Reverse of `edit()`'s interleave: drop each pasted clip, then undo
        // its carve (restoring the originals into the now-empty span).
        for (i, (track_idx, clip)) in self.targets.iter_mut().enumerate().rev() {
            if let Some((metadata, lifted)) = sequencer.lift_clip(*track_idx, clip.id()) {
                unpasted.push(metadata);
                // Redo re-adds the clip as it was when undone, not as pasted.
                // Its extent still fits on redo: a widening trim was bounded
                // by the same neighbours the carve leaves in place.
                *clip = detach(lifted);
            }

            if let Some(carve) = carves.get_mut(i).and_then(|c| c.as_mut()) {
                carved.absorb(carve.undo(sequencer));
            }
        }

        EditResult::ClipsUnpasted {
            carve_updated: carved.updated,
            carve_added: carved.added,
            carve_removed: carved.removed,
            unpasted,
            selected_track_idx: self.selected_track_idx,
            selected_clip_id: self.selected_clip_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use crate::core::input_event::TimeSelectionRect;

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

    fn select(sequencer: &mut Sequencer, track_idx: usize, clip_id: Option<Uuid>) {
        let track_id = sequencer.track_id_by_index(track_idx).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(clip_id);
    }

    fn set_cursor(sequencer: &Sequencer, tick: i32) {
        sequencer.cursor_tick.store(tick, Ordering::Relaxed);
    }

    #[test]
    fn paste_into_empty_space_adds_clip_at_cursor() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select(&mut sequencer, 0, Some(clip_id));
        sequencer.copy_clips_to_clipboard(0, 960);

        set_cursor(&sequencer, 1920);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        assert_eq!(pasted.len(), 1);
        assert_eq!(starts(&sequencer, 0), vec![(0, 960), (1920, 2880)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 960)]);
    }

    /// A non-undoable change to a pasted clip (an edge trim) survives an
    /// undo/redo round trip: redo re-adds the clip as it was when undone.
    #[test]
    fn redo_re_adds_a_pasted_clip_as_it_was_when_undone() {
        let mut sequencer = test_sequencer();
        let clip = clip_at(0, 960);
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        select(&mut sequencer, 0, Some(clip_id));
        sequencer.copy_clips_to_clipboard(0, 960);

        set_cursor(&sequencer, 1920);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        let pasted_id = pasted[0].clip_id;

        // Trim the pasted clip's right edge, then round-trip.
        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(pasted_id)
            .unwrap()
            .region_mut()
            .set_region(None, Some(480));
        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 960)]);
        edit.edit(&mut sequencer);

        assert_eq!(starts(&sequencer, 0), vec![(0, 960), (1920, 2400)]);
        assert_eq!(
            sequencer.tracks()[0].clips()[1].id(),
            pasted_id,
            "same id on redo"
        );
    }

    #[test]
    fn paste_carves_overlapping_existing_clip_and_undo_restores_it() {
        let mut sequencer = test_sequencer();
        // Source clip on track 2, copied whole via a range exactly covering it.
        let src = clip_at(2400, 480);
        sequencer.tracks_mut()[2].add_clip(&src);
        sequencer.copy_clips_to_clipboard(2400, 2880);

        // Destination: a long clip on the same track that the paste will land
        // inside — pieces always paste back onto their absolute source track.
        select(&mut sequencer, 2, None);
        let dst = clip_at(0, 1920); // [0, 1920)
        let dst_id = dst.id();
        sequencer.tracks_mut()[2].add_clip(&dst);

        // Paste at cursor 480 -> pasted clip [480, 960).
        set_cursor(&sequencer, 480);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted {
            carve_updated,
            carve_added,
            carve_removed,
            pasted,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipsPasted");
        };

        assert_eq!(pasted.len(), 1);
        assert_eq!(carve_updated.len(), 1); // left half of dst keeps its id
        assert_eq!(carve_updated[0].clip_id, dst_id);
        assert_eq!(carve_added.len(), 1); // right remainder of dst
        assert!(carve_removed.is_empty());

        // dst split into [0,480) + [960,1920); pasted clip fills [480,960);
        // the source clip at [2400,2880) is untouched.
        assert_eq!(
            starts(&sequencer, 2),
            vec![(0, 480), (480, 960), (960, 1920), (2400, 2880)]
        );

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 2), vec![(0, 1920), (2400, 2880)]);
        assert_eq!(sequencer.tracks()[2].clips()[0].id(), dst_id);
    }

    #[test]
    fn range_paste_keeps_absolute_tracks_and_is_stable_across_redo() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480)); // [0, 480)
        sequencer.tracks_mut()[2].add_clip(&clip_at(0, 480)); // [0, 480)
        select(&mut sequencer, 0, None);

        sequencer.copy_clips_to_clipboard(0, 480);
        set_cursor(&sequencer, 960);

        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        assert_eq!(pasted.len(), 2);
        assert_eq!(starts(&sequencer, 0), vec![(0, 480), (960, 1440)]);
        assert_eq!(starts(&sequencer, 2), vec![(0, 480), (960, 1440)]);
        let first_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![(0, 480)]);
        assert_eq!(starts(&sequencer, 2), vec![(0, 480)]);

        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted on redo");
        };
        let redo_ids: Vec<_> = pasted.iter().map(|m| m.clip_id).collect();
        assert_eq!(first_ids, redo_ids, "redo must reuse the frozen clip ids");
    }

    /// Pieces keep their absolute source track, so a paste needs no track
    /// selection at all.
    #[test]
    fn paste_does_not_need_a_selected_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 480));
        sequencer.copy_clips_to_clipboard(0, 480);

        sequencer.select_track(None);
        set_cursor(&sequencer, 960);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(starts(&sequencer, 1), vec![(0, 480), (960, 1440)]);
    }

    /// A marquee-scoped copy (plain `⌘C`) pastes onto the selected track with
    /// the marquee's shape: tracks 3..=5 copied, track 2 selected -> 2..=4.
    #[test]
    fn scoped_paste_re_anchors_the_marquee_shape_onto_the_selected_track() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_count(6);
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 480));
        sequencer.tracks_mut()[4].add_clip(&clip_at(240, 240));
        sequencer.tracks_mut()[5].add_clip(&clip_at(0, 480));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 480,
            track_start: 3,
            track_end: 5,
        });

        select(&mut sequencer, 2, None);
        set_cursor(&sequencer, 960);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        assert_eq!(pasted.len(), 3);
        assert_eq!(starts(&sequencer, 2), vec![(960, 1440)]);
        assert_eq!(starts(&sequencer, 3), vec![(0, 480), (1200, 1440)]);
        assert_eq!(starts(&sequencer, 4), vec![(240, 480), (960, 1440)]);
        assert_eq!(starts(&sequencer, 5), vec![(0, 480)]);

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 2), vec![]);
        assert_eq!(starts(&sequencer, 3), vec![(0, 480)]);
        assert_eq!(starts(&sequencer, 4), vec![(240, 480)]);
    }

    /// Pieces re-anchored past the last track are dropped; the rest still
    /// paste. Re-anchoring so that *every* piece falls off yields no edit.
    #[test]
    fn scoped_paste_drops_pieces_re_anchored_past_the_last_track() {
        let mut sequencer = test_sequencer();
        let last = sequencer.tracks().len() - 1;
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480));
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 480));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 480,
            track_start: 0,
            track_end: 1,
        });

        // Selected = last track: track 0's piece lands on it, track 1's is
        // dropped.
        select(&mut sequencer, last, None);
        set_cursor(&sequencer, 960);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        assert_eq!(pasted.len(), 1);
        assert_eq!(starts(&sequencer, last), vec![(960, 1440)]);
        assert_eq!(starts(&sequencer, 1), vec![(0, 480)]);
    }

    /// A marquee-scoped copy with no selected track pastes back onto its
    /// source tracks, exactly like the all-track form.
    #[test]
    fn scoped_paste_without_a_selected_track_keeps_source_tracks() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 480));
        sequencer.copy_clips_scoped_to_clipboard(TimeSelectionRect {
            start: 0,
            end: 480,
            track_start: 3,
            track_end: 3,
        });

        sequencer.select_track(None);
        set_cursor(&sequencer, 960);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(starts(&sequencer, 3), vec![(0, 480), (960, 1440)]);
    }

    /// The all-track `Shift+⌘C` clipboard ignores the selected track on paste.
    #[test]
    fn all_track_paste_ignores_the_selected_track() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 480));
        sequencer.copy_clips_to_clipboard(0, 480);

        select(&mut sequencer, 0, None);
        set_cursor(&sequencer, 960);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(starts(&sequencer, 0), vec![]);
        assert_eq!(starts(&sequencer, 3), vec![(0, 480), (960, 1440)]);
    }

    #[test]
    fn two_pasted_clips_carve_one_spanning_existing_clip_correctly() {
        let mut sequencer = test_sequencer();
        // Source track 3 has two clips with a gap -> two clipboard pieces on
        // one track: [0,400) and [800,1200) relative to the copy start.
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 400));
        sequencer.tracks_mut()[3].add_clip(&clip_at(800, 400));
        select(&mut sequencer, 3, None);
        sequencer.copy_clips_to_clipboard(0, 1200);

        // Destination track 3 also has one long clip covering the whole paste
        // zone. Wipe it first and drop a single spanning clip.
        sequencer.tracks_mut()[3].clear_clips();
        let dst = clip_at(0, 4000);
        let dst_id = dst.id();
        sequencer.tracks_mut()[3].add_clip(&dst);

        set_cursor(&sequencer, 1000);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };

        // Both pieces land: [1000,1400) and [1800,2200); the spanning clip is
        // carved around both, leaving [0,1000), gap-piece [1400,1800), [2200,4000).
        assert_eq!(pasted.len(), 2);
        assert_eq!(
            starts(&sequencer, 3),
            vec![
                (0, 1000),
                (1000, 1400),
                (1400, 1800),
                (1800, 2200),
                (2200, 4000),
            ]
        );

        edit.undo(&mut sequencer);
        assert_eq!(starts(&sequencer, 3), vec![(0, 4000)]);
        assert_eq!(sequencer.tracks()[3].clips()[0].id(), dst_id);
    }

    fn clip_with_open_note(start_tick: i32) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut().set_region(Some(0), Some(960));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: undoing a paste while the pasted clip is currently
    /// sounding must release its open note instead of stranding it.
    #[test]
    fn undoing_a_paste_releases_the_playing_pasted_clips_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);

        let src = clip_with_open_note(0);
        let src_id = src.id();
        sequencer.tracks_mut()[0].add_clip(&src);
        select(&mut sequencer, 0, Some(src_id));
        sequencer.copy_clips_to_clipboard(src.start_tick(), src.end_tick());

        set_cursor(&sequencer, 2000);
        let mut edit = PasteClipsEdit::from_sequencer(&sequencer).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        let pasted_start = pasted[0].start_tick;

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(pasted_start);

        // One tick sounds the pasted clip's note-on.
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        edit.undo(&mut sequencer);
        sequencer.tick(Instant::now());

        // The stranded note gets a real note-off.
        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
