//! Driving the computer keyboard (`view::computer_keyboard`) from `Display`:
//! the `⌘/Ctrl+K` toggle (the header chip is drawn in
//! `rendering/overlays.rs`), the gate that decides whether
//! the note keys play this frame, and sending the notes they play to the
//! `"computerkeys"` thread (`computer_keys_tx`), stamped with the time the
//! frame read them. See `010-keybindings.md` § Computer keyboard and
//! `030-ui-design.md` § Header & Footer.

use egui::{Context, Key};

use crate::core::input_event::KeyModifiers;
use crate::core::midi::message::Midi3;
use crate::core::time::monotonic_nanos;

use super::Display;

/// `⌘/Ctrl+K` — turns the computer keyboard on or off (Logic Pro's Musical
/// Typing chord).
pub(super) fn is_computer_keyboard_chord(key: Key, modifiers: KeyModifiers) -> bool {
    key == Key::K && modifiers.command && !modifiers.shift && !modifiers.alt
}

impl Display {
    /// Turns the computer keyboard on or off; turning it off ends the notes
    /// its keys hold at once, rather than on the next frame's gate (an idle
    /// window may not draw one soon).
    pub(super) fn toggle_computer_keyboard(&mut self) {
        self.computer_keyboard.toggle();
        if !self.computer_keyboard.enabled() {
            self.release_computer_keys();
        }
    }

    /// Whether the note keys play this frame: the keyboard is on, the window
    /// has the OS keyboard, and nothing ahead of the main dispatch is taking
    /// every key (`keyboard_taken`).
    pub(super) fn computer_keys_live(&self, ctx: &Context) -> bool {
        self.computer_keyboard.enabled() && !self.keyboard_taken() && ctx.input(|i| i.focused)
    }

    /// Ends every note the keys hold — they stopped playing (turned off, or a
    /// text field, an overlay or another window took them), and their
    /// releases may never come here.
    pub(super) fn release_computer_keys(&mut self) {
        let note_offs = self.computer_keyboard.release_all();
        self.send_computer_keys(note_offs);
    }

    /// Plays this frame's note-key edges, set aside by the poll.
    pub(super) fn play_computer_keys(&mut self) {
        let messages = self
            .input_poller
            .drain_note_keys()
            .filter_map(|(key, pressed)| {
                if pressed {
                    self.computer_keyboard.press(key)
                } else {
                    self.computer_keyboard.release(key)
                }
            })
            .collect();
        self.send_computer_keys(messages);
    }

    /// Sends `messages` to the `"computerkeys"` thread, stamped now.
    fn send_computer_keys(&self, messages: Vec<Midi3>) {
        if messages.is_empty() {
            return;
        }
        let now = monotonic_nanos();
        for message in messages {
            self.computer_keys_tx.send((message, now)).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toggle_is_command_k_alone() {
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        assert!(is_computer_keyboard_chord(Key::K, command));
        assert!(!is_computer_keyboard_chord(Key::K, KeyModifiers::default()));
        let shifted = KeyModifiers {
            shift: true,
            ..command
        };
        assert!(!is_computer_keyboard_chord(Key::K, shifted));
        assert!(!is_computer_keyboard_chord(Key::J, command));
    }
}
