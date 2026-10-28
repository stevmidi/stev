//! The footer's shortcut hint: while the pointer is on a control that has a
//! key of its own, the footer names the key — `New project folder —
//! Cmd+Shift+N`. The clickable controls are one list ([`HoveredControl`]):
//! it gives the pointing hand, and each control whose press does what a key
//! does gives its hint. A command's hint follows its rebinding (the live
//! [`Keymap`]); an unbound command has none. Drawn by `draw_status_bar`,
//! under a passing message. See `030-ui-design.md` § Header & Footer.

use crate::core::keymap::{Action, Chord, Keymap};

use super::Display;
use super::browser::BrowserCategory;
use super::keyboard_tab::{KEYBOARD_TAB_ROW, chords_label};
use super::output_menu::OutputMenuHit;

/// A clickable control under the pointer: what shows the pointing hand,
/// bar the track header's S/M buttons (their own hover state).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum HoveredControl {
    /// An item of the open output menu, which hangs over the rest.
    OutputMenu(OutputMenuHit),
    /// The arranger's add-track row.
    AddTrack,
    /// The header's `?` chip.
    HelpChip,
    /// The header's piano chip.
    KeyboardChip,
    /// The footer's panel toggle.
    BrowserToggle,
    /// A browser category row's `+`, which runs this command
    /// (`BrowserCategory::plus_action`).
    CategoryPlus(Action),
}

impl HoveredControl {
    /// The control's footer hint under `keymap`: its key, when its press
    /// does what a key does and that key is bound.
    fn hint(self, keymap: &Keymap) -> Option<String> {
        let action = match self {
            HoveredControl::OutputMenu(OutputMenuHit::Rename) => Action::RenameTrack,
            HoveredControl::OutputMenu(_) => return None,
            HoveredControl::AddTrack => Action::AddTrack,
            HoveredControl::HelpChip => {
                let (keys, label) = KEYBOARD_TAB_ROW;
                return Some(hint_text(label, keys));
            }
            HoveredControl::KeyboardChip => Action::ToggleComputerKeyboard,
            HoveredControl::BrowserToggle => Action::ToggleBrowser,
            HoveredControl::CategoryPlus(action) => action,
        };
        command_hint(action.info().label, keymap.chords(action))
    }
}

impl Display {
    /// The clickable control under the pointer, if any: none while a modal
    /// has the pointer (`canvas_pointer`).
    pub(super) fn hovered_control(&mut self) -> Option<HoveredControl> {
        if let Some(item) = self.hovered_output_menu_item() {
            Some(HoveredControl::OutputMenu(item))
        } else if self.is_pointer_on_add_track_row() {
            Some(HoveredControl::AddTrack)
        } else if self.is_pointer_on_help_chip() {
            Some(HoveredControl::HelpChip)
        } else if self.is_pointer_on_keyboard_chip() {
            Some(HoveredControl::KeyboardChip)
        } else if self.is_pointer_on_browser_toggle() {
            Some(HoveredControl::BrowserToggle)
        } else {
            self.browser_category_plus_under_pointer()
                .and_then(BrowserCategory::plus_action)
                .map(HoveredControl::CategoryPlus)
        }
    }

    /// The hint for the control under the pointer, if it has one.
    pub(super) fn footer_hint(&mut self) -> Option<String> {
        self.hovered_control()?.hint(&self.keymap)
    }
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

    use super::{HoveredControl, command_hint, hint_text};
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::{Action, Chord, Keymap};
    use crate::view::display::output_menu::OutputMenuHit;

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

    /// Only a control whose press does what a key does has a hint.
    #[test]
    fn only_controls_with_a_key_twin_hint() {
        let keymap = Keymap::default();
        assert_eq!(
            HoveredControl::HelpChip.hint(&keymap).as_deref(),
            Some("Keyboard shortcuts — ?")
        );
        assert!(
            HoveredControl::CategoryPlus(Action::NewProjectFolder)
                .hint(&keymap)
                .is_some()
        );
        assert!(
            HoveredControl::OutputMenu(OutputMenuHit::Rename)
                .hint(&keymap)
                .is_some()
        );
        assert_eq!(
            HoveredControl::OutputMenu(OutputMenuHit::DeleteTrack).hint(&keymap),
            None
        );
    }
}
