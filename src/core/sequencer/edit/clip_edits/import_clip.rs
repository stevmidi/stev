//! The MIDI clip import — a `.mid` dropped on a track lane (from the file
//! manager or the browser panel) or put on the selected track at the cursor
//! with Enter in the browser. Not an edit type of its own: it builds a
//! [`PasteClipsEdit`] of the one imported clip. See `060-persistence.md`
//! § MIDI clip import.

use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::PasteLead;
use super::paste::PasteClipsEdit;

impl PasteClipsEdit {
    /// Puts `clip` (built by [`Clip::imported`]) on `track_idx` at
    /// `start_tick`, as a fresh clip: the paste carves what it overlaps, like
    /// `⌘/Ctrl+V`, and owns freeze-for-redo and undo. It lands like a dropped
    /// clip band drag — the cursor on its start, its track selected, it the
    /// lead, its span marqueed (`PasteLead::ClipSpan`). `None` past the last
    /// track.
    pub(crate) fn importing(
        sequencer: &Sequencer,
        track_idx: usize,
        start_tick: i32,
        mut clip: Clip,
    ) -> Option<Self> {
        clip.generate_new_id();
        clip.set_start_tick(start_tick.max(0));
        let lead = PasteLead::ClipSpan {
            track_idx,
            clip_id: clip.id(),
        };
        Self::from_targets(sequencer, vec![(track_idx, clip)], lead)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::edit::EditResult;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::core::time::{Meter, PPQN, bars_to_ticks};
    use crate::models::event::Event;

    use super::*;

    /// A one-bar imported clip holding one beat-long note.
    fn imported() -> Clip {
        let events = vec![
            Event::new(0, 0, vec![0x90, 60, 100]),
            Event::new(PPQN, 0, vec![0x80, 60, 0]),
        ];
        Clip::imported(events, 0, Meter::FOUR_FOUR).unwrap()
    }

    fn spans(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    #[test]
    fn lands_on_the_track_at_the_tick_and_names_itself_the_lead_span() {
        let mut sequencer = test_sequencer();
        let bar = bars_to_ticks(1);

        let mut edit = PasteClipsEdit::importing(&sequencer, 2, bar, imported()).unwrap();
        let EditResult::ClipsPasted { pasted, lead, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };

        assert_eq!(spans(&sequencer, 2), vec![(bar, bar * 2)]);
        assert_eq!(
            lead,
            PasteLead::ClipSpan {
                track_idx: 2,
                clip_id: pasted[0].clip_id
            }
        );
        assert_eq!(sequencer.tracks()[2].clips()[0].events().len(), 2);
    }

    #[test]
    fn carves_what_it_lands_on_and_undo_puts_it_back() {
        let mut sequencer = test_sequencer();
        let bar = bars_to_ticks(1);
        let existing = clip_at(0, bar * 4);
        sequencer.tracks_mut()[0].add_clip(&existing);

        let mut edit = PasteClipsEdit::importing(&sequencer, 0, bar, imported()).unwrap();
        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted");
        };
        let imported_id = pasted[0].clip_id;
        assert_eq!(
            spans(&sequencer, 0),
            vec![(0, bar), (bar, bar * 2), (bar * 2, bar * 4)]
        );

        edit.undo(&mut sequencer);
        assert_eq!(spans(&sequencer, 0), vec![(0, bar * 4)]);
        assert_eq!(sequencer.tracks()[0].clips()[0].id(), existing.id());

        let EditResult::ClipsPasted { pasted, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsPasted on redo");
        };
        assert_eq!(pasted[0].clip_id, imported_id, "redo keeps the clip id");
    }

    #[test]
    fn importing_the_same_file_twice_gives_two_clips() {
        let mut sequencer = test_sequencer();
        let bar = bars_to_ticks(1);
        let clip = imported();
        PasteClipsEdit::importing(&sequencer, 0, 0, clip.clone())
            .unwrap()
            .edit(&mut sequencer);
        PasteClipsEdit::importing(&sequencer, 0, bar, clip)
            .unwrap()
            .edit(&mut sequencer);

        let clips = sequencer.tracks()[0].clips();
        assert_eq!(spans(&sequencer, 0), vec![(0, bar), (bar, bar * 2)]);
        assert_ne!(clips[0].id(), clips[1].id());
    }

    #[test]
    fn nothing_past_the_last_track() {
        let sequencer = test_sequencer();
        let past = sequencer.tracks().len();
        assert!(PasteClipsEdit::importing(&sequencer, past, 0, imported()).is_none());
    }
}
