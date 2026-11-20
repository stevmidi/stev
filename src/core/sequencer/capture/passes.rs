//! The pass grid of a looping running take: which pass of the loop each
//! note was played in, and one pass framed into a new clip's content as the
//! running `/` frames it. The `/` frames the last pass
//! ([`normalize_running_capture`](Sequencer::normalize_running_capture));
//! `,`/`.` frame the earlier ones from the buffer the commit kept
//! (`540-earlier-pass.md`).

use crate::core::config;
use crate::models::clip::Clip;
use crate::models::event::EventType;

use super::Sequencer;

/// The grid a looping new-clip commit was framed on, kept with it
/// so a later loop change doesn't shift the passes (`540-earlier-pass.md`
/// decision 6). Passes are numbered in the capture clock's ticks, read
/// [`LATE_NOTE_TOLERANCE_TICKS`](config::LATE_NOTE_TOLERANCE_TICKS) early:
/// a note just before a wrap anticipates the downbeat after it and belongs
/// to the next pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PassGrid {
    /// The loop region's start.
    loop_start: i32,
    /// The loop's length, at least a beat.
    loop_len: i32,
    /// Where the clip's region starts in the pass: the anchor's phase.
    phase: i32,
    /// The clip's region length.
    region_length: i32,
}

impl PassGrid {
    /// The grid of a loop from `loop_start` for `loop_len` ticks, the region
    /// `region_length` long from `anchor_tick`'s phase.
    pub(super) fn new(
        loop_start: i32,
        loop_len: i32,
        anchor_tick: i32,
        region_length: i32,
    ) -> Self {
        Self {
            loop_start,
            loop_len,
            phase: (anchor_tick - loop_start).rem_euclid(loop_len),
            region_length,
        }
    }

    /// The pass a note-on at `tick` (capture clock) belongs to.
    fn pass_of(self, tick: i32) -> i32 {
        (tick - (self.loop_start - config::LATE_NOTE_TOLERANCE_TICKS)).div_euclid(self.loop_len)
    }

    /// The pass the running `/` takes from `buffer`: the one holding its most
    /// recently played note-on (`100-running-capture.md` § What a new clip
    /// retains).
    pub(crate) fn last_pass(self, buffer: &Clip) -> i32 {
        self.pass_of(Sequencer::take_framing_tick(buffer))
    }

    /// The passes of `buffer` holding a note-on, in order, up to and
    /// including `last`: a pass spent listening isn't one
    /// (`540-earlier-pass.md` decision 7).
    pub(crate) fn passes(self, buffer: &Clip, last: i32) -> Vec<i32> {
        let mut passes: Vec<i32> = buffer
            .events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| self.pass_of(event.tick()))
            .filter(|&pass| pass <= last)
            .collect();
        passes.sort_unstable();
        passes.dedup();
        passes
    }

    /// `capture` framed on pass `pass`, as the running `/` frames its last
    /// pass: the notes within the tolerance of its end dropped (they are the
    /// next pass's), a note within the tolerance of its start moved onto it,
    /// the wheels held into the pass carried onto its start and the knobs'
    /// start values held there, the pass folded whole into the loop's
    /// phases, the region the slice from the anchor's phase. The rest of the
    /// pass sits outside the region for the edges to reveal.
    pub(crate) fn frame(self, mut capture: Clip, pass: i32) -> Clip {
        let tolerance = config::LATE_NOTE_TOLERANCE_TICKS;
        let pass_start = self.loop_start + pass * self.loop_len;
        let pass_end = pass_start + self.loop_len;
        capture.drop_notes_starting_in((pass_end - tolerance).max(pass_start)..pass_end);
        capture.relocate_late_notes_to_region_start(pass_start, pass_end, tolerance);
        capture.carry_resting_onto(pass_start);
        capture.hold_start_values(pass_start);
        capture.fold_into_loop(pass_start, pass_end, self.loop_start, self.loop_len);
        capture
            .region_mut()
            .set_region(Some(self.phase), Some(self.phase + self.region_length));
        capture
    }
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::test_support::{note_off, note_on};
    use crate::core::time::Meter;

    use super::*;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    fn buffer(note_ons: &[i32]) -> Clip {
        let mut clip = Clip::new();
        for &tick in note_ons {
            clip.add_event(note_on(tick));
            clip.add_event(note_off(tick + 240));
        }
        clip
    }

    #[test]
    fn a_note_just_before_a_wrap_belongs_to_the_next_pass() {
        let grid = PassGrid::new(0, BAR, 0, BAR);
        assert_eq!(grid.pass_of(0), 0);
        assert_eq!(grid.pass_of(BAR - config::LATE_NOTE_TOLERANCE_TICKS - 1), 0);
        assert_eq!(grid.pass_of(BAR - config::LATE_NOTE_TOLERANCE_TICKS), 1);
        assert_eq!(grid.pass_of(BAR), 1);
    }

    #[test]
    fn silent_passes_and_later_ones_are_not_counted() {
        let grid = PassGrid::new(0, BAR, 0, BAR);
        // Passes 0 and 2 played, 1 silent, 4 a stray later note.
        let clip = buffer(&[100, BAR * 2 + 100, BAR * 4 + 100, BAR * 3 + 100]);
        assert_eq!(grid.last_pass(&clip), 3, "the last note-on played");
        assert_eq!(grid.passes(&clip, 3), vec![0, 2, 3]);
    }

    #[test]
    fn the_region_starts_at_the_anchors_phase() {
        let grid = PassGrid::new(BAR, BAR * 2, BAR * 6, BAR);
        let framed = grid.frame(buffer(&[BAR * 5 + 100]), 2);
        assert_eq!(framed.region().start(), BAR);
        assert_eq!(framed.region().end(), BAR * 2);
        let ticks: Vec<i32> = framed.events().iter().map(|e| e.tick()).collect();
        assert_eq!(ticks, vec![100, 340], "pass 2's phases");
    }
}
