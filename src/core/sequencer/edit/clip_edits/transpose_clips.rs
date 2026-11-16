//! Undoable marquee transpose (`⌥↑`/`⌥↓`, `⇧` for an octave): every note in
//! the clips an arranger marquee covers whole, moved together across its
//! tracks. See `500-marquee-transpose.md`.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::models::event::Event;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::{in_tracks, interior_clip_ids, metadata_for};

// ---------------------------------------------------------------------------
// TransposeClips
// ---------------------------------------------------------------------------

/// Why a marquee transpose was refused: nothing moves and no undo step is
/// recorded, and the footer says why (`500-marquee-transpose.md` decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransposeRefusal {
    /// A clip on the marquee's tracks overlaps its tick range without lying
    /// fully inside it (decision 2).
    CutThrough,
    /// A note in a covered clip would go below 0 or above 127 (decision 6).
    OutOfRange,
    /// No clip lies inside the marquee, or none of those holds a note.
    NothingCovered,
}

impl TransposeRefusal {
    /// The footer line.
    pub(crate) fn message(self) -> &'static str {
        match self {
            TransposeRefusal::CutThrough => "The marquee cuts through a clip",
            TransposeRefusal::OutOfRange => "A note would leave the MIDI range",
            TransposeRefusal::NothingCovered => "No clips in the marquee",
        }
    }
}

/// Every note in each clip lying fully inside the marquee, on its tracks,
/// moved `semitones` (`Clip::transpose_all_notes`): exactly what `⌘A` then
/// `↑`/`↓` does in each clip, done together as one undo step. Each clip goes
/// through [`Sequencer::edit_clip_events`], so a note moved while it sounds
/// is released, and keeps its event list from before for undo. The clips'
/// event selections, the marquee and the transport are left alone.
pub(crate) struct TransposeClipsEdit {
    /// How far every note moves.
    semitones: i32,
    /// `(track, clip)` of every covered clip holding a note, found once at
    /// construction; redo trusts them, the undo stack being linear.
    targets: Vec<(usize, Uuid)>,
    /// Each target's event list before the last `edit()`, one per target in
    /// `targets` order (`None` for a clip gone), for `undo()`: shared with
    /// the clip until the transpose writes it (`Clip::shared_events`).
    pre_events: Vec<Option<Arc<Vec<Event>>>>,
}

impl TransposeClipsEdit {
    /// Builds the edit for `rect`, or the refusal: a clip the marquee cuts
    /// through, no covered clip with a note (a rect without a tick range
    /// covers none), or a note that would leave MIDI's range. A clip
    /// touching the range only at an edge neither moves nor refuses.
    pub(crate) fn from_marquee(
        sequencer: &Sequencer,
        rect: TimeSelectionRect,
        semitones: i32,
    ) -> Result<Self, TransposeRefusal> {
        if !rect.has_tick_range() {
            return Err(TransposeRefusal::NothingCovered);
        }
        let (start, end) = (rect.start, rect.end);
        let tracks = Some((rect.track_start, rect.track_end));

        let cuts_through = sequencer
            .tracks()
            .iter()
            .enumerate()
            .filter(|&(track_idx, _)| in_tracks(tracks, track_idx))
            .any(|(_, track)| {
                let within = track.find_clip_ids_within(start, end);
                track
                    .find_clip_ids_in(start, end)
                    .iter()
                    .any(|id| !within.contains(id))
            });
        if cuts_through {
            return Err(TransposeRefusal::CutThrough);
        }

        let mut targets = Vec::new();
        let mut fits = true;
        for (track_idx, id) in interior_clip_ids(sequencer, tracks, start, end) {
            if let Some(clip) = sequencer.clip_on(track_idx, id)
                && clip.has_notes()
            {
                fits &= clip.notes_fit_transposed(semitones);
                targets.push((track_idx, id));
            }
        }
        if targets.is_empty() {
            return Err(TransposeRefusal::NothingCovered);
        }
        if !fits {
            return Err(TransposeRefusal::OutOfRange);
        }

        Ok(Self {
            semitones,
            targets,
            pre_events: Vec::new(),
        })
    }

    /// Snapshots each target's events and moves all its notes. Returns
    /// [`EditResult::ClipsTransposed`].
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let semitones = self.semitones;
        self.pre_events = self
            .targets
            .iter()
            .map(|&(track_idx, id)| {
                sequencer.edit_clip_events(track_idx, id, |clip| {
                    let pre_events = Arc::clone(clip.shared_events());
                    clip.transpose_all_notes(semitones);
                    pre_events
                })
            })
            .collect();
        self.transposed(sequencer)
    }

    /// Puts back each target's events from before. Returns
    /// [`EditResult::ClipsTransposed`].
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let pre_events = std::mem::take(&mut self.pre_events);
        for (&(track_idx, id), events) in self.targets.iter().zip(pre_events) {
            let Some(events) = events else {
                continue;
            };
            sequencer.edit_clip_events(track_idx, id, |clip| {
                clip.restore_shared_events(events);
            });
        }
        self.transposed(sequencer)
    }

    /// `ClipsTransposed` with every target's metadata now, or `NoOp` with
    /// all of them gone.
    fn transposed(&self, sequencer: &Sequencer) -> EditResult {
        let ids: Vec<Uuid> = self.targets.iter().map(|&(_, id)| id).collect();
        let clips = metadata_for(sequencer, &ids, &HashSet::new());
        if clips.is_empty() {
            return EditResult::NoOp;
        }
        EditResult::ClipsTransposed { clips }
    }
}

#[cfg(test)]
mod tests {

    use crate::core::config::MAX_TRACKS;
    use crate::core::sequencer::test_support::{clip_at, rect, test_sequencer};
    use crate::models::clip::Clip;

    use super::*;

    /// `clip_at` with one note of `pitch` at its start, 120 ticks long.
    fn clip_with_note(start_tick: i32, length: i32, pitch: u8) -> Clip {
        let mut clip = clip_at(start_tick, length);
        clip.add_event(Event::new(0, 0, vec![0x90, pitch, 100]));
        clip.add_event(Event::new(120, 0, vec![0x80, pitch, 0]));
        clip
    }

    /// Every note pitch on `track_idx`, clip by clip in start order.
    fn pitches(sequencer: &Sequencer, track_idx: usize) -> Vec<Vec<u8>> {
        let mut clips: Vec<&Clip> = sequencer.tracks()[track_idx].clips().iter().collect();
        clips.sort_by_key(|clip| clip.start_tick());
        clips
            .iter()
            .map(|clip| {
                clip.events()
                    .iter()
                    .filter_map(Event::note_number)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn moves_every_covered_clip_across_tracks_and_undoes() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 960, 60));
        sequencer.tracks_mut()[1].add_clip(&clip_with_note(960, 480, 64));
        sequencer.tracks_mut()[1].add_clip(&clip_with_note(1440, 480, 67));
        // Outside the marquee's tracks: stays.
        sequencer.tracks_mut()[2].add_clip(&clip_with_note(960, 960, 70));

        let mut edit =
            TransposeClipsEdit::from_marquee(&sequencer, rect(960, 1920, 0, 1), 12).unwrap();
        let result = edit.edit(&mut sequencer);

        let EditResult::ClipsTransposed { clips } = result else {
            panic!("expected ClipsTransposed");
        };
        assert_eq!(clips.len(), 3);
        assert_eq!(pitches(&sequencer, 0), vec![vec![72, 72]]);
        assert_eq!(pitches(&sequencer, 1), vec![vec![76, 76], vec![79, 79]]);
        assert_eq!(pitches(&sequencer, 2), vec![vec![70, 70]]);

        edit.undo(&mut sequencer);
        assert_eq!(pitches(&sequencer, 0), vec![vec![60, 60]]);
        assert_eq!(pitches(&sequencer, 1), vec![vec![64, 64], vec![67, 67]]);

        edit.edit(&mut sequencer);
        assert_eq!(pitches(&sequencer, 0), vec![vec![72, 72]]);
        assert_eq!(pitches(&sequencer, 1), vec![vec![76, 76], vec![79, 79]]);
    }

    /// Clips ending on the marquee's start or starting on its end only touch
    /// it: they neither move nor refuse the press.
    #[test]
    fn edge_touching_clips_neither_move_nor_refuse() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(0, 960, 50));
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 960, 60));
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(1920, 960, 70));

        let mut edit =
            TransposeClipsEdit::from_marquee(&sequencer, rect(960, 1920, 0, 0), -1).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(
            pitches(&sequencer, 0),
            vec![vec![50, 50], vec![59, 59], vec![70, 70]]
        );
    }

    #[test]
    fn a_clip_cut_through_on_any_marqueed_track_refuses() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 960, 60));
        sequencer.tracks_mut()[1].add_clip(&clip_with_note(480, 960, 64));

        assert_eq!(
            TransposeClipsEdit::from_marquee(&sequencer, rect(960, 1920, 0, 1), 1).err(),
            Some(TransposeRefusal::CutThrough)
        );
        // Off the marquee's tracks, the same clip is no reason to refuse.
        assert!(TransposeClipsEdit::from_marquee(&sequencer, rect(960, 1920, 0, 0), 1).is_ok());
    }

    /// A clip with no length on the marquee's start edge lies inside it
    /// without overlapping it: nothing is cut, so the press isn't refused.
    #[test]
    fn a_zero_length_clip_on_the_edge_is_not_a_cut() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(960, 960, 60));
        sequencer.tracks_mut()[1].add_clip(&clip_at(960, 0));

        assert!(TransposeClipsEdit::from_marquee(&sequencer, rect(960, 1920, 0, 1), 1).is_ok());
    }

    #[test]
    fn a_note_leaving_the_midi_range_refuses() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_with_note(0, 960, 60));
        sequencer.tracks_mut()[1].add_clip(&clip_with_note(0, 960, 120));

        assert_eq!(
            TransposeClipsEdit::from_marquee(&sequencer, rect(0, 960, 0, 1), 12).err(),
            Some(TransposeRefusal::OutOfRange)
        );
        assert!(TransposeClipsEdit::from_marquee(&sequencer, rect(0, 960, 0, 1), 7).is_ok());
    }

    #[test]
    fn nothing_covered_refuses() {
        let mut sequencer = test_sequencer();
        // Covered, but empty.
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 960));

        for marquee in [
            rect(0, 960, 0, MAX_TRACKS - 1),
            rect(1920, 2880, 0, MAX_TRACKS - 1),
            rect(480, 480, 0, MAX_TRACKS - 1),
        ] {
            assert_eq!(
                TransposeClipsEdit::from_marquee(&sequencer, marquee, 1).err(),
                Some(TransposeRefusal::NothingCovered)
            );
        }
    }
}
