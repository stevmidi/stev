//! What a track's clip playback has done to the synth's performance
//! controllers — pitch bend, the mod wheel (CC1) and the parameters its knobs
//! drive — so that no discontinuity leaves one where the last message happened to put it.
//!
//! Notes have a release (a `NoteOff`); a controller has none, so a take that
//! ends mid-bend would leave the synth detuned after a stop, a seek, a loop
//! wrap or a clip end. A [`ControllerTracker`] per track remembers the last
//! value its playback sent each controller and which values are due at the
//! next tick: every track seek [`chase`](ControllerTracker::chase)s the values
//! in force where playback lands (`Clip::controllers_at`), sending only what
//! differs. The two wheels have a rest: in a gap they chase back to it, and a
//! stop [`take_resets`](ControllerTracker::take_resets) them there. A
//! parameter has none (`400-parameter-knobs.md` decision 9): it is chased
//! where a clip has data for it and otherwise left where it was, a stop
//! included. Values are `f64`: a MIDI controller's in its own units, a
//! parameter's normalized. Pure data, unit-tested at the bottom of the file.
//! See `050-undo-redo.md` § Invariants.

use crate::core::midi::message::{Controller, parse_controller};
use crate::models::event::Event;

/// The value in force per controller, in [`Controller`] order: a wheel's
/// rest where nothing moved it, nothing for anything else nothing moved. A
/// list, since a parameter's ID is an arbitrary number and a clip moves few
/// controllers.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ControllerValues(Vec<(Controller, f64)>);

impl ControllerValues {
    /// `controller`'s value in force: its last move, else its rest — `None`
    /// for one without a rest that nothing moved.
    pub(crate) fn get(&self, controller: Controller) -> Option<f64> {
        self.0
            .binary_search_by_key(&controller, |&(moved, _)| moved)
            .ok()
            .map(|at| self.0[at].1)
            .or(controller.rest_value())
    }

    /// Puts `controller` at `value`.
    pub(crate) fn set(&mut self, controller: Controller, value: f64) {
        match self
            .0
            .binary_search_by_key(&controller, |&(moved, _)| moved)
        {
            Ok(at) => self.0[at].1 = value,
            Err(at) => self.0.insert(at, (controller, value)),
        }
    }

    /// Forgets `controller`, returning the value it was put at: nothing for
    /// one nothing moved, whatever its rest.
    fn remove(&mut self, controller: Controller) -> Option<f64> {
        let at = self
            .0
            .binary_search_by_key(&controller, |&(moved, _)| moved)
            .ok()?;
        Some(self.0.remove(at).1)
    }

    /// Folds `event` into the values: an unmuted controller move sets its
    /// controller; a muted one never sounds, and anything else isn't a move.
    pub(crate) fn apply(&mut self, event: &Event) {
        if let Some((controller, value)) = event.controller_move()
            && !event.is_muted()
        {
            self.set(controller, value);
        }
    }

    /// Every controller something moved, with the value it moved to.
    pub(crate) fn moved(&self) -> impl Iterator<Item = (Controller, f64)> + '_ {
        self.0.iter().copied()
    }
}

/// A [`ControllerTracker::sent`] value the synth may not have heard — never
/// equal to any value, itself included, so the next chase resends.
const STALE: f64 = f64::NAN;

/// One track's controller bookkeeping. See the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct ControllerTracker {
    /// Per controller: the last value the track's playback sent — since the
    /// last [`take_resets`](Self::take_resets), for a wheel — [`STALE`]
    /// after an [`invalidate`](Self::invalidate). Nothing for one it sent
    /// none (the synth is at rest, or where the player put it).
    sent: ControllerValues,
    /// The values to send at the next tick, in the order they go.
    due: Vec<(Controller, f64)>,
}

impl ControllerTracker {
    /// Records a clip move the track just played: `controller` is at
    /// `value` now.
    pub(crate) fn observe(&mut self, controller: Controller, value: f64) {
        self.sent.set(controller, value);
    }

    /// Playback moved to where `target` is in force (a seek, a re-seek onto
    /// another clip or into a gap, the arrival at a clip's start): due is
    /// every controller of `followed` with a value there not already at it,
    /// and nothing else. A wheel the track never sent counts as at rest, so a
    /// plain note track sends nothing, and a wheel the player moved is only
    /// taken over by a clip value. A controller with no value in `target` is
    /// left where it is. Only `followed` is visited — a seek runs every tick
    /// once playback is past a track's last clip.
    pub(crate) fn chase(
        &mut self,
        target: &ControllerValues,
        followed: impl IntoIterator<Item = Controller>,
    ) {
        self.due.clear();
        for controller in followed {
            let known = self.sent.get(controller);
            if let Some(value) = target.get(controller).filter(|&value| known != Some(value)) {
                self.due.push((controller, value));
            }
        }
    }

    /// The synth may not have heard the last value sent — the `"midiout"`
    /// thread drops its delay queue on every stop and jump
    /// (`160-midi-out-offset.md`), so a
    /// bend's return to centre just before a loop wrap can be lost. The next
    /// [`chase`](Self::chase) resends every controller the track has sent
    /// that has a value where playback lands.
    pub(crate) fn invalidate(&mut self) {
        for (_, sent) in &mut self.sent.0 {
            *sent = STALE;
        }
    }

    /// The next due controller and its value, taken off the due list —
    /// recorded as sent once the caller [`observe`](Self::observe)s it, which
    /// it skips for a value it drops (a knob the hand holds). `None` when
    /// nothing is due.
    pub(crate) fn next_due(&mut self) -> Option<(Controller, f64)> {
        (!self.due.is_empty()).then(|| self.due.remove(0))
    }

    /// For a transport stop or a track leaving the arrangement: a message
    /// putting every wheel the track sent back to its rest (channel 0) —
    /// even one it last sent at rest, which the stop may have dropped — and
    /// nothing left due. A controller with no rest stays where it is.
    pub(crate) fn take_resets(&mut self) -> Vec<Vec<u8>> {
        self.due.clear();
        Controller::WHEELS
            .into_iter()
            .filter_map(|wheel| {
                self.sent.remove(wheel)?;
                Some(wheel.message(wheel.rest()?))
            })
            .collect()
    }

    /// The player moved `msg`'s controller live on this track
    /// ([`hand_over`](Self::hand_over)). Anything but a controller message is
    /// ignored.
    pub(crate) fn hand_to_player(&mut self, msg: &[u8]) {
        if let Some((controller, _)) = parse_controller(msg) {
            self.hand_over(controller);
        }
    }

    /// The player moved `controller` on this track: the synth now sits where
    /// the player put it, not where a clip did, so a stop leaves that wheel
    /// alone (as it spares a live-held note) and a seek only moves it to a
    /// clip value.
    pub(crate) fn hand_over(&mut self, controller: Controller) {
        self.sent.remove(controller);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::knobs::test_param;

    const BEND_UP: [u8; 3] = [0xE0, 0x00, 0x60];
    const BEND_CENTRE: [u8; 3] = [0xE0, 0x00, 0x40];
    const MOD_HALF: [u8; 3] = [0xB0, 0x01, 64];

    /// A parameter a knob holds.
    const CUTOFF: Controller = Controller::Param(test_param(714));

    /// The wheels and one knob's parameter, as a track with a knob on it
    /// follows them.
    const FOLLOWED: [Controller; 3] = [Controller::PitchBend, Controller::MOD_WHEEL, CUTOFF];

    /// Records a played message, as the track does.
    fn observe(tracker: &mut ControllerTracker, msg: &[u8]) {
        if let Some((controller, value)) = parse_controller(msg) {
            tracker.observe(controller, f64::from(value));
        }
    }

    /// Sends everything due, as the track does: each recorded as sent.
    fn drain(tracker: &mut ControllerTracker) -> Vec<Vec<u8>> {
        let mut sent = Vec::new();
        while let Some((controller, value)) = tracker.next_due() {
            tracker.observe(controller, value);
            sent.push(controller.message(value as u16));
        }
        sent
    }

    /// Values with the bend at `bend` and the mod wheel at `modulation`.
    fn wheels(bend: u16, modulation: u16) -> ControllerValues {
        let mut values = ControllerValues::default();
        values.set(Controller::PitchBend, f64::from(bend));
        values.set(Controller::MOD_WHEEL, f64::from(modulation));
        values
    }

    #[test]
    fn an_untouched_track_chased_to_rest_sends_nothing() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &[0x90, 60, 100]);
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert!(drain(&mut tracker).is_empty());
        assert!(tracker.take_resets().is_empty());
    }

    #[test]
    fn a_chase_sends_only_the_controllers_not_already_there() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &MOD_HALF);
        // Land where the clip holds a bend and the mod wheel is at rest.
        tracker.chase(&wheels(0x3000, 0), FOLLOWED);
        assert_eq!(
            drain(&mut tracker),
            vec![BEND_UP.to_vec(), vec![0xB0, 0x01, 0]]
        );

        // Already there: nothing, however often playback re-seeks.
        tracker.chase(&wheels(0x3000, 0), FOLLOWED);
        assert!(drain(&mut tracker).is_empty());
    }

    #[test]
    fn a_later_chase_replaces_what_an_earlier_one_left_due() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &BEND_UP);
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        tracker.chase(&wheels(0x3000, 0), FOLLOWED);
        assert!(drain(&mut tracker).is_empty());
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
    }

    #[test]
    fn after_an_invalidate_a_chase_resends_what_the_track_sent() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &BEND_UP);
        observe(&mut tracker, &BEND_CENTRE);
        tracker.invalidate();
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
        // A wheel it never sent stays untouched.
        tracker.invalidate();
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert_eq!(drain(&mut tracker), vec![BEND_CENTRE.to_vec()]);
    }

    #[test]
    fn a_stop_resets_every_wheel_sent_once_and_drops_what_was_due() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &BEND_UP);
        observe(&mut tracker, &BEND_CENTRE);
        tracker.chase(&wheels(0x2000, 90), FOLLOWED);
        assert_eq!(tracker.take_resets(), vec![BEND_CENTRE.to_vec()]);
        assert!(tracker.next_due().is_none());
        assert!(tracker.take_resets().is_empty());
    }

    #[test]
    fn a_sent_chase_counts_as_sent() {
        let mut tracker = ControllerTracker::default();
        tracker.chase(&wheels(0x2000, 90), FOLLOWED);
        drain(&mut tracker);
        assert_eq!(tracker.take_resets(), vec![vec![0xB0, 0x01, 0]]);
    }

    #[test]
    fn a_wheel_the_player_moved_last_is_left_alone() {
        let mut tracker = ControllerTracker::default();
        observe(&mut tracker, &BEND_UP);
        observe(&mut tracker, &MOD_HALF);
        tracker.hand_to_player(&[0xB3, 0x01, 20]);
        // Stop resets the bend the clip left, not the player's mod wheel.
        assert_eq!(tracker.take_resets(), vec![BEND_CENTRE.to_vec()]);

        observe(&mut tracker, &MOD_HALF);
        tracker.hand_to_player(&[0xB0, 0x01, 20]);
        // A seek to rest doesn't pull it back either; a clip value does.
        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert!(drain(&mut tracker).is_empty());
        tracker.chase(&wheels(0x2000, 64), FOLLOWED);
        assert_eq!(drain(&mut tracker), vec![MOD_HALF.to_vec()]);
    }

    /// The next due move, as `(controller, value)`, recorded as sent.
    fn next_sent(tracker: &mut ControllerTracker) -> Option<(Controller, f64)> {
        let (controller, value) = tracker.next_due()?;
        tracker.observe(controller, value);
        Some((controller, value))
    }

    /// A parameter has no rest: a stop leaves it, a chase with no data for
    /// it leaves it, and one with data moves it
    /// (`400-parameter-knobs.md` decision 9).
    #[test]
    fn a_parameter_is_never_reset_and_only_chased_to_data() {
        let mut tracker = ControllerTracker::default();
        tracker.observe(CUTOFF, 0.5);
        observe(&mut tracker, &BEND_UP);
        assert_eq!(tracker.take_resets(), vec![BEND_CENTRE.to_vec()]);

        tracker.chase(&ControllerValues::default(), FOLLOWED);
        assert!(tracker.next_due().is_none());

        let mut target = ControllerValues::default();
        target.set(CUTOFF, 0.75);
        tracker.chase(&target, FOLLOWED);
        assert_eq!(next_sent(&mut tracker), Some((CUTOFF, 0.75)));
        tracker.chase(&target, FOLLOWED);
        assert!(tracker.next_due().is_none());

        // A parameter the track never sent goes out when a clip has data.
        let mut fresh = ControllerTracker::default();
        fresh.chase(&target, FOLLOWED);
        assert_eq!(next_sent(&mut fresh), Some((CUTOFF, 0.75)));
    }

    /// Once the hand has had a parameter, the track no longer knows where
    /// it stands: the next chase sends the clip's value, even the one it
    /// sent last.
    #[test]
    fn a_parameter_handed_over_is_chased_back_to_the_clip() {
        let mut tracker = ControllerTracker::default();
        tracker.observe(CUTOFF, 0.75);
        tracker.hand_over(CUTOFF);
        let mut target = ControllerValues::default();
        target.set(CUTOFF, 0.75);
        tracker.chase(&target, FOLLOWED);
        assert_eq!(next_sent(&mut tracker), Some((CUTOFF, 0.75)));
    }

    #[test]
    fn after_an_invalidate_a_chase_resends_a_parameter() {
        let mut tracker = ControllerTracker::default();
        tracker.observe(CUTOFF, 0.75);
        tracker.invalidate();
        let mut target = ControllerValues::default();
        target.set(CUTOFF, 0.75);
        tracker.chase(&target, FOLLOWED);
        assert_eq!(next_sent(&mut tracker), Some((CUTOFF, 0.75)));
    }

    #[test]
    fn values_keep_a_wheels_rest_and_a_parameters_last_move() {
        let mut values = ControllerValues::default();
        assert_eq!(values.get(Controller::PitchBend), Some(8192.0));
        assert_eq!(values.get(CUTOFF), None);
        values.apply(&Event::controller_move_at(0, CUTOFF, 0.25, 0));
        values.apply(&Event::new(0, 0, MOD_HALF.to_vec()));
        assert_eq!(values.get(CUTOFF), Some(0.25));
        assert_eq!(
            values.moved().collect::<Vec<_>>(),
            vec![(Controller::MOD_WHEEL, 64.0), (CUTOFF, 0.25)]
        );
    }

    #[test]
    fn a_muted_parameter_move_moves_nothing() {
        let mut muted = Event::controller_move_at(0, CUTOFF, 0.25, 0);
        muted.set_muted(true);
        let mut values = ControllerValues::default();
        values.apply(&muted);
        assert_eq!(values.get(CUTOFF), None);
    }

    /// A controller the track doesn't follow (a sustain pedal in an
    /// imported clip, a parameter no knob holds) is never chased.
    #[test]
    fn a_chase_visits_only_the_followed_controllers() {
        let mut tracker = ControllerTracker::default();
        let mut target = ControllerValues::default();
        target.set(CUTOFF, 0.5);
        target.set(Controller::Cc(64), 127.0);
        target.set(Controller::Param(test_param(1)), 0.5);
        tracker.chase(&target, FOLLOWED);
        assert_eq!(next_sent(&mut tracker), Some((CUTOFF, 0.5)));
        assert!(tracker.next_due().is_none());
    }
}
