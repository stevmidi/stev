//! `Sequencer`-side accessors for the arranger performance lane: its armed
//! state and its run.
//!
//! The lane model itself is [`PerformanceLane`]; the trigger routing and the
//! capture live in `event_handlers/performance_lane_handler.rs`. The armed
//! flag is shared with `MidiInputForwarder` (a different thread) so it can
//! suppress MIDI thru for trigger keys, and with `Transport`, which doesn't
//! wrap at the loop end while it is set — see `110-performance-lane.md` and
//! `520-performance-capture.md`.

use std::sync::atomic::Ordering;

use crate::models::performance_lane::{PerformanceLane, key_index_from_note, key_tick};

use super::Sequencer;

impl Sequencer {
    /// Mutable access to the lane's run.
    pub(crate) fn performance_lane_mut(&mut self) -> &mut PerformanceLane {
        &mut self.performance_lane
    }

    /// Whether the lane is armed (the keyboard jumps the transport instead of
    /// playing notes).
    pub(crate) fn is_performance_lane_armed(&self) -> bool {
        self.performance_lane_armed.load(Ordering::Relaxed)
    }

    /// Arms / disarms the lane. Shared with `MidiInputForwarder` and
    /// `Transport`. Disarming ends the run: the presses before it can't be
    /// kept once the lane is left.
    pub(crate) fn set_performance_lane_armed(&mut self, armed: bool) {
        self.performance_lane_armed.store(armed, Ordering::Relaxed);
        if !armed {
            self.performance_lane.clear();
        }
    }

    /// Where a run's press jumped to: its lane key's start at the lane's key
    /// length (`key_tick`), `None` below the keys. What the render plays by
    /// (`Sequencer::render_performance`).
    pub(super) fn performance_key_target(&self) -> impl Fn(u8) -> Option<i32> + use<> {
        let (meter, length) = (self.meter(), self.performance_lane_key_length());
        move |note| key_index_from_note(note).map(|key| key_tick(meter, length, key))
    }
}
