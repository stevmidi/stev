//! The knob panel's link to the selected track's plugin
//! (`400-parameter-knobs.md`): the hint shown instead of knobs where they
//! can't drive anything, a parameter's name, default and value, setting it,
//! and what the plugin's own editor reports — a turn's start, which assigns
//! an armed knob, and a move of a knob's parameter, a hand turn for the
//! unsaved prompt. Plugins are hosted on macOS only; elsewhere every track
//! reads as having none.

#[cfg(target_os = "macos")]
use crate::core::input_event::InputEvent;
#[cfg(target_os = "macos")]
use crate::core::plugin_host::editor::ParameterGesture;
use crate::models::knobs::ParamId;
#[cfg(target_os = "macos")]
use crate::models::knobs::param_taken_by;

use super::Display;
#[cfg(target_os = "macos")]
use super::instrument::TrackInstrument;
#[cfg(target_os = "macos")]
use super::status_message::StatusMessage;

/// The panel's hint on a track with no plugin (a MIDI Out track, or a plugin
/// that isn't installed or failed to load): no parameters to drive.
pub(super) const NO_PLUGIN_HINT: &str = "No plugin on this track";

#[cfg(target_os = "macos")]
impl Display {
    /// The plugin on the track at `track_idx`, when the knobs can drive it.
    fn knob_plugin(&self, track_idx: usize) -> Option<&TrackInstrument> {
        self.track_instrument(track_idx)
            .filter(|loaded| loaded.editor.has_parameters())
    }

    /// Why the knobs can't drive the track at `track_idx` — the line the
    /// panel shows instead of them — or `None` when they can: no plugin, or
    /// a format whose parameters aren't reached yet (CLAP, until
    /// `400-parameter-knobs.md` phase 3).
    pub(super) fn knob_panel_hint(&self, track_idx: usize) -> Option<String> {
        match self.track_instrument(track_idx) {
            None => Some(NO_PLUGIN_HINT.to_owned()),
            Some(loaded) if !loaded.editor.has_parameters() => Some(format!(
                "No knobs for {} plugins yet",
                loaded.entry.format.label()
            )),
            Some(_) => None,
        }
    }

    /// Parameter `param`'s name on the track at `track_idx`'s plugin.
    pub(super) fn knob_param_name(&self, track_idx: usize, param: ParamId) -> Option<&str> {
        let info = self.knob_plugin(track_idx)?.editor.parameter_info(param)?;
        Some(info.name.as_str())
    }

    /// Parameter `param`'s default, normalized — a ⌘-click's target.
    pub(super) fn knob_param_default(&self, track_idx: usize, param: ParamId) -> Option<f64> {
        let info = self.knob_plugin(track_idx)?.editor.parameter_info(param)?;
        Some(info.default_value)
    }

    /// Parameter `param`'s value in force, normalized: what its knob shows.
    pub(super) fn knob_param_value(&self, track_idx: usize, param: ParamId) -> Option<f64> {
        self.knob_plugin(track_idx)?.editor.parameter_value(param)
    }

    /// Sets parameter `param` of the track at `track_idx`'s plugin to
    /// `value` — the sound and the plugin's editor follow.
    pub(super) fn set_knob_param_value(&mut self, track_idx: usize, param: ParamId, value: f64) {
        let Some(slot) = self.track_slot(track_idx) else {
            return;
        };
        if let Some(Some(loaded)) = self.instruments.track_instruments.get_mut(slot) {
            loaded.editor.set_parameter(param, value);
        }
    }

    /// Per frame: what every plugin's editor reported since the last frame
    /// ([`handle_parameter_gesture`](Self::handle_parameter_gesture)).
    pub(super) fn handle_parameter_gestures(&mut self) {
        let mut gestures = Vec::new();
        for (slot, loaded) in self.instruments.track_instruments.iter_mut().enumerate() {
            if let Some(loaded) = loaded {
                gestures.extend(
                    loaded
                        .editor
                        .take_parameter_gestures()
                        .into_iter()
                        .map(|gesture| (slot, gesture)),
                );
            }
        }
        for (slot, gesture) in gestures {
            self.handle_parameter_gesture(slot, gesture);
        }
    }

    /// One gesture from the editor of the plugin in engine slot `slot`. A
    /// turn's start assigns the armed knob, when that plugin is its track's
    /// (`400-parameter-knobs.md` decision 3); a move of a parameter one of
    /// that track's knobs holds is a hand turn, which the unsaved prompt
    /// counts (decision 9). Anything else is the plugin's own business.
    fn handle_parameter_gesture(&mut self, slot: usize, gesture: ParameterGesture) {
        let Some(track_idx) = self.tracks.iter().position(|lane| lane.slot == slot) else {
            return;
        };
        match gesture {
            ParameterGesture::Begin(param) => {
                if let Some(knob) = self.armed_knob()
                    && self.knobs_track() == Some(track_idx)
                {
                    self.assign_armed_knob(track_idx, knob, param);
                }
            }
            ParameterGesture::Move(param, value) => {
                let lane = &self.tracks[track_idx];
                if let Some(knob) = lane.knob_params.iter().position(|&p| p == Some(param)) {
                    self.input_event_tx
                        .send(InputEvent::SetKnobValue {
                            track_idx,
                            knob,
                            value,
                        })
                        .ok();
                }
            }
        }
    }

    /// The armed knob `knob` of `track_idx` takes `param`, the control just
    /// turned in the plugin's editor, and arming ends. Refused, with the
    /// footer naming the parameter, when another knob of the track holds it
    /// (decision 4).
    fn assign_armed_knob(&mut self, track_idx: usize, knob: usize, param: ParamId) {
        self.disarm_knob();
        if let Some(other) = param_taken_by(&self.tracks[track_idx].knob_params, knob, param) {
            let name = self
                .knob_param_name(track_idx, param)
                .unwrap_or("That parameter");
            let message = format!("{name} is on knob {} already", other + 1);
            self.render.status = Some(StatusMessage::new(message));
            return;
        }
        self.input_event_tx
            .send(InputEvent::SetKnobParam {
                track_idx,
                knob,
                param: Some(param),
            })
            .ok();
    }
}

#[cfg(not(target_os = "macos"))]
impl Display {
    /// No plugins off macOS: every track's knobs show the no-plugin hint.
    pub(super) fn knob_panel_hint(&self, _track_idx: usize) -> Option<String> {
        Some(NO_PLUGIN_HINT.to_owned())
    }

    /// No plugins off macOS, so no parameter names.
    pub(super) fn knob_param_name(&self, _track_idx: usize, _param: ParamId) -> Option<&str> {
        None
    }

    /// No plugins off macOS, so no defaults.
    pub(super) fn knob_param_default(&self, _track_idx: usize, _param: ParamId) -> Option<f64> {
        None
    }

    /// No plugins off macOS, so no values.
    pub(super) fn knob_param_value(&self, _track_idx: usize, _param: ParamId) -> Option<f64> {
        None
    }

    /// No plugins off macOS: nothing to set.
    pub(super) fn set_knob_param_value(&mut self, _track_idx: usize, _param: ParamId, _value: f64) {
    }
}
