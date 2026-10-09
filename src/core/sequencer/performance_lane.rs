//! `Sequencer`-side accessors for the arranger performance lane's armed state.
//!
//! The lane model itself is [`PerformanceLane`]; the trigger routing lives in
//! `event_handlers/performance_lane_handler.rs`. The armed flag is shared with
//! `MidiInputForwarder` (a different thread) so it can suppress MIDI thru for
//! trigger keys — see `110-performance-lane.md`.

use std::sync::atomic::Ordering;

use crate::models::performance_lane::PerformanceLane;

use super::Sequencer;

impl Sequencer {
    /// Mutable access to the lane's held-note bookkeeping.
    pub(crate) fn performance_lane_mut(&mut self) -> &mut PerformanceLane {
        &mut self.performance_lane
    }

    /// Whether the lane is armed (the keyboard jumps bars instead of playing
    /// notes).
    pub(crate) fn is_performance_lane_armed(&self) -> bool {
        self.performance_lane_armed.load(Ordering::Relaxed)
    }

    /// Arms / disarms the lane. Shared with `MidiInputForwarder`.
    pub(crate) fn set_performance_lane_armed(&mut self, armed: bool) {
        self.performance_lane_armed.store(armed, Ordering::Relaxed);
    }
}
