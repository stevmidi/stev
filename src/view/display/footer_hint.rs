//! The footer's shortcut hint: while the pointer is on a control that has a
//! key of its own, the footer names the key — `New project folder —
//! Cmd+Shift+N`. Only controls whose press does what the key does carry one,
//! and only these: the footer's panel toggle, the browser's new-folder `+`,
//! the header's piano and `?` chips, the arranger's add-track row and the
//! output menu's `Rename`. A command's hint follows its rebinding (the live
//! [`Keymap`](crate::core::keymap::Keymap)); an unbound command has none.
//! Drawn by `draw_status_bar`, under a passing message. See
//! `030-ui-design.md` § Header & Footer.

use crate::core::keymap::{Action, Chord};

use super::Display;
use super::keyboard_tab::chords_label;
use super::output_menu::OutputMenuHit;

impl Display {
    /// The hint for the control under the pointer, if it has one: none
    /// while a modal or a project dialog has the pointer.
    pub(super) fn footer_hint(&mut self) -> Option<String> {
        if self.overlay.is_some() || self.project_dialog.is_some() {
            return None;
        }
        let action = if self.is_pointer_on_browser_toggle() {
            Action::ToggleBrowser
        } else if self.is_pointer_on_browser_new_folder() {
            Action::NewProjectFolder
        } else if self.is_pointer_on_keyboard_chip() {
            Action::ToggleComputerKeyboard
        } else if self.is_pointer_on_help_chip() {
            return Some(hint_text(Self::KEYBOARD_TAB_LABEL, "?"));
        } else if self.is_pointer_on_add_track_row() {
            Action::AddTrack
        } else if self.hovered_output_menu_item() == Some(OutputMenuHit::Rename) {
            Action::RenameTrack
        } else {
            return None;
        };
        command_hint(action.info().label, self.keymap.chords(action))
    }

    /// What the `?` chip's hint calls the Keyboard tab — its row in the
    /// tab's fixed keys.
    const KEYBOARD_TAB_LABEL: &str = "Keyboard shortcuts";
}

/// A command's hint: its `label` and its `chords`, or `None` while it is
/// unbound.
fn command_hint(label: &str, chords: &[Chord]) -> Option<String> {
    (!chords.is_empty()).then(|| hint_text(label, &chords_label(chords)))
}

/// A hint's line: what the control does, then its keys.
fn hint_text(label: &str, keys: &str) -> String {
    format!("{label} — {keys}")
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{command_hint, hint_text};
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::Chord;

    #[test]
    fn a_hint_names_the_command_then_its_keys() {
        assert_eq!(
            hint_text("Keyboard shortcuts", "?"),
            "Keyboard shortcuts — ?"
        );
    }

    #[test]
    fn an_unbound_command_has_no_hint() {
        assert_eq!(command_hint("New project folder", &[]), None);
    }

    #[test]
    fn a_command_hint_lists_every_chord() {
        let plain = |key| Chord::from_press(key, KeyModifiers::default());
        let chords = [plain(Key::Backslash), plain(Key::Slash)];
        assert_eq!(
            command_hint("Commit", &chords).as_deref(),
            Some("Commit — \\  /")
        );
    }
}
