//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a bar instead
//! of capturing notes (`110-performance-lane.md`). All this type holds is the
//! one note currently held, so a `NoteOff` can be told from a stale release of
//! a note a later `NoteOn` already superseded. [`key_index_from_note`] is the
//! note-number → key mapping the lane triggers on, [`key_tick`] where a key
//! jumps to (one [`KeyLength`] of the project's meter per key: a bar, half a
//! bar or a beat), and [`note_for_key`] the mapping's inverse, which the
//! arranger draws the lane's keys from (`510-performance-lane-keyboard.md`).

use crate::core::config;
use crate::core::time::Meter;

/// Transport-automation lane: not a `Track`. Tracks which note (if any) is
/// currently held on the physical MIDI keyboard while the lane is armed, so
/// a NoteOff can be told apart from a stale release of an already-superseded
/// note. Purely a live-input concern for now — no recording/committing, see
/// `110-performance-lane.md` for the current scope.
#[derive(Clone, Debug, Default)]
pub(crate) struct PerformanceLane {
    /// The note currently held on the keyboard, or `None`. Monophonic.
    live_active: Option<u8>,
}

impl PerformanceLane {
    /// A lane with nothing held.
    pub(crate) fn new() -> Self {
        PerformanceLane::default()
    }

    /// Forgets any held note — used when the lane is disarmed.
    pub(crate) fn clear(&mut self) {
        self.live_active = None;
    }

    /// Starts a new live-held trigger. Monophonic: a new NoteOn simply
    /// supersedes whatever was previously held, with no bookkeeping of the
    /// note it replaced.
    pub(crate) fn begin_live_trigger(&mut self, note_number: u8) {
        self.live_active = Some(note_number);
    }

    /// Ends the live trigger, but only if `note_number` matches the
    /// currently active one. Returns `true` if it matched (the caller
    /// should treat this as a real release); a stale NoteOff for a note
    /// already superseded by a later NoteOn returns `false` and is a no-op.
    pub(crate) fn end_live_trigger(&mut self, note_number: u8) -> bool {
        if self.live_active == Some(note_number) {
            self.live_active = None;
            true
        } else {
            false
        }
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
    /// beat 2½, half an eighth in 7/8), which plays as a beat. The rule the
    /// arranger grid's half-bar rung follows (`grid.rs`). The stored choice is
    /// kept, so a project that goes to 3/4 and back is at a half bar again.
    pub(crate) fn in_meter(self, meter: Meter) -> KeyLength {
        match self {
            KeyLength::HalfBar if !offers_half_bar(meter) => KeyLength::Beat,
            length => length,
        }
    }

    /// One key's length in `meter`, in ticks, as it plays
    /// ([`in_meter`](Self::in_meter)).
    pub(crate) fn ticks(self, meter: Meter) -> i32 {
        match self.in_meter(meter) {
            KeyLength::Bar => meter.bar_ticks(),
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

/// Whether half a bar of `meter` lands on a beat: its numerator is even.
fn offers_half_bar(meter: Meter) -> bool {
    meter.numerator().is_multiple_of(2)
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

    #[test]
    fn begin_live_trigger_supersedes_previous_note() {
        let mut lane = PerformanceLane::new();
        lane.begin_live_trigger(60);
        lane.begin_live_trigger(62);

        assert!(!lane.end_live_trigger(60), "60 was superseded, not active");
        assert!(lane.end_live_trigger(62), "62 is the currently active note");
    }

    #[test]
    fn end_live_trigger_matching_note_clears_it() {
        let mut lane = PerformanceLane::new();
        lane.begin_live_trigger(60);

        assert!(lane.end_live_trigger(60));
        assert!(
            !lane.end_live_trigger(60),
            "already cleared, a second release is a no-op"
        );
    }

    #[test]
    fn end_live_trigger_stale_note_is_noop() {
        let mut lane = PerformanceLane::new();
        lane.begin_live_trigger(60);
        lane.begin_live_trigger(62); // supersedes 60

        assert!(!lane.end_live_trigger(60), "stale release must not match");
    }

    #[test]
    fn clear_drops_an_in_progress_trigger() {
        let mut lane = PerformanceLane::new();
        lane.begin_live_trigger(60);

        lane.clear();

        assert!(!lane.end_live_trigger(60));
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
