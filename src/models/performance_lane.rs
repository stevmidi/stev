//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a key's start
//! instead of capturing notes (`110-performance-lane.md`). What it holds is
//! the **run**: the presses since playback started, on a clock of its own
//! that counts only the ticks playback advances, which `/` keeps as a
//! performance (`520-performance-capture.md` decision 8), and the eight
//! **pads** the kept performances sit on ([`PerformancePads`], decision 6),
//! and the **replay** `⌥Space` plays a pad's performance with, on the same
//! kind of clock (decision 4).
//! [`key_index_from_note`] is the note-number → key mapping the lane triggers
//! on, [`key_tick`] where a key jumps to (one [`KeyLength`] of the project's
//! meter per key: a bar, half a bar or a beat), and [`note_for_key`] the
//! mapping's inverse, which the arranger draws the lane's keys from
//! (`510-performance-lane-keyboard.md`).

use std::array;

use uuid::Uuid;

use crate::core::config;
use crate::core::time::Meter;
use crate::models::{
    clip::Clip,
    event::{Event, EventType},
};

/// How many pads the lane has: the knob panel's eight cells
/// (`520-performance-capture.md` decision 6).
pub(crate) const PAD_COUNT: usize = 8;

/// The lane's live state: the run its presses make, and the replay of a
/// pad's performance. Not saved, not an undo step. See the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformanceLane {
    /// The presses since the run began, `None` with no run.
    run: Option<Run>,
    /// The performance `⌥Space` plays, `None` when none plays.
    replay: Option<Replay>,
}

/// A pad's performance played back through the live trigger path
/// (`520-performance-capture.md` decision 4): each note jumps playback to its
/// key as its press did, on a clock of the replay's own that loops over the
/// performance's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Replay {
    /// The pad it plays.
    pad: usize,
    /// Where in the performance the next tick plays, from its window start.
    elapsed: i32,
    /// Where playback is when only the replay has moved it, as the run's
    /// [`Run::expected_tick`]; `None` before its first tick, which takes
    /// playback from wherever it is.
    expected_tick: Option<i32>,
    /// In a gap between notes, which is silence (decision 2).
    silent: bool,
}

/// What the tick pump does for the replay before playback advances a tick
/// ([`PerformanceLane::step_replay`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayCue {
    /// A note starts: playback jumps to this tick, the notes there chased,
    /// as a press's jump.
    Jump(i32),
    /// A gap starts: what sounds is released, and nothing sounds until the
    /// next note.
    Silence,
}

/// Where the replay is, for the clip view's playhead: its pad and the tick
/// it plays, from the performance's window start. Packed into one atomic
/// ([`to_bits`](Self::to_bits)), `-1` for none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplayPosition {
    /// The pad the replay plays.
    pub(crate) pad: usize,
    /// The tick it is at, from the performance's window start.
    pub(crate) tick: i32,
}

impl ReplayPosition {
    /// `position` as one value for `SharedAtomics::performance_replay`: the
    /// pad above the tick, `-1` for none.
    pub(crate) fn to_bits(position: Option<ReplayPosition>) -> i64 {
        position.map_or(-1, |position| {
            ((position.pad as i64) << 32) | i64::from(position.tick.max(0))
        })
    }

    /// The position [`to_bits`](Self::to_bits) packed; `None` for a
    /// negative value.
    pub(crate) fn from_bits(bits: i64) -> Option<ReplayPosition> {
        (bits >= 0).then_some(ReplayPosition {
            pad: (bits >> 32) as usize,
            tick: (bits & 0xFFFF_FFFF) as i32,
        })
    }
}

/// One run of presses (`520-performance-capture.md` decision 8): from the
/// press that started it, each press stamped with the run's own clock.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    /// Ticks playback has advanced since the run's first press: musical time
    /// while running, never the transport position (it jumps) nor the
    /// clock's (realigned at every jump, `150`).
    elapsed: i32,
    /// Each press, `(elapsed, note, velocity)`, in order.
    presses: Vec<(i32, u8, u8)>,
    /// Where playback is when the lane alone has moved it: the last press's
    /// key plus the ticks since. Playback anywhere else means something else
    /// moved it (a ruler click, `Space` from the cursor), so what follows is
    /// no longer what the presses played and the run ends.
    expected_tick: i32,
}

impl PerformanceLane {
    /// A lane with no run.
    pub(crate) fn new() -> Self {
        PerformanceLane::default()
    }

    /// Ends the run and the replay: the lane was disarmed, or a project
    /// replaced.
    pub(crate) fn clear(&mut self) {
        self.run = None;
        self.replay = None;
    }

    /// `⌥Space`: starts replaying pad `pad`'s performance from `from` ticks
    /// into it, replacing any replay. Ends the run: the replay moves playback,
    /// so the presses before it no longer say what played.
    pub(crate) fn start_replay(&mut self, pad: usize, from: i32) {
        self.run = None;
        self.replay = Some(Replay {
            pad,
            elapsed: from.max(0),
            expected_tick: None,
            silent: false,
        });
    }

    /// Ends the replay: playback stopped, or a live press took over.
    pub(crate) fn stop_replay(&mut self) {
        self.replay = None;
    }

    /// Where the replay is ([`ReplayPosition`]), `None` when none plays.
    pub(crate) fn replay_position(&self) -> Option<ReplayPosition> {
        self.replay.map(|replay| ReplayPosition {
            pad: replay.pad,
            tick: replay.elapsed,
        })
    }

    /// Whether the replay is in a gap, where nothing sounds (decision 2).
    pub(crate) fn is_replay_silent(&self) -> bool {
        self.replay.is_some_and(|replay| replay.silent)
    }

    /// Playback is about to advance one tick from `playback_tick` while the
    /// replay plays `clip` (its pad's performance, read each tick so an edit
    /// is heard on the next pass): what to do first, and the replay's clock
    /// counts the tick, back to the start at the performance's length.
    /// `target` is where a note's key jumps to (`None` for a note below the
    /// lane's keys, which plays as a gap).
    ///
    /// A note starting now jumps; with several the latest in the clip wins,
    /// as live. The first tick instead takes the note sounding there, from
    /// as far into it as the replay starts. Where no note sounds is a gap,
    /// silence (decision 2); a note ending under a longer one plays on, as
    /// live the latest press wins. The replay ends, with no cue, when
    /// playback isn't where it left it (something else moved it: a ruler
    /// click, `Space` from the cursor) or the performance is gone.
    pub(crate) fn step_replay(
        &mut self,
        playback_tick: i32,
        clip: Option<&Clip>,
        target: impl Fn(u8) -> Option<i32>,
    ) -> Option<ReplayCue> {
        let replay = self.replay.as_mut()?;
        let moved = replay
            .expected_tick
            .is_some_and(|expected| expected != playback_tick);
        let Some(clip) = clip.filter(|clip| clip.region_length() > 0 && !moved) else {
            self.replay = None;
            return None;
        };
        let length = clip.region_length();
        // Wrapped again here: an edit may have shortened the performance.
        let elapsed = replay.elapsed % length;
        let at = clip.region().start() + elapsed;
        let notes = || replay_notes(clip, &target);
        let jump = match replay.expected_tick {
            None => notes()
                .filter(|&(start, end, _)| start <= at && at < end)
                .max_by_key(|&(start, _, _)| start),
            Some(_) => notes().filter(|&(start, _, _)| start == at).last(),
        }
        .map(|(start, _, key)| key + at - start);
        // Only a tick with no jump needs to know: a jump lands on a note.
        let sounding = jump.is_some() || notes().any(|(start, end, _)| start <= at && at < end);
        let cue = match jump {
            Some(tick) => Some(ReplayCue::Jump(tick)),
            None if !sounding && !replay.silent => Some(ReplayCue::Silence),
            None => None,
        };
        replay.silent = jump.is_none() && !sounding;
        let now = match cue {
            Some(ReplayCue::Jump(tick)) => tick,
            _ => playback_tick,
        };
        replay.expected_tick = Some(now + 1);
        replay.elapsed = (elapsed + 1) % length;
        cue
    }

    /// A press of `note` jumped playback to `target_tick`. It starts a run
    /// when there is none, or when `started` (it started the transport: the
    /// run is "from the press that started the transport"), and is added to
    /// it. A press on the same tick as the one before replaces it, as live
    /// the latest press wins.
    pub(crate) fn press(&mut self, note: u8, velocity: u8, target_tick: i32, started: bool) {
        if started {
            self.run = None;
        }
        let run = self.run.get_or_insert_with(|| Run {
            elapsed: 0,
            presses: Vec::new(),
            expected_tick: target_tick,
        });
        if run
            .presses
            .last()
            .is_some_and(|&(at, _, _)| at == run.elapsed)
        {
            run.presses.pop();
        }
        run.presses.push((run.elapsed, note, velocity));
        run.expected_tick = target_tick;
    }

    /// Playback is about to advance one tick from `playback_tick`: the run's
    /// clock counts it, or the run ends if playback isn't where the presses
    /// left it ([`Run::expected_tick`]). Nothing with no run.
    pub(crate) fn advance(&mut self, playback_tick: i32) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.expected_tick != playback_tick {
            self.run = None;
            return;
        }
        run.elapsed += 1;
        run.expected_tick += 1;
    }

    /// `/`: the run as a performance clip ([`performance_clip`]), ending now
    /// (running) or where playback stopped (stopped: the clock stood still
    /// since). The run is used up, so the next press starts the next one.
    /// `None` with no run, or nothing in it that lasts a tick.
    pub(crate) fn take_run(&mut self) -> Option<Clip> {
        let run = self.run.take()?;
        performance_clip(&run.presses, run.elapsed)
    }
}

/// The notes of performance `clip` the replay plays, `(start, end, key)`:
/// each unmuted note on a lane key, with where its key jumps to (`target`).
fn replay_notes<'a>(
    clip: &'a Clip,
    target: &'a impl Fn(u8) -> Option<i32>,
) -> impl Iterator<Item = (i32, i32, i32)> + 'a {
    clip.events()
        .iter()
        .filter(|event| event.event_type() == Some(EventType::NoteOn) && !event.is_muted())
        .filter_map(move |event| {
            let key = target(event.note_number()?)?;
            Some((event.tick(), event.end_tick(), key))
        })
}

/// One stretch of what a performance plays, for its render
/// (`520-performance-capture.md` decision 5): `len` ticks of the
/// arrangement from tick `from`, placed `at` ticks into the performance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Where in the performance it starts, from its window start.
    pub(crate) at: i32,
    /// How long it plays.
    pub(crate) len: i32,
    /// The arrangement tick it plays from.
    pub(crate) from: i32,
}

/// What performance `clip` plays, once through from its start, as the
/// stretches of the arrangement it plays ([`Segment`]), in order: exactly
/// what [`PerformanceLane::step_replay`] plays from the start, so the render
/// sounds as the replay does. The first stretch takes the note sounding at
/// the start, from as far into its key; after that each note start jumps
/// (several on one tick: the last in the clip), a note ending under a longer
/// one plays on, and where no note sounds is a gap, which places nothing.
/// `target` is where a note's key jumps to, as for the replay.
pub(crate) fn render_segments(clip: &Clip, target: impl Fn(u8) -> Option<i32>) -> Vec<Segment> {
    let (start, end) = (clip.region().start(), clip.region().end());
    let notes = || replay_notes(clip, &target);
    // The ticks something changes on: the start, and each note's ends
    // inside the window.
    let mut changes: Vec<i32> = notes()
        .flat_map(|(on, off, _)| [on, off])
        .filter(|&tick| start < tick && tick < end)
        .chain([start])
        .collect();
    changes.sort_unstable();
    changes.dedup();

    let mut segments = Vec::new();
    // The stretch playing: where it started in the performance, and from.
    let mut open: Option<(i32, i32)> = None;
    let mut close = |open: &mut Option<(i32, i32)>, at: i32| {
        if let Some((began, from)) = open.take()
            && at > began
        {
            segments.push(Segment {
                at: began - start,
                len: at - began,
                from,
            });
        }
    };
    for &at in &changes {
        let jump = if at == start {
            notes()
                .filter(|&(on, off, _)| on <= at && at < off)
                .max_by_key(|&(on, _, _)| on)
        } else {
            notes().filter(|&(on, _, _)| on == at).last()
        };
        if let Some((on, _, key)) = jump {
            close(&mut open, at);
            open = Some((at, key + at - on));
        } else if !notes().any(|(on, off, _)| on <= at && at < off) {
            close(&mut open, at);
        }
    }
    close(&mut open, end);
    segments
}

/// A performance clip from `presses` (`(tick, note, velocity)`, in order,
/// the first at 0) ending at `end` (`520-performance-capture.md` decision 1):
/// a note per press, its pitch the key, lasting until the next press (a
/// release doesn't stop the transport, so that is what played), the last
/// until `end`. Its window is `0..end`. `None` when nothing lasts a tick.
pub(crate) fn performance_clip(presses: &[(i32, u8, u8)], end: i32) -> Option<Clip> {
    let ends = presses.iter().skip(1).map(|&(at, _, _)| at).chain([end]);
    let mut events = Vec::new();
    for (&(at, note, velocity), until) in presses.iter().zip(ends) {
        if until <= at {
            continue;
        }
        events.push(Event::new(
            at,
            until - at,
            vec![0x90, note, velocity.max(1)],
        ));
        events.push(Event::new(until, 0, vec![0x80, note, 0]));
    }
    if events.is_empty() {
        return None;
    }
    let mut clip = Clip::new();
    clip.region_mut().set_region(Some(0), Some(end));
    for event in events {
        clip.add_event(event);
    }
    clip.calculate_note_lengths();
    Some(clip)
}

/// The lane's eight pads (`520-performance-capture.md` decision 6): each
/// empty or holding a performance clip. Saved with the project, changed only
/// through undoable edits (`SetPerformancePadEdit`).
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformancePads {
    /// Each pad's performance, by pad.
    pads: [Option<Clip>; PAD_COUNT],
}

impl PerformancePads {
    /// Pad `pad`'s performance; `None` when it is empty or there is no such
    /// pad.
    pub(crate) fn get(&self, pad: usize) -> Option<&Clip> {
        self.pads.get(pad)?.as_ref()
    }

    /// The performance with this id, on whichever pad holds it.
    pub(crate) fn find(&self, clip_id: Uuid) -> Option<&Clip> {
        self.pads.iter().flatten().find(|clip| clip.id() == clip_id)
    }

    /// Mutable [`find`](Self::find).
    pub(crate) fn find_mut(&mut self, clip_id: Uuid) -> Option<&mut Clip> {
        self.pads
            .iter_mut()
            .flatten()
            .find(|clip| clip.id() == clip_id)
    }

    /// Puts `clip` on pad `pad` (empties it for `None`). Nothing for a pad
    /// past the last.
    pub(crate) fn set(&mut self, pad: usize, clip: Option<Clip>) {
        if let Some(slot) = self.pads.get_mut(pad) {
            *slot = clip;
        }
    }

    /// Empties every pad.
    pub(crate) fn clear(&mut self) {
        self.pads = Default::default();
    }

    /// The pads that hold a performance, with it, in pad order.
    pub(crate) fn lit(&self) -> impl Iterator<Item = (usize, &Clip)> {
        self.pads
            .iter()
            .enumerate()
            .filter_map(|(pad, clip)| Some((pad, clip.as_ref()?)))
    }

    /// Each pad's length in ticks, `None` for an empty one: what the knob
    /// panel draws.
    pub(crate) fn lengths(&self) -> [Option<i32>; PAD_COUNT] {
        array::from_fn(|pad| self.get(pad).map(Clip::region_length))
    }
}

/// A performance's length as the footer names it, in whole bars and beats
/// of `meter`: `6 bars`, `1 bar 2 beats`, `3 beats`; `under a beat` for less.
pub(crate) fn length_label(ticks: i32, meter: Meter) -> String {
    let bars = ticks / meter.bar_ticks();
    let beats = ticks % meter.bar_ticks() / meter.beat_ticks();
    let count = |n: i32, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    match (bars, beats) {
        (0, 0) => "under a beat".to_owned(),
        (0, beats) => count(beats, "beat"),
        (bars, 0) => count(bars, "bar"),
        (bars, beats) => format!("{} {}", count(bars, "bar"), count(beats, "beat")),
    }
}

/// Maps a MIDI note number to a 0-based key index (note
/// `PERFORMANCE_LANE_BASE_NOTE` = key 0, the start of bar 1), or `None` for
/// notes below the base note (ignored — there is nothing before bar 1).
pub(crate) fn key_index_from_note(note_number: u8) -> Option<i32> {
    note_number
        .checked_sub(config::PERFORMANCE_LANE_BASE_NOTE)
        .map(i32::from)
}

/// How many keys the lane has: from `PERFORMANCE_LANE_BASE_NOTE` (the start
/// of bar 1) to note 127. 68 from C3: 68 bars a bar each, 17 bars of 4/4 a
/// beat each.
pub(crate) const KEY_COUNT: i32 = 128 - config::PERFORMANCE_LANE_BASE_NOTE as i32;

/// The note of 0-based key `key_index`: the inverse of
/// [`key_index_from_note`]. `None` outside `0..KEY_COUNT`: past note 127, or
/// before the first key.
pub(crate) fn note_for_key(key_index: i32) -> Option<u8> {
    (0..KEY_COUNT)
        .contains(&key_index)
        .then(|| config::PERFORMANCE_LANE_BASE_NOTE + key_index as u8)
}

/// How far each of the lane's keys jumps: a bar, half a bar or a beat of
/// the project's meter, picked from the lane's gutter and saved with the
/// project (`510-performance-lane-keyboard.md` § Step 1b). The stored choice;
/// what plays is [`in_meter`](Self::in_meter) of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum KeyLength {
    /// A whole bar per key: 68 keys reach 68 bars.
    Bar,
    /// Half a bar per key, in a meter whose numerator is even; in an odd one
    /// it plays as [`Beat`](Self::Beat).
    HalfBar,
    /// One counted beat per key (a quarter in x/4, an eighth in x/8): what a
    /// project saved before the choice existed plays.
    #[default]
    Beat,
}

impl KeyLength {
    /// The three, in the gutter's order.
    pub(crate) const ALL: [KeyLength; 3] = [KeyLength::Bar, KeyLength::HalfBar, KeyLength::Beat];

    /// The length that plays in `meter`: this one, except a half bar in a
    /// meter with an odd numerator, where half a bar falls between beats (3/4's
    /// beat 2½, half an eighth in 7/8), which plays as a beat: no
    /// [`Meter::half_bar_ticks`], the rule the arranger grid's half-bar rung
    /// follows too. The stored choice is
    /// kept, so a project that goes to 3/4 and back is at a half bar again.
    pub(crate) fn in_meter(self, meter: Meter) -> KeyLength {
        match self {
            KeyLength::HalfBar if meter.half_bar_ticks().is_none() => KeyLength::Beat,
            length => length,
        }
    }

    /// One key's length in `meter`, in ticks, as it plays
    /// ([`in_meter`](Self::in_meter)).
    pub(crate) fn ticks(self, meter: Meter) -> i32 {
        match self.in_meter(meter) {
            KeyLength::Bar => meter.bar_ticks(),
            // `in_meter` keeps a half bar only where the meter has one.
            KeyLength::HalfBar => meter.bar_ticks() / 2,
            KeyLength::Beat => meter.beat_ticks(),
        }
    }

    /// Whether `meter` offers this length: all but a half bar in an odd meter.
    pub(crate) fn is_offered(self, meter: Meter) -> bool {
        self.in_meter(meter) == self
    }

    /// The length as one byte, for `SharedAtomics::performance_lane_key_length`.
    pub(crate) fn to_bits(self) -> u8 {
        match self {
            KeyLength::Bar => 0,
            KeyLength::HalfBar => 1,
            KeyLength::Beat => 2,
        }
    }

    /// The length [`to_bits`](Self::to_bits) packed; a beat for any other
    /// byte.
    pub(crate) fn from_bits(bits: u8) -> KeyLength {
        match bits {
            0 => KeyLength::Bar,
            1 => KeyLength::HalfBar,
            _ => KeyLength::Beat,
        }
    }
}

/// Where key `key_index` jumps to, in ticks: the start of its key, one
/// `length` of `meter` per key ([`KeyLength::ticks`]), so every jump lands on
/// a beat in every meter. A beat a key from C3 in 4/4: C3 = bar 1, C♯3 =
/// bar 1 beat 2, E3 = bar 2, C4 = bar 4; a bar a key: C♯3 = bar 2, C4 =
/// bar 13. The one place the mapping lives, for the trigger and the drawing.
pub(crate) fn key_tick(meter: Meter, length: KeyLength, key_index: i32) -> i32 {
    length.ticks(meter) * key_index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequencer::test_support::clip_notes;

    /// Plays `ticks` ticks from `from`, as the tick pump would.
    fn play(lane: &mut PerformanceLane, from: i32, ticks: i32) {
        for tick in from..from + ticks {
            lane.advance(tick);
        }
    }

    #[test]
    fn each_press_lasts_until_the_next_and_the_last_until_the_capture() {
        let mut lane = PerformanceLane::new();
        lane.press(64, 100, 3840, true);
        play(&mut lane, 3840, 1000);
        lane.press(60, 90, 0, false);
        play(&mut lane, 0, 500);

        let clip = lane.take_run().expect("a run");
        assert_eq!(clip_notes(&clip), [(0, 64, 1000), (1000, 60, 500)]);
        assert_eq!((clip.region().start(), clip.region().end()), (0, 1500));
        assert!(lane.take_run().is_none(), "the capture uses the run up");
    }

    #[test]
    fn a_stopped_capture_ends_where_playback_stopped() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 700);
        // Stopped: no ticks reach the lane.
        let clip = lane.take_run().expect("a run");
        assert_eq!(clip_notes(&clip), [(0, 60, 700)]);
    }

    #[test]
    fn the_next_run_starts_at_the_first_press_after_a_capture() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 100);
        assert!(lane.take_run().is_some());
        play(&mut lane, 100, 50);
        assert!(lane.take_run().is_none(), "nothing pressed since");

        lane.press(62, 100, 960, false);
        play(&mut lane, 960, 30);
        let clip = lane.take_run().expect("a run");
        assert_eq!(clip_notes(&clip), [(0, 62, 30)]);
    }

    #[test]
    fn a_press_that_starts_the_transport_starts_a_new_run() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 100);
        lane.press(65, 100, 1920, true);
        play(&mut lane, 1920, 10);
        assert_eq!(clip_notes(&lane.take_run().unwrap()), [(0, 65, 10)]);
    }

    #[test]
    fn playback_moved_by_anything_but_a_press_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 100);
        // A ruler click took playback to bar 5.
        play(&mut lane, 3840, 1);
        assert!(lane.take_run().is_none());
    }

    #[test]
    fn presses_on_one_tick_keep_the_latest() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        lane.press(62, 100, 480, false);
        play(&mut lane, 480, 10);
        assert_eq!(clip_notes(&lane.take_run().unwrap()), [(0, 62, 10)]);
    }

    #[test]
    fn a_run_with_nothing_lasting_a_tick_captures_nothing() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        assert!(lane.take_run().is_none());
        assert!(PerformanceLane::new().take_run().is_none());
    }

    #[test]
    fn clearing_the_lane_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 10);
        lane.clear();
        assert!(lane.take_run().is_none());
    }

    /// Where each key jumps in these tests: a beat a key, in 4/4.
    fn beat_key(note: u8) -> Option<i32> {
        key_index_from_note(note).map(|key| key_tick(Meter::FOUR_FOUR, KeyLength::Beat, key))
    }

    /// A performance of `notes` (`(start, end, note)`) with window `0..end`,
    /// as an edit in the piano roll can leave it.
    fn clip_of(notes: &[(i32, i32, u8)], end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(end));
        for &(start, until, note) in notes {
            clip.add_event(Event::new(start, until - start, vec![0x90, note, 100]));
            clip.add_event(Event::new(until, 0, vec![0x80, note, 0]));
        }
        clip.calculate_note_lengths();
        clip
    }

    /// Replays `clip` for `ticks` ticks from playback at `playback`, as the
    /// tick pump would: each cue, by the replay's tick it came on, and where
    /// playback ends up.
    fn replay(
        lane: &mut PerformanceLane,
        clip: &Clip,
        mut playback: i32,
        ticks: i32,
    ) -> (Vec<(i32, ReplayCue)>, i32) {
        let mut cues = Vec::new();
        for _ in 0..ticks {
            let at = lane.replay_position().map(|position| position.tick);
            if let Some(cue) = lane.step_replay(playback, Some(clip), beat_key) {
                cues.push((at.unwrap(), cue));
                if let ReplayCue::Jump(tick) = cue {
                    playback = tick;
                }
            }
            playback += 1;
        }
        (cues, playback)
    }

    #[test]
    fn a_replay_jumps_at_each_note_and_loops_over_the_length() {
        // E3 (bar 2) for 100 ticks, then C3 (bar 1) for 50.
        let clip = performance_clip(&[(0, 64, 100), (100, 60, 100)], 150).unwrap();
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let (cues, playback) = replay(&mut lane, &clip, 7000, 310);
        assert_eq!(
            cues,
            [
                (0, ReplayCue::Jump(3840)),
                (100, ReplayCue::Jump(0)),
                (0, ReplayCue::Jump(3840)),
                (100, ReplayCue::Jump(0)),
                (0, ReplayCue::Jump(3840)),
            ]
        );
        assert_eq!(playback, 3840 + 10);
        assert!(!lane.is_replay_silent());
    }

    #[test]
    fn a_replay_from_inside_a_note_starts_as_far_into_its_key() {
        let clip = performance_clip(&[(0, 64, 100), (100, 60, 100)], 150).unwrap();
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 30);
        let (cues, _) = replay(&mut lane, &clip, 0, 1);
        assert_eq!(cues, [(30, ReplayCue::Jump(3840 + 30))]);
    }

    #[test]
    fn a_gap_is_silence_until_the_next_note() {
        // E3 for 40 ticks, then a gap to C3 at 100.
        let clip = clip_of(&[(0, 40, 64), (100, 200, 60)], 200);
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let (cues, _) = replay(&mut lane, &clip, 0, 41);
        assert_eq!(cues, [(0, ReplayCue::Jump(3840)), (40, ReplayCue::Silence)]);
        assert!(lane.is_replay_silent());
        let (cues, _) = replay(&mut lane, &clip, 3840 + 41, 60);
        assert_eq!(cues, [(100, ReplayCue::Jump(0))]);
        assert!(!lane.is_replay_silent());
    }

    #[test]
    fn a_replay_starting_in_a_gap_is_silent_from_its_first_tick() {
        let clip = clip_of(&[(100, 200, 60)], 200);
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let (cues, _) = replay(&mut lane, &clip, 500, 101);
        assert_eq!(cues, [(0, ReplayCue::Silence), (100, ReplayCue::Jump(0))]);
    }

    #[test]
    fn a_note_ending_under_a_longer_one_plays_on() {
        let clip = clip_of(&[(0, 200, 60), (50, 100, 64)], 200);
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let (cues, _) = replay(&mut lane, &clip, 0, 200);
        assert_eq!(cues, [(0, ReplayCue::Jump(0)), (50, ReplayCue::Jump(3840))]);
    }

    #[test]
    fn a_muted_note_plays_as_a_gap() {
        let mut clip = clip_of(&[(0, 100, 60)], 100);
        let _ = clip.select_all_events();
        clip.toggle_muted_for_selected_events();
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let (cues, _) = replay(&mut lane, &clip, 0, 100);
        assert_eq!(cues, [(0, ReplayCue::Silence)]);
    }

    #[test]
    fn playback_moved_by_anything_but_the_replay_ends_it() {
        let clip = performance_clip(&[(0, 60, 100)], 200).unwrap();
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        replay(&mut lane, &clip, 0, 10);
        // A ruler click took playback to bar 5.
        assert_eq!(lane.step_replay(3840, Some(&clip), beat_key), None);
        assert_eq!(lane.replay_position(), None);
    }

    #[test]
    fn an_emptied_pad_ends_the_replay() {
        let mut lane = PerformanceLane::new();
        lane.start_replay(3, 0);
        assert_eq!(lane.replay_position().map(|position| position.pad), Some(3));
        assert_eq!(lane.step_replay(0, None, beat_key), None);
        assert_eq!(lane.replay_position(), None);
    }

    /// What one pass of the replay plays, tick by tick from the start:
    /// `(tick in the performance, arrangement tick)` for each tick that
    /// sounds.
    fn replayed(clip: &Clip) -> Vec<(i32, i32)> {
        let mut lane = PerformanceLane::new();
        lane.start_replay(0, 0);
        let mut playback = 7000;
        let mut heard = Vec::new();
        for at in 0..clip.region_length() {
            if let Some(ReplayCue::Jump(tick)) = lane.step_replay(playback, Some(clip), beat_key) {
                playback = tick;
            }
            if !lane.is_replay_silent() {
                heard.push((at, playback));
            }
            playback += 1;
        }
        heard
    }

    /// The same, from the render's segments.
    fn rendered(clip: &Clip) -> Vec<(i32, i32)> {
        render_segments(clip, beat_key)
            .into_iter()
            .flat_map(|segment| (0..segment.len).map(move |i| (segment.at + i, segment.from + i)))
            .collect()
    }

    #[test]
    fn the_render_is_a_segment_per_press() {
        let clip = performance_clip(&[(0, 64, 100), (100, 60, 100), (130, 61, 100)], 150).unwrap();
        assert_eq!(
            render_segments(&clip, beat_key),
            [
                Segment {
                    at: 0,
                    len: 100,
                    from: 3840
                },
                Segment {
                    at: 100,
                    len: 30,
                    from: 0
                },
                Segment {
                    at: 130,
                    len: 20,
                    from: 960
                },
            ]
        );
    }

    /// Decision 5 and § Open questions 3: the render plays what the replay
    /// does, gaps, overlaps, mutes and a window that starts inside a note
    /// included.
    #[test]
    fn the_render_plays_what_the_replay_plays() {
        let mut muted = clip_of(&[(0, 100, 60), (100, 200, 64)], 200);
        muted.select_event(muted.events().first().map(Event::id));
        muted.toggle_muted_for_selected_events();
        let mut late_window = clip_of(&[(0, 200, 64), (150, 300, 60)], 300);
        late_window.region_mut().set_region(Some(50), Some(250));
        let clips = [
            performance_clip(&[(0, 64, 100), (100, 60, 100)], 150).unwrap(),
            clip_of(&[(0, 40, 64), (100, 200, 60)], 200),
            clip_of(&[(100, 200, 60)], 200),
            clip_of(&[(0, 200, 60), (50, 100, 64)], 200),
            clip_of(&[(0, 100, 60), (0, 100, 64), (100, 150, 30)], 150),
            muted,
            late_window,
        ];
        for clip in &clips {
            assert_eq!(rendered(clip), replayed(clip), "{:?}", clip_notes(clip));
        }
    }

    #[test]
    fn a_replay_ends_the_run_and_clearing_ends_the_replay() {
        let mut lane = PerformanceLane::new();
        lane.press(60, 100, 0, true);
        play(&mut lane, 0, 10);
        lane.start_replay(0, 0);
        assert!(lane.take_run().is_none());
        lane.clear();
        assert_eq!(lane.replay_position(), None);
    }

    #[test]
    fn a_replay_position_packs_into_one_value() {
        for position in [
            None,
            Some(ReplayPosition { pad: 0, tick: 0 }),
            Some(ReplayPosition {
                pad: 7,
                tick: 123_456,
            }),
        ] {
            assert_eq!(
                ReplayPosition::from_bits(ReplayPosition::to_bits(position)),
                position
            );
        }
    }

    #[test]
    fn a_pad_holds_one_performance() {
        let mut pads = PerformancePads::default();
        let first = performance_clip(&[(0, 60, 100)], 960).unwrap();
        let second = performance_clip(&[(0, 62, 100)], 1920).unwrap();
        let second_id = second.id();
        pads.set(2, Some(first.clone()));
        pads.set(2, Some(second));
        assert_eq!(pads.get(2).map(Clip::id), Some(second_id), "replaced");
        assert_eq!(
            pads.lengths(),
            [None, None, Some(1920), None, None, None, None, None]
        );
        assert_eq!(pads.lit().map(|(pad, _)| pad).collect::<Vec<_>>(), [2]);
        pads.set(PAD_COUNT, Some(first));
        assert!(pads.get(PAD_COUNT).is_none(), "no ninth pad");
        pads.clear();
        assert_eq!(pads.lengths(), [None; PAD_COUNT]);
    }

    #[test]
    fn a_length_reads_in_bars_and_beats() {
        let four = Meter::FOUR_FOUR;
        let (bar, beat) = (four.bar_ticks(), four.beat_ticks());
        assert_eq!(length_label(6 * bar + 100, four), "6 bars");
        assert_eq!(length_label(bar + 2 * beat, four), "1 bar 2 beats");
        assert_eq!(length_label(2 * bar + beat, four), "2 bars 1 beat");
        assert_eq!(length_label(3 * beat, four), "3 beats");
        assert_eq!(length_label(beat - 1, four), "under a beat");
    }

    #[test]
    fn key_index_from_note_below_base_is_none() {
        assert_eq!(
            key_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE - 1),
            None
        );
    }

    #[test]
    fn key_index_from_note_at_base_is_zero() {
        assert_eq!(
            key_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE),
            Some(0)
        );
    }

    #[test]
    fn key_index_from_note_above_base_increments() {
        assert_eq!(
            key_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE + 3),
            Some(3)
        );
    }

    #[test]
    fn note_for_key_is_the_inverse_of_key_index_from_note() {
        for key_index in 0..68 {
            let note = note_for_key(key_index).expect("every key up to 68 has a note");
            assert_eq!(key_index_from_note(note), Some(key_index));
        }
        assert_eq!(note_for_key(0), Some(config::PERFORMANCE_LANE_BASE_NOTE));
        assert_eq!(note_for_key(67), Some(127));
    }

    #[test]
    fn note_for_key_is_none_where_no_key_reaches() {
        assert_eq!(note_for_key(68), None, "past note 127");
        assert_eq!(note_for_key(1000), None);
        assert_eq!(note_for_key(-1), None, "before bar 1");
        assert_eq!(note_for_key(i32::MAX), None);
    }

    fn meter(numerator: u8, denominator: u8) -> Meter {
        Meter::new(numerator, denominator).expect("a valid meter")
    }

    #[test]
    fn each_key_jumps_a_beat() {
        let meter = Meter::FOUR_FOUR;
        let beat = KeyLength::Beat;
        assert_eq!(key_tick(meter, beat, 0), 0, "C3: bar 1");
        assert_eq!(
            key_tick(meter, beat, 1),
            meter.beat_ticks(),
            "C#3: bar 1 beat 2"
        );
        assert_eq!(
            key_tick(meter, beat, 4),
            meter.bars_to_ticks(1),
            "E3: bar 2"
        );
        assert_eq!(
            key_tick(meter, beat, 12),
            meter.bars_to_ticks(3),
            "C4: bar 4"
        );
    }

    #[test]
    fn each_key_jumps_a_bar() {
        let meter = Meter::FOUR_FOUR;
        let bar = KeyLength::Bar;
        assert_eq!(key_tick(meter, bar, 0), 0, "C3: bar 1");
        assert_eq!(
            key_tick(meter, bar, 1),
            meter.bars_to_ticks(1),
            "C#3: bar 2"
        );
        assert_eq!(
            key_tick(meter, bar, 12),
            meter.bars_to_ticks(12),
            "C4: bar 13"
        );
        assert_eq!(
            key_tick(meter, bar, KEY_COUNT - 1),
            meter.bars_to_ticks(67),
            "G8: bar 68"
        );
    }

    #[test]
    fn each_key_jumps_half_a_bar() {
        let meter = Meter::FOUR_FOUR;
        let half = KeyLength::HalfBar;
        assert_eq!(
            key_tick(meter, half, 1),
            2 * meter.beat_ticks(),
            "C#3: bar 1 beat 3"
        );
        assert_eq!(
            key_tick(meter, half, 2),
            meter.bars_to_ticks(1),
            "D3: bar 2"
        );
    }

    #[test]
    fn a_bar_is_as_many_beat_keys_as_the_meter_has_beats() {
        for (numerator, denominator) in [(3, 4), (7, 8), (1, 8), (5, 4), (6, 8)] {
            let meter = meter(numerator, denominator);
            assert_eq!(
                key_tick(meter, KeyLength::Beat, i32::from(numerator)),
                meter.bar_ticks(),
                "{numerator}/{denominator}"
            );
        }
    }

    #[test]
    fn key_lengths_in_ticks() {
        for (numerator, denominator) in [(4, 4), (3, 4), (6, 8), (7, 8)] {
            let meter = meter(numerator, denominator);
            assert_eq!(KeyLength::Bar.ticks(meter), meter.bar_ticks());
            assert_eq!(KeyLength::Beat.ticks(meter), meter.beat_ticks());
        }
        assert_eq!(
            KeyLength::HalfBar.ticks(Meter::FOUR_FOUR),
            2 * Meter::FOUR_FOUR.beat_ticks()
        );
        let six_eight = meter(6, 8);
        assert_eq!(
            KeyLength::HalfBar.ticks(six_eight),
            3 * six_eight.beat_ticks()
        );
    }

    #[test]
    fn half_a_bar_plays_as_a_beat_in_an_odd_meter() {
        for (numerator, denominator) in [(3, 4), (7, 8), (5, 4), (1, 8)] {
            let meter = meter(numerator, denominator);
            assert_eq!(
                KeyLength::HalfBar.in_meter(meter),
                KeyLength::Beat,
                "{numerator}/{denominator}"
            );
            assert_eq!(KeyLength::HalfBar.ticks(meter), meter.beat_ticks());
            assert!(!KeyLength::HalfBar.is_offered(meter));
        }
        for (numerator, denominator) in [(4, 4), (6, 8), (2, 4)] {
            let meter = meter(numerator, denominator);
            assert_eq!(KeyLength::HalfBar.in_meter(meter), KeyLength::HalfBar);
            assert!(KeyLength::HalfBar.is_offered(meter));
        }
    }

    #[test]
    fn a_bar_and_a_beat_play_in_every_meter() {
        for (numerator, denominator) in [(4, 4), (3, 4), (6, 8), (7, 8)] {
            let meter = meter(numerator, denominator);
            assert_eq!(KeyLength::Bar.in_meter(meter), KeyLength::Bar);
            assert_eq!(KeyLength::Beat.in_meter(meter), KeyLength::Beat);
        }
    }

    #[test]
    fn key_length_bits_round_trip() {
        for length in KeyLength::ALL {
            assert_eq!(KeyLength::from_bits(length.to_bits()), length);
        }
        assert_eq!(KeyLength::from_bits(200), KeyLength::Beat);
        assert_eq!(KeyLength::default(), KeyLength::Beat);
    }
}
