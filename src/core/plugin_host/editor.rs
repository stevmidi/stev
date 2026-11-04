//! The format-agnostic main-thread view of one hosted instrument: its editor
//! window and everything that has to touch the plugin's `!Send` half.
//!
//! `Display` holds one boxed [`InstrumentEditor`] per instrument track (see
//! `view::display::instrument`) and pumps them all once per frame. The matching
//! `Send` half — the audio processor — lives in the mixer as an
//! [`InstrumentVoice`](super::voice::InstrumentVoice).
//!
//! Implemented by `clap::ClapEditor` and `vst3::Vst3Editor`. Deliberately
//! **not** `Send`: the CLAP `PluginInstance` (and a VST3 `IEditController`) may
//! only be touched from the thread with the AppKit run loop.
//!
//! The knob panel reaches a plugin's parameters through it too
//! (`400-parameter-knobs.md`): their names, their values, setting one, and
//! the gestures the plugin's own editor reports.

use std::time::Duration;

use crate::models::knobs::ParamId;

/// One of a plugin's parameters, as the knob panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParameterInfo {
    /// Its name, as the plugin gives it.
    pub(crate) name: String,
    /// Its default, normalized — where a ⌘-click on its knob puts it.
    pub(crate) default_value: f64,
    /// How many steps it moves in, one fewer than its positions, or 0 when
    /// it moves smoothly. A stepped parameter's knob stops at each position
    /// and its lane holds each value until the next
    /// (`400-parameter-knobs.md` decision 12).
    pub(crate) steps: u32,
}

/// A parameter change the plugin reported from its own editor — the user
/// turning one of its controls, or a preset it loaded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParameterGesture {
    /// A turn began (VST3 `beginEdit`, a CLAP gesture-begin event): what assigns an armed knob, and
    /// what holds a knob's parameter. Carries where the parameter stood,
    /// normalized, read as the turn began — by the time the gesture is
    /// handled the turn has moved it — and when, as a
    /// [`Move`](Self::Move) does.
    Begin(ParamId, Option<f64>, u64),
    /// The parameter moved to a normalized value (VST3 `performEdit`, a CLAP
    /// value event), at
    /// the given [`monotonic_nanos`](crate::core::time::monotonic_nanos):
    /// a gesture is handled a frame later, and a take wants when it came.
    Move(ParamId, f64, u64),
    /// A turn ended (VST3 `endEdit`).
    End(ParamId),
    /// The plugin's values changed as a whole (VST3 `restartComponent` with
    /// `kParamValuesChanged`, a CLAP `rescan` of its values): a preset it
    /// loaded. A change to the project in its own right, with or without a
    /// knob on anything it moved.
    ValuesChanged,
}

/// One hosted plugin's main-thread half. See the module docs.
pub(crate) trait InstrumentEditor {
    /// Opens the plugin's editor window, creating its GUI if this is the first
    /// time (or the first since a [`close`](Self::close)).
    fn show(&mut self);

    /// Whether the editor window is currently shown.
    fn is_open(&self) -> bool;

    /// Closes the editor window and **destroys** the plugin's GUI rather than
    /// hiding it. Hiding a live GUI leaves the plugin's own animation timers
    /// running and measurably raises idle CPU, so every format must genuinely
    /// tear the view down in [`teardown_gui`](Self::teardown_gui) and rebuild
    /// it on the next `show`.
    fn close(&mut self) {
        self.teardown_gui();
    }

    /// Toggles the editor window (the Arranger's `v` binding).
    fn toggle(&mut self) {
        if self.is_open() {
            self.close();
        } else {
            self.show();
        }
    }

    /// Destroys the plugin's GUI without touching the plugin itself — the first
    /// half of removing a plugin from a track, run before the mixer is told to
    /// drop the matching voice.
    fn teardown_gui(&mut self);

    /// Serialises the plugin's current preset for the project file, or `None`
    /// if this plugin cannot save state or the save failed. The caller keeps
    /// the last good blob on `None` rather than overwriting it with nothing.
    fn save_state(&mut self) -> Option<Vec<u8>>;

    /// Deactivates the plugin, once the mixer has acked that its voice is
    /// dropped. The editor is dropped immediately afterwards.
    fn deactivate(&mut self);

    /// Per-frame servicing on the main thread: plugin timer callbacks, window
    /// state, pending resizes. `yield_to_main` while the main window shows a
    /// dialog the user must answer: the editor window then goes behind it
    /// instead of floating (`window::editor_level`). Returns the shortest
    /// interval at which this plugin wants to be pumped again, so the caller
    /// can schedule the next repaint; `None` if it needs no timed wake-up.
    fn pump(&mut self, yield_to_main: bool) -> Option<Duration>;

    /// Whether the knobs can drive this plugin's parameters.
    fn has_parameters(&self) -> bool;

    /// Parameter `id`'s name and default, if the plugin has such a
    /// parameter.
    fn parameter_info(&self, id: ParamId) -> Option<&ParameterInfo>;

    /// Parameter `id`'s value in force, normalized to `0.0..=1.0` — what its
    /// knob shows, wherever it came from.
    fn parameter_value(&self, id: ParamId) -> Option<f64>;

    /// Sets parameter `id` to `value` (normalized), for the sound and for
    /// the plugin's editor alike — a knob turned in Stev. Never reported
    /// back as a [`ParameterGesture`].
    fn set_parameter(&mut self, id: ParamId, value: f64);

    /// Tells the plugin's editor that playback moved parameter `id` to
    /// `value` (normalized): the processor heard it already, on its sample
    /// (`InstrumentVoice::queue_param`), so this only moves what the editor
    /// shows. Never reported back as a [`ParameterGesture`].
    fn follow_parameter(&mut self, id: ParamId, value: f64);

    /// The gestures the plugin's editor has reported since the last call,
    /// oldest first.
    fn take_parameter_gestures(&mut self) -> Vec<ParameterGesture>;
}
