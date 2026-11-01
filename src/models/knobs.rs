//! A track's eight knobs: the knob panel's virtual controllers, each
//! driving one parameter of the track's plugin, by the parameter's ID
//! (`400-parameter-knobs.md`). A knob is assigned by turning the control in
//! the plugin's own editor; until then it drives nothing. Only the
//! assignment is the track's: where a parameter stands is the plugin's, in
//! its own state. Pure data, unit-tested at the bottom of the file.

/// How many knobs a track has: two columns of four.
pub(crate) const KNOB_COUNT: usize = 8;

/// A plugin parameter's ID: VST3's `ParamID`, CLAP's `clap_id` — an
/// arbitrary number the plugin gives each parameter, never a position in
/// its parameter list.
pub(crate) type ParamId = u32;

/// What each of a track's knobs drives, in reading order (the first column
/// top to bottom, then the second): a parameter, or `None` while the knob is
/// unassigned. No two knobs hold the same parameter ([`param_taken_by`]).
pub(crate) type KnobParams = [Option<ParamId>; KNOB_COUNT];

/// A fresh track's knobs: none assigned.
pub(crate) const UNASSIGNED_KNOBS: KnobParams = [None; KNOB_COUNT];

/// The other knob of `knobs` that holds `param` already, if any — knob
/// `knob` can't take it then: both would claim the same data
/// (`400-parameter-knobs.md` decision 4). A knob's own parameter is no
/// refusal.
pub(crate) fn param_taken_by(knobs: &KnobParams, knob: usize, param: ParamId) -> Option<usize> {
    knobs
        .iter()
        .enumerate()
        .position(|(i, other)| i != knob && *other == Some(param))
}

/// The knobs a saved track names, or none assigned when it names a set no
/// track could have: the wrong count, or two knobs on one parameter. A
/// project saved before parameter knobs (its CC knobs) reads as eight
/// unassigned knobs, its entries naming no parameter.
pub(crate) fn knobs_from_saved(saved: &[Option<ParamId>]) -> KnobParams {
    let Ok(knobs) = KnobParams::try_from(saved) else {
        return UNASSIGNED_KNOBS;
    };
    let duplicate = knobs
        .iter()
        .enumerate()
        .any(|(i, param)| param.is_some_and(|param| param_taken_by(&knobs, i, param).is_some()));
    if duplicate { UNASSIGNED_KNOBS } else { knobs }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_another_knob_holds_is_taken_but_a_knobs_own_is_not() {
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[0] = Some(714);
        knobs[5] = Some(7_000_000);
        assert_eq!(param_taken_by(&knobs, 3, 714), Some(0));
        assert_eq!(param_taken_by(&knobs, 0, 7_000_000), Some(5));
        assert_eq!(param_taken_by(&knobs, 0, 714), None);
        assert_eq!(param_taken_by(&knobs, 3, 1), None);
    }

    #[test]
    fn a_saved_set_loads_as_saved() {
        let mut saved = UNASSIGNED_KNOBS;
        saved[2] = Some(714);
        saved[7] = Some(0);
        assert_eq!(knobs_from_saved(&saved), saved);
    }

    #[test]
    fn a_set_no_track_could_have_loads_unassigned() {
        assert_eq!(knobs_from_saved(&[]), UNASSIGNED_KNOBS);
        assert_eq!(knobs_from_saved(&[Some(1); 7]), UNASSIGNED_KNOBS);
        let mut duplicate = UNASSIGNED_KNOBS;
        duplicate[1] = Some(714);
        duplicate[6] = Some(714);
        assert_eq!(knobs_from_saved(&duplicate), UNASSIGNED_KNOBS);
    }
}
