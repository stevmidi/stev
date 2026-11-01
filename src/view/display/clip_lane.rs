//! [`ClipLane`] — which lane the piano roll shows under its notes: the
//! velocity bars, one of the two wheels' recorded curves, or one of the
//! knobs' lanes. View state only (`RenderState::clip_lane`): never saved,
//! never switched by anything but the user. See `380-wheel-lane.md` and
//! `400-parameter-knobs.md` decision 12.

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
    /// Knob `n`'s lane (`0..KNOB_COUNT`, the knobs' reading order): by
    /// position, so the lane stays up across a change of track and shows
    /// that track's knob there. Empty until a clip can hold a parameter's
    /// moves (`400-parameter-knobs.md` phase 2).
    Knob(usize),
}

impl ClipLane {
    /// The lanes the gutter switches and their labels, top to bottom as it
    /// stacks them. The knob lanes switch from the knobs' own labels.
    pub(super) const GUTTER: [(ClipLane, &'static str); 3] = [
        (ClipLane::Velocity, "VEL"),
        (ClipLane::PitchBend, "PB"),
        (ClipLane::Modulation, "MW"),
    ];

    /// The lane after this one (`Action::CycleClipLane`): VEL → PB → MW →
    /// back to VEL, the gutter's three. The knobs' lanes are picked by a
    /// click on the knob only (`400-parameter-knobs.md` decision 14): from
    /// one, the cycle starts again at VEL.
    pub(super) fn next(self) -> ClipLane {
        match self {
            ClipLane::Velocity => ClipLane::PitchBend,
            ClipLane::PitchBend => ClipLane::Modulation,
            ClipLane::Modulation | ClipLane::Knob(_) => ClipLane::Velocity,
        }
    }

    /// The controller the lane draws: `None` for velocity, and for a knob's
    /// lane, which has no data to draw yet.
    pub(super) fn controller(self) -> Option<Controller> {
        match self {
            ClipLane::Velocity | ClipLane::Knob(_) => None,
            ClipLane::PitchBend => Some(Controller::PitchBend),
            ClipLane::Modulation => Some(Controller::MOD_WHEEL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClipLane;
    use crate::core::midi::message::Controller;
    use crate::models::knobs::KNOB_COUNT;

    /// The cycle is the gutter's three lanes, VEL, PB, MW, back to VEL; the
    /// knobs' lanes are never in it, and from one the cycle goes to VEL.
    #[test]
    fn the_lanes_cycle_vel_pb_mw_and_skip_the_knobs() {
        assert_eq!(ClipLane::default(), ClipLane::Velocity);
        assert_eq!(ClipLane::Velocity.next(), ClipLane::PitchBend);
        assert_eq!(ClipLane::PitchBend.next(), ClipLane::Modulation);
        assert_eq!(ClipLane::Modulation.next(), ClipLane::Velocity);
        for knob in 0..KNOB_COUNT {
            assert_eq!(ClipLane::Knob(knob).next(), ClipLane::Velocity);
        }
    }

    /// The wheels' lanes draw their controllers; velocity and a knob's lane
    /// none.
    #[test]
    fn only_the_wheel_lanes_draw_a_controller() {
        assert_eq!(
            ClipLane::PitchBend.controller(),
            Some(Controller::PitchBend)
        );
        assert_eq!(
            ClipLane::Modulation.controller(),
            Some(Controller::MOD_WHEEL)
        );
        assert_eq!(ClipLane::Velocity.controller(), None);
        assert_eq!(ClipLane::Knob(0).controller(), None);
    }
}
