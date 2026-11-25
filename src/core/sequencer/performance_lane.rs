//! `Sequencer`-side accessors for the arranger performance lane: its armed
//! state, its run and the player's hand on it ([`Hand`],
//! `570-lane-octave-buttons.md`).
//!
//! The lane model itself is [`PerformanceLane`]; the trigger routing and the
//! capture live in `event_handlers/performance_lane_handler.rs`. The armed
//! flag is shared with `MidiInputForwarder` (a different thread) so it can
//! suppress MIDI thru for trigger keys, and with `Transport`, which doesn't
//! wrap at the loop end while it is set — see `110-performance-lane.md` and
//! `archive/520-performance-capture.md`.

use std::sync::atomic::Ordering;

use crate::models::performance_lane::{Hand, KeybedSize, PerformanceLane};

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
    /// kept once the lane is left. Arming places the hand around the cursor,
    /// so the buttons' moves from before are gone (`570` § 4).
    pub(crate) fn set_performance_lane_armed(&mut self, armed: bool) {
        self.performance_lane_armed.store(armed, Ordering::Relaxed);
        if armed {
            self.place_performance_lane_hand();
        } else {
            self.performance_lane.clear();
        }
    }

    /// The player's hand on the lane, as published to the view.
    pub(crate) fn performance_lane_hand(&self) -> Hand {
        Hand::from_bits(self.performance_lane_hand.load(Ordering::Relaxed))
    }

    /// Places the hand around the cursor, undoing the buttons' moves
    /// (`570-lane-octave-buttons.md` § 2): called at every cursor move and
    /// every change that moves the cursor's key (the meter, the key length),
    /// and when the lane is selected.
    pub(crate) fn place_performance_lane_hand(&self) {
        self.place_hand_of(self.performance_lane_hand().size());
    }

    /// The keyboard has `size` keys (settings): the hand is placed afresh
    /// for it.
    pub(crate) fn set_keybed_size(&self, size: KeybedSize) {
        self.place_hand_of(size);
    }

    /// Places a `size` keyboard's hand around the cursor.
    fn place_hand_of(&self, size: KeybedSize) {
        let hand = Hand::around(
            size,
            self.meter(),
            self.performance_lane_key_length(),
            self.cursor_tick(),
        );
        self.performance_lane_hand
            .store(hand.to_bits(), Ordering::Relaxed);
    }

    /// The keyboard's previous/next button: the hand `octaves` octaves along
    /// from where it is, until the next cursor move (`570` § 3). Returns
    /// whether it moved: an octave down that would put the lowest key before
    /// bar 1 does nothing.
    pub(crate) fn move_performance_lane_hand(&self, octaves: i32) -> bool {
        let Some(hand) = self.performance_lane_hand().moved(octaves) else {
            return false;
        };
        self.performance_lane_hand
            .store(hand.to_bits(), Ordering::Relaxed);
        true
    }
}
