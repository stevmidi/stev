//! `⌘/Ctrl+J` (Merge Clips, Ableton's Consolidate) — bake the marquee into
//! one clip per marqueed track. Not an edit type of its own: it builds a
//! [`PasteClipsEdit`] of the merged clips.

use crate::core::input_event::TimeSelectionRect;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::PasteLead;
use super::paste::PasteClipsEdit;

// ---------------------------------------------------------------------------
// MergeClips
// ---------------------------------------------------------------------------

impl PasteClipsEdit {
    /// `⌘/Ctrl+J` in the Arranger — merge the clips inside the marquee into
    /// one clip per marqueed track, spanning exactly the marquee's tick
    /// range; never across tracks. Each track's merged clip is baked by
    /// [`Clip::merged`] from the region-windowed `[start, end)` pieces plain
    /// `⌘C` would copy (`Sequencer::copy_range_to_clipboard`), so a clip
    /// sticking out of the range contributes only its inside part, gaps
    /// become silence, and only what plays survives — the merge sounds
    /// exactly as before. The merged clips are pasted back at `start`: the
    /// paste carves the range on each of those tracks (removing the clips
    /// inside, trimming a straddler to the outside part), adds the merged
    /// clip, and owns freeze-for-redo and undo. The merged clip on the
    /// selected track becomes the lead, wherever the cursor sits (it may sit
    /// on the marquee's end, outside the clip); with none there, the lead
    /// stays — it is on the selected track, which nothing carved.
    ///
    /// Tracks with no clip in the range are left alone, and so is a track
    /// whose merge would change nothing: one clip spanning exactly the
    /// range, with no hidden material (`MergedClip::lossless`). `None` if
    /// the tick range is empty/backwards, overlaps no clip on the marqueed
    /// tracks, or the merge would change nothing on every one of them.
    pub(crate) fn merging(sequencer: &Sequencer, rect: TimeSelectionRect) -> Option<Self> {
        let clipboard = sequencer.copy_range_to_clipboard(
            (rect.track_start, rect.track_end),
            rect.start,
            rect.end,
        )?;
        let length = rect.end - rect.start;

        let mut targets = Vec::new();
        for track_idx in rect.track_start..=rect.track_end {
            let pieces: Vec<&Clip> = clipboard
                .clips()
                .iter()
                .filter(|piece| piece.track_idx == track_idx)
                .map(|piece| &piece.clip)
                .collect();
            if pieces.is_empty() {
                continue;
            }
            let merged = Clip::merged(&pieces, length);
            if merged.lossless && is_one_clip_spanning(sequencer, track_idx, rect) {
                continue;
            }

            let mut clip = merged.clip;
            clip.set_start_tick(rect.start);
            targets.push((track_idx, clip));
        }

        let selected_track_idx = sequencer.selected_track_index();
        let lead = targets
            .iter()
            .find(|(track_idx, _)| Some(*track_idx) == selected_track_idx)
            .map(|(_, clip)| clip.id())
            .or_else(|| sequencer.selected_clip_id())
            .map_or(PasteLead::CursorRule, PasteLead::Clip);
        Self::from_targets(sequencer, targets, lead)
    }
}

/// Whether the only clip on `track_idx` overlapping `rect` spans exactly its
/// tick range.
fn is_one_clip_spanning(sequencer: &Sequencer, track_idx: usize, rect: TimeSelectionRect) -> bool {
    let Some(track) = sequencer.tracks().get(track_idx) else {
        return false;
    };
    match track.find_clip_ids_in(rect.start, rect.end)[..] {
        [only] => track
            .get_clip_by_id(only)
            .is_some_and(|clip| clip.start_tick() == rect.start && clip.end_tick() == rect.end),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use crate::core::sequencer::test_support::{
        clip_at, drain, instrument_track_0, rect, sequencer_with, test_sequencer,
    };
    use crate::models::event::{Event, EventType};

    use crate::core::sequencer::edit::EditResult;

    use super::*;

    /// A `clip_at(start_tick, length)` with one note `(on, off)` of `pitch`.
    fn clip_with_note(start_tick: i32, length: i32, note: (i32, i32), pitch: u8) -> Clip {
        let mut clip = clip_at(start_tick, length);
        clip.add_event(Event::new(note.0, 0, vec![0x90, pitch, 100]));
        clip.add_event(Event::new(note.1, 0, vec![0x80, pitch, 0]));
        clip.calculate_note_lengths();
        clip
    }

    fn spans(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    /// `(arrangement tick, pitch)` of every note-on on the track.
    fn note_starts(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, u8)> {
        let mut starts: Vec<(i32, u8)> = sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .flat_map(|clip| {
                clip.events()
                    .iter()
                    .filter(|e| e.event_type() == Some(EventType::NoteOn))
                    .filter(|e| clip.is_in_window(e.tick()))
                    .map(|e| {
                        (
                            clip.arrangement_tick_from_event_tick(e.tick()),
                            e.note_number().unwrap(),
                        )
                    })
            })
            .collect();
        starts.sort();
        starts
    }

    #[test]
    fn is_none_on_empty_range_or_no_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 480));
        assert!(PasteClipsEdit::merging(&sequencer, rect(480, 480, 0, 0)).is_none());
        assert!(PasteClipsEdit::merging(&sequencer, rect(960, 1920, 0, 0)).is_none());
        assert!(PasteClipsEdit::merging(&sequencer, rect(0, 480, 1, 2)).is_none());
    }

    #[test]
    fn merges_clips_and_the_gap_between_them_into_one_and_undo_restores_them() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(0, 480, (0, 240), 60));
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 480, (100, 200), 62));
        let before = note_starts(&sequencer, 0);

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(0, 1440, 0, 0)).unwrap();
        let EditResult::ClipsPasted {
            pasted: merged,
            carve_removed,
            lead,
            ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("expected ClipsPasted");
        };
        assert_eq!(
            lead,
            PasteLead::CursorRule,
            "no selected track: the cursor rule decides"
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(carve_removed.len(), 2);
        assert_eq!(spans(&sequencer, 0), vec![(0, 1440)]);
        assert_eq!(note_starts(&sequencer, 0), before, "sounds as before");

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, 480), (960, 1440)]);

        edit.edit(&mut sequencer);
        assert_eq!(
            sequencer.tracks()[0].clips()[0].id(),
            merged[0].clip_id,
            "same id on redo"
        );
    }

    /// A clip sticking out of the range keeps its outside part as its own
    /// clip; only the inside part is merged.
    #[test]
    fn a_straddling_clip_keeps_its_outside_part() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(0, 960, (100, 200), 60));
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 960, (100, 200), 62));

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(480, 1440, 0, 0)).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(
            spans(&sequencer, 0),
            vec![(0, 480), (480, 1440), (1440, 1920)]
        );
        assert_eq!(note_starts(&sequencer, 0), vec![(100, 60), (1060, 62)]);
    }

    /// One merged clip per marqueed track — never across tracks; an empty
    /// marqueed track gets nothing and a track outside the marquee is
    /// untouched.
    #[test]
    fn merges_each_marqueed_track_on_its_own() {
        let mut sequencer = test_sequencer();
        for track_idx in [0, 2, 3] {
            sequencer.tracks_mut()[track_idx].add_clip(&clip_at(0, 480));
            sequencer.tracks_mut()[track_idx].add_clip(&clip_at(480, 480));
        }

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(0, 960, 0, 2)).unwrap();
        let EditResult::ClipsPasted { pasted: merged, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };

        let merged_tracks: Vec<usize> = merged.iter().map(|m| m.track_idx).collect();
        assert_eq!(merged_tracks, vec![0, 2]);
        assert_eq!(spans(&sequencer, 0), vec![(0, 960)]);
        assert!(spans(&sequencer, 1).is_empty());
        assert_eq!(spans(&sequencer, 2), vec![(0, 960)]);
        assert_eq!(spans(&sequencer, 3), vec![(0, 480), (480, 960)]);
    }

    /// One clip spanning exactly the range with nothing hidden: merging
    /// would change nothing, so there is no edit (and no undo step).
    #[test]
    fn one_clip_spanning_the_range_with_nothing_hidden_is_no_edit() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(480, 480, (0, 240), 60));
        assert!(PasteClipsEdit::merging(&sequencer, rect(480, 960, 0, 0)).is_none());
    }

    /// The same clip with hidden material is cropped: the hidden note goes.
    #[test]
    fn one_clip_with_hidden_material_is_cropped() {
        let mut sequencer = test_sequencer();
        let mut clip = clip_with_note(480, 480, (0, 240), 60);
        clip.add_event(Event::new(600, 0, vec![0x90, 64, 100])); // past the window
        clip.add_event(Event::new(700, 0, vec![0x80, 64, 0]));
        clip.sort_events_by_tick();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(480, 960, 0, 0)).unwrap();
        edit.edit(&mut sequencer);

        let merged = &sequencer.tracks()[0].clips()[0];
        assert_eq!((merged.start_tick(), merged.end_tick()), (480, 960));
        assert_eq!(merged.events().len(), 2, "only the note that plays");
    }

    /// A marquee wider than the clip pads it with silence.
    #[test]
    fn a_wider_range_pads_one_clip_with_silence() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(480, 480, (0, 240), 60));

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(0, 1920, 0, 0)).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(spans(&sequencer, 0), vec![(0, 1920)]);
        assert_eq!(note_starts(&sequencer, 0), vec![(480, 60)]);
    }

    /// The merged clip on the selected track becomes the lead, wherever the
    /// cursor sits (here on the marquee's end, outside the clip).
    #[test]
    fn the_merged_clip_on_the_selected_track_is_the_lead() {
        let mut sequencer = test_sequencer();
        let first = clip_at(0, 480);
        sequencer.tracks_mut()[1].add_clip(&first);
        sequencer.tracks_mut()[1].add_clip(&clip_at(480, 480));
        let track_id = sequencer.track_id_by_index(1).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(Some(first.id()));

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(0, 960, 0, 1)).unwrap();
        let EditResult::ClipsPasted { pasted, lead, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };

        assert_eq!(lead, PasteLead::Clip(pasted[0].clip_id));
    }

    /// Merging while a merged-away clip's note sounds must not hang it.
    #[test]
    fn merging_during_playback_releases_a_sounding_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(0, 960, (0, 480), 60));
        sequencer.tracks_mut()[0].add_clip(&clip_at(960, 960));

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        let mut edit = PasteClipsEdit::merging(&sequencer, rect(0, 1920, 0, 0)).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
