//! The arranger performance lane's model state.
//!
//! The lane arms the physical keyboard to jump the transport to a bar instead
//! of capturing notes (`110-performance-lane.md`). All this type holds is the
//! one note currently held, so a `NoteOff` can be told from a stale release of
//! a note a later `NoteOn` already superseded. [`bar_index_from_note`] is the
//! note-number → bar-index mapping the lane triggers on.

use crate::core::config;

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

/// Maps a MIDI note number to a 0-based bar index (note
/// `PERFORMANCE_LANE_BASE_NOTE` = bar 0 / "bar 1"), or `None` for notes
/// below the base note (ignored — there is no bar before bar 1).
pub(crate) fn bar_index_from_note(note_number: u8) -> Option<i32> {
    note_number
        .checked_sub(config::PERFORMANCE_LANE_BASE_NOTE)
        .map(i32::from)
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
    fn bar_index_from_note_below_base_is_none() {
        assert_eq!(
            bar_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE - 1),
            None
        );
    }

    #[test]
    fn bar_index_from_note_at_base_is_zero() {
        assert_eq!(
            bar_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE),
            Some(0)
        );
    }

    #[test]
    fn bar_index_from_note_above_base_increments() {
        assert_eq!(
            bar_index_from_note(config::PERFORMANCE_LANE_BASE_NOTE + 3),
            Some(3)
        );
    }
}
