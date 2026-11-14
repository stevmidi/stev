//! `handle_settings_input_event` — the settings modal's keymap and mouse: Esc
//! or `⌘/Ctrl+,` closes it, the tab chords switch tabs, `?` shows the
//! Keyboard tab, and inside a tab the arrows move a cursor (the Audio
//! sections, the MIDI rows, the theme list, the Keyboard tab's rows), Enter
//! chooses an audio device, rate or buffer, connects a port, records a
//! shortcut, flips *Allow AI agents* or (About, macOS) checks for updates, and a click or wheel is resolved by
//! `settings/mod.rs`. The showing tab sees a key before the modal's Esc
//! and `?` do, so the Keyboard tab's Esc clears a search first and its `?`
//! is typing; every other key there is the search field's. A Keyboard tab
//! row recording a shortcut sees every key before this does
//! (`handle_shortcut_recorder_input_event`, earlier in the input chain).
//! See `020-views-and-state.md`.

use crate::view::display::modal_focus::{AudioSettingsFocus, SettingsTab};
use crate::view::display::settings::about_tab::UPDATE_CHECK;
use crate::view::display::settings::keyboard_tab::is_keyboard_tab_key;
use crate::view::display::settings::{is_settings_chord, settings_tab_step};

use super::*;

impl Display {
    /// An event while the settings modal is up — it takes every event:
    /// keys, presses, the wheel; the pointer's moves and releases are
    /// swallowed, so nothing under it reacts.
    pub(super) fn handle_settings_input_event(&mut self, input_event: &InputEvent) {
        match *input_event {
            InputEvent::KeyPressed { key, modifiers } => {
                // `⌘/Ctrl+,` and the tab chords are the modal's on every
                // tab; then the showing tab has first look, and what it
                // leaves is the modal's again (Esc closes, `?` shows the
                // Keyboard tab).
                if is_settings_chord(key, modifiers) {
                    self.close_overlay();
                } else if let Some(step) = settings_tab_step(key, modifiers) {
                    self.settings.tab = self.settings.tab.stepped(step);
                } else if !self.handle_settings_tab_key(key, modifiers) {
                    if key == Key::Escape {
                        self.close_overlay();
                    } else if is_keyboard_tab_key(key, modifiers) {
                        self.settings.tab = SettingsTab::Keyboard;
                    }
                }
            }
            InputEvent::MouseClicked { x, y, .. } => self.click_settings(x, y, false),
            InputEvent::MouseDoubleClicked { x, y } => self.click_settings(x, y, true),
            InputEvent::TimelineScroll { delta_y, .. } => self.scroll_settings(delta_y),
            _ => {}
        }
    }

    /// A key for the showing tab; returns whether the tab took it.
    fn handle_settings_tab_key(&mut self, key: Key, modifiers: KeyModifiers) -> bool {
        match self.settings.tab {
            SettingsTab::Audio => self.handle_audio_tab_key(key, modifiers),
            SettingsTab::Midi => self.handle_midi_tab_key(key, modifiers),
            SettingsTab::Appearance => self.handle_appearance_tab_key(key),
            SettingsTab::Keyboard => self.handle_keyboard_tab_key(key, modifiers),
            SettingsTab::Ai => self.handle_ai_tab_key(key),
            SettingsTab::About => self.handle_about_tab_key(key),
        }
    }

    /// The About tab's keys: Enter checks for updates, as the button does
    /// (`UPDATE_CHECK`; no keys elsewhere). Returns whether it took the key.
    fn handle_about_tab_key(&mut self, key: Key) -> bool {
        if UPDATE_CHECK && key == Key::Enter {
            self.settings.about.request_check();
            return true;
        }
        false
    }

    /// The AI tab's keys: Enter, Space and ↑/↓ flip *Allow AI agents*, its
    /// one switch. Returns whether it took the key.
    fn handle_ai_tab_key(&mut self, key: Key) -> bool {
        if !matches!(key, Key::Enter | Key::Space | Key::ArrowUp | Key::ArrowDown) {
            return false;
        }
        self.toggle_agent_bridge();
        true
    }

    /// The Audio tab's keys: Tab cycles the three sections, ⇧Tab backwards;
    /// in the device list ↑/↓ move its cursor, in the rates and buffer sizes
    /// ←/→ move along them and ↑/↓ go to the section above or below; Enter
    /// chooses what is under the cursor, applied at once. While an audio
    /// export runs the tab is read-only: those keys are taken and do
    /// nothing. Returns whether it took the key.
    fn handle_audio_tab_key(&mut self, key: Key, modifiers: KeyModifiers) -> bool {
        let tab_key = matches!(
            key,
            Key::Tab
                | Key::Enter
                | Key::ArrowUp
                | Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
        );
        if self.is_exporting_audio() {
            return tab_key;
        }
        let audio = &mut self.settings.audio;
        let in_list = audio.focus == AudioSettingsFocus::Device;
        match key {
            Key::Tab if modifiers.shift => audio.focus = audio.focus.previous(),
            Key::Tab => audio.focus = audio.focus.next(),
            Key::Enter => {
                let choice = audio.cursor_choice();
                self.choose_audio(choice);
            }
            Key::ArrowUp if in_list => audio.move_device(-1),
            Key::ArrowDown if in_list => audio.move_device(1),
            Key::ArrowUp => audio.focus = audio.focus.previous(),
            Key::ArrowDown => audio.focus = audio.focus.next(),
            Key::ArrowLeft if !in_list => audio.move_choice(-1),
            Key::ArrowRight if !in_list => audio.move_choice(1),
            _ => return false,
        }
        true
    }

    /// The MIDI tab's keys: Tab and ←/→ cycle the three rows, ⇧Tab
    /// backwards; ↑/↓ move the focused port list's cursor or nudge the
    /// offset (applied and saved at once); Enter connects the port under the
    /// cursor. Returns whether it took the key.
    fn handle_midi_tab_key(&mut self, key: Key, modifiers: KeyModifiers) -> bool {
        let focus = self.settings.midi.focus;
        let step = match key {
            Key::Tab if modifiers.shift => {
                self.settings.midi.focus = self.settings.midi.focus.previous();
                return true;
            }
            Key::Tab | Key::ArrowLeft | Key::ArrowRight => {
                self.settings.midi.focus = self.settings.midi.focus.next();
                return true;
            }
            Key::Enter => {
                self.connect_focused_port();
                return true;
            }
            Key::ArrowUp => -1,
            Key::ArrowDown => 1,
            _ => return false,
        };
        match focus {
            MidiSettingsFocus::InPort => self.settings.midi.input.move_selection(step),
            MidiSettingsFocus::OutPort => self.settings.midi.output.move_selection(step),
            // Up raises the offset.
            MidiSettingsFocus::OutOffset => self.nudge_midi_out_offset(-step as i32),
        }
        true
    }

    /// The Appearance tab's keys: ↑/↓ step through the themes, each applied
    /// and saved as it is reached. Returns whether it took the key.
    fn handle_appearance_tab_key(&mut self, key: Key) -> bool {
        match key {
            Key::ArrowUp => self.step_theme(-1),
            Key::ArrowDown => self.step_theme(1),
            _ => return false,
        }
        true
    }
}
