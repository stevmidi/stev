//! What a track's clip playback has done to the synth's two performance
//! wheels — pitch bend and the mod wheel (CC1) — so that no discontinuity
//! leaves one where the last message happened to put it.
//!
//! Notes have a release (a `NoteOff`); a wheel has none, so a take that ends
//! mid-bend would leave the synth detuned after a stop, a seek, a loop wrap or
//! a clip end. A [`WheelTracker`] per track remembers the last value its
//! playback sent each wheel and which values are due at the next tick: every
//! track seek [`chase`](WheelTracker::chase)s the values in force where
//! playback lands (`Clip::wheels_at`, neutral in a gap), sending only what
//! differs, and a stop [`take_resets`](WheelTracker::take_resets) back to
//! neutral. Pure data, unit-tested at the bottom of the file. See
//! `050-undo-redo.md` § Invariants.

use crate::core::midi::message::{Wheel, parse_wheel};
use crate::models::event::Event;

/// One value per wheel, indexed by [`Wheel::index`].
pub(crate) type WheelValues = [u16; 2];

/// Both wheels at rest: bend centred, mod wheel down.
pub(crate) const NEUTRAL_WHEELS: WheelValues =
    [Wheel::PitchBend.neutral(), Wheel::Modulation.neutral()];

/// Folds `event` into `values`: an unmuted wheel move sets its wheel; a
/// muted one never sounds, and anything else isn't a wheel.
pub(crate) fn apply_wheel_move(values: &mut WheelValues, event: &Event) {
    if let Some((wheel, value)) = parse_wheel(event.midi_message())
        && !event.is_muted()
    {
        values[wheel.index()] = value;
    }
}

/// A [`WheelTracker::sent`] value the synth may not have heard — never equal
/// to a real wheel value (14 bits at most), so the next chase resends.
const STALE: u16 = u16::MAX;

/// One track's wheel bookkeeping. See the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct WheelTracker {
    /// Per wheel: the last value the track's playback sent since the last
    /// [`take_resets`](Self::take_resets), [`STALE`] after an
    /// [`invalidate`](Self::invalidate), `None` when it sent none (the synth is at
    /// rest, or where the player put it).
    sent: [Option<u16>; 2],
    /// Per wheel: the value to send at the next tick, if any.
    due: [Option<u16>; 2],
}

impl WheelTracker {
    /// Records a clip event the track just played: a wheel message is that
    /// wheel's value now; anything else is ignored.
    pub(crate) fn observe(&mut self, msg: &[u8]) {
        if let Some((wheel, value)) = parse_wheel(msg) {
            self.sent[wheel.index()] = Some(value);
        }
    }

    /// Playback moved to where `target` is in force (a seek, a re-seek onto
    /// another clip or into a gap, the arrival at a clip's start): due is
    /// every wheel not already there. A wheel the track never sent counts as
    /// at rest, so a plain note track sends nothing, and a wheel the player
    /// moved is only taken over by a clip value.
    pub(crate) fn chase(&mut self, target: WheelValues) {
        for wheel in Wheel::ALL {
            let i = wheel.index();
            let at = self.sent[i].unwrap_or(wheel.neutral());
            self.due[i] = (at != target[i]).then_some(target[i]);
        }
    }

    /// The synth may not have heard the last value sent — the `"midiout"`
    /// thread drops its delay queue on every stop and jump
    /// (`160-midi-out-offset.md`), so a
    /// bend's return to centre just before a loop wrap can be lost. The next
    /// [`chase`](Self::chase) resends every wheel the track has sent.
    pub(crate) fn invalidate(&mut self) {
        for sent in self.sent.iter_mut().flatten() {
            *sent = STALE;
        }
    }

    /// The next due message (channel 0, for the output to rewrite), recorded
    /// as sent. `None` when nothing is due.
    pub(crate) fn next_due(&mut self) -> Option<Vec<u8>> {
        let wheel = Wheel::ALL
            .into_iter()
            .find(|wheel| self.due[wheel.index()].is_some())?;
        let value = self.due[wheel.index()].take()?;
        self.sent[wheel.index()] = Some(value);
        Some(wheel.message(value))
    }

    /// For a transport stop or a track leaving the arrangement: a message
    /// putting every wheel the track sent back to neutral (channel 0) — even
    /// one it last sent at neutral, which the stop may have dropped — and
    /// nothing left sent or due.
    pub(crate) fn take_resets(&mut self) -> Vec<Vec<u8>> {
        self.due = [None; 2];
        Wheel::ALL
            .into_iter()
            .filter(|wheel| self.sent[wheel.index()].take().is_some())
            .map(|wheel| wheel.message(wheel.neutral()))
            .collect()
    }

    /// The player moved `msg`'s wheel live on this track: the synth now sits
    /// where the player put it, not where a clip did, so a stop leaves that
    /// wheel alone (as it spares a live-held note) and a seek only moves it
    /// to a clip value. Anything but a wheel message is ignored.
    pub(crate) fn hand_to_player(&mut self, msg: &[u8]) {
        if let Some((wheel, _)) = parse_wheel(msg) {
            self.sent[wheel.index()] = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEND_UP: [u8; 3] = [0xE0, 0x00, 0x60];
    const BEND_CENTRE: [u8; 3] = [0xE0, 0x00, 0x40];
    const MOD_HALF: [u8; 3] = [0xB0, 0x01, 64];

    fn drain(tracker: &mut WheelTracker) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| tracker.next_due()).collect()
    }

    #[test]
    fn an_untouched_track_chased_to_rest_sends_nothing() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&[0x90, 60, 100]);
        tracker.chase(NEUTRAL_WHEELS);
        assert!(drain(&mut tracker).is_empty());
        assert!(tracker.take_resets().is_empty());
    }

    #[test]
    fn a_chase_sends_only_the_wheels_not_already_there() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&MOD_HALF);
        // Land where the clip holds a bend and the mod wheel is at rest.
        tracker.chase([0x3000, 0]);
        assert_eq!(
            drain(&mut tracker),
            vec![BEND_UP.to_vec(), vec![0xB0, 0x01, 0]]
        );

        // Already there: nothing, however often playback re-seeks.
        tracker.chase([0x3000, 0]);
        assert!(drain(&mut tracker).is_empty());
    }

    #[test]
    fn a_later_chase_replaces_what_an_earlier_one_left_due() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&BEND_UP);
        tracker.chase(NEUTRAL_WHEELS);
        tracker.chase([0x3000, 0]);
        assert!(drain(&mut tracker).is_empty());
        tracker.chase(NEUTRAL_WHEELS);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
    }

    #[test]
    fn after_an_invalidate_a_chase_resends_what_the_track_sent() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&BEND_UP);
        tracker.observe(&BEND_CENTRE);
        tracker.invalidate();
        tracker.chase(NEUTRAL_WHEELS);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
        // A wheel it never sent stays untouched.
        tracker.invalidate();
        tracker.chase(NEUTRAL_WHEELS);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
    }

    #[test]
    fn a_stop_resets_every_wheel_sent_once_and_drops_what_was_due() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&BEND_UP);
        tracker.observe(&BEND_CENTRE);
        tracker.chase([0x2000, 90]);
        assert_eq!(tracker.take_resets(), vec![BEND_CENTRE.to_vec()]);
        assert!(tracker.next_due().is_none());
        assert!(tracker.take_resets().is_empty());
    }

    #[test]
    fn a_sent_chase_counts_as_sent() {
        let mut tracker = WheelTracker::default();
        tracker.chase([0x2000, 90]);
        drain(&mut tracker);
        assert_eq!(tracker.take_resets(), vec![vec![0xB0, 0x01, 0]]);
    }

    #[test]
    fn a_wheel_the_player_moved_last_is_left_alone() {
        let mut tracker = WheelTracker::default();
        tracker.observe(&BEND_UP);
        tracker.observe(&MOD_HALF);
        tracker.hand_to_player(&[0xB3, 0x01, 20]);
        // Stop resets the bend the clip left, not the player's mod wheel.
        assert_eq!(tracker.take_resets(), vec![BEND_CENTRE.to_vec()]);

        tracker.observe(&MOD_HALF);
        tracker.hand_to_player(&[0xB0, 0x01, 20]);
        // A seek to rest doesn't pull it back either; a clip value does.
        tracker.chase(NEUTRAL_WHEELS);
        assert!(drain(&mut tracker).is_empty());
        tracker.chase([0x2000, 64]);
        assert_eq!(drain(&mut tracker), vec![MOD_HALF.to_vec()]);
    }
}
