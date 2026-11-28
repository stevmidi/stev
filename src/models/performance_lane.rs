//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a key's start
//! instead of capturing notes (`110-performance-lane.md`). What it holds is
//! the **run**: the presses since playback started, each with where it
//! jumped, on a clock of its own that counts only the ticks playback
//! advances, which `/` renders into clips at the arranger cursor
//! (`archive/520-performance-capture.md` decisions 1 and 2): [`PerformanceLane::take_run`]
//! gives it as a [`Take`], the stretches of the arrangement it played.
//! The lane's keys run on along the timeline from bar 1, a keyboard of
//! them, key 0 a C ([`key_pitch_class`], which the arranger draws them
//! from, `archive/510-performance-lane-keyboard.md`; unnamed,
//! `570-lane-octave-buttons.md` § 4). [`Hand`] is the player's hand on
//! the lane: the notes a keyboard of a [`KeybedSize`] from settings sends
//! at its home octave, laid along the timeline by whole octaves around the
//! arranger cursor and moved an octave by the keyboard's previous/next
//! buttons (`570-lane-octave-buttons.md`); it maps a pressed note to its key.
//! [`key_tick`] is where a key starts (one [`KeyLength`] of the project's
//! meter per key: a bar, half a bar or a beat). A press while running off a
//! beat waits for the next one ([`is_on_beat`]) and lands on its key's start,
//! so a key always starts from the same place and the beat never breaks
//! (`610-launch-quantize.md`).

use crate::core::time::{Meter, snap_to_grid};

/// The lane's live state: the run its presses make, and a jump or a stop
/// waiting for the next beat. Not saved, not an undo step. See the module
/// docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformanceLane {
    /// The presses since the run began, `None` with no run.
    run: Option<Run>,
    /// A lane press or a marker recall while running, off a beat: it jumps
    /// on playback's next beat (`610-launch-quantize.md`). The latest wins.
    waiting: Option<WaitingJump>,
    /// `Space` while armed, off a beat: playback runs on to the next beat
    /// and stops there (`archive/550-phase-jumps.md` § The stop on the beat).
    stop_pending: bool,
}

/// A jump waiting for playback's next beat (`610-launch-quantize.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaitingJump {
    /// Where it jumps: a key's start, or a marker.
    target: i32,
    /// A lane press, which joins the run when it lands; a marker recall
    /// doesn't (`610` decision 8).
    press: bool,
}

/// One run of presses (`archive/520-performance-capture.md` decision 2): from the
/// press that started it, each press stamped with the run's own clock.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    /// Ticks playback has advanced since the run's first press: musical time
    /// while running, never the transport position (it jumps) nor the
    /// clock's (realigned at every jump, `150`).
    elapsed: i32,
    /// Each press, `(elapsed, target)`, in order: when it came and where it
    /// jumped. The target is kept as it was, so a key length or meter
    /// changed in the middle of a run renders what was heard.
    presses: Vec<(i32, i32)>,
    /// Where playback is when the lane alone has moved it: the last press's
    /// key plus the ticks since. Playback anywhere else means something else
    /// moved it (a ruler click, `Space` from the cursor), so what follows is
    /// no longer what the presses played and the run ends.
    expected_tick: i32,
}

/// What a run played, for its render (`archive/520-performance-capture.md`
/// decision 1): its stretches of the arrangement, in order, and its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Take {
    /// A stretch per press, laid end to end from 0.
    pub(crate) segments: Vec<Segment>,
    /// The run's length: where the last stretch ends.
    pub(crate) length: i32,
}

/// One stretch of what a run played: `len` ticks of the arrangement from
/// tick `from`, placed `at` ticks into the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Where in the run it starts.
    pub(crate) at: i32,
    /// How long it plays.
    pub(crate) len: i32,
    /// The arrangement tick it plays from.
    pub(crate) from: i32,
}

impl PerformanceLane {
    /// A lane with no run.
    pub(crate) fn new() -> Self {
        PerformanceLane::default()
    }

    /// Ends the run and drops a waiting jump: the lane was disarmed, or a
    /// project replaced. A waiting stop still lands on the beat
    /// (`archive/550-phase-jumps.md` § The stop on the beat).
    pub(crate) fn clear(&mut self) {
        self.run = None;
        self.waiting = None;
    }

    /// A press jumped playback to its key's start, `target_tick`. It starts
    /// a run when there is none, or when `started` (it started the
    /// transport: the run is "from the press that started the transport"),
    /// and is added to it. A press on the same tick as the one before
    /// replaces it, as live the latest press wins. A jump still waiting is
    /// dropped: this one went first.
    pub(crate) fn press(&mut self, target_tick: i32, started: bool) {
        self.stop_pending = false;
        self.waiting = None;
        if started {
            self.run = None;
        }
        let run = self.run.get_or_insert_with(|| Run {
            elapsed: 0,
            presses: Vec::new(),
            expected_tick: target_tick,
        });
        if run.presses.last().is_some_and(|&(at, _)| at == run.elapsed) {
            run.presses.pop();
        }
        run.presses.push((run.elapsed, target_tick));
        run.expected_tick = target_tick;
    }

    /// A lane press while running, off a beat: it waits to jump to its key's
    /// start, `target_tick`, on playback's next beat, replacing a jump that
    /// was waiting. Playing on means the performance isn't over, so a
    /// waiting stop is dropped (`610-launch-quantize.md` decisions 4 and 5).
    pub(crate) fn wait_to_press(&mut self, target_tick: i32) {
        self.stop_pending = false;
        self.waiting = Some(WaitingJump {
            target: target_tick,
            press: true,
        });
    }

    /// A marker recall while running, off a beat: it waits to jump to the
    /// marker, `target_tick`, on playback's next beat, replacing a jump that
    /// was waiting. It isn't a press, so it joins no run
    /// (`610-launch-quantize.md` decision 8).
    pub(crate) fn wait_to_recall(&mut self, target_tick: i32) {
        self.waiting = Some(WaitingJump {
            target: target_tick,
            press: false,
        });
    }

    /// A recall jumped at once, on a beat: a jump still waiting is dropped,
    /// as the latest wins.
    pub(crate) fn drop_waiting_jump(&mut self) {
        self.waiting = None;
    }

    /// The waiting jump's target if it is due, with playback about to play
    /// `playback_tick`: that tick is on a beat of `meter`. A lane press
    /// joins the run as it lands ([`press`](Self::press)), so the run holds
    /// it from the tick it was heard. A jump that is due is used up.
    pub(crate) fn land_due_jump(&mut self, meter: Meter, playback_tick: i32) -> Option<i32> {
        if !is_on_beat(meter, playback_tick) {
            return None;
        }
        let jump = self.waiting.take()?;
        if jump.press {
            self.press(jump.target, false);
        }
        Some(jump.target)
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

    /// `Space` while running, with playback about to play `playback_tick`:
    /// whether to stop now. During a performance (the lane `armed` and a run
    /// of presses going) with playback off a beat of `meter`, the stop waits
    /// for the next beat instead
    /// ([`take_due_stop`](Self::take_due_stop)); a second press while it
    /// waits stops at once (`archive/550-phase-jumps.md` § The stop on the
    /// beat). A waiting jump is dropped either way: the stop ends the
    /// performance (`610-launch-quantize.md` decision 5).
    pub(crate) fn stop_now(&mut self, meter: Meter, playback_tick: i32, armed: bool) -> bool {
        self.waiting = None;
        let wait =
            armed && self.run.is_some() && !self.stop_pending && !is_on_beat(meter, playback_tick);
        self.stop_pending = wait;
        !wait
    }

    /// Drops a jump or a stop waiting for the next beat: playback is
    /// starting again from somewhere else.
    pub(crate) fn cancel_waiting(&mut self) {
        self.waiting = None;
        self.stop_pending = false;
    }

    /// Whether a waiting stop is due now, with playback about to play
    /// `playback_tick`: it is on a beat of `meter`. A stop that is due is
    /// used up.
    pub(crate) fn take_due_stop(&mut self, meter: Meter, playback_tick: i32) -> bool {
        let due = self.stop_pending && is_on_beat(meter, playback_tick);
        if due {
            self.stop_pending = false;
        }
        due
    }

    /// `/`: what the run played, ending now (running) or where playback
    /// stopped (stopped: the clock stood still since): a stretch per press
    /// from where it jumped, lasting until the next press (a release doesn't
    /// stop the transport, so that is what played), the last until the
    /// capture. The run is used up, so the next press starts the next one.
    /// `None` with no run, or nothing in it that lasts a tick.
    ///
    /// Every press landed on its key's start (`610-launch-quantize.md`
    /// decision 7), so the first key starts at 0 and every stretch sits on
    /// the grid it was heard on.
    pub(crate) fn take_run(&mut self) -> Option<Take> {
        let run = self.run.take()?;
        let ends = run.presses.iter().skip(1).map(|&(at, _)| at);
        let segments: Vec<Segment> = run
            .presses
            .iter()
            .zip(ends.chain([run.elapsed]))
            .filter_map(|(&(at, from), until)| {
                let len = until - at;
                (len > 0).then_some(Segment { at, len, from })
            })
            .collect();
        (!segments.is_empty()).then_some(Take {
            segments,
            length: run.elapsed,
        })
    }
}

/// The pitch class of key `key_index` (0 for a C), counted along the
/// timeline from bar 1, key 0 a C: what makes it a white or black key in
/// the lane's strip. A key has no octave of its own: which note plays it
/// depends on where the hand is placed ([`Hand`], `570-lane-octave-buttons.md`).
pub(crate) fn key_pitch_class(key_index: i32) -> u8 {
    key_index.rem_euclid(12) as u8
}

/// How many keys the player's keyboard has (`560-performance-lane-reach.md`
/// § 3): a setting, not the project's, since it describes the hardware.
/// Assumed to start on a C. What a [`Hand`] spans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum KeybedSize {
    /// 25 keys, two octaves C to C.
    Keys25,
    /// 37 keys, three octaves.
    Keys37,
    /// 49 keys, four octaves: the home-octave range the design names
    /// (notes 36–84), so the default.
    #[default]
    Keys49,
    /// 61 keys, five octaves.
    Keys61,
    /// 88 keys, taken as starting on a C too.
    Keys88,
}

impl KeybedSize {
    /// The five, smallest first: the settings row's order.
    pub(crate) const ALL: [KeybedSize; 5] = [
        KeybedSize::Keys25,
        KeybedSize::Keys37,
        KeybedSize::Keys49,
        KeybedSize::Keys61,
        KeybedSize::Keys88,
    ];

    /// How many keys.
    pub(crate) fn keys(self) -> u8 {
        match self {
            KeybedSize::Keys25 => 25,
            KeybedSize::Keys37 => 37,
            KeybedSize::Keys49 => 49,
            KeybedSize::Keys61 => 61,
            KeybedSize::Keys88 => 88,
        }
    }

    /// The size with `keys` keys, as `settings.json` keeps it; the default
    /// for any other count (a hand-edited file).
    pub(crate) fn from_keys(keys: u8) -> KeybedSize {
        KeybedSize::ALL
            .into_iter()
            .find(|size| size.keys() == keys)
            .unwrap_or_default()
    }

    /// The lowest note the keyboard sends at its home octave, a C: centred
    /// on C3 (note 60) to the octave below, so a 49-key keybed sends notes
    /// 36–84 and a 25-key one 48–72 (`570-lane-octave-buttons.md` § 1).
    pub(crate) fn home_low(self) -> i32 {
        let octaves = (i32::from(self.keys()) - 1) / 12;
        60 - 12 * (octaves / 2)
    }

    /// The size `step` places along [`ALL`](Self::ALL) from this one,
    /// stopping at either end.
    pub(crate) fn stepped(self, step: isize) -> KeybedSize {
        // The variants are declared in `ALL`'s order.
        let at = (self as usize).saturating_add_signed(step);
        KeybedSize::ALL[at.min(KeybedSize::ALL.len() - 1)]
    }
}

/// Keys in an octave: the step the hand moves by
/// (`570-lane-octave-buttons.md`).
const OCTAVE_KEYS: i32 = 12;

/// The player's hand on the lane (`570-lane-octave-buttons.md`): the notes
/// the keyboard sends at its home octave ([`KeybedSize::home_low`], the
/// keyboard's own octave left at home), laid unbroken along the timeline
/// from a C, note `n` on key `n + shift`. Placed by whole octaves around the
/// arranger cursor ([`around`](Self::around)), moved an octave by the
/// keyboard's previous/next buttons ([`moved`](Self::moved)), never before
/// bar 1. The sequencer owns it and publishes it
/// (`SharedAtomics::performance_lane_hand`); a press jumps by it and the
/// lane lights it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hand {
    /// The keyboard's size: how many keys the hand spans.
    size: KeybedSize,
    /// Keys from a note's number to its key along the timeline: a multiple
    /// of an octave, at least minus the lowest home note.
    shift: i32,
}

impl Default for Hand {
    /// The default keyboard's hand around bar 1, at the default key length
    /// in 4/4: the hand at launch, before settings or a project set it.
    fn default() -> Self {
        Hand::around(
            KeybedSize::default(),
            Meter::FOUR_FOUR,
            KeyLength::default(),
            0,
        )
    }
}

impl Hand {
    /// The hand of a `size` keyboard placed around `around_tick` (the
    /// arranger cursor) at `length` keys of `meter`: the whole octave
    /// placing its middle key nearest `around_tick`, so the cursor is
    /// always within half an octave of the hand's middle; a tie, exactly
    /// half an octave, takes the octave ahead; a placement whose lowest key
    /// would fall before bar 1 the octave up (`570` § 2).
    pub(crate) fn around(
        size: KeybedSize,
        meter: Meter,
        length: KeyLength,
        around_tick: i32,
    ) -> Hand {
        let low = size.home_low();
        let middle = low + (i32::from(size.keys()) - 1) / 2;
        let octave = key_tick(meter, length, OCTAVE_KEYS);
        // A half rounds up: the tie goes to the octave ahead.
        let octaves = snap_to_grid(around_tick - key_tick(meter, length, middle), octave) / octave;
        Hand {
            size,
            shift: (octaves * OCTAVE_KEYS).max(-low),
        }
    }

    /// The hand `octaves` octaves along (down when negative), or `None`
    /// when that puts its lowest key before bar 1 (`570` § 3).
    pub(crate) fn moved(self, octaves: i32) -> Option<Hand> {
        let shift = self.shift + octaves * OCTAVE_KEYS;
        (self.size.home_low() + shift >= 0).then_some(Hand { shift, ..self })
    }

    /// The keyboard's size.
    pub(crate) fn size(self) -> KeybedSize {
        self.size
    }

    /// The key along the timeline that `note` jumps to. A note outside the
    /// home notes jumps too, past the lit hand's edge (`570` § 5), so a key
    /// before bar 1 is possible for a low enough note; the caller clamps.
    pub(crate) fn key_index(self, note: u8) -> i32 {
        i32::from(note) + self.shift
    }

    /// The note of the hand that jumps to the key at `key_index`, if any:
    /// [`key_index`](Self::key_index)'s inverse within the home notes, what
    /// the lane lights.
    pub(crate) fn note_on(self, key_index: i32) -> Option<u8> {
        let note = key_index - self.shift;
        let low = self.size.home_low();
        (low..low + i32::from(self.size.keys()))
            .contains(&note)
            .then_some(note as u8)
    }

    /// The hand as 32 bits, for `SharedAtomics::performance_lane_hand`: the
    /// key count over the shift in octaves.
    pub(crate) fn to_bits(self) -> u32 {
        let octaves = (self.shift / OCTAVE_KEYS) as i16 as u16;
        (u32::from(self.size.keys()) << 16) | u32::from(octaves)
    }

    /// The hand [`to_bits`](Self::to_bits) packed.
    pub(crate) fn from_bits(bits: u32) -> Hand {
        Hand {
            size: KeybedSize::from_keys((bits >> 16) as u8),
            shift: i32::from(bits as u16 as i16) * OCTAVE_KEYS,
        }
    }
}

/// How far each of the lane's keys jumps: a bar, half a bar or a beat of
/// the project's meter, picked from the lane's gutter and saved with the
/// project (`archive/510-performance-lane-keyboard.md` § Step 1b). The stored choice;
/// what plays is [`in_meter`](Self::in_meter) of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum KeyLength {
    /// A whole bar per key.
    #[default]
    Bar,
    /// Half a bar per key, in a meter whose numerator is even; in an odd one
    /// it plays as [`Beat`](Self::Beat).
    HalfBar,
    /// One counted beat per key (a quarter in x/4, an eighth in x/8): what a
    /// project saved before the choice existed plays.
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
/// a beat in every meter. A beat a key in 4/4: C-2 = bar 1, C♯-2 =
/// bar 1 beat 2, E-2 = bar 2, C-1 = bar 4, C3 = bar 16; a bar a key:
/// C♯-2 = bar 2, C-1 = bar 13. The one place the mapping lives, for the
/// trigger and the drawing.
pub(crate) fn key_tick(meter: Meter, length: KeyLength, key_index: i32) -> i32 {
    length.ticks(meter) * key_index
}

/// Whether `playback_tick` is on a beat of `meter` (a quarter in x/4, an
/// eighth in x/8), before bar 1 too. A lane press or a marker recall while
/// running jumps at once on a beat and waits for the next one off it, so a
/// key's start always sounds on a beat of the grid playback was on
/// (`610-launch-quantize.md`); a stop on the armed lane waits the same way.
pub(crate) fn is_on_beat(meter: Meter, playback_tick: i32) -> bool {
    playback_tick.rem_euclid(meter.beat_ticks()) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::time::PPQN;

    /// Plays `ticks` ticks from `from`, as the tick pump would.
    fn play(lane: &mut PerformanceLane, from: i32, ticks: i32) {
        for tick in from..from + ticks {
            lane.advance(tick);
        }
    }

    /// A take's stretches as `(at, len, from)`, and its length.
    type Taken = (Vec<(i32, i32, i32)>, i32);

    /// The lane's take, as [`Taken`].
    fn taken(lane: &mut PerformanceLane) -> Option<Taken> {
        lane.take_run().map(|take| {
            let stretches = take
                .segments
                .iter()
                .map(|segment| (segment.at, segment.len, segment.from))
                .collect();
            (stretches, take.length)
        })
    }

    #[test]
    fn each_press_lasts_until_the_next_and_the_last_until_the_capture() {
        let mut lane = PerformanceLane::new();
        lane.press(3840, true);
        play(&mut lane, 3840, 1000);
        lane.press(0, false);
        play(&mut lane, 0, 500);

        assert_eq!(
            taken(&mut lane),
            Some((vec![(0, 1000, 3840), (1000, 500, 0)], 1500))
        );
        assert!(lane.take_run().is_none(), "the capture uses the run up");
    }

    #[test]
    fn a_stopped_capture_ends_where_playback_stopped() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        play(&mut lane, 0, 700);
        // Stopped: no ticks reach the lane.
        assert_eq!(taken(&mut lane), Some((vec![(0, 700, 0)], 700)));
    }

    #[test]
    fn the_next_run_starts_at_the_first_press_after_a_capture() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        play(&mut lane, 0, 100);
        assert!(lane.take_run().is_some());
        play(&mut lane, 100, 50);
        assert!(lane.take_run().is_none(), "nothing pressed since");

        lane.press(960, false);
        play(&mut lane, 960, 30);
        assert_eq!(taken(&mut lane), Some((vec![(0, 30, 960)], 30)));
    }

    #[test]
    fn a_press_that_starts_the_transport_starts_a_new_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        play(&mut lane, 0, 100);
        lane.press(1920, true);
        play(&mut lane, 1920, 10);
        assert_eq!(taken(&mut lane), Some((vec![(0, 10, 1920)], 10)));
    }

    #[test]
    fn playback_moved_by_anything_but_a_press_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        play(&mut lane, 0, 100);
        // A ruler click took playback to bar 5.
        play(&mut lane, 3840, 1);
        assert!(lane.take_run().is_none());
    }

    #[test]
    fn presses_on_one_tick_keep_the_latest() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        lane.press(480, false);
        play(&mut lane, 480, 10);
        assert_eq!(taken(&mut lane), Some((vec![(0, 10, 480)], 10)));
    }

    #[test]
    fn a_run_with_nothing_lasting_a_tick_captures_nothing() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        assert!(lane.take_run().is_none());
        assert!(PerformanceLane::new().take_run().is_none());
    }

    #[test]
    fn clearing_the_lane_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        play(&mut lane, 0, 10);
        lane.clear();
        assert!(lane.take_run().is_none());
    }

    /// A waiting press joins the run when it lands on the beat, not when it
    /// was pressed: the run holds what was heard.
    #[test]
    fn a_waiting_press_joins_the_run_where_it_lands() {
        let meter = Meter::FOUR_FOUR;
        let mut lane = PerformanceLane::new();
        lane.press(3840, true);
        play(&mut lane, 3840, 300);
        lane.wait_to_press(960);
        assert_eq!(lane.land_due_jump(meter, 3840 + 300), None, "off a beat");
        play(&mut lane, 3840 + 300, PPQN - 300);
        assert_eq!(lane.land_due_jump(meter, 3840 + PPQN), Some(960));
        assert_eq!(lane.land_due_jump(meter, 960), None, "used up");
        play(&mut lane, 960, 100);
        assert_eq!(
            taken(&mut lane),
            Some((vec![(0, PPQN, 3840), (PPQN, 100, 960)], PPQN + 100))
        );
    }

    /// A run whose first press came while running starts on its key.
    #[test]
    fn a_first_press_while_running_starts_the_take_at_zero() {
        let meter = Meter::FOUR_FOUR;
        let mut lane = PerformanceLane::new();
        lane.wait_to_press(1920);
        assert_eq!(lane.land_due_jump(meter, 5 * PPQN), Some(1920));
        play(&mut lane, 1920, 200);
        assert_eq!(taken(&mut lane), Some((vec![(0, 200, 1920)], 200)));
    }

    #[test]
    fn the_latest_waiting_jump_wins() {
        let meter = Meter::FOUR_FOUR;
        let mut lane = PerformanceLane::new();
        lane.wait_to_press(960);
        lane.wait_to_recall(5000);
        assert_eq!(lane.land_due_jump(meter, PPQN), Some(5000));
        assert!(lane.take_run().is_none(), "a recall joins no run");
        lane.wait_to_recall(5000);
        lane.wait_to_press(1920);
        assert_eq!(lane.land_due_jump(meter, PPQN), Some(1920));
        assert!(lane.take_run().is_none(), "nothing played since it landed");
    }

    #[test]
    fn a_waiting_jump_is_dropped_by_a_stop_a_restart_a_clear_or_a_press() {
        let meter = Meter::FOUR_FOUR;
        let drops: [fn(&mut PerformanceLane); 4] = [
            |lane| {
                lane.stop_now(Meter::FOUR_FOUR, 300, true);
            },
            PerformanceLane::cancel_waiting,
            PerformanceLane::clear,
            |lane| lane.press(0, true),
        ];
        for drop in drops {
            let mut lane = PerformanceLane::new();
            lane.wait_to_press(960);
            drop(&mut lane);
            assert_eq!(lane.land_due_jump(meter, PPQN), None);
        }
    }

    /// A lane with a run going, as a performance leaves it.
    fn performing() -> PerformanceLane {
        let mut lane = PerformanceLane::new();
        lane.press(0, true);
        lane
    }

    #[test]
    fn a_stop_off_a_beat_waits_for_the_next_one() {
        let meter = Meter::FOUR_FOUR;
        let mut lane = performing();
        assert!(!lane.take_due_stop(meter, 0), "nothing asked");
        assert!(!lane.stop_now(meter, PPQN + 1, true));
        assert!(!lane.take_due_stop(meter, 2 * PPQN - 1));
        assert!(lane.take_due_stop(meter, 2 * PPQN));
        assert!(!lane.take_due_stop(meter, 3 * PPQN), "used up");
    }

    #[test]
    fn a_stop_on_a_beat_unarmed_or_asked_twice_is_now() {
        let meter = Meter::FOUR_FOUR;
        let mut lane = performing();
        assert!(lane.stop_now(meter, 2 * PPQN, true), "on a beat");
        assert!(lane.stop_now(meter, PPQN + 1, false), "unarmed");
        assert!(!lane.stop_now(meter, PPQN + 1, true));
        assert!(lane.stop_now(meter, PPQN + 2, true), "asked again");
        assert!(!lane.take_due_stop(meter, 2 * PPQN), "nothing left waiting");
    }

    /// With the lane selected but no key pressed, playback is just the
    /// arrangement: `Space` stops at once.
    #[test]
    fn a_stop_with_no_run_is_now() {
        let mut lane = PerformanceLane::new();
        assert!(lane.stop_now(Meter::FOUR_FOUR, PPQN + 1, true));
        assert!(!lane.take_due_stop(Meter::FOUR_FOUR, 2 * PPQN));
    }

    #[test]
    fn a_press_cancels_a_waiting_stop() {
        let mut lane = performing();
        assert!(!lane.stop_now(Meter::FOUR_FOUR, 300, true));
        lane.wait_to_press(960);
        assert!(!lane.take_due_stop(Meter::FOUR_FOUR, PPQN));
    }

    /// Disarming drops the run but not a waiting stop: it was asked for.
    #[test]
    fn clearing_keeps_a_waiting_stop() {
        let mut lane = performing();
        assert!(!lane.stop_now(Meter::FOUR_FOUR, 300, true));
        lane.clear();
        assert!(lane.take_due_stop(Meter::FOUR_FOUR, PPQN));
    }

    /// A beat is a quarter in x/4, before bar 1 too.
    #[test]
    fn a_tick_is_on_a_beat_at_every_quarter() {
        for (tick, on) in [
            (0, true),
            (7 * PPQN, true),
            (PPQN + 1, false),
            (PPQN - 1, false),
            (PPQN / 2, false),
            (-PPQN, true),
            (-100, false),
        ] {
            assert_eq!(is_on_beat(Meter::FOUR_FOUR, tick), on, "tick {tick}");
        }
    }

    #[test]
    fn x_8_counts_the_eighth() {
        let six_eight = meter(6, 8);
        let eighth = six_eight.beat_ticks();
        assert_eq!(eighth, PPQN / 2);
        assert!(is_on_beat(six_eight, 3 * eighth));
        assert!(!is_on_beat(six_eight, 3 * eighth + 100));
    }

    #[test]
    fn a_key_is_a_c_every_twelve_from_bar_one() {
        assert_eq!(key_pitch_class(0), 0);
        assert_eq!(key_pitch_class(12 * 9), 0);
        assert_eq!(key_pitch_class(1), 1);
        assert_eq!(key_pitch_class(12 * 30 + 11), 11);
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
            key_tick(meter, bar, 119),
            meter.bars_to_ticks(119),
            "key 119: bar 120"
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

    /// The notes a hand lights, lowest and highest, as the keys they land on.
    fn lit(hand: Hand) -> (i32, i32) {
        let low = hand.size().home_low();
        let high = low + i32::from(hand.size().keys()) - 1;
        (hand.key_index(low as u8), hand.key_index(high as u8))
    }

    /// 49 keys at a beat a key in 4/4.
    fn beat_hand(around_tick: i32) -> Hand {
        Hand::around(
            KeybedSize::Keys49,
            Meter::FOUR_FOUR,
            KeyLength::Beat,
            around_tick,
        )
    }

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// An octave at a beat a key in 4/4: three bars.
    const OCTAVE_4_4: i32 = 3 * BAR;

    #[test]
    fn a_keybed_sends_its_home_octave() {
        let home = |size: KeybedSize| {
            (
                size.home_low(),
                size.home_low() + i32::from(size.keys()) - 1,
            )
        };
        assert_eq!(home(KeybedSize::Keys25), (48, 72));
        assert_eq!(home(KeybedSize::Keys37), (48, 84));
        assert_eq!(home(KeybedSize::Keys49), (36, 84));
        assert_eq!(home(KeybedSize::Keys61), (36, 96));
        assert_eq!(home(KeybedSize::Keys88), (24, 111));
    }

    /// 49 keys at home (36–84, middle C3), a beat a key in 4/4: from a
    /// cursor at bar 73, the octave placing C3 nearest it puts C3 on bar 73.
    #[test]
    fn the_hand_sits_around_the_cursor() {
        let hand = beat_hand(72 * BAR);
        let at = |note| key_tick(Meter::FOUR_FOUR, KeyLength::Beat, hand.key_index(note));
        assert_eq!(at(60), 72 * BAR, "C3: bar 73");
        assert_eq!(at(36), 66 * BAR, "C1: bar 67");
        assert_eq!(at(84), 78 * BAR, "C5: bar 79");
    }

    /// The hand's middle stays within half an octave of the cursor: it
    /// moves an octave once the cursor is half an octave past it, a tie
    /// taking the octave ahead.
    #[test]
    fn the_hand_moves_by_octaves_either_side_of_halfway() {
        // C3 home at bar 16 (tick 15 bars).
        let middle = |cursor| {
            key_tick(
                Meter::FOUR_FOUR,
                KeyLength::Beat,
                beat_hand(cursor).key_index(60),
            )
        };
        let home = 15 * BAR;
        let half = OCTAVE_4_4 / 2;
        assert_eq!(middle(home), home);
        assert_eq!(middle(home + half - 1), home);
        assert_eq!(middle(home + half), home + OCTAVE_4_4, "the tie: ahead");
        assert_eq!(middle(home - half), home, "the tie back: ahead");
        assert_eq!(middle(home - half - 1), home - OCTAVE_4_4);
    }

    /// Near bar 1 the hand would start before it: it takes the octave up,
    /// its lowest key on bar 1 at the lowest.
    #[test]
    fn a_hand_never_lands_before_bar_one() {
        assert_eq!(lit(beat_hand(0)), (0, 48));
        assert_eq!(lit(beat_hand(BAR)), (0, 48));
        // 88 keys (24–111, middle 67): its middle nearest bar 1 would put
        // note 24 at key -36; the lowest it goes is key 0.
        let wide = Hand::around(KeybedSize::Keys88, Meter::FOUR_FOUR, KeyLength::Bar, 0);
        assert_eq!(lit(wide), (0, 87));
    }

    #[test]
    fn the_buttons_move_the_hand_an_octave() {
        let hand = beat_hand(72 * BAR);
        let up = hand.moved(1).unwrap();
        assert_eq!(lit(up), (lit(hand).0 + 12, lit(hand).1 + 12));
        assert_eq!(up.moved(-1), Some(hand));
    }

    #[test]
    fn an_octave_down_is_refused_at_bar_one() {
        let hand = beat_hand(0);
        assert_eq!(lit(hand).0, 0);
        assert_eq!(hand.moved(-1), None);
        assert!(hand.moved(1).unwrap().moved(-1).is_some());
    }

    /// The octave steps count in the chosen key length and meter.
    #[test]
    fn the_placement_counts_in_the_length_and_meter() {
        // A bar a key in 4/4: an octave is 12 bars. C3 home at bar 61;
        // from bar 80, bar 85 (5 ahead) is nearer than bar 73 (7 back).
        let bars = Hand::around(
            KeybedSize::Keys49,
            Meter::FOUR_FOUR,
            KeyLength::Bar,
            79 * BAR,
        );
        assert_eq!(bars.key_index(60), 84);
        // An eighth a key in 7/8: an octave is 12 eighths.
        let seven_eight = meter(7, 8);
        let eighth = seven_eight.beat_ticks();
        let eighths = Hand::around(
            KeybedSize::Keys25,
            seven_eight,
            KeyLength::Beat,
            100 * eighth,
        );
        assert_eq!(
            eighths.key_index(60),
            96,
            "the middle on eighth 97, the cursor on 101"
        );
        // Half a bar in 6/8: an octave is 6 bars.
        let six_eight = meter(6, 8);
        let halves = Hand::around(KeybedSize::Keys25, six_eight, KeyLength::HalfBar, 0);
        assert_eq!(lit(halves), (0, 24));
    }

    /// A hand starts on a C, so a note's letter is its key's.
    #[test]
    fn a_note_lands_on_a_key_of_its_letter() {
        for size in KeybedSize::ALL {
            for cursor_bar in [1, 16, 40, 73, 200] {
                let hand = Hand::around(
                    size,
                    Meter::FOUR_FOUR,
                    KeyLength::Beat,
                    (cursor_bar - 1) * BAR,
                );
                for note in 0..=127u8 {
                    let key = hand.key_index(note);
                    assert_eq!(key_pitch_class(key), note % 12);
                }
            }
        }
    }

    /// `note_on` is `key_index`'s inverse over the home notes, and lights
    /// nothing outside them.
    #[test]
    fn note_on_is_the_inverse_of_key_index_within_the_hand() {
        for size in KeybedSize::ALL {
            let hand = Hand::around(size, Meter::FOUR_FOUR, KeyLength::Beat, 73 * BAR);
            let low = size.home_low();
            let high = low + i32::from(size.keys()) - 1;
            for note in 0..=127u8 {
                let lights = (low..=high).contains(&i32::from(note));
                let back = hand.note_on(hand.key_index(note));
                assert_eq!(back, lights.then_some(note), "{size:?} note {note}");
            }
        }
    }

    #[test]
    fn a_hand_round_trips_through_bits() {
        for size in KeybedSize::ALL {
            for cursor_bar in [1, 16, 200, 5000] {
                let hand = Hand::around(
                    size,
                    Meter::FOUR_FOUR,
                    KeyLength::Beat,
                    (cursor_bar - 1) * BAR,
                );
                assert_eq!(Hand::from_bits(hand.to_bits()), hand);
                let up = hand.moved(3).unwrap();
                assert_eq!(Hand::from_bits(up.to_bits()), up);
            }
        }
        // A hand shifted down below its home notes.
        let low = Hand::around(KeybedSize::Keys49, Meter::FOUR_FOUR, KeyLength::Beat, 0);
        assert_eq!(Hand::from_bits(low.to_bits()), low);
    }

    #[test]
    fn keybed_sizes_by_key_count_and_step() {
        for size in KeybedSize::ALL {
            assert_eq!(KeybedSize::from_keys(size.keys()), size);
        }
        assert_eq!(KeybedSize::from_keys(30), KeybedSize::Keys49, "unknown");
        assert_eq!(KeybedSize::default().keys(), 49);
        assert_eq!(KeybedSize::Keys49.stepped(1), KeybedSize::Keys61);
        assert_eq!(KeybedSize::Keys49.stepped(-2), KeybedSize::Keys25);
        assert_eq!(KeybedSize::Keys88.stepped(1), KeybedSize::Keys88);
        assert_eq!(KeybedSize::Keys25.stepped(-1), KeybedSize::Keys25);
    }

    #[test]
    fn key_length_bits_round_trip() {
        for length in KeyLength::ALL {
            assert_eq!(KeyLength::from_bits(length.to_bits()), length);
        }
        assert_eq!(KeyLength::from_bits(200), KeyLength::Beat);
        assert_eq!(KeyLength::default(), KeyLength::Bar);
    }
}
