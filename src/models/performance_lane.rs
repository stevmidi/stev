//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a key's start
//! instead of capturing notes (`110-performance-lane.md`). What it holds is
//! the **run**: the presses since playback started, on a clock of its own
//! that counts only the ticks playback advances, which `/` keeps as a
//! performance (`520-performance-capture.md` decision 8), and the eight
//! **pads** the kept performances sit on ([`PerformancePads`], decision 6).
//! [`key_index_from_note`] is the note-number → key mapping the lane triggers
//! on, [`key_tick`] where a key jumps to (one [`KeyLength`] of the project's
//! meter per key: a bar, half a bar or a beat), and [`note_for_key`] the
//! mapping's inverse, which the arranger draws the lane's keys from
//! (`510-performance-lane-keyboard.md`).

use std::array;

use uuid::Uuid;

use crate::core::config;
use crate::core::time::Meter;
use crate::models::{clip::Clip, event::Event};

/// How many pads the lane has: the knob panel's eight cells
/// (`520-performance-capture.md` decision 6).
pub(crate) const PAD_COUNT: usize = 8;

/// The lane's live state: the run its presses make. Not saved, not an undo
/// step. See the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformanceLane {
    /// The presses since the run began, `None` with no run.
    run: Option<Run>,
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

    /// Ends the run: the lane was disarmed, or a project replaced.
    pub(crate) fn clear(&mut self) {
        self.run = None;
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

    /// Mutable [`get`](Self::get).
    pub(crate) fn get_mut(&mut self, pad: usize) -> Option<&mut Clip> {
        self.pads.get_mut(pad)?.as_mut()
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
