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

use std::time::Duration;

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
}
