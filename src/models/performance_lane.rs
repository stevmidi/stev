//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a key's start
//! instead of capturing notes (`110-performance-lane.md`). What it holds is
//! the **run**: the presses since playback started, each with where it
//! jumped, on a clock of its own that counts only the ticks playback
//! advances, which `/` renders into clips at the arranger cursor
//! (`520-performance-capture.md` decisions 1 and 2): [`PerformanceLane::take_run`]
//! gives it as a [`Take`], the stretches of the arrangement it played.
//! The lane's keys are a cycle of [`CYCLE_KEYS`] repeating along the
//! timeline (`560-performance-lane-reach.md`), each named by
//! [`note_for_key`], which the arranger draws them from
//! (`510-performance-lane-keyboard.md`). [`KeybedWindow`] is the guess of
//! the notes the player's keyboard covers, of a [`KeybedSize`] from
//! settings; its [`Hand`] around the arranger cursor maps a pressed note to
//! its key along the timeline, the whole hand on the stretch of copies
//! nearest the cursor ([`nearest_copy_tick`] of its middle key, `560` § 2).
//! [`key_tick`] is where a key starts (one [`KeyLength`] of the project's
//! meter per key: a bar, half a bar or a beat), and [`phase_offset`] how far
//! into it a press while running lands, so the beat never breaks
//! (`550-phase-jumps.md`).

use crate::core::time::{Meter, snap_to_grid};

/// The lane's live state: the run its presses make, and a stop waiting for
/// the next beat. Not saved, not an undo step. See the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformanceLane {
    /// The presses since the run began, `None` with no run.
    run: Option<Run>,
    /// `Space` while armed, off a beat: playback runs on to the next beat
    /// and stops there (`550-phase-jumps.md` § The stop on the beat).
    stop_pending: bool,
}

/// One run of presses (`520-performance-capture.md` decision 2): from the
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
    /// The first press's phase offset ([`phase_offset`]): how far past its
    /// key's start it jumped. `take_run` shifts the take by it, so the first
    /// key's start lands at 0 (`550-phase-jumps.md` decision 5).
    lead: i32,
    /// Where playback is when the lane alone has moved it: the last press's
    /// key plus the ticks since. Playback anywhere else means something else
    /// moved it (a ruler click, `Space` from the cursor), so what follows is
    /// no longer what the presses played and the run ends.
    expected_tick: i32,
}

/// What a run played, for its render (`520-performance-capture.md`
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

    /// Ends the run: the lane was disarmed, or a project replaced.
    pub(crate) fn clear(&mut self) {
        self.run = None;
    }

    /// A press jumped playback to `target_tick`, `offset` past its key's
    /// start ([`phase_offset`], 0 for a press that starts the transport). It
    /// starts a run when there is none, or when `started` (it started the
    /// transport: the run is "from the press that started the transport"),
    /// and is added to it. A
    /// press on the same tick as the one before replaces it, as live the
    /// latest press wins.
    pub(crate) fn press(&mut self, target_tick: i32, offset: i32, started: bool) {
        self.stop_pending = false;
        if started {
            self.run = None;
        }
        let run = self.run.get_or_insert_with(|| Run {
            elapsed: 0,
            presses: Vec::new(),
            lead: 0,
            expected_tick: target_tick,
        });
        if run.presses.last().is_some_and(|&(at, _)| at == run.elapsed) {
            run.presses.pop();
        }
        if run.presses.is_empty() {
            run.lead = offset;
        }
        run.presses.push((run.elapsed, target_tick));
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

    /// `Space` while running, with playback about to play `playback_tick`:
    /// whether to stop now. During a performance (the lane `armed` and a run
    /// of presses going) with playback off a beat of `meter`, the stop waits
    /// for the next beat instead
    /// ([`take_due_stop`](Self::take_due_stop)); a second press while it
    /// waits stops at once (`550-phase-jumps.md` § The stop on the beat).
    pub(crate) fn stop_now(&mut self, meter: Meter, playback_tick: i32, armed: bool) -> bool {
        let wait = armed
            && self.run.is_some()
            && !self.stop_pending
            && phase_offset(meter, playback_tick) != 0;
        self.stop_pending = wait;
        !wait
    }

    /// Drops a stop waiting for the next beat: playback is starting again.
    pub(crate) fn cancel_stop(&mut self) {
        self.stop_pending = false;
    }

    /// Whether a waiting stop is due now, with playback about to play
    /// `playback_tick`: it is on a beat of `meter`. A stop that is due is
    /// used up.
    pub(crate) fn take_due_stop(&mut self, meter: Meter, playback_tick: i32) -> bool {
        let due = self.stop_pending && phase_offset(meter, playback_tick) == 0;
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
    /// The take is placed so the first press's key starts at 0
    /// (`550-phase-jumps.md` decision 5): a first press that jumped late
    /// leaves a silence of its offset before the first stretch, one that
    /// jumped early drops the lead-in it played before its key. Every later
    /// stretch then sits on the grid it was heard on.
    pub(crate) fn take_run(&mut self) -> Option<Take> {
        let run = self.run.take()?;
        let ends = run.presses.iter().skip(1).map(|&(at, _)| at);
        let segments: Vec<Segment> = run
            .presses
            .iter()
            .zip(ends.chain([run.elapsed]))
            .filter_map(|(&(at, from), until)| {
                let at = at + run.lead;
                let cut = (-at).max(0);
                let len = until + run.lead - at - cut;
                (len > 0).then_some(Segment {
                    at: at + cut,
                    len,
                    from: from + cut,
                })
            })
            .collect();
        (!segments.is_empty()).then_some(Take {
            segments,
            length: run.elapsed + run.lead,
        })
    }
}

/// How many keys the lane's cycle has (`560-performance-lane-reach.md`
/// § 1): ten octaves, so the note names run unbroken across the wrap. The
/// keys repeat along the timeline, a cycle of them after another: 120 bars
/// at a bar a key, 30 bars of 4/4 at a beat.
pub(crate) const CYCLE_KEYS: i32 = 120;

/// The name of key `key_index`, counted along the timeline from bar 1, so
/// any copy of a key: a note in `0..120`, a key being its note number in the
/// cycle (C-2 = key 0, bar 1's in the first cycle; keys 0–7 named by notes
/// 0–7, not by 120–127, which the next cycle's keys 0–7 play from a hand
/// reaching past the cycle, [`Hand`]). The arranger draws the lane's keys
/// from it (`510-performance-lane-keyboard.md`).
pub(crate) fn note_for_key(key_index: i32) -> u8 {
    // `rem_euclid` keeps it in `0..120`.
    key_index.rem_euclid(CYCLE_KEYS) as u8
}

/// How many keys the player's keyboard has (`560-performance-lane-reach.md`
/// § 3): a setting, not the project's, since it describes the hardware.
/// Assumed to start on a C. What [`KeybedWindow`] lights.
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

    /// The size `step` places along [`ALL`](Self::ALL) from this one,
    /// stopping at either end.
    pub(crate) fn stepped(self, step: isize) -> KeybedSize {
        // The variants are declared in `ALL`'s order.
        let at = (self as usize).saturating_add_signed(step);
        KeybedSize::ALL[at.min(KeybedSize::ALL.len() - 1)]
    }
}

/// Stev's guess of the notes the player's keyboard covers
/// (`560-performance-lane-reach.md` § 4): a window of the keybed's size
/// starting on a C, moved by whole octaves, just enough, to take in a note
/// played outside it, as a hardware octave button shifts it. A press jumps
/// by it ([`hand`](Self::hand)), and the lane lights it: a wrong guess (an
/// octave off until a note near the edge) can put the whole hand a cycle
/// away, where it is lit (`560` § 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeybedWindow {
    /// The keyboard's size.
    size: KeybedSize,
    /// The lowest note it covers, a C (a multiple of 12).
    low: i32,
}

impl KeybedWindow {
    /// A `size` keyboard at its home octave: centred on C3 (note 60) to the
    /// octave below, so a 49-key keybed covers notes 36–84 and a 25-key one
    /// 48–72.
    pub(crate) fn new(size: KeybedSize) -> Self {
        let octaves = (i32::from(size.keys()) - 1) / 12;
        KeybedWindow {
            size,
            low: 60 - 12 * (octaves / 2),
        }
    }

    /// The keyboard's size.
    pub(crate) fn size(self) -> KeybedSize {
        self.size
    }

    /// The same guess for a keyboard of `size`, from the same lowest C, but
    /// no higher than the lowest C that still reaches note 127, so a bigger
    /// keybed never covers mostly notes past MIDI's range. The next note
    /// outside it puts it right.
    pub(crate) fn resized(self, size: KeybedSize) -> Self {
        let keys = i32::from(size.keys());
        KeybedWindow {
            size,
            low: self.low.min((127 - keys + 12) / 12 * 12),
        }
    }

    /// The notes covered, `low..=high`.
    fn high(self) -> i32 {
        self.low + i32::from(self.size.keys()) - 1
    }

    /// Whether `note` is in the window.
    pub(crate) fn contains(self, note: u8) -> bool {
        (self.low..=self.high()).contains(&i32::from(note))
    }

    /// `note` was played: a note outside the window moves it by whole
    /// octaves, just enough to take it in.
    pub(crate) fn played(&mut self, note: u8) {
        let note = i32::from(note);
        if note < self.low {
            // The C at or below it.
            self.low = note / 12 * 12;
        } else if note > self.high() {
            // The lowest C that still reaches it.
            let keys = i32::from(self.size.keys());
            self.low = (note - keys + 12) / 12 * 12;
        }
    }

    /// The hand around `around_tick` (the arranger cursor) at `length` keys
    /// of `meter`: the notes laid unbroken along the timeline, note `n` on
    /// key `n` plus a whole number of cycles, chosen so the middle key's copy
    /// is the one nearest `around_tick` ([`nearest_copy_tick`]: a tie the
    /// copy ahead), and no note before bar 1: a hand reaching past the
    /// cycle's end whose middle is nearest the cycle's start (a high hand
    /// near bar 1) sits a cycle on, whole. One stretch for the whole hand,
    /// so it never splits (`560-performance-lane-reach.md` § 2).
    pub(crate) fn hand(self, meter: Meter, length: KeyLength, around_tick: i32) -> Hand {
        let middle = self.low + (i32::from(self.size.keys()) - 1) / 2;
        let middle_key = middle.rem_euclid(CYCLE_KEYS);
        let first = key_tick(meter, length, middle_key);
        let cycles = (nearest_copy_tick(meter, length, middle_key, around_tick) - first)
            / key_tick(meter, length, CYCLE_KEYS);
        // A middle past the cycle (120–127) on its first copy would put the
        // hand's notes below 120 before bar 1; 0 is the next copy up.
        Hand {
            shift: (middle_key - middle + cycles * CYCLE_KEYS).max(0),
        }
    }

    /// The guess as 16 bits, for `LiveNotes`: the key count over the lowest
    /// note.
    pub(crate) fn to_bits(self) -> u16 {
        (u16::from(self.size.keys()) << 8) | self.low as u16
    }

    /// The guess [`to_bits`](Self::to_bits) packed.
    pub(crate) fn from_bits(bits: u16) -> Self {
        KeybedWindow {
            size: KeybedSize::from_keys((bits >> 8) as u8),
            low: i32::from(bits & 0xFF),
        }
    }
}

/// Where the notes of the player's hand land along the timeline, from one
/// cursor position ([`KeybedWindow::hand`]): each note `n` on key `n +
/// shift`, `shift` a whole number of cycles, so the hand lies unbroken and
/// in order, across a cycle's wrap too, and never before bar 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hand {
    /// Keys from a note's number to its key along the timeline, a
    /// non-negative multiple of [`CYCLE_KEYS`].
    shift: i32,
}

impl Hand {
    /// The key along the timeline that `note` jumps to.
    pub(crate) fn key_index(self, note: u8) -> i32 {
        i32::from(note) + self.shift
    }

    /// The note that jumps to the key at `key_index`, if any:
    /// [`key_index`](Self::key_index)'s inverse, what lights a held key.
    pub(crate) fn note_on(self, key_index: i32) -> Option<u8> {
        u8::try_from(key_index - self.shift)
            .ok()
            .filter(|&note| note <= 127)
    }
}

/// How far each of the lane's keys jumps: a bar, half a bar or a beat of
/// the project's meter, picked from the lane's gutter and saved with the
/// project (`510-performance-lane-keyboard.md` § Step 1b). The stored choice;
/// what plays is [`in_meter`](Self::in_meter) of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum KeyLength {
    /// A whole bar per key: a cycle of keys reaches 120 bars.
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

/// Where the copy of key `key_index` (in the cycle, `0..CYCLE_KEYS`) nearest
/// `around_tick` starts: the keys repeat every [`CYCLE_KEYS`] keys, and a
/// press jumps to the copy nearest the arranger cursor, running or stopped
/// (`560-performance-lane-reach.md` § 2), so the keys reach about half a
/// cycle back and half ahead of it, and only a cursor move changes which. A tie, exactly half a cycle either way, picks
/// the copy ahead; a copy before bar 1 is never chosen.
fn nearest_copy_tick(meter: Meter, length: KeyLength, key_index: i32, around_tick: i32) -> i32 {
    let first = key_tick(meter, length, key_index);
    let cycle = key_tick(meter, length, CYCLE_KEYS);
    // A half rounds up: the tie goes to the copy ahead.
    first + snap_to_grid(around_tick - first, cycle).max(0)
}

/// How far past its key's start a press while running jumps, when playback
/// is about to play `playback_tick`: its distance from the nearest beat of
/// `meter` ([`snap_to_grid`]), in `-beat/2 .. beat/2` (exactly half a beat
/// counts as early, for the next beat).
///
/// The press jumps to its key plus this, so the key's start sounds on a beat
/// of the grid playback was on and the beat never breaks: a late press skips
/// the start of its key, an early one plays the end of what comes before it
/// (`550-phase-jumps.md`).
pub(crate) fn phase_offset(meter: Meter, playback_tick: i32) -> i32 {
    playback_tick - snap_to_grid(playback_tick, meter.beat_ticks())
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
        lane.press(3840, 0, true);
        play(&mut lane, 3840, 1000);
        lane.press(0, 0, false);
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
        lane.press(0, 0, true);
        play(&mut lane, 0, 700);
        // Stopped: no ticks reach the lane.
        assert_eq!(taken(&mut lane), Some((vec![(0, 700, 0)], 700)));
    }

    #[test]
    fn the_next_run_starts_at_the_first_press_after_a_capture() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        play(&mut lane, 0, 100);
        assert!(lane.take_run().is_some());
        play(&mut lane, 100, 50);
        assert!(lane.take_run().is_none(), "nothing pressed since");

        lane.press(960, 0, false);
        play(&mut lane, 960, 30);
        assert_eq!(taken(&mut lane), Some((vec![(0, 30, 960)], 30)));
    }

    #[test]
    fn a_press_that_starts_the_transport_starts_a_new_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        play(&mut lane, 0, 100);
        lane.press(1920, 0, true);
        play(&mut lane, 1920, 10);
        assert_eq!(taken(&mut lane), Some((vec![(0, 10, 1920)], 10)));
    }

    #[test]
    fn playback_moved_by_anything_but_a_press_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        play(&mut lane, 0, 100);
        // A ruler click took playback to bar 5.
        play(&mut lane, 3840, 1);
        assert!(lane.take_run().is_none());
    }

    #[test]
    fn presses_on_one_tick_keep_the_latest() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        lane.press(480, 0, false);
        play(&mut lane, 480, 10);
        assert_eq!(taken(&mut lane), Some((vec![(0, 10, 480)], 10)));
    }

    #[test]
    fn a_run_with_nothing_lasting_a_tick_captures_nothing() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        assert!(lane.take_run().is_none());
        assert!(PerformanceLane::new().take_run().is_none());
    }

    #[test]
    fn clearing_the_lane_ends_the_run() {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
        play(&mut lane, 0, 10);
        lane.clear();
        assert!(lane.take_run().is_none());
    }

    /// A first press that jumped late leaves its offset as silence before
    /// the first stretch; the later stretches stay on the grid they were
    /// heard on, and the take is that much longer.
    #[test]
    fn a_late_first_press_starts_its_key_at_zero() {
        let mut lane = PerformanceLane::new();
        lane.press(3840 + 100, 100, false);
        play(&mut lane, 3840 + 100, 860);
        lane.press(960, 0, false);
        play(&mut lane, 960, 960);
        assert_eq!(
            taken(&mut lane),
            Some((vec![(100, 860, 3940), (960, 960, 960)], 1920))
        );
    }

    /// A first press that jumped early drops the lead-in it played before
    /// its key, so the key's start is at 0 and the take that much shorter.
    #[test]
    fn an_early_first_press_drops_its_lead_in() {
        let mut lane = PerformanceLane::new();
        lane.press(3840 - 100, -100, false);
        play(&mut lane, 3840 - 100, 1060);
        lane.press(0, 0, false);
        play(&mut lane, 0, 500);
        assert_eq!(
            taken(&mut lane),
            Some((vec![(0, 960, 3840), (960, 500, 0)], 1460))
        );
    }

    /// A lead-in longer than the first stretch drops it whole and cuts into
    /// the next.
    #[test]
    fn a_lead_in_past_the_first_stretch_cuts_into_the_next() {
        let mut lane = PerformanceLane::new();
        lane.press(3840 - 300, -300, false);
        play(&mut lane, 3840 - 300, 200);
        lane.press(960, 0, false);
        play(&mut lane, 960, 500);
        assert_eq!(taken(&mut lane), Some((vec![(0, 400, 1060)], 400)));
    }

    /// Only the run's first press sets the shift, and a press replacing it on
    /// the same tick replaces its offset too.
    #[test]
    fn the_shift_is_the_first_press_that_lasted() {
        let mut lane = PerformanceLane::new();
        lane.press(3840 + 50, 50, false);
        lane.press(960 + 20, 20, false);
        play(&mut lane, 980, 100);
        lane.press(1920 + 30, 30, false);
        play(&mut lane, 1950, 100);
        assert_eq!(
            taken(&mut lane),
            Some((vec![(20, 100, 980), (120, 100, 1950)], 220))
        );
    }

    /// A lane with a run going, as a performance leaves it.
    fn performing() -> PerformanceLane {
        let mut lane = PerformanceLane::new();
        lane.press(0, 0, true);
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
        lane.press(960, 0, false);
        assert!(!lane.take_due_stop(Meter::FOUR_FOUR, 960));
    }

    /// Where a press jumps relative to its key, by where playback is: on a
    /// beat at it, late past it, early ahead of it, half a beat counting as
    /// early for the next beat, before bar 1 alike.
    #[test]
    fn a_press_lands_its_offset_from_the_nearest_beat() {
        for (tick, offset) in [
            (0, 0),
            (7 * PPQN, 0),
            (2 * PPQN + 300, 300),
            (PPQN + 1, 1),
            (2 * PPQN - 200, -200),
            (PPQN - 1, -1),
            (PPQN / 2 - 1, PPQN / 2 - 1),
            (PPQN / 2, -(PPQN / 2)),
            (-100, -100),
            (-PPQN + 100, 100),
        ] {
            assert_eq!(phase_offset(Meter::FOUR_FOUR, tick), offset, "tick {tick}");
        }
    }

    #[test]
    fn x_8_measures_to_the_eighth() {
        let six_eight = meter(6, 8);
        let eighth = six_eight.beat_ticks();
        assert_eq!(eighth, PPQN / 2);
        assert_eq!(phase_offset(six_eight, 3 * eighth + 100), 100);
        assert_eq!(phase_offset(six_eight, 3 * eighth - 100), -100);
    }

    #[test]
    fn a_key_is_named_by_its_note_number() {
        for key_index in 0..CYCLE_KEYS {
            assert_eq!(i32::from(note_for_key(key_index)), key_index);
        }
        assert_eq!(note_for_key(7), 7, "G-2, not G8");
    }

    #[test]
    fn note_for_key_repeats_along_the_timeline() {
        for key_index in [0, 5, 59, 60, 119] {
            assert_eq!(
                note_for_key(key_index + CYCLE_KEYS),
                note_for_key(key_index)
            );
            assert_eq!(
                note_for_key(key_index + 7 * CYCLE_KEYS),
                note_for_key(key_index)
            );
        }
    }

    /// A beat a key in 4/4: a cycle is 30 bars.
    const CYCLE_4_4: i32 = 30 * 3840;

    fn nearest_beat_key(key_index: i32, around_tick: i32) -> i32 {
        nearest_copy_tick(Meter::FOUR_FOUR, KeyLength::Beat, key_index, around_tick)
    }

    #[test]
    fn a_cycle_is_thirty_bars_at_a_beat_a_key_in_four_four() {
        assert_eq!(
            key_tick(Meter::FOUR_FOUR, KeyLength::Beat, CYCLE_KEYS),
            CYCLE_4_4
        );
    }

    /// From bar 10, C-2 lands on bar 1; from bar 25, on bar 31.
    #[test]
    fn a_press_takes_the_copy_nearest_the_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(nearest_beat_key(0, 9 * bar), 0, "bar 10: bar 1");
        assert_eq!(nearest_beat_key(0, 24 * bar), CYCLE_4_4, "bar 25: bar 31");
        // E-2 (bar 2) from bar 40: its second copy, bar 32.
        assert_eq!(nearest_beat_key(4, 39 * bar), CYCLE_4_4 + bar);
        // Far along: the copy around bar 100.
        assert_eq!(nearest_beat_key(0, 99 * bar), 3 * CYCLE_4_4);
    }

    #[test]
    fn half_a_cycle_either_way_picks_the_copy_ahead() {
        let half = CYCLE_4_4 / 2;
        assert_eq!(nearest_beat_key(0, half - 1), 0);
        assert_eq!(nearest_beat_key(0, half), CYCLE_4_4, "the tie");
        assert_eq!(nearest_beat_key(10, 10 * 960 + half), 10 * 960 + CYCLE_4_4);
        assert_eq!(nearest_beat_key(10, 10 * 960 + half - 1), 10 * 960);
    }

    #[test]
    fn a_copy_before_bar_one_is_never_chosen() {
        // B7, the cycle's last key, from bar 1: its copy before bar 1 is a
        // beat away, the first one a cycle less a beat.
        assert_eq!(nearest_beat_key(CYCLE_KEYS - 1, 0), CYCLE_4_4 - 960);
        assert_eq!(nearest_beat_key(CYCLE_KEYS - 1, -5000), CYCLE_4_4 - 960);
    }

    #[test]
    fn the_nearest_copy_counts_in_the_chosen_length_and_meter() {
        let four_four = Meter::FOUR_FOUR;
        let bar = four_four.bar_ticks();
        // A bar a key: a cycle is 120 bars, so the keys reach ±60.
        assert_eq!(nearest_copy_tick(four_four, KeyLength::Bar, 0, 59 * bar), 0);
        assert_eq!(
            nearest_copy_tick(four_four, KeyLength::Bar, 0, 60 * bar),
            120 * bar
        );
        // Half a bar in 6/8 (three eighths): a cycle is 60 bars.
        let six_eight = meter(6, 8);
        let six_bar = six_eight.bar_ticks();
        assert_eq!(
            nearest_copy_tick(six_eight, KeyLength::HalfBar, 2, 50 * six_bar),
            61 * six_bar
        );
        // A beat in 7/8: a cycle is 120 eighths.
        let seven_eight = meter(7, 8);
        let eighth = seven_eight.beat_ticks();
        assert_eq!(
            nearest_copy_tick(seven_eight, KeyLength::Beat, 3, 100 * eighth),
            123 * eighth
        );
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
            key_tick(meter, bar, CYCLE_KEYS - 1),
            meter.bars_to_ticks(119),
            "B7: bar 120, the cycle's last key"
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

    /// The notes a window covers, as `(lowest, highest)`.
    fn covered(window: KeybedWindow) -> (u8, u8) {
        let notes: Vec<u8> = (0..=127).filter(|&n| window.contains(n)).collect();
        (notes[0], notes[notes.len() - 1])
    }

    #[test]
    fn a_keybed_starts_at_its_home_octave() {
        let home = |size| covered(KeybedWindow::new(size));
        assert_eq!(home(KeybedSize::Keys25), (48, 72));
        assert_eq!(home(KeybedSize::Keys37), (48, 84));
        assert_eq!(home(KeybedSize::Keys49), (36, 84));
        assert_eq!(home(KeybedSize::Keys61), (36, 96));
        assert_eq!(home(KeybedSize::Keys88), (24, 111));
    }

    #[test]
    fn a_note_inside_the_window_leaves_it() {
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        for note in [48, 60, 72] {
            window.played(note);
            assert_eq!(covered(window), (48, 72), "note {note}");
        }
    }

    #[test]
    fn a_note_outside_moves_the_window_by_whole_octaves_just_enough() {
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(73);
        assert_eq!(covered(window), (60, 84), "an octave up");
        window.played(47);
        assert_eq!(covered(window), (36, 60), "two down: one is too few");
        window.played(110);
        assert_eq!(covered(window), (96, 120), "five up at once");
        window.played(127);
        assert_eq!(covered(window), (108, 127), "the top: covered up to 132");
        window.played(0);
        assert_eq!(covered(window), (0, 24), "the bottom");
    }

    #[test]
    fn a_resized_window_keeps_its_lowest_c() {
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(96);
        let window = window.resized(KeybedSize::Keys49);
        assert_eq!(window.size(), KeybedSize::Keys49);
        assert_eq!(covered(window), (72, 120));
    }

    /// A beat a key in 4/4, the hand at home on 49 keys (36–84, middle C3
    /// at bar 16): from a cursor at bar 73, C3's nearest copy is bar 76, so
    /// C2 lands on bar 73 and the hand spans bars 70–82.
    #[test]
    fn the_hand_sits_around_the_cursor() {
        let meter = Meter::FOUR_FOUR;
        let bar = meter.bar_ticks();
        let hand = KeybedWindow::new(KeybedSize::Keys49).hand(meter, KeyLength::Beat, 72 * bar);
        let at = |note| key_tick(meter, KeyLength::Beat, hand.key_index(note));
        assert_eq!(at(48), 72 * bar, "C2: bar 73");
        assert_eq!(at(36), 69 * bar, "C1: bar 70");
        assert_eq!(at(60), 75 * bar, "C3: bar 76");
        assert_eq!(at(84), 81 * bar, "C4: bar 82");
    }

    /// The whole hand moves together, when its middle's copy switches: a
    /// 25-key hand on notes 0–24, middle C-1 at bar 4, stays until the
    /// cursor is half a cycle past it (bar 19), then every note moves on.
    #[test]
    fn a_cursor_move_switches_the_hand_whole() {
        let meter = Meter::FOUR_FOUR;
        let bar = meter.bar_ticks();
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(0);
        for (cursor_bar, shift) in [(10, 0), (18, 0), (19, CYCLE_KEYS)] {
            let hand = window.hand(meter, KeyLength::Beat, (cursor_bar - 1) * bar);
            for note in [0, 12, 24] {
                assert_eq!(
                    hand.key_index(note),
                    i32::from(note) + shift,
                    "note {note} from bar {cursor_bar}"
                );
            }
        }
    }

    /// A hand reaching past the cycle's end lies on unbroken: note 125 (F8)
    /// is key 5 of the next cycle, right after B7.
    #[test]
    fn the_hand_runs_on_across_a_cycle() {
        let meter = Meter::FOUR_FOUR;
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(127);
        // Notes 108–132, middle 120: key 0 of the second cycle.
        let hand = window.hand(meter, KeyLength::Bar, 115 * meter.bar_ticks());
        assert_eq!(hand.key_index(108), 108);
        assert_eq!(hand.key_index(119), 119);
        assert_eq!(hand.key_index(120), CYCLE_KEYS);
        assert_eq!(hand.key_index(125), CYCLE_KEYS + 5);
    }

    /// From bar 1 that hand's middle (key 0) is bar 1 itself, which would put
    /// its notes below 120 before bar 1: the whole hand sits a cycle on
    /// instead, unbroken.
    #[test]
    fn a_hand_never_lands_before_bar_one() {
        let meter = Meter::FOUR_FOUR;
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(127);
        let hand = window.hand(meter, KeyLength::Bar, 0);
        assert_eq!(hand.key_index(108), 108);
        assert_eq!(hand.key_index(120), CYCLE_KEYS);
        assert_eq!(hand.key_index(127), CYCLE_KEYS + 7);
        assert_eq!(hand.note_on(0), Some(0), "outside the hand");
        assert_eq!(hand.note_on(CYCLE_KEYS + 8), None, "past note 127");
    }

    /// `note_on` is `key_index`'s inverse, whatever the hand and cursor.
    #[test]
    fn note_on_is_the_inverse_of_key_index() {
        let meter = Meter::FOUR_FOUR;
        let bar = meter.bar_ticks();
        for played in [0, 60, 127] {
            for cursor_bar in [1, 16, 40, 73, 200] {
                let mut window = KeybedWindow::new(KeybedSize::Keys61);
                window.played(played);
                let hand = window.hand(meter, KeyLength::Beat, (cursor_bar - 1) * bar);
                for note in 0..=127 {
                    let key = hand.key_index(note);
                    assert!(key >= 0);
                    assert_eq!(
                        hand.note_on(key),
                        Some(note),
                        "note {note}, hand at {played}, bar {cursor_bar}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_keybed_window_round_trips_through_bits() {
        for size in KeybedSize::ALL {
            let mut window = KeybedWindow::new(size);
            assert_eq!(KeybedWindow::from_bits(window.to_bits()), window);
            window.played(127);
            assert_eq!(KeybedWindow::from_bits(window.to_bits()), window);
            window.played(0);
            assert_eq!(KeybedWindow::from_bits(window.to_bits()), window);
        }
    }

    /// A 25-key window at its top (108–132) resized to 88 keys comes down to
    /// 48–135, not 108–195; a window low enough keeps its lowest C.
    #[test]
    fn a_resized_window_stays_within_midi() {
        let mut window = KeybedWindow::new(KeybedSize::Keys25);
        window.played(127);
        assert_eq!(covered(window.resized(KeybedSize::Keys88)), (48, 127));
        assert_eq!(covered(window.resized(KeybedSize::Keys49)), (84, 127));
        let home = KeybedWindow::new(KeybedSize::Keys25);
        assert_eq!(covered(home.resized(KeybedSize::Keys61)), (48, 108));
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
