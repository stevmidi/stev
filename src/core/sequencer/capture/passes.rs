//! The pass grids of a looping running take: which pass of the loop each
//! note was played in, and one pass framed as the running `/` frames it,
//! into a new clip's content ([`PassGrid`]) or the events an insert lands in
//! the lead clip ([`InsertGrid`]). The `/` frames the last pass; `,`/`.`
//! frame the earlier ones from the buffer the edit kept
//! (`archive/540-earlier-pass.md`).

use std::iter;

use crate::core::config;
use crate::models::clip::Clip;
use crate::models::event::{Event, EventType};

use super::{Sequencer, insert_lands};

/// What `,`/`.` read from a kept capture buffer through its grid: the pass
/// `/` took and the passes there are to step through.
pub(crate) trait PassFraming {
    /// The pass the running `/` takes from `buffer`.
    fn last_pass(&self, buffer: &Clip) -> i32;

    /// The passes of `buffer` to step through, in order, up to and including
    /// `last`: a pass spent listening isn't one (`archive/540-earlier-pass.md`
    /// decision 7).
    fn passes(&self, buffer: &Clip, last: i32) -> Vec<i32>;
}

/// The grid a looping new-clip commit was framed on, kept with it
/// so a later loop change doesn't shift the passes (`archive/540-earlier-pass.md`
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

impl PassFraming for PassGrid {
    /// The one holding `buffer`'s most recently played note-on
    /// (`100-running-capture.md` § What a new clip retains).
    fn last_pass(&self, buffer: &Clip) -> i32 {
        self.pass_of(Sequencer::take_framing_tick(buffer))
    }

    /// The passes holding a note-on.
    fn passes(&self, buffer: &Clip, last: i32) -> Vec<i32> {
        counted(
            buffer
                .events()
                .iter()
                .filter(|event| event.event_type() == Some(EventType::NoteOn))
                .map(|event| self.pass_of(event.tick())),
            last,
        )
    }
}

/// `passes` sorted and deduplicated, those after `last` dropped and `last`
/// itself always among them.
fn counted(passes: impl Iterator<Item = i32>, last: i32) -> Vec<i32> {
    let mut passes: Vec<i32> = passes
        .filter(|&pass| pass < last)
        .chain(iter::once(last))
        .collect();
    passes.sort_unstable();
    passes.dedup();
    passes
}

/// How a running insert into the lead clip lands a pass
/// ([`Sequencer::build_running_capture_insert`]), kept with the insert so
/// `,`/`.` land an earlier pass as the `/` landed the last
/// (`archive/540-earlier-pass.md` phase 2). Unlike a new clip's [`PassGrid`], the
/// passes are windows of the clip's length pinned at the anchor's phase:
/// pass `k` starts at `anchor + k · loop_len`. A linear take has one window,
/// from the anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InsertGrid {
    /// Where the window starts, in the capture clock: the first arrangement
    /// tick of the clip playback goes through.
    pub(super) anchor: i32,
    /// The loop's length while looping, at least a beat; `None` linear.
    pub(super) loop_len: Option<i32>,
    /// The anchor's phase in the loop (0 linear).
    pub(super) phase: i32,
    /// The window's length: the clip's region.
    pub(super) region_length: i32,
    /// Where the window's tick 0 lands, in the clip's event ticks.
    pub(super) insert_at: i32,
    /// How much of the window lands: up to the clip's end.
    pub(super) room: i32,
    /// Where the wheels the take moves latch, in the clip's event ticks.
    pub(super) wheel_end: i32,
    /// The punch-in span, in the window's ticks.
    pub(super) punch_in: Option<(i32, i32)>,
}

impl InsertGrid {
    /// Whether the take looped, so it has passes to step through.
    pub(crate) fn loops(&self) -> bool {
        self.loop_len.is_some()
    }

    /// Where the wheels the take moves latch, in the clip's event ticks
    /// ([`Clip::land_take`]).
    pub(crate) fn wheel_end(&self) -> i32 {
        self.wheel_end
    }

    /// The window a tick at `tick` (capture clock) falls in.
    fn window_of(&self, tick: i32, loop_len: i32) -> i32 {
        (tick - self.anchor).div_euclid(loop_len)
    }

    /// The pass a note-on at `tick` lands in, or `None` when it lands in
    /// none, as [`land`](Self::land) places it: its window, at its offset
    /// into it. A note within
    /// [`LATE_NOTE_TOLERANCE_TICKS`](config::LATE_NOTE_TOLERANCE_TICKS) of a
    /// loop wrap anticipates the downbeat after it, and is pulled onto the
    /// start of the loop pass there ([`frame`](Self::frame)): the start of
    /// the next window when the anchor is the loop start, before every
    /// window otherwise, where the crop drops it.
    fn pass_of_note(&self, tick: i32, loop_len: i32) -> Option<i32> {
        let loop_phase = (tick - self.anchor + self.phase).rem_euclid(loop_len);
        let to_wrap = loop_len - loop_phase;
        if to_wrap <= config::LATE_NOTE_TOLERANCE_TICKS {
            let downbeat = tick + to_wrap;
            return (self.phase == 0 && self.lands_at(0))
                .then(|| self.window_of(downbeat, loop_len));
        }
        self.pass_of_move(tick, loop_len)
    }

    /// The pass an event at `tick` that stays where it was played lands in,
    /// or `None` when its offset into its window falls outside what lands.
    fn pass_of_move(&self, tick: i32, loop_len: i32) -> Option<i32> {
        self.lands_at((tick - self.anchor).rem_euclid(loop_len))
            .then(|| self.window_of(tick, loop_len))
    }

    /// Whether an event `offset` ticks into its window lands: inside the
    /// room before the clip's end, and inside the punch-in when there is
    /// one.
    fn lands_at(&self, offset: i32) -> bool {
        offset < self.room
            && self
                .punch_in
                .is_none_or(|(start, end)| (start..end).contains(&offset))
    }

    /// `capture` framed on pass `pass`, as the running `/` frames `last`,
    /// the pass it took, cropped to the window and rebased to 0: the wheels
    /// held into it carried onto its start, a note within the tolerance of
    /// its loop pass's wrap pulled onto that pass's start (kept only when
    /// the anchor is the loop start). An earlier pass leaves out what the pass after it
    /// claims: the notes just before its loop pass's wrap, and anything past
    /// one loop from its start when the clip is longer than the loop. `None`
    /// when nothing in it lands.
    pub(super) fn frame(&self, mut capture: Clip, pass: i32, last: i32) -> Option<Clip> {
        let tolerance = config::LATE_NOTE_TOLERANCE_TICKS;
        let earlier = pass < last;
        let window_start = self.anchor + self.loop_len.map_or(0, |len| pass * len);
        let window_len = match self.loop_len {
            Some(len) if earlier => self.region_length.min(len),
            _ => self.region_length,
        };
        capture
            .region_mut()
            .set_region(Some(window_start), Some(window_start + window_len));
        capture.sort_events_by_tick();
        capture.carry_resting_onto(window_start);
        capture.calculate_note_lengths();
        if let Some(len) = self.loop_len {
            // The loop pass the window sits in: the window starts at the
            // anchor's phase into it.
            let loop_start = window_start - self.phase;
            let wrap = loop_start + len;
            if earlier {
                capture.drop_notes_starting_in((wrap - tolerance).max(loop_start)..wrap);
            }
            capture.relocate_late_notes_to_region_start(loop_start, wrap, tolerance);
        }
        capture.crop(self.region_length);
        insert_lands(&capture).then_some(capture)
    }

    /// The events pass `pass` of `capture` lands in the clip, in its event
    /// ticks, `last` the pass `/` took: the [framed](Self::frame) window,
    /// punched in, the notes the clip's end would cut dropped. `None` when
    /// nothing lands.
    pub(crate) fn land(&self, capture: Clip, pass: i32, last: i32) -> Option<Vec<Event>> {
        let mut capture = self.frame(capture, pass, last)?;
        if let Some((start, end)) = self.punch_in {
            capture.punch_in(start, end);
        }
        let events = Sequencer::capture_events_to_insert(&capture, self.insert_at, self.room);
        (!events.is_empty()).then_some(events)
    }
}

impl PassFraming for InsertGrid {
    /// The window [`Sequencer::calculate_running_region`] pins: the one
    /// holding `buffer`'s most recently played note-on, or with no note its
    /// last controller move; the one window, linear.
    fn last_pass(&self, buffer: &Clip) -> i32 {
        self.loop_len.map_or(0, |len| {
            let (start, _) = Sequencer::calculate_running_region(
                buffer,
                self.anchor,
                self.region_length,
                Some(len),
            );
            self.window_of(start, len)
        })
    }

    /// The passes a note-on lands in, or in a take of controller moves alone
    /// the passes one lands in: a pass that would land nothing, its notes
    /// all outside the punch-in or past the clip's end, isn't one.
    fn passes(&self, buffer: &Clip, last: i32) -> Vec<i32> {
        let Some(len) = self.loop_len else {
            return vec![last];
        };
        let events = buffer.events().iter();
        if buffer.has_notes() {
            let note_ons = events.filter(|event| event.event_type() == Some(EventType::NoteOn));
            counted(
                note_ons.filter_map(|event| self.pass_of_note(event.tick(), len)),
                last,
            )
        } else {
            let moves = events.filter(|event| event.controller_move().is_some());
            counted(
                moves.filter_map(|event| self.pass_of_move(event.tick(), len)),
                last,
            )
        }
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

    // --- the running insert's grid ---

    /// A running insert into a clip `region_length` long at the loop's
    /// start, over a loop `loop_len` long from 0.
    fn insert_grid(loop_len: i32, region_length: i32) -> InsertGrid {
        InsertGrid {
            anchor: 0,
            loop_len: Some(loop_len),
            phase: 0,
            region_length,
            insert_at: 0,
            room: region_length,
            wheel_end: region_length.min(loop_len),
            punch_in: None,
        }
    }

    /// The note-on ticks pass `pass` of `clip` lands, in the clip's ticks.
    fn landed(grid: InsertGrid, clip: &Clip, pass: i32) -> Vec<i32> {
        grid.land(clip.clone(), pass, grid.last_pass(clip))
            .unwrap_or_default()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(Event::tick)
            .collect()
    }

    #[test]
    fn an_insert_lands_each_window_and_skips_a_silent_one() {
        let grid = insert_grid(BAR, BAR);
        let clip = buffer(&[100, BAR + 200, BAR * 3 + 300]);
        assert_eq!(grid.last_pass(&clip), 3);
        assert_eq!(grid.passes(&clip, 3), vec![0, 1, 3]);
        assert_eq!(landed(grid, &clip, 1), vec![200]);
        assert_eq!(landed(grid, &clip, 3), vec![300]);
    }

    /// An early chord before a wrap starts the pass after it: that pass
    /// counts it and pulls it onto its downbeat, and the pass before
    /// leaves it out.
    #[test]
    fn an_early_chord_is_the_next_windows_downbeat() {
        let grid = insert_grid(BAR, BAR);
        let clip = buffer(&[100, BAR - 40, BAR + 500]);
        assert_eq!(grid.last_pass(&clip), 1);
        assert_eq!(grid.passes(&clip, 1), vec![0, 1]);
        assert_eq!(landed(grid, &clip, 1), vec![0, 500]);
        assert_eq!(landed(grid, &clip, 0), vec![100]);

        // Played last, it frames the pass it ends, as `/` lands it.
        let clip = buffer(&[100, BAR - 40]);
        assert_eq!(grid.last_pass(&clip), 0);
        assert_eq!(grid.passes(&clip, 0), vec![0]);
        assert_eq!(landed(grid, &clip, 0), vec![0, 100]);
    }

    /// A clip longer than the loop: an earlier pass lands one loop of
    /// itself, never the start of the pass after it.
    #[test]
    fn an_earlier_window_stops_at_the_next_pass() {
        let grid = insert_grid(BAR, BAR * 2);
        let clip = buffer(&[100, BAR + 200, BAR * 2 + 300]);
        assert_eq!(landed(grid, &clip, 1), vec![200]);
        assert_eq!(landed(grid, &clip, 2), vec![300]);
    }

    /// A punch-in limits every pass to its span, not only the last.
    #[test]
    fn a_punch_in_frames_every_pass() {
        let grid = InsertGrid {
            punch_in: Some((0, BAR / 2)),
            ..insert_grid(BAR, BAR)
        };
        let clip = buffer(&[100, BAR * 3 / 4, BAR + 200, BAR + BAR * 3 / 4]);
        assert_eq!(landed(grid, &clip, 0), vec![100]);
        assert_eq!(landed(grid, &clip, 1), vec![200]);
    }

    /// A pass whose notes all miss the punch-in lands nothing, so it isn't
    /// one to step to.
    #[test]
    fn a_pass_landing_nothing_is_not_counted() {
        let grid = InsertGrid {
            punch_in: Some((0, BAR / 2)),
            ..insert_grid(BAR, BAR)
        };
        let clip = buffer(&[100, BAR + BAR * 3 / 4, BAR * 2 + 200]);
        assert_eq!(grid.passes(&clip, 2), vec![0, 2]);
    }

    /// With the anchor mid-loop, a note just before the loop's wrap lands in
    /// no window (the relocation puts it before each), so a pass holding
    /// only that note isn't counted either.
    #[test]
    fn an_early_note_off_the_anchor_counts_nowhere() {
        let grid = InsertGrid {
            phase: BAR,
            ..insert_grid(BAR * 2, BAR * 2)
        };
        // Window 0 is loop ticks [BAR, 3·BAR): the wrap sits at its middle.
        let clip = buffer(&[100, BAR - 40, BAR * 3 + 100]);
        assert_eq!(grid.passes(&clip, 1), vec![0, 1]);
        let clip = buffer(&[BAR - 40, BAR * 2 + 100]);
        assert_eq!(grid.passes(&clip, 1), vec![1]);
        assert!(grid.land(clip.clone(), 0, 1).is_none(), "nothing lands");
    }
}
