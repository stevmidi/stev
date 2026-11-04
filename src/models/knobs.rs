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

/// Which plugin a [`PluginParam`] belongs to: [`plugin_tag`] of its id.
pub(crate) type PluginTag = u32;

/// One plugin's parameter. A [`ParamId`] means something only to its own
/// plugin, and a clip can travel to a track with another one: the plugin goes
/// with the ID, so a move only ever matches the knob of the plugin it was
/// recorded on (`400-parameter-knobs.md` decision 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PluginParam {
    /// Its plugin.
    pub(crate) plugin: PluginTag,
    /// The ID that plugin gives it.
    pub(crate) id: ParamId,
}

/// The tag of the plugin `plugin_id` (VST3's class ID, CLAP's id, each
/// unique to a plugin wherever it is installed): its 32-bit FNV-1a hash,
/// saved in the project, so it must never change.
pub(crate) fn plugin_tag(plugin_id: &str) -> PluginTag {
    plugin_id.bytes().fold(0x811C_9DC5, |hash, byte| {
        (hash ^ PluginTag::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// What each of a track's knobs drives, in reading order (the first column
/// top to bottom, then the second): a parameter of the track's plugin, or
/// `None` while the knob is unassigned. No two knobs hold the same parameter
/// ([`param_taken_by`]).
pub(crate) type KnobParams = [Option<PluginParam>; KNOB_COUNT];

/// A fresh track's knobs: none assigned.
pub(crate) const UNASSIGNED_KNOBS: KnobParams = [None; KNOB_COUNT];

/// The other knob of `knobs` that holds parameter `id` already, if any —
/// knob `knob` can't take it then: both would claim the same data
/// (`400-parameter-knobs.md` decision 4). A knob's own parameter is no
/// refusal.
pub(crate) fn param_taken_by(knobs: &KnobParams, knob: usize, id: ParamId) -> Option<usize> {
    knob_with_id(knobs, id).filter(|&other| other != knob)
}

/// The knob of `knobs` that holds `param`, if any: none for a parameter of
/// another plugin, whatever its ID.
pub(crate) fn knob_holding(knobs: &KnobParams, param: PluginParam) -> Option<usize> {
    knobs.iter().position(|&held| held == Some(param))
}

/// The knob of `knobs` that holds parameter `id` of the track's own plugin,
/// if any: what the plugin reports, in the only IDs it knows.
pub(crate) fn knob_with_id(knobs: &KnobParams, id: ParamId) -> Option<usize> {
    knobs
        .iter()
        .position(|held| held.is_some_and(|held| held.id == id))
}

/// The footer's refusal when knob `other` (counted from 0) holds the
/// parameter already: by `name` when it is known.
pub(crate) fn param_taken_message(name: Option<&str>, other: usize) -> String {
    let name = name.unwrap_or("That parameter");
    format!("{name} is on knob {} already", other + 1)
}

/// The knobs a saved track on `plugin` names, or none assigned when it names
/// a set no track could have: the wrong count, or two knobs on one parameter.
/// A project saved before parameter knobs (its CC knobs) reads as eight
/// unassigned knobs, its entries naming no parameter.
pub(crate) fn knobs_from_saved(saved: &[Option<ParamId>], plugin: PluginTag) -> KnobParams {
    let Ok(ids) = <[Option<ParamId>; KNOB_COUNT]>::try_from(saved) else {
        return UNASSIGNED_KNOBS;
    };
    let knobs = ids.map(|id| id.map(|id| PluginParam { plugin, id }));
    let duplicate = knobs
        .iter()
        .enumerate()
        .any(|(i, param)| param.is_some_and(|param| param_taken_by(&knobs, i, param.id).is_some()));
    if duplicate { UNASSIGNED_KNOBS } else { knobs }
}

/// `value` (normalized) at the nearest position a parameter of `steps` steps
/// can stand, each position holding an equal share of the range as VST3 lays
/// them out; `value` itself for a parameter with none (`steps` 0). A turned
/// knob goes where the plugin will, so a take holds no value between two
/// (`400-parameter-knobs.md` decision 12).
pub(crate) fn snap_to_steps(value: f64, steps: u32) -> f64 {
    if steps == 0 {
        return value;
    }
    let steps = f64::from(steps);
    (value * (steps + 1.0)).floor().min(steps) / steps
}

/// Parameter `id` of one plugin, the same in every test.
#[cfg(test)]
pub(crate) const fn test_param(id: ParamId) -> PluginParam {
    PluginParam { plugin: 1, id }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A switch flips at the middle, and three positions split the range in
    /// thirds, not at the points halfway between them.
    #[test]
    fn a_value_snaps_to_the_step_whose_share_of_the_range_holds_it() {
        assert_eq!(snap_to_steps(0.49, 1), 0.0);
        assert_eq!(snap_to_steps(0.5, 1), 1.0);
        assert_eq!(snap_to_steps(0.3, 2), 0.0);
        assert_eq!(snap_to_steps(0.4, 2), 0.5);
        assert_eq!(snap_to_steps(0.7, 2), 1.0);
        assert_eq!(snap_to_steps(1.0, 2), 1.0);
    }

    #[test]
    fn a_snapped_value_stays_put_and_a_smooth_one_is_left_alone() {
        for steps in 1..=128 {
            for position in 0..=steps {
                let value = f64::from(position) / f64::from(steps);
                assert_eq!(snap_to_steps(value, steps), value, "{position}/{steps}");
            }
        }
        assert_eq!(snap_to_steps(0.37, 0), 0.37);
    }

    #[test]
    fn a_parameter_another_knob_holds_is_taken_but_a_knobs_own_is_not() {
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[0] = Some(test_param(714));
        knobs[5] = Some(test_param(7_000_000));
        assert_eq!(param_taken_by(&knobs, 3, 714), Some(0));
        assert_eq!(param_taken_by(&knobs, 0, 7_000_000), Some(5));
        assert_eq!(param_taken_by(&knobs, 0, 714), None);
        assert_eq!(param_taken_by(&knobs, 3, 1), None);
        assert_eq!(knob_with_id(&knobs, 714), Some(0));
        assert_eq!(knob_with_id(&knobs, 1), None);
    }

    /// Plugins number their parameters as they like, so two can share an
    /// ID: only the knob's own plugin's parameter is the knob's.
    #[test]
    fn a_parameter_of_another_plugin_is_no_knobs_whatever_its_id() {
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[0] = Some(test_param(714));
        assert_eq!(knob_holding(&knobs, test_param(714)), Some(0));
        let foreign = PluginParam { plugin: 2, id: 714 };
        assert_eq!(knob_holding(&knobs, foreign), None);
    }

    /// Saved in projects, so these never change.
    #[test]
    fn a_plugins_tag_is_its_ids_fnv_hash() {
        assert_eq!(plugin_tag(""), 0x811C_9DC5);
        assert_eq!(plugin_tag("a"), 0xE40C_292C);
    }

    #[test]
    fn the_taken_message_names_the_parameter_when_known() {
        assert_eq!(
            param_taken_message(Some("Cutoff"), 2),
            "Cutoff is on knob 3 already"
        );
        assert_eq!(
            param_taken_message(None, 0),
            "That parameter is on knob 1 already"
        );
    }

    #[test]
    fn a_saved_set_loads_as_saved() {
        let mut saved = [None; KNOB_COUNT];
        saved[2] = Some(714);
        saved[7] = Some(0);
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[2] = Some(test_param(714));
        knobs[7] = Some(test_param(0));
        assert_eq!(knobs_from_saved(&saved, 1), knobs);
    }

    #[test]
    fn a_set_no_track_could_have_loads_unassigned() {
        assert_eq!(knobs_from_saved(&[], 1), UNASSIGNED_KNOBS);
        assert_eq!(knobs_from_saved(&[Some(1); 7], 1), UNASSIGNED_KNOBS);
        let mut duplicate = [None; KNOB_COUNT];
        duplicate[1] = Some(714);
        duplicate[6] = Some(714);
        assert_eq!(knobs_from_saved(&duplicate, 1), UNASSIGNED_KNOBS);
    }
}
