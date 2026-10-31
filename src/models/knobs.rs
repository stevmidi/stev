//! A track's eight knobs: the virtual controllers of the knob panel, each
//! sending one MIDI CC the track chooses, so a plugin's macros (Spire's on
//! CC 14–17) can be turned with the mouse. Each [`Knob`] holds its CC number
//! and its position, `None` until it is first turned — nothing is sent for a
//! knob that never moved, so a plugin keeps what its own state holds. Pure
//! data, unit-tested at the bottom of the file. See `390-knob-panel.md`.

/// How many knobs a track has: two columns of four.
pub(crate) const KNOB_COUNT: usize = 8;

/// The CC a new track's first knob sends; the rest follow on, so a fresh
/// track's knobs are CC 14–21 — undefined or general-purpose in the MIDI
/// spec, and right for Spire's macros with no setup (`390` decision 4).
const FIRST_DEFAULT_CC: u8 = 14;

/// The mod wheel, which has a lane of its own and is never a knob.
const MOD_WHEEL_CC: u8 = 1;

/// The first of the channel mode messages (120–127), which no knob may send.
const FIRST_CHANNEL_MODE_CC: u8 = 120;

/// The highest value a CC carries.
pub(crate) const CC_VALUE_MAX: u8 = 127;

/// One knob: the CC it sends and where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Knob {
    /// The controller number it sends, never one [`cc_refusal`] refuses.
    pub(crate) cc: u8,
    /// Its position, `0..=CC_VALUE_MAX`; `None` until first turned.
    pub(crate) value: Option<u8>,
}

/// A track's knobs, in reading order: the first column top to bottom, then
/// the second.
pub(crate) type Knobs = [Knob; KNOB_COUNT];

/// A fresh track's knobs: CC 14–21, none turned yet.
pub(crate) fn default_knobs() -> Knobs {
    std::array::from_fn(|i| Knob {
        cc: FIRST_DEFAULT_CC + i as u8,
        value: None,
    })
}

/// Why a knob can't take a CC number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CcRefusal {
    /// Not a number a knob may send: past 127, the mod wheel, or a channel
    /// mode message.
    Reserved,
    /// Another knob of the track sends it already — this one, by index.
    Taken(usize),
}

/// Whether `cc` may be sent by any knob: `0..=119` but the mod wheel.
pub(crate) fn is_knob_cc(cc: u8) -> bool {
    cc != MOD_WHEEL_CC && cc < FIRST_CHANNEL_MODE_CC
}

/// Why knob `knob` of `knobs` can't send `cc`, or `None` when it can (its
/// own number included). No two knobs of a track share a CC: both would
/// claim the same data (`390` decision 5).
pub(crate) fn cc_refusal(knobs: &Knobs, knob: usize, cc: u8) -> Option<CcRefusal> {
    if !is_knob_cc(cc) {
        return Some(CcRefusal::Reserved);
    }
    knobs
        .iter()
        .enumerate()
        .position(|(i, other)| i != knob && other.cc == cc)
        .map(CcRefusal::Taken)
}

/// The knobs a saved track names, or a fresh track's when it names none (a
/// project saved before knobs) or a set no track could have: the wrong
/// count, a reserved CC, two knobs on one CC. A position past
/// [`CC_VALUE_MAX`] is clamped.
pub(crate) fn knobs_from_saved(saved: &[Knob]) -> Knobs {
    let Ok(knobs) = <Knobs>::try_from(saved) else {
        return default_knobs();
    };
    if (0..KNOB_COUNT).any(|i| cc_refusal(&knobs, i, knobs[i].cc).is_some()) {
        return default_knobs();
    }
    knobs.map(|knob| Knob {
        value: knob.value.map(|value| value.min(CC_VALUE_MAX)),
        ..knob
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_track_sends_cc_14_to_21_with_no_knob_turned() {
        let knobs = default_knobs();
        let ccs: Vec<u8> = knobs.iter().map(|knob| knob.cc).collect();
        assert_eq!(ccs, (14..=21).collect::<Vec<u8>>());
        assert!(knobs.iter().all(|knob| knob.value.is_none()));
    }

    #[test]
    fn the_mod_wheel_and_the_channel_mode_messages_are_refused() {
        let knobs = default_knobs();
        for cc in [1, 120, 127, 128, 255] {
            assert_eq!(cc_refusal(&knobs, 0, cc), Some(CcRefusal::Reserved));
        }
        for cc in [0, 2, 74, 119] {
            assert_eq!(cc_refusal(&knobs, 0, cc), None);
        }
    }

    #[test]
    fn a_cc_another_knob_sends_is_refused_but_a_knobs_own_is_not() {
        let knobs = default_knobs();
        assert_eq!(cc_refusal(&knobs, 0, 15), Some(CcRefusal::Taken(1)));
        assert_eq!(cc_refusal(&knobs, 7, 14), Some(CcRefusal::Taken(0)));
        assert_eq!(cc_refusal(&knobs, 0, 14), None);
    }

    #[test]
    fn a_saved_set_loads_as_saved_with_positions_clamped() {
        let mut saved = default_knobs();
        saved[2] = Knob {
            cc: 74,
            value: Some(200),
        };
        saved[5].value = Some(64);
        let knobs = knobs_from_saved(&saved);
        assert_eq!(
            knobs[2],
            Knob {
                cc: 74,
                value: Some(CC_VALUE_MAX)
            }
        );
        assert_eq!(knobs[5].value, Some(64));
    }

    #[test]
    fn a_set_no_track_could_have_loads_as_a_fresh_tracks() {
        assert_eq!(knobs_from_saved(&[]), default_knobs());
        assert_eq!(knobs_from_saved(&default_knobs()[..7]), default_knobs());
        let mut duplicate = default_knobs();
        duplicate[3].cc = 14;
        assert_eq!(knobs_from_saved(&duplicate), default_knobs());
        let mut reserved = default_knobs();
        reserved[3].cc = 1;
        assert_eq!(knobs_from_saved(&reserved), default_knobs());
    }
}
