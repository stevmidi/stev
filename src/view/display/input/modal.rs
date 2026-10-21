//! `handle_settings_input_event` — the settings modal's keymap and mouse: Esc
//! or `⌘/Ctrl+,` closes it, the tab chords switch tabs, `?` shows the
//! Keyboard tab, and inside a tab the arrows move a cursor (the MIDI rows,
//! the theme list, the Keyboard tab's rows), Enter connects a port, and a
//! click or wheel is resolved by `settings_modal.rs`. On the Keyboard tab
//! every other key is the search field's, and Esc clears a search before
//! it closes the modal. See `020-views-and-state.md`.

use crate::view::display::keyboard_tab::is_keyboard_tab_key;
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
                let on_keyboard_tab = self.settings.tab == SettingsTab::Keyboard;
                if key == Key::Escape && on_keyboard_tab && self.clear_keyboard_search() {
                    // Esc cleared the search; the next one closes.
                } else if key == Key::Escape || is_settings_chord(key, modifiers) {
                    self.close_overlay();
                } else if let Some(step) = settings_tab_step(key, modifiers) {
                    self.settings.tab = self.settings.tab.stepped(step);
                } else if !on_keyboard_tab && is_keyboard_tab_key(key, modifiers) {
                    // On the Keyboard tab `?` is typing, for the search.
                    self.settings.tab = SettingsTab::Keyboard;
                } else {
                    match self.settings.tab {
                        SettingsTab::Midi => self.handle_midi_tab_key(key),
                        SettingsTab::Appearance => self.handle_appearance_tab_key(key),
                        SettingsTab::Keyboard => self.handle_keyboard_tab_key(key),
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
                self.settings.midi.focus = self.settings.midi.focus.next();
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
        match self.settings.midi.focus.port_side() {
            Some(side) => self.settings.midi.list_mut(side).move_selection(step),
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
