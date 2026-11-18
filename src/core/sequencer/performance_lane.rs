//! `Sequencer`-side accessors for the arranger performance lane: its armed
//! state, its run and its pads.
//!
//! The lane model itself is [`PerformanceLane`] and [`PerformancePads`]; the
//! trigger routing lives in `event_handlers/performance_lane_handler.rs`. The
//! armed flag is shared with `MidiInputForwarder` (a different thread) so it
//! can suppress MIDI thru for trigger keys, and with `Transport`, which
//! doesn't wrap at the loop end while it is set — see
//! `110-performance-lane.md` and `520-performance-capture.md`.

use std::sync::atomic::Ordering;

use crate::models::{
    clip::Clip,
    performance_lane::{PAD_COUNT, PerformanceLane, PerformancePads},
};

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

    /// The lane's pads.
    pub(crate) fn performance_pads(&self) -> &PerformancePads {
        &self.performance_pads
    }

    /// Puts `clip` on pad `pad` (empties it for `None`):
    /// `SetPerformancePadEdit`'s one write.
    pub(crate) fn set_performance_pad(&mut self, pad: usize, clip: Option<Clip>) {
        self.performance_pads.set(pad, clip);
    }

    /// The selected pad (`520-performance-capture.md` decision 6).
    pub(crate) fn selected_pad(&self) -> usize {
        self.selected_pad
    }

    /// Selects pad `pad`; nothing past the last.
    pub(crate) fn select_pad(&mut self, pad: usize) {
        if pad < PAD_COUNT {
            self.selected_pad = pad;
        }
    }
}
