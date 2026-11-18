//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a bar instead
//! of capturing notes (`110-performance-lane.md`). All this type holds is the
//! one note currently held, so a `NoteOff` can be told from a stale release of
//! a note a later `NoteOn` already superseded. [`key_index_from_note`] is the
//! note-number → key mapping the lane triggers on, [`key_tick`] where a key
//! jumps to (one beat of the project's meter per key),
//! and [`note_for_key`] the mapping's inverse, which the arranger draws the
//! lane's keys from (`510-performance-lane-keyboard.md`).

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

/// The note of 0-based key `key_index`: the inverse of
/// [`key_index_from_note`]. `None` for a key past note 127 (key index 68 on,
/// from C3) or before the first.
pub(crate) fn note_for_key(key_index: i32) -> Option<u8> {
    let note = u8::try_from(key_index)
        .ok()?
        .checked_add(config::PERFORMANCE_LANE_BASE_NOTE)?;
    (note <= 127).then_some(note)
}

/// Where key `key_index` jumps to, in ticks: the start of its beat, one
/// counted beat of `meter` per key (a quarter in x/4, an eighth in x/8), so
/// every jump lands on a beat in every meter. From C3 in 4/4: C3 = bar 1,
/// C♯3 = bar 1 beat 2, E3 = bar 2, C4 = bar 4; 68 keys reach 17 bars.
pub(crate) fn key_tick(meter: Meter, key_index: i32) -> i32 {
    meter.beat_ticks() * key_index
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

    #[test]
    fn each_key_jumps_a_beat() {
        let meter = Meter::FOUR_FOUR;
        assert_eq!(key_tick(meter, 0), 0, "C3: bar 1");
        assert_eq!(key_tick(meter, 1), meter.beat_ticks(), "C#3: bar 1 beat 2");
        assert_eq!(key_tick(meter, 4), meter.bars_to_ticks(1), "E3: bar 2");
        assert_eq!(key_tick(meter, 12), meter.bars_to_ticks(3), "C4: bar 4");
    }

    #[test]
    fn a_bar_is_as_many_keys_as_the_meter_has_beats() {
        for (numerator, denominator) in [(3, 4), (7, 8), (1, 8), (5, 4), (6, 8)] {
            let meter = Meter::new(numerator, denominator).expect("a valid meter");
            assert_eq!(
                key_tick(meter, i32::from(numerator)),
                meter.bar_ticks(),
                "{numerator}/{denominator}"
            );
        }
    }
}
