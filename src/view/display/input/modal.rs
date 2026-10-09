//! `handle_settings_input_event` — the settings modal's keymap and mouse: Esc
//! or `⌘/Ctrl+,` closes it, the tab chords switch tabs, and inside a tab the
//! arrows move a cursor (the MIDI rows, the theme list), Enter connects a
//! port, and a click or wheel is resolved by `settings_modal.rs`. See
//! `020-views-and-state.md`.

use crate::view::display::modal_focus::SettingsTab;
use crate::view::display::settings_modal::{is_settings_chord, settings_tab_step};

use super::*;

impl Display {
    /// An event while the settings modal is up — it takes every event:
    /// keys, presses, the wheel; the pointer's moves and releases are
    /// swallowed, so nothing under it reacts.
    pub(super) fn handle_settings_input_event(&mut self, input_event: &InputEvent) {
        match *input_event {
            InputEvent::KeyPressed { key, modifiers } => {
                if key == Key::Escape || is_settings_chord(key, modifiers) {
                    self.close_overlay();
                } else if let Some(step) = settings_tab_step(key, modifiers) {
                    self.settings.tab = self.settings.tab.stepped(step);
                } else {
                    match self.settings.tab {
                        SettingsTab::Midi => self.handle_midi_tab_key(key),
                        SettingsTab::Appearance => self.handle_appearance_tab_key(key),
                    }
                }
            }
            InputEvent::MouseClicked { x, y, .. } => self.click_settings(x, y, false),
            InputEvent::MouseDoubleClicked { x, y } => self.click_settings(x, y, true),
            InputEvent::TimelineScroll { delta_y, .. } => self.scroll_settings(delta_y),
            _ => {}
        }
    }

    /// The MIDI tab's keys: Tab and ←/→ cycle the three rows; ↑/↓ move the
    /// focused port list's cursor or nudge the offset (applied and saved at
    /// once); Enter connects the port under the cursor.
    fn handle_midi_tab_key(&mut self, key: Key) {
        let step = match key {
            Key::Tab | Key::ArrowLeft | Key::ArrowRight => {
                self.midi.focus = self.midi.focus.next();
                return;
            }
            Key::Enter => {
                self.connect_focused_port();
                return;
            }
            Key::ArrowUp => -1,
            Key::ArrowDown => 1,
            _ => return,
        };
        match self.midi.focus.port_side() {
            Some(side) => self.midi.list_mut(side).move_selection(step),
            // Up raises the offset.
            None => self.nudge_midi_out_offset(-step as i32),
        }
    }

    /// The Appearance tab's keys: ↑/↓ step through the themes, each applied
    /// and saved as it is reached.
    fn handle_appearance_tab_key(&mut self, key: Key) {
        match key {
            Key::ArrowUp => self.step_theme(-1),
            Key::ArrowDown => self.step_theme(1),
            _ => {}
        }
    }
}
