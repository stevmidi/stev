//! [`ClipLane`] — which lane the piano roll shows under its notes: the
//! velocity bars or one of the two wheels' recorded curves. View state only
//! (`RenderState::clip_lane`): never saved, never switched by anything but
//! the user. See `380-wheel-lane.md`.

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
}

impl ClipLane {
    /// Every lane, top to bottom as the gutter stacks their labels — the
    /// order [`next`](Self::next) cycles too.
    pub(super) const ALL: [ClipLane; 3] = [
        ClipLane::Velocity,
        ClipLane::PitchBend,
        ClipLane::Modulation,
    ];

    /// The lane after this one, back to velocity after the last
    /// (`Action::CycleClipLane`).
    pub(super) fn next(self) -> ClipLane {
        match self {
            ClipLane::Velocity => ClipLane::PitchBend,
            ClipLane::PitchBend => ClipLane::Modulation,
            ClipLane::Modulation => ClipLane::Velocity,
        }
    }

    /// The lane's gutter label.
    pub(super) fn label(self) -> &'static str {
        match self {
            ClipLane::Velocity => "VEL",
            ClipLane::PitchBend => "PB",
            ClipLane::Modulation => "MW",
        }
    }

    /// The wheel the lane draws, `None` for velocity.
    pub(super) fn wheel(self) -> Option<Controller> {
        match self {
            ClipLane::Velocity => None,
            ClipLane::PitchBend => Some(Controller::PitchBend),
            ClipLane::Modulation => Some(Controller::MOD_WHEEL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClipLane;

    #[test]
    fn the_lanes_cycle_vel_pb_mw_and_back() {
        assert_eq!(ClipLane::default(), ClipLane::Velocity);
        assert_eq!(ClipLane::Velocity.next(), ClipLane::PitchBend);
        assert_eq!(ClipLane::PitchBend.next(), ClipLane::Modulation);
        assert_eq!(ClipLane::Modulation.next(), ClipLane::Velocity);
    }
}
