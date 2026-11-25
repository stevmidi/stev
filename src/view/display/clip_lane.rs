//! [`ClipLane`] — which lane the piano roll shows under its notes: the
//! velocity bars, one of the resting controllers' recorded curves (the two
//! wheels and the sustain pedal), or one of the knobs' lanes. View state
//! only (`RenderState::clip_lane`): never saved, never switched by anything
//! but the user. See `archive/380-wheel-lane.md`, `archive/400-parameter-knobs.md` decision
//! 12 and `archive/530-sustain-capture.md` decision 7.

use crate::core::midi::message::Controller;

/// The piano roll's lower lane.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum ClipLane {
    /// The notes' velocity bars — every launch starts here.
    #[default]
    Velocity,
    /// The lead clip's pitch bend, centred.
    PitchBend,
    /// The lead clip's mod wheel (CC1).
    Modulation,
    /// The lead clip's sustain pedal (CC64), drawn as the mod wheel is.
    Sustain,
    /// Knob `n`'s lane (`0..KNOB_COUNT`, the knobs' reading order): by
    /// position, so the lane stays up across a change of track and shows
    /// that track's knob there (`Display::clip_lane_controller`).
    Knob(usize),
}

impl ClipLane {
    /// The lanes the gutter switches and their labels, top to bottom as it
    /// stacks them. The knob lanes switch from the knobs' own labels.
    pub(super) const GUTTER: [(ClipLane, &'static str); 4] = [
        (ClipLane::Velocity, "VEL"),
        (ClipLane::PitchBend, "PB"),
        (ClipLane::Modulation, "MW"),
        (ClipLane::Sustain, "SUS"),
    ];

    /// The lane after this one (`Action::CycleClipLane`): the next of
    /// [`GUTTER`](Self::GUTTER), top to bottom, the last back to the first.
    /// The knobs' lanes are picked by a click on the knob only
    /// (`archive/400-parameter-knobs.md` decision 14): from one, the cycle starts
    /// again at VEL.
    pub(super) fn next(self) -> ClipLane {
        Self::GUTTER
            .iter()
            .position(|&(lane, _)| lane == self)
            .map_or(ClipLane::Velocity, |i| {
                Self::GUTTER[(i + 1) % Self::GUTTER.len()].0
            })
    }

    /// The controller the lane draws — a wheel or the pedal: `None` for
    /// velocity, and for a knob's lane, whose parameter is the selected
    /// track's to say.
    pub(super) fn controller(self) -> Option<Controller> {
        match self {
            ClipLane::Velocity | ClipLane::Knob(_) => None,
            ClipLane::PitchBend => Some(Controller::PitchBend),
            ClipLane::Modulation => Some(Controller::MOD_WHEEL),
            ClipLane::Sustain => Some(Controller::SUSTAIN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClipLane;
    use crate::core::midi::message::Controller;
    use crate::models::knobs::KNOB_COUNT;

    /// The cycle is the gutter's four lanes, VEL, PB, MW, SUS, back to VEL;
    /// the knobs' lanes are never in it, and from one the cycle goes to VEL.
    #[test]
    fn the_lanes_cycle_vel_pb_mw_sus_and_skip_the_knobs() {
        assert_eq!(ClipLane::default(), ClipLane::Velocity);
        assert_eq!(ClipLane::Velocity.next(), ClipLane::PitchBend);
        assert_eq!(ClipLane::PitchBend.next(), ClipLane::Modulation);
        assert_eq!(ClipLane::Modulation.next(), ClipLane::Sustain);
        assert_eq!(ClipLane::Sustain.next(), ClipLane::Velocity);
        for knob in 0..KNOB_COUNT {
            assert_eq!(ClipLane::Knob(knob).next(), ClipLane::Velocity);
        }
    }

    /// The wheels' and the pedal's lanes draw their controllers; velocity
    /// and a knob's lane none.
    #[test]
    fn only_the_resting_lanes_draw_a_controller() {
        assert_eq!(
            ClipLane::PitchBend.controller(),
            Some(Controller::PitchBend)
        );
        assert_eq!(
            ClipLane::Modulation.controller(),
            Some(Controller::MOD_WHEEL)
        );
        assert_eq!(ClipLane::Sustain.controller(), Some(Controller::SUSTAIN));
        assert_eq!(ClipLane::Velocity.controller(), None);
        assert_eq!(ClipLane::Knob(0).controller(), None);
    }
}
