//! `⇧⌘/Ctrl+M` (Ableton's Insert MIDI Clip) — an empty clip on every track
//! the marquee spans, or one bar at the cursor on the selected track. Not an
//! edit type of its own: it builds a [`PasteClipsEdit`] of the empty clips,
//! one undo step for them all.

use crate::core::input_event::TimeSelectionRect;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::PasteLead;
use super::paste::PasteClipsEdit;

impl PasteClipsEdit {
    /// Empty clips over the marquee's tick range on every track it spans
    /// (`track_start..=track_end`), or, with no marquee or a zero-width one,
    /// one bar at the cursor on the selected track. All or nothing, as in
    /// Ableton: `None` (nothing enters the record) when any of them would
    /// overlap a clip already there, a spanned track doesn't exist, or there
    /// is no marquee and no selected track. Nothing is carved: every clip
    /// was checked to fit. The lead is the new clip on the selected track;
    /// when the selected track is outside the marquee the lead stays as it
    /// was, since making another track's clip the lead would select that
    /// track (`PasteLead::KeepSelection`). The marquee is kept either way.
    pub(crate) fn empty_clips(
        sequencer: &Sequencer,
        time_bounds: Option<TimeSelectionRect>,
    ) -> Option<Self> {
        let selected = sequencer.selected_track_index();
        let (start, length, track_range) = match time_bounds {
            Some(rect) if rect.has_tick_range() => (
                rect.start,
                rect.end - rect.start,
                rect.track_start..=rect.track_end,
            ),
            _ => {
                let track_idx = selected?;
                (
                    sequencer.cursor_tick(),
                    sequencer.meter().bar_ticks(),
                    track_idx..=track_idx,
                )
            }
        };

        let targets = track_range
            .map(|track_idx| {
                let mut clip = Clip::new();
                clip.set_start_tick(start);
                clip.region_mut().set_region(None, Some(length));
                let fits = sequencer.tracks().get(track_idx)?.fits(&clip);
                fits.then_some((track_idx, clip))
            })
            .collect::<Option<Vec<_>>>()?;

        let lead = targets
            .iter()
            .find(|(track_idx, _)| Some(*track_idx) == selected)
            .map_or(PasteLead::KeepSelection, |(track_idx, clip)| {
                PasteLead::Clip {
                    track_idx: *track_idx,
                    clip_id: clip.id(),
                }
            });
        Self::from_targets(sequencer, targets, lead)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::core::sequencer::edit::EditResult;
    use crate::core::sequencer::test_support::{clip_at, note_off, note_on, test_sequencer};
    use crate::core::time::Meter;

    use super::*;

    /// A test sequencer with track `selected` selected.
    fn selecting(selected: usize) -> Sequencer {
        let mut sequencer = test_sequencer();
        let track_id = sequencer.track_id_by_index(selected).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer
    }

    fn marquee(start: i32, end: i32, tracks: (usize, usize)) -> Option<TimeSelectionRect> {
        Some(TimeSelectionRect {
            start,
            end,
            track_start: tracks.0,
            track_end: tracks.1,
        })
    }

    fn spans(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    /// Applies the insert and returns the `(track, start, end)` of each new
    /// clip and the lead it names.
    fn insert(
        sequencer: &mut Sequencer,
        time_bounds: Option<TimeSelectionRect>,
    ) -> (PasteClipsEdit, Vec<(usize, i32, i32)>, PasteLead) {
        let mut edit = PasteClipsEdit::empty_clips(sequencer, time_bounds).expect("room for it");
        let EditResult::ClipsPasted {
            pasted,
            lead,
            carve_updated,
            carve_added,
            carve_removed,
        } = edit.edit(sequencer)
        else {
            panic!("expected ClipsPasted");
        };
        assert!(
            carve_updated.is_empty() && carve_added.is_empty() && carve_removed.is_empty(),
            "an empty-clip insert never carves"
        );
        let placed = pasted
            .iter()
            .map(|c| (c.track_idx, c.start_tick, c.end_tick))
            .collect();
        (edit, placed, lead)
    }

    fn lead_track(lead: PasteLead) -> usize {
        let PasteLead::Clip { track_idx, .. } = lead else {
            panic!("expected PasteLead::Clip, got {lead:?}");
        };
        track_idx
    }

    #[test]
    fn a_marquee_over_several_tracks_puts_an_empty_clip_on_each_as_one_step() {
        let mut sequencer = selecting(1);
        let (mut edit, placed, lead) = insert(&mut sequencer, marquee(960, 2880, (0, 2)));

        assert_eq!(placed, vec![(0, 960, 2880), (1, 960, 2880), (2, 960, 2880)]);
        for track_idx in 0..=2 {
            let clip = &sequencer.tracks()[track_idx].clips()[0];
            assert!(clip.events().is_empty());
        }
        assert_eq!(lead_track(lead), 1, "the selected track's clip is the lead");

        edit.undo(&mut sequencer);
        for track_idx in 0..=2 {
            assert!(
                spans(&sequencer, track_idx).is_empty(),
                "one undo lifts all"
            );
        }

        edit.edit(&mut sequencer);
        for track_idx in 0..=2 {
            assert_eq!(spans(&sequencer, track_idx), vec![(960, 2880)]);
        }
    }

    /// Naming another track's clip the lead would select that track, so the
    /// selection stays as it is.
    #[test]
    fn the_selection_is_kept_when_the_selected_track_is_outside_the_marquee() {
        let mut sequencer = selecting(0);
        let (_, placed, lead) = insert(&mut sequencer, marquee(0, 960, (1, 2)));
        assert_eq!(placed, vec![(1, 0, 960), (2, 0, 960)]);
        assert_eq!(lead, PasteLead::KeepSelection);
        assert!(spans(&sequencer, 0).is_empty());
    }

    /// All or nothing, as in Ableton: one occupied track refuses the whole
    /// insert, the empty tracks included.
    #[test]
    fn a_collision_on_any_spanned_track_refuses_the_whole_insert() {
        let mut sequencer = selecting(0);
        sequencer.tracks_mut()[2].add_clip(&clip_at(1920, 960));

        assert!(PasteClipsEdit::empty_clips(&sequencer, marquee(960, 2880, (0, 2))).is_none());
        assert!(PasteClipsEdit::empty_clips(&sequencer, marquee(2400, 3000, (2, 2))).is_none());
        let (_, placed, _) = insert(&mut sequencer, marquee(960, 1920, (0, 2)));
        assert_eq!(
            placed.len(),
            3,
            "touching the next clip is adjacent, not a collision"
        );
    }

    #[test]
    fn a_marquee_past_the_last_track_refuses_the_insert() {
        let sequencer = selecting(0);
        let past = sequencer.tracks().len();
        assert!(PasteClipsEdit::empty_clips(&sequencer, marquee(0, 960, (0, past))).is_none());
    }

    /// No marquee, or a zero-width (track-only) one spanning other tracks:
    /// one bar at the cursor, on the selected track only.
    #[test]
    fn without_a_tick_range_it_is_one_bar_at_the_cursor_on_the_selected_track() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        for time_bounds in [None, marquee(bar * 3, bar * 3, (0, 2))] {
            let mut sequencer = selecting(1);
            sequencer.cursor_tick.store(bar, Ordering::Relaxed);
            let (_, placed, lead) = insert(&mut sequencer, time_bounds);
            assert_eq!(placed, vec![(1, bar, bar * 2)]);
            assert_eq!(lead_track(lead), 1);
        }
    }

    #[test]
    fn one_bar_follows_the_meter() {
        let mut sequencer = selecting(0);
        let three_four = Meter::new(3, 4).unwrap();
        sequencer.set_meter(three_four);
        let (_, placed, _) = insert(&mut sequencer, None);
        assert_eq!(placed, vec![(0, 0, three_four.bar_ticks())]);
    }

    #[test]
    fn one_bar_at_the_cursor_is_refused_when_it_reaches_a_clip() {
        let mut sequencer = selecting(0);
        sequencer.tracks_mut()[0].add_clip(&clip_at(1920, 960));
        assert!(PasteClipsEdit::empty_clips(&sequencer, None).is_none());
    }

    #[test]
    fn without_a_marquee_it_needs_a_selected_track() {
        let sequencer = test_sequencer();
        assert!(PasteClipsEdit::empty_clips(&sequencer, None).is_none());
    }

    /// An empty clip leaves a take in the capture buffer alone.
    #[test]
    fn it_never_consumes_the_take() {
        let mut sequencer = selecting(0);
        sequencer.capture_clip.add_event(note_on(600));
        sequencer.capture_clip.add_event(note_off(900));
        insert(&mut sequencer, marquee(960, 1920, (0, 0)));
        assert_eq!(sequencer.capture_clip.events().len(), 2);
    }
}
