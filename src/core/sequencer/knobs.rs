//! The knob panel's sequencer side (`400-parameter-knobs.md`): what each
//! track's knobs drive, set by a project load, and where the hand last put
//! them, for the unsaved check. Turning a knob is not undoable, like volume
//! and pan; assigning one is (`SetKnobParamEdit`). The parameter itself is
//! set by the view, straight on the plugin, and its value is the plugin's:
//! nothing here sends anything.

use crate::models::knobs::KnobParams;

use super::Sequencer;

impl Sequencer {
    /// Gives the track at `track_idx` `knobs` (a project load). Out-of-range
    /// indices are ignored.
    pub(crate) fn set_track_knobs(&mut self, track_idx: usize, knobs: KnobParams) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.set_knobs(knobs);
        }
    }

    /// The hand turned knob `knob` of the track at `track_idx` to `value`
    /// (normalized, clamped to `0.0..=1.0`) — a drag in the knob panel, or
    /// the parameter's control turned in the plugin's editor. Counts toward
    /// the unsaved prompt (`400-parameter-knobs.md` decision 9). Out-of-range
    /// indices are ignored.
    pub(crate) fn set_knob_value(&mut self, track_idx: usize, knob: usize, value: f64) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.set_knob_by_hand(knob, value.clamp(0.0, 1.0));
        }
    }
}
