//! The settings modal (`⌘/Ctrl+,`), an overlay over the view: opening and
//! closing it, its tab chords, the theme list's scroll window, its mouse,
//! and applying a theme. The MIDI tab's own state is `midi_state.rs`; the
//! keys inside a tab are `input/modal.rs`; drawing and the click hit-test
//! share one layout in `rendering/modals/settings.rs`. See
//! `020-views-and-state.md` and `030-ui-design.md` § Settings modal.

use egui::{Key, pos2};

use crate::core::config;
use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::settings::clamp_midi_out_offset_ms;
use crate::view::theme;

use super::browser::scroll_to_show;
use super::{Display, MidiSettingsFocus, Overlay, PortSide, SettingsTab};

/// Theme rows the Appearance tab shows at once; the rest scroll.
pub(super) const THEME_ROWS: usize = 14;

/// The settings modal's view-local state. Session-only: it opens on the tab
/// last used, MIDI after launch. Whether it is up is `Display::overlay`.
#[derive(Default)]
pub(super) struct SettingsModal {
    /// The tab showing.
    pub(super) tab: SettingsTab,
    /// First visible row of the theme list.
    pub(super) theme_scroll: usize,
    /// Wheel travel over the theme list not yet turned into whole rows, in
    /// points.
    pub(super) wheel: f32,
}

/// What a click in the settings modal lands on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SettingsHit {
    /// A tab in the strip.
    Tab(SettingsTab),
    /// A MIDI section — its header, the offset row, or the empty and ghost
    /// slots of a port list: focuses that row.
    Section(MidiSettingsFocus),
    /// A port in the `side` list, by index into it.
    Port(PortSide, usize),
    /// A theme, by index.
    Theme(usize),
    /// Anywhere else inside the panel.
    Panel,
}

/// `⌘/Ctrl+,` — opens the settings modal, or closes it when it is up.
pub(super) fn is_settings_chord(key: Key, modifiers: KeyModifiers) -> bool {
    key == Key::Comma && modifiers.command && !modifiers.shift && !modifiers.alt
}

/// The tab step a key press asks for: `⌘⇧]` or `Ctrl+Tab` the next tab,
/// `⌘⇧[` or `Ctrl+⇧Tab` the previous one. The brackets match their shifted
/// forms too, which a layout may report with ⇧ held.
pub(super) fn settings_tab_step(key: Key, modifiers: KeyModifiers) -> Option<isize> {
    let bracket_chord = modifiers.command && modifiers.shift && !modifiers.alt;
    match key {
        Key::Tab if modifiers.ctrl && !modifiers.alt => Some(if modifiers.shift { -1 } else { 1 }),
        Key::CloseBracket | Key::CloseCurlyBracket if bracket_chord => Some(1),
        Key::OpenBracket | Key::OpenCurlyBracket if bracket_chord => Some(-1),
        _ => None,
    }
}

/// `scroll` moved by `rows` (down when positive), kept inside a `len`-row
/// list shown `visible` rows at a time.
pub(super) fn scroll_by(scroll: usize, rows: isize, len: usize, visible: usize) -> usize {
    window_start(scroll.saturating_add_signed(rows), len, visible)
}

/// The first row a `visible`-row window scrolled to `scroll` shows of a
/// `len`-row list: `scroll`, pulled back so the window never shows empty
/// rows past the end.
pub(super) fn window_start(scroll: usize, len: usize, visible: usize) -> usize {
    scroll.min(len.saturating_sub(visible))
}

impl Display {
    /// Opens the settings modal over the view on the tab last used:
    /// the MIDI focus back on IN PORT, each port list's cursor on its
    /// connected port, the theme list scrolled to the active theme. The port
    /// lists are already current — the `"midiwatcher"` keeps them so.
    pub(super) fn open_settings(&mut self) {
        self.overlay = Some(Overlay::Settings);
        self.midi.focus = MidiSettingsFocus::InPort;
        self.midi.input.select_current();
        self.midi.output.select_current();
        self.settings.theme_scroll = scroll_to_show(
            self.settings.theme_scroll,
            theme::active_theme_index(),
            THEME_ROWS,
        );
    }

    /// Makes theme `idx` the active one and, when that changes it, has the
    /// sequencer save it as the theme the app starts with — the theme on
    /// screen is always the saved one. The list scrolls to show it.
    pub(super) fn apply_theme(&mut self, idx: usize) {
        let before = theme::active_theme_index();
        theme::set_active_theme(idx);
        let idx = theme::active_theme_index();
        self.settings.theme_scroll = scroll_to_show(self.settings.theme_scroll, idx, THEME_ROWS);
        if idx != before {
            self.input_event_tx
                .send(InputEvent::SaveTheme { theme_index: idx })
                .ok();
        }
    }

    /// Steps the theme by `direction` along the list, wrapping, and applies
    /// it ([`apply_theme`](Self::apply_theme)).
    pub(super) fn step_theme(&mut self, direction: isize) {
        let count = theme::theme_count() as isize;
        let idx = (theme::active_theme_index() as isize + direction).rem_euclid(count);
        self.apply_theme(idx as usize);
    }

    /// Nudges the MIDI-output offset by `steps` of
    /// `config::MIDI_OUT_OFFSET_STEP_MS`, then has the sequencer apply and
    /// save it.
    pub(super) fn nudge_midi_out_offset(&mut self, steps: i32) {
        self.midi.out_offset_ms = clamp_midi_out_offset_ms(
            self.midi.out_offset_ms + steps * config::MIDI_OUT_OFFSET_STEP_MS,
        );
        self.input_event_tx
            .send(InputEvent::SetMidiOutOffset {
                out_offset_ms: self.midi.out_offset_ms,
            })
            .ok();
    }

    /// Connects the port under the focused list's cursor and has the
    /// sequencer save both connected ports (Enter, or a double-click on a
    /// port). Nothing on the offset row, whose nudges already apply.
    pub(super) fn connect_focused_port(&mut self) {
        let Some(side) = self.midi.focus.port_side() else {
            return;
        };
        self.midi.list_mut(side).connect();
        self.input_event_tx
            .send(InputEvent::ConfirmMidiPorts {
                in_port: self.midi.input.current_name(),
                out_port: self.midi.output.current_name(),
            })
            .ok();
    }

    /// A press in the settings modal: a tab shows it, a section focuses its
    /// row, a port is focused with its cursor there, a theme is applied, and
    /// a press outside the panel closes the modal. `double` (the second
    /// press of a double-click) then acts as Enter on a port.
    pub(super) fn click_settings(&mut self, x: f32, y: f32, double: bool) {
        let Some(hit) = self.settings_hit_at(pos2(x, y)) else {
            if !double {
                self.close_overlay();
            }
            return;
        };
        match hit {
            SettingsHit::Tab(tab) => self.settings.tab = tab,
            SettingsHit::Section(focus) => self.midi.focus = focus,
            SettingsHit::Port(side, idx) => {
                self.midi.focus = side.focus();
                self.midi.list_mut(side).select(idx);
                if double {
                    self.connect_focused_port();
                }
            }
            SettingsHit::Theme(idx) => self.apply_theme(idx),
            SettingsHit::Panel => {}
        }
    }

    /// A wheel or trackpad scroll in the settings modal: scrolls the theme
    /// list a row per row height of travel. The MIDI tab's lists follow
    /// their cursors only.
    pub(super) fn scroll_settings(&mut self, delta_y: f32) {
        if self.settings.tab != SettingsTab::Appearance {
            return;
        }
        let row_h = Self::SETTINGS_ROW_H;
        self.settings.wheel += delta_y;
        let rows = (self.settings.wheel / row_h).trunc();
        if rows == 0.0 {
            return;
        }
        self.settings.wheel -= rows * row_h;
        // Content follows the fingers: a positive delta (scrolling up)
        // brings earlier rows into view.
        self.settings.theme_scroll = scroll_by(
            self.settings.theme_scroll,
            -(rows as isize),
            theme::theme_count(),
            THEME_ROWS,
        );
    }
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{is_settings_chord, scroll_by, settings_tab_step, window_start};
    use crate::core::input_event::KeyModifiers;

    /// Modifier state from its four flags.
    fn mods(command: bool, ctrl: bool, shift: bool, alt: bool) -> KeyModifiers {
        KeyModifiers {
            shift,
            command,
            ctrl,
            alt,
        }
    }

    #[test]
    fn only_a_bare_command_comma_is_the_settings_chord() {
        assert!(is_settings_chord(
            Key::Comma,
            mods(true, false, false, false)
        ));
        assert!(!is_settings_chord(
            Key::Comma,
            mods(false, false, false, false)
        ));
        assert!(!is_settings_chord(
            Key::Comma,
            mods(true, false, true, false)
        ));
        assert!(!is_settings_chord(
            Key::Comma,
            mods(true, false, false, true)
        ));
        assert!(!is_settings_chord(
            Key::Period,
            mods(true, false, false, false)
        ));
    }

    #[test]
    fn ctrl_tab_steps_the_tab_either_way() {
        assert_eq!(
            settings_tab_step(Key::Tab, mods(false, true, false, false)),
            Some(1)
        );
        assert_eq!(
            settings_tab_step(Key::Tab, mods(false, true, true, false)),
            Some(-1)
        );
        assert_eq!(
            settings_tab_step(Key::Tab, mods(false, false, false, false)),
            None
        );
    }

    #[test]
    fn command_shift_brackets_step_the_tab_in_either_form() {
        let chord = mods(true, false, true, false);
        assert_eq!(settings_tab_step(Key::CloseBracket, chord), Some(1));
        assert_eq!(settings_tab_step(Key::CloseCurlyBracket, chord), Some(1));
        assert_eq!(settings_tab_step(Key::OpenBracket, chord), Some(-1));
        assert_eq!(settings_tab_step(Key::OpenCurlyBracket, chord), Some(-1));
        let no_shift = mods(true, false, false, false);
        assert_eq!(settings_tab_step(Key::CloseBracket, no_shift), None);
    }

    #[test]
    fn a_wheel_scroll_stays_inside_the_list() {
        assert_eq!(scroll_by(0, -3, 30, 14), 0);
        assert_eq!(scroll_by(10, 3, 30, 14), 13);
        assert_eq!(scroll_by(10, 100, 30, 14), 16);
        // A list that fits never scrolls.
        assert_eq!(scroll_by(0, 5, 10, 14), 0);
    }

    #[test]
    fn a_window_never_runs_past_the_end_of_the_list() {
        assert_eq!(window_start(3, 10, 4), 3);
        assert_eq!(window_start(6, 8, 4), 4);
        assert_eq!(window_start(2, 3, 4), 0);
    }
}
