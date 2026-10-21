//! The settings modal's Keyboard tab (`?`, the header's `?` chip, or a tab
//! step inside Settings): every command in the keymap with its chords,
//! then the keys that are not commands as read-only groups, so the tab is
//! the whole reference. A search field at the top filters the list as you
//! type, on the label and the key label both.
//!
//! The search field and the list take turns at the keyboard
//! ([`KeyboardFocus`]): ↓ or Enter leaves the field for the list, ↑ on the
//! first row or a click on the field goes back. A command's chords are
//! rebound in the list: `Enter` or a click on its chords records the next
//! key press as its only chord, `⌫` unbinds it, `⌘⌫` puts its defaults
//! back, and Reset all at the foot of the tab puts back every
//! one. A chord another command has asks for a second press, and that
//! command loses it. Each change is saved at once (`Display::keymap_changed`).
//!
//! The rows are generated: the commands from [`ACTIONS`] with their chords
//! in the live [`Keymap`], the fixed keys from [`FIXED`]. Drawing, the
//! field and the click hit-test share one layout in
//! `rendering/modals/settings.rs`; the keys are `input/modal.rs`. See
//! `docs/320-keyboard-shortcuts.md` and `010-keybindings.md` § Keyboard Tab.

use egui::Key;

use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::keymap::{ACTIONS, Action, Chord, Keymap, is_modifier, refusal};
use crate::core::settings::update_settings;
use crate::view::computer_keyboard;

use super::browser::scroll_to_show;
use super::modal_focus::SettingsTab;
use super::settings_modal::{scroll_by, window_start};
use super::status_message::StatusMessage;
use super::text_field::TextField;
use super::{Display, Overlay};

/// The Keyboard tab's view-local state. Session-only; a fresh search each
/// time the modal opens (the default).
#[derive(Default)]
pub(super) struct KeyboardTab {
    /// The search field.
    pub(super) search: TextField,
    /// The selected row, counted among the rows the search matches. A
    /// change to the search puts it back at 0; a rebinding can leave it
    /// past the last match, which then selects nothing.
    pub(super) selection: usize,
    /// The first visible line of the list (headers count).
    pub(super) scroll: usize,
    /// The command whose row waits for a key press, while one does.
    pub(super) recording: Option<Recording>,
    /// Whether Reset all was clicked once and waits for the confirming
    /// second click.
    pub(super) reset_all_armed: bool,
    /// Whether the search field or the list has the keyboard.
    pub(super) focus: KeyboardFocus,
}

/// Which part of the Keyboard tab has the keyboard. Only the focused part
/// takes keys: with the list focused nothing can be typed in the search,
/// and the field shows no caret.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) enum KeyboardFocus {
    /// The search field: typing filters the list (the tab opens here).
    #[default]
    Search,
    /// The list: the arrows move the selection, Enter records, `⌫` / `⌘⌫`
    /// unbind / reset.
    List,
}

/// A command's row waiting for its new chord.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Recording {
    /// The command being rebound.
    pub(super) action: Action,
    /// A chord pressed that another command has, and that command: a
    /// second press of the chord confirms, any other key cancels.
    pub(super) replacing: Option<(Chord, Action)>,
}

/// What the Keyboard tab did with a key press or a click.
#[derive(Debug, PartialEq)]
pub(super) enum TabKey {
    /// Not the tab's: the modal's, or the search field's.
    Left,
    /// Taken, the keymap unchanged.
    Taken,
    /// Taken, and the keymap changed: save it.
    Changed,
    /// Taken and refused, saying why — the chord pressed can't be bound.
    Refused(String),
}

impl KeyboardTab {
    /// The tab's keys, with the list showing `visible` lines. While a row
    /// records, every key is the recorder's ([`record`](Self::record)).
    /// Otherwise Esc clears a non-empty search (back in the field) from
    /// either part, Tab and ⇧Tab move to the other part (the next and the
    /// previous stop, wrapping — with two stops, either one), and the rest
    /// depends on the focus:
    ///
    /// - **The search field**: ↓ or Enter moves into the list, when the
    ///   search matches anything; every other key is the field's typing
    ///   (`?` included), or the modal's.
    /// - **The list**: ↑/↓ move the selection through the matching rows,
    ///   scrolling to keep it in view, and ↑ on the first row goes back to
    ///   the field; Enter records the selected command; `⌫` (or Delete)
    ///   unbinds it and `⌘⌫` resets it. Every other key is taken and does
    ///   nothing, so nothing is typed, but Esc and `⌘,` reach the modal.
    pub(super) fn handle_key(
        &mut self,
        key: Key,
        modifiers: KeyModifiers,
        visible: usize,
        keymap: &mut Keymap,
    ) -> TabKey {
        if self.recording.is_some() {
            return self.record(Chord::from_press(key, modifiers), keymap);
        }
        self.reset_all_armed = false;
        if key == Key::Escape {
            return if self.clear_search() {
                TabKey::Taken
            } else {
                TabKey::Left
            };
        }
        if key == Key::Tab {
            match self.focus {
                KeyboardFocus::Search => self.focus_list(keymap),
                KeyboardFocus::List => self.focus = KeyboardFocus::Search,
            }
            return TabKey::Taken;
        }
        if self.focus == KeyboardFocus::Search {
            return match key {
                Key::ArrowDown | Key::Enter => {
                    self.focus_list(keymap);
                    TabKey::Taken
                }
                Key::Questionmark => TabKey::Taken,
                _ => TabKey::Left,
            };
        }
        let step = match key {
            Key::ArrowUp if self.selection == 0 => {
                self.focus = KeyboardFocus::Search;
                return TabKey::Taken;
            }
            Key::ArrowUp => -1,
            Key::ArrowDown => 1,
            Key::Enter => {
                if let Some(action) = self.selected_action(keymap) {
                    self.start_recording(action);
                }
                return TabKey::Taken;
            }
            Key::Backspace | Key::Delete => {
                let Some(action) = self.selected_action(keymap) else {
                    return TabKey::Taken;
                };
                if modifiers.command {
                    keymap.reset(action);
                } else {
                    keymap.unbind(action);
                }
                return TabKey::Changed;
            }
            _ => return TabKey::Taken,
        };
        let lines = shortcut_lines(&self.search.text, keymap);
        let rows = row_lines(&lines);
        let Some(last) = rows.len().checked_sub(1) else {
            return TabKey::Taken;
        };
        self.selection = self.selection.saturating_add_signed(step).min(last);
        let scroll = window_start(self.scroll, lines.len(), visible);
        self.scroll = scroll_to_show_line(scroll, rows[self.selection], &lines, visible);
        TabKey::Taken
    }

    /// A press while a row records, as the chord it is. A modifier key
    /// pressed on its own (egui reports ⌘ going down as a key of its own,
    /// ahead of the key it modifies) changes nothing: the row keeps
    /// waiting for the key, in either state. Waiting for a key: Esc cancels, a refused chord says why and keeps waiting, a chord
    /// another command has asks for a second press, and any other chord
    /// becomes the command's only one. Waiting for that second press: the
    /// same chord binds it, any other key cancels.
    fn record(&mut self, chord: Chord, keymap: &mut Keymap) -> TabKey {
        if is_modifier(chord.key) {
            return TabKey::Taken;
        }
        let Some(recording) = self.recording.take() else {
            return TabKey::Left;
        };
        let action = recording.action;
        if let Some((pending, _)) = recording.replacing {
            if chord == pending {
                keymap.bind(action, chord);
                return TabKey::Changed;
            }
            return TabKey::Taken;
        }
        if chord.key == Key::Escape {
            return TabKey::Taken;
        }
        if let Some(reason) = refusal_for(action, chord) {
            self.recording = Some(recording);
            return TabKey::Refused(format!("{} {reason}", chord_label(chord)));
        }
        match keymap.conflict(action, chord) {
            Some(other) => {
                self.recording = Some(Recording {
                    replacing: Some((chord, other)),
                    ..recording
                });
                TabKey::Taken
            }
            None => {
                keymap.bind(action, chord);
                TabKey::Changed
            }
        }
    }

    /// A clipboard chord while a row records: egui-winit turns `⌘C` / `⌘X`
    /// / `⌘V` into clipboard events, so the key never arrives. Refused
    /// while waiting for a key; a cancel while waiting for a second press,
    /// like any other key.
    pub(super) fn record_clipboard(&mut self) -> TabKey {
        match self.recording {
            Some(Recording {
                replacing: None, ..
            }) => {
                let keys = key_label("⌘C  X  V");
                TabKey::Refused(format!("{keys} are the clipboard"))
            }
            _ => {
                self.recording = None;
                TabKey::Taken
            }
        }
    }

    /// Moves the keyboard into the list, its selection kept on a row that
    /// matches; stays in the search field when nothing matches.
    fn focus_list(&mut self, keymap: &Keymap) {
        let rows = row_lines(&shortcut_lines(&self.search.text, keymap)).len();
        if let Some(last) = rows.checked_sub(1) {
            self.selection = self.selection.min(last);
            self.focus = KeyboardFocus::List;
        }
    }

    /// Starts recording a new chord for `action`.
    fn start_recording(&mut self, action: Action) {
        self.recording = Some(Recording {
            action,
            replacing: None,
        });
    }

    /// A click on row `row` (counted among the rows the search matches):
    /// ends any recording and disarms Reset all, focuses the list on the
    /// row, and on
    /// its chords (`on_keys`) records a new one for its command.
    pub(super) fn click_row(&mut self, row: usize, on_keys: bool, keymap: &Keymap) {
        self.recording = None;
        self.reset_all_armed = false;
        self.focus = KeyboardFocus::List;
        self.selection = row;
        if on_keys && let Some(action) = self.selected_action(keymap) {
            self.start_recording(action);
        }
    }

    /// A click on Reset all: the first arms it, the second puts every
    /// command back on its defaults.
    pub(super) fn click_reset_all(&mut self, keymap: &mut Keymap) -> TabKey {
        self.recording = None;
        if !self.reset_all_armed {
            self.reset_all_armed = true;
            return TabKey::Taken;
        }
        self.reset_all_armed = false;
        keymap.reset_all();
        TabKey::Changed
    }

    /// A click anywhere else: ends any recording and disarms Reset all.
    pub(super) fn click_away(&mut self) {
        self.recording = None;
        self.reset_all_armed = false;
    }

    /// A click on the search field: it takes the keyboard back.
    pub(super) fn click_search(&mut self) {
        self.click_away();
        self.focus = KeyboardFocus::Search;
    }

    /// The command on the selected row; `None` on a fixed key's row, or
    /// with nothing selected.
    fn selected_action(&self, keymap: &Keymap) -> Option<Action> {
        let lines = shortcut_lines(&self.search.text, keymap);
        let line = *row_lines(&lines).get(self.selection)?;
        match &lines[line] {
            ShortcutLine::Row(row) => row.action,
            ShortcutLine::Header(_) => None,
        }
    }

    /// Clears a non-empty search, back to the tab as it opens; returns
    /// whether there was one to clear.
    fn clear_search(&mut self) -> bool {
        if self.search.text.is_empty() {
            return false;
        }
        *self = KeyboardTab::default();
        true
    }

    /// The selection and the scroll back at the top — when the search
    /// changes what the list holds.
    pub(super) fn reset_list(&mut self) {
        self.selection = 0;
        self.scroll = 0;
    }

    /// A wheel scroll: `rows` lines down (up when negative), kept inside a
    /// list showing `visible` lines.
    pub(super) fn scroll(&mut self, rows: isize, visible: usize, keymap: &Keymap) {
        let len = shortcut_lines(&self.search.text, keymap).len();
        self.scroll = scroll_by(self.scroll, rows, len, visible);
    }
}

/// Why `chord` can't be `action`'s, as [`refusal`] words it — and the
/// computer keyboard's toggle can't be a key the keyboard takes while it
/// plays, or nothing could turn it off.
fn refusal_for(action: Action, chord: Chord) -> Option<&'static str> {
    refusal(chord).or_else(|| {
        (action == Action::ToggleComputerKeyboard && computer_keyboard::shadows(chord))
            .then_some("plays a note: the keyboard could not be turned off")
    })
}

/// One row of the tab: a label and the keys that do it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ShortcutRow {
    /// The group the row is listed under.
    pub(super) category: &'static str,
    /// What the keys do.
    pub(super) label: &'static str,
    /// The keys, spelled out for this platform ([`key_label`]); empty for
    /// an unbound command.
    pub(super) keys: String,
    /// The command, or `None` for a key that is not one: listed, never
    /// rebindable.
    pub(super) action: Option<Action>,
    /// A command whose chords differ from its defaults.
    pub(super) changed: bool,
    /// A command with a chord the computer keyboard takes while it plays
    /// ([`computer_keyboard::shadows`]).
    pub(super) piano: bool,
}

impl ShortcutRow {
    /// A command with no chord.
    pub(super) fn is_unbound(&self) -> bool {
        self.action.is_some() && self.keys.is_empty()
    }
}

/// One line of the list: a group's heading, or a row under it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ShortcutLine {
    /// A group heading, drawn uppercase.
    Header(&'static str),
    /// A row.
    Row(ShortcutRow),
}

/// One read-only group of [`FIXED`]: its heading and `(keys, label)` rows,
/// the keys written with macOS's `⌘` `⌥` `⇧` like a chord label.
type FixedGroup = (&'static str, &'static [(&'static str, &'static str)]);

/// The keys that are not commands, listed after them, read-only: the
/// project chords and the ways into Settings, the arrow grammar, the
/// context keys, the clipboard, the mouse, the browser and the piano
/// layout. Hand-written: these are matched by hand where they are handled
/// (`docs/320-keyboard-shortcuts.md` decision 1), so a change there updates
/// its row here.
const FIXED: &[FixedGroup] = &[
    (
        "Files & settings",
        &[
            ("⌘S", "Save"),
            ("⌘⇧S", "Save as"),
            ("⌘N", "New project"),
            ("⌘,", "Settings"),
            ("?", "Keyboard shortcuts"),
        ],
    ),
    (
        "This tab",
        &[
            ("Tab  ⇧Tab", "Between the search and the list"),
            ("↓  Enter", "From the search into the list"),
            ("↑", "Back to the search, from the first row"),
            ("Enter", "Record a new key for the command"),
            ("Backspace", "Unbind the command"),
            ("⌘Backspace", "Reset the command to its default"),
        ],
    ),
    (
        "Navigation",
        &[
            ("←  →", "Move the cursor"),
            ("↑  ↓", "Select the track"),
            ("⇧ arrows", "Extend the selection"),
            ("⌥←  →", "Jump to the next clip edge"),
            ("⇧⌥←  →", "Extend to the next clip edge"),
            ("⌘←  →", "Move the selected block"),
            ("←  →", "Clip: cursor, or nudge notes"),
            ("⌘←  →", "Clip: nudge notes finely"),
            ("⇧←  →", "Clip: shorten / lengthen notes"),
            ("↑  ↓", "Clip: transpose (Shift: an octave)"),
            ("Tab", "Arranger / clip view focus"),
            ("⇧Tab", "Clip panel on / off"),
        ],
    ),
    (
        "Enter & Esc",
        &[
            ("Enter", "Fit the tempo to the only clip"),
            ("Esc", "Deselect notes"),
            ("Esc", "Cancel the export"),
            ("Enter", "Framing a take: insert it"),
            ("Esc", "Framing a take: cancel"),
        ],
    ),
    (
        "Clipboard",
        &[
            ("⌘C  X  V", "Copy / cut / paste"),
            ("⌘⇧C", "Copy across every track"),
        ],
    ),
    (
        "Mouse",
        &[
            ("Double-click", "Clip: insert a note"),
            ("Drag", "Clip: move, or resize by an edge"),
            ("⌘ drag", "Clip: change the velocity"),
            ("Band drag", "Clip: move the clip's start / end"),
            ("Legend drag", "Clip: zoom / scroll the note rows"),
        ],
    ),
    (
        "Browser",
        &[
            ("Enter", "Open the project, import the clip"),
            ("⌘R", "Rename the file"),
        ],
    ),
    (
        "Computer keyboard",
        &[
            ("A  S  D  F  G  H  J  K  L  ;  '", "White keys from C"),
            ("W  E  T  Y  U  O  P", "Black keys"),
            ("Z  X", "Octave down / up"),
            ("C  V", "Velocity down / up"),
        ],
    ),
];

/// `?` with no ⌘/Ctrl or ⌥ — opens Settings on the Keyboard tab. Fixed, so
/// the tab is always reachable. ⇧ is ignored: it is part of typing `?` on
/// most layouts, and egui-winit reports the logical key, so this is `⇧/` on
/// US layouts and `⇧+` on Nordic ones.
pub(super) fn is_keyboard_tab_key(key: Key, modifiers: KeyModifiers) -> bool {
    key == Key::Questionmark && !modifiers.command && !modifiers.alt
}

/// `keys` as shown on this platform ([`key_label_for`]).
fn key_label(keys: &str) -> String {
    key_label_for(keys, cfg!(target_os = "macos"))
}

/// `keys` with its modifier glyphs spelled out: `⌘` as `Cmd+` on macOS and
/// `Ctrl+` elsewhere, `⌥` as `Opt+` / `Alt+`, `⇧` as `Shift+`, `⌃` (macOS's
/// Control) as `Ctrl+`. Spelled out on macOS too: the bundled fonts have no
/// `⌥`, and a mix of glyphs and words reads worse than words.
fn key_label_for(keys: &str, mac: bool) -> String {
    let mut label = String::with_capacity(keys.len() + 8);
    let mut chars = keys.chars().peekable();
    while let Some(c) = chars.next() {
        let name = match c {
            '⌘' if mac => "Cmd",
            '⌘' | '⌃' => "Ctrl",
            '⌥' if mac => "Opt",
            '⌥' => "Alt",
            '⇧' => "Shift",
            c => {
                label.push(c);
                continue;
            }
        };
        label.push_str(name);
        // A modifier standing alone ("⇧ arrows", "⌘ drag") keeps its space
        // rather than a dangling `+`.
        if chars.peek() != Some(&' ') {
            label.push('+');
        }
    }
    label
}

/// `chord` as shown on this platform: `Cmd+Shift+Z`, `\`.
pub(super) fn chord_label(chord: Chord) -> String {
    key_label(&chord_glyphs(chord))
}

/// `chord` written as [`FIXED`]'s keys are: the modifier glyphs, then the
/// key's symbol or name (`\`, `Space`, `Delete`).
fn chord_glyphs(chord: Chord) -> String {
    let mut glyphs = String::new();
    for (held, glyph) in [
        (chord.command, '⌘'),
        (chord.ctrl, '⌃'),
        (chord.alt, '⌥'),
        (chord.shift, '⇧'),
    ] {
        if held {
            glyphs.push(glyph);
        }
    }
    glyphs.push_str(match chord.key {
        // egui's symbol is U+2212, a minus sign; the key prints a hyphen.
        Key::Minus => "-",
        key => key.symbol_or_name(),
    });
    glyphs
}

/// Every row, in the tab's order: each command on its chords in `keymap`,
/// then the fixed keys.
fn shortcut_rows(keymap: &Keymap) -> Vec<ShortcutRow> {
    let commands = ACTIONS.iter().map(|info| {
        let chords = keymap.chords(info.action);
        ShortcutRow {
            category: info.category,
            label: info.label,
            keys: chords
                .iter()
                .map(|&chord| chord_label(chord))
                .collect::<Vec<_>>()
                .join("  "),
            action: Some(info.action),
            changed: !keymap.is_default(info.action),
            piano: chords
                .iter()
                .any(|&chord| computer_keyboard::shadows(chord)),
        }
    });
    let fixed = FIXED.iter().flat_map(|&(category, rows)| {
        rows.iter().map(move |&(keys, label)| ShortcutRow {
            category,
            label,
            keys: key_label(keys),
            action: None,
            changed: false,
            piano: false,
        })
    });
    commands.chain(fixed).collect()
}

/// What an unbound command's row reads in place of its keys.
pub(super) const UNBOUND: &str = "unbound";

/// Whether `row` matches `query`, already trimmed and lowercased: a
/// substring of its label or of its keys, case-blind (so "cmd+l" finds Set
/// loop, and so does "loop"). An unbound command's keys read
/// [`UNBOUND`], so the search finds those too.
fn row_matches(row: &ShortcutRow, query: &str) -> bool {
    let keys = if row.is_unbound() { UNBOUND } else { &row.keys };
    query.is_empty()
        || row.label.to_lowercase().contains(query)
        || keys.to_lowercase().contains(query)
}

/// The list the tab shows for the search `query` over `keymap`: the
/// matching rows, each group's heading over its first match. Empty when
/// nothing matches.
pub(super) fn shortcut_lines(query: &str, keymap: &Keymap) -> Vec<ShortcutLine> {
    let query = query.trim().to_lowercase();
    let mut lines = Vec::new();
    let mut category = None;
    for row in shortcut_rows(keymap)
        .into_iter()
        .filter(|row| row_matches(row, &query))
    {
        if category != Some(row.category) {
            category = Some(row.category);
            lines.push(ShortcutLine::Header(row.category));
        }
        lines.push(ShortcutLine::Row(row));
    }
    lines
}

/// The line each row of `lines` is on, in order — row `i` is on line
/// `row_lines(lines)[i]`, and a line is a row where it is found here.
pub(super) fn row_lines(lines: &[ShortcutLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| matches!(line, ShortcutLine::Row(_)).then_some(i))
        .collect()
}

/// `scroll` moved just enough that a `visible`-line window shows the row on
/// `line` — and the heading over it, when it is its group's first row.
fn scroll_to_show_line(
    scroll: usize,
    line: usize,
    lines: &[ShortcutLine],
    visible: usize,
) -> usize {
    let scroll = scroll_to_show(scroll, line, visible);
    let under_header = line > 0 && matches!(lines[line - 1], ShortcutLine::Header(_));
    if under_header && scroll == line {
        line - 1
    } else {
        scroll
    }
}

impl Display {
    /// Opens Settings on the Keyboard tab (`?`, the header's `?` chip).
    pub(super) fn open_keyboard_tab(&mut self) {
        self.open_settings();
        self.settings.tab = SettingsTab::Keyboard;
    }

    /// Whether the settings modal is up on its Keyboard tab.
    pub(super) fn keyboard_tab_showing(&self) -> bool {
        self.overlay == Some(Overlay::Settings) && self.settings.tab == SettingsTab::Keyboard
    }

    /// Whether a Keyboard tab row is waiting for a key press — which then
    /// goes to it ahead of every other key handler, the fixed project chords
    /// and the modal's own included.
    pub(super) fn recording_shortcut(&self) -> bool {
        self.keyboard_tab_showing() && self.settings.keys.recording.is_some()
    }

    /// A key press on the Keyboard tab; returns whether the tab took it.
    pub(super) fn handle_keyboard_tab_key(&mut self, key: Key, modifiers: KeyModifiers) -> bool {
        let visible = self.keyboard_visible_lines();
        let result = self
            .settings
            .keys
            .handle_key(key, modifiers, visible, &mut self.keymap);
        self.apply_tab_key(result)
    }

    /// Acts on what the Keyboard tab did: a changed keymap is saved and
    /// handed to the plugin-editor key guard; a refusal shows in the
    /// footer. Returns whether the tab took the event.
    pub(super) fn apply_tab_key(&mut self, result: TabKey) -> bool {
        match result {
            TabKey::Left => return false,
            TabKey::Taken => {}
            TabKey::Changed => self.keymap_changed(),
            TabKey::Refused(message) => self.render.status = Some(StatusMessage::new(message)),
        }
        true
    }

    /// After a rebinding: saves the user's changes to `settings.json` (only
    /// the differences from the defaults, `Keymap::overrides`) and, on
    /// macOS, moves the plugin-editor key guard to the new bare keys.
    fn keymap_changed(&mut self) {
        let overrides = self.keymap.overrides();
        update_settings("keyboard shortcuts", |settings| {
            settings.keymap = overrides;
        });
        #[cfg(target_os = "macos")]
        self.guard_plugin_editor_keys();
    }

    /// A clipboard event while the settings modal is up: refused, or a
    /// cancel, while a row records (`KeyboardTab::record_clipboard`);
    /// nothing otherwise.
    pub(super) fn settings_clipboard_event(&mut self, event: &InputEvent) {
        if self.recording_shortcut()
            && matches!(
                event,
                InputEvent::Copy { .. } | InputEvent::Cut { .. } | InputEvent::Paste
            )
        {
            let result = self.settings.keys.record_clipboard();
            self.apply_tab_key(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::Key;

    use super::{
        FIXED, KeyboardFocus, KeyboardTab, Recording, ShortcutLine, TabKey, TextField,
        chord_glyphs, is_keyboard_tab_key, key_label_for, row_lines, scroll_to_show_line,
        shortcut_lines, shortcut_rows,
    };
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::{ACTIONS, Action, Chord, Keymap};

    /// No modifier held.
    const NONE: KeyModifiers = KeyModifiers {
        shift: false,
        command: false,
        alt: false,
        ctrl: false,
    };

    /// ⌘/Ctrl held.
    const CMD: KeyModifiers = KeyModifiers {
        command: true,
        ..NONE
    };

    /// The default keymap's lines for `query`.
    fn lines(query: &str) -> Vec<ShortcutLine> {
        shortcut_lines(query, &Keymap::default())
    }

    /// A tab searching `query`, the list focused with its selection on
    /// `label`'s row.
    fn tab_on(label: &str, query: &str, keymap: &Keymap) -> KeyboardTab {
        let lines = shortcut_lines(query, keymap);
        let selection = row_lines(&lines)
            .iter()
            .position(|&line| matches!(&lines[line], ShortcutLine::Row(row) if row.label == label))
            .unwrap();
        KeyboardTab {
            search: TextField::new(query.into()),
            selection,
            focus: KeyboardFocus::List,
            ..KeyboardTab::default()
        }
    }

    /// `tab` handling a press of `key` with `modifiers`, a ten-line window.
    fn press(
        tab: &mut KeyboardTab,
        key: Key,
        modifiers: KeyModifiers,
        keymap: &mut Keymap,
    ) -> TabKey {
        tab.handle_key(key, modifiers, 10, keymap)
    }

    /// The row labels `lines` shows, in order.
    fn labels(lines: &[ShortcutLine]) -> Vec<&'static str> {
        lines
            .iter()
            .filter_map(|line| match line {
                ShortcutLine::Row(row) => Some(row.label),
                ShortcutLine::Header(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_question_mark_opens_the_tab_with_or_without_shift() {
        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        assert!(is_keyboard_tab_key(
            Key::Questionmark,
            KeyModifiers::default()
        ));
        assert!(is_keyboard_tab_key(Key::Questionmark, shift));
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        assert!(!is_keyboard_tab_key(Key::Questionmark, command));
        assert!(!is_keyboard_tab_key(Key::Questionmark, alt));
        assert!(!is_keyboard_tab_key(Key::Slash, shift));
    }

    #[test]
    fn modifier_glyphs_are_spelled_out_per_platform() {
        assert_eq!(key_label_for("⌘⇧S", true), "Cmd+Shift+S");
        assert_eq!(key_label_for("⌘⇧S", false), "Ctrl+Shift+S");
        assert_eq!(key_label_for("⇧⌥←  →", true), "Shift+Opt+←  →");
        assert_eq!(key_label_for("⌥Space", false), "Alt+Space");
        assert_eq!(key_label_for("⌃M", true), "Ctrl+M");
        assert_eq!(key_label_for("⇧ arrows", false), "Shift arrows");
        assert_eq!(key_label_for("⌘ drag", true), "Cmd drag");
        assert_eq!(key_label_for("Q", true), "Q");
    }

    #[test]
    fn a_chord_is_written_with_its_modifiers_then_its_key() {
        let redo = Chord {
            key: Key::Z,
            command: true,
            alt: false,
            shift: true,
            ctrl: false,
        };
        assert_eq!(key_label_for(&chord_glyphs(redo), true), "Cmd+Shift+Z");
        let stretch = Chord {
            key: Key::Minus,
            command: false,
            alt: true,
            shift: false,
            ctrl: false,
        };
        assert_eq!(key_label_for(&chord_glyphs(stretch), false), "Alt+-");
        let commit = Chord {
            key: Key::Backslash,
            command: false,
            alt: false,
            shift: false,
            ctrl: false,
        };
        assert_eq!(chord_glyphs(commit), "\\");
        let stop = Chord {
            key: Key::Num0,
            ..commit
        };
        assert_eq!(chord_glyphs(stop), "0");
    }

    #[test]
    fn every_command_is_listed_once_with_its_keys() {
        let rows = shortcut_rows(&Keymap::default());
        for info in ACTIONS {
            let listed: Vec<_> = rows
                .iter()
                .filter(|row| row.action == Some(info.action))
                .collect();
            assert_eq!(listed.len(), 1, "{}", info.label);
            assert!(!listed[0].keys.is_empty(), "{}", info.label);
        }
        // Several defaults are listed together: `\` and `/` for Commit.
        let commit = rows
            .iter()
            .find(|row| row.label == "Commit the take to a clip")
            .unwrap();
        assert_eq!(commit.keys, "\\  /");
    }

    #[test]
    fn every_row_has_keys_and_a_label_and_no_unfontable_glyph() {
        for row in shortcut_rows(&Keymap::default()) {
            assert!(!row.keys.trim().is_empty() && !row.label.trim().is_empty());
            assert!(
                !row.keys.contains(['⌘', '⌥', '⇧', '⌃', '⌫', '−']),
                "{} has a glyph the fonts lack",
                row.keys
            );
        }
    }

    #[test]
    fn the_fixed_groups_are_not_command_groups() {
        let commands: HashSet<_> = ACTIONS.iter().map(|info| info.category).collect();
        for &(category, rows) in FIXED {
            assert!(!commands.contains(category), "{category}");
            assert!(!rows.is_empty(), "{category}");
        }
    }

    #[test]
    fn no_group_lists_the_same_row_twice() {
        let mut seen = HashSet::new();
        for row in shortcut_rows(&Keymap::default()) {
            assert!(
                seen.insert((row.category, row.label, row.keys.clone())),
                "{} twice in {}",
                row.label,
                row.category
            );
        }
    }

    #[test]
    fn an_empty_search_lists_everything_under_its_headings() {
        let lines = lines("");
        assert_eq!(
            row_lines(&lines).len(),
            shortcut_rows(&Keymap::default()).len()
        );
        assert_eq!(
            lines[0],
            ShortcutLine::Header(ACTIONS[0].category),
            "the first command's group heads the list"
        );
        // Each heading shows once.
        let headers: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                ShortcutLine::Header(category) => Some(*category),
                ShortcutLine::Row(_) => None,
            })
            .collect();
        let unique: HashSet<_> = headers.iter().collect();
        assert_eq!(headers.len(), unique.len());
    }

    #[test]
    fn the_search_matches_the_label_or_the_keys_case_blind() {
        #[cfg(target_os = "macos")]
        let by_keys = "cmd+l";
        #[cfg(not(target_os = "macos"))]
        let by_keys = "ctrl+l";
        for query in ["loop", "LOOP", by_keys] {
            let lines = lines(query);
            assert!(
                labels(&lines).contains(&"Set the loop, or loop on / off"),
                "{query}"
            );
            // Only the groups with a match keep their heading.
            assert!(matches!(lines[0], ShortcutLine::Header(_)));
        }
        assert!(lines("no such shortcut").is_empty());
    }

    #[test]
    fn row_lines_skip_the_headings() {
        let lines = lines("");
        let rows = row_lines(&lines);
        assert_eq!(rows[0], 1, "the first heading is no row");
        for &line in &rows {
            assert!(matches!(lines[line], ShortcutLine::Row(_)));
        }
    }

    #[test]
    fn the_arrows_move_the_selection_and_stop_at_the_ends() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab {
            focus: KeyboardFocus::List,
            ..KeyboardTab::default()
        };
        assert_eq!(press(&mut tab, Key::ArrowDown, NONE, keymap), TabKey::Taken);
        assert_eq!(tab.selection, 1);
        let count = row_lines(&lines("")).len();
        for _ in 0..count + 5 {
            press(&mut tab, Key::ArrowDown, NONE, keymap);
        }
        assert_eq!(tab.selection, count - 1);
        // The last row is in the window.
        let lines = lines("");
        assert!(tab.scroll + 10 > row_lines(&lines)[count - 1]);
        // Nothing matched: the arrows are still the tab's, and do nothing.
        tab.search = TextField::new("no such shortcut".into());
        tab.reset_list();
        assert_eq!(press(&mut tab, Key::ArrowDown, NONE, keymap), TabKey::Taken);
        assert_eq!(tab.selection, 0);
    }

    #[test]
    fn escape_clears_a_search_before_it_closes_the_modal() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab {
            search: TextField::new("loop".into()),
            selection: 1,
            scroll: 2,
            ..KeyboardTab::default()
        };
        assert_eq!(press(&mut tab, Key::Escape, NONE, keymap), TabKey::Taken);
        assert!(tab.search.text.is_empty());
        assert_eq!((tab.selection, tab.scroll), (0, 0));
        // No search left: Esc is the modal's.
        assert_eq!(press(&mut tab, Key::Escape, NONE, keymap), TabKey::Left);
    }

    #[test]
    fn a_question_mark_is_typing_and_other_keys_are_the_modals() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab::default();
        assert_eq!(
            press(&mut tab, Key::Questionmark, NONE, keymap),
            TabKey::Taken
        );
        assert_eq!(press(&mut tab, Key::A, NONE, keymap), TabKey::Left);
        assert_eq!(press(&mut tab, Key::A, CMD, keymap), TabKey::Left);
    }

    #[test]
    fn scrolling_to_a_groups_first_row_shows_its_heading_too() {
        let lines = lines("");
        let second_header = lines
            .iter()
            .skip(1)
            .position(|line| matches!(line, ShortcutLine::Header(_)))
            .unwrap()
            + 1;
        let first_row = second_header + 1;
        // Scrolled past it, moving back up to it brings the heading along.
        assert_eq!(
            scroll_to_show_line(first_row + 3, first_row, &lines, 10),
            second_header
        );
        // Scrolling down to a row inside a group shows just enough.
        assert_eq!(scroll_to_show_line(0, 12, &lines, 10), 3);
        // A visible row leaves the scroll alone.
        assert_eq!(scroll_to_show_line(2, 5, &lines, 10), 2);
    }

    #[test]
    fn enter_records_the_next_press_as_the_only_chord() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Tap tempo", "", keymap);
        assert_eq!(press(&mut tab, Key::Enter, NONE, keymap), TabKey::Taken);
        assert_eq!(
            tab.recording,
            Some(Recording {
                action: Action::TapTempo,
                replacing: None
            })
        );
        assert_eq!(press(&mut tab, Key::G, CMD, keymap), TabKey::Changed);
        assert_eq!(tab.recording, None);
        assert_eq!(
            keymap.chords(Action::TapTempo),
            &[Chord::from_press(Key::G, CMD)]
        );
    }

    #[test]
    fn escape_cancels_a_recording() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Tap tempo", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        assert_eq!(press(&mut tab, Key::Escape, NONE, keymap), TabKey::Taken);
        assert_eq!(tab.recording, None);
        assert!(keymap.is_all_default());
    }

    #[test]
    fn a_refused_chord_says_why_and_keeps_waiting() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Tap tempo", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        let TabKey::Refused(message) = press(&mut tab, Key::S, CMD, keymap) else {
            panic!("⌘S is fixed");
        };
        assert!(message.ends_with("is fixed: save, save as and new project"));
        assert!(tab.recording.is_some());
        assert!(matches!(tab.record_clipboard(), TabKey::Refused(_)));
        assert!(tab.recording.is_some());
        assert!(keymap.is_all_default());
    }

    /// The computer keyboard's toggle on a key the keyboard plays could
    /// never be turned off again.
    #[test]
    fn the_piano_toggle_refuses_a_piano_key() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Play notes on the keyboard", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        assert!(matches!(
            press(&mut tab, Key::A, NONE, keymap),
            TabKey::Refused(_)
        ));
        assert_eq!(press(&mut tab, Key::F5, NONE, keymap), TabKey::Changed);
    }

    #[test]
    fn a_conflict_asks_for_a_second_press() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Merge clips", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        assert_eq!(press(&mut tab, Key::E, CMD, keymap), TabKey::Taken);
        assert_eq!(
            tab.recording.and_then(|recording| recording.replacing),
            Some((Chord::from_press(Key::E, CMD), Action::Split))
        );
        assert!(keymap.is_all_default(), "nothing moves before the confirm");
        assert_eq!(press(&mut tab, Key::E, CMD, keymap), TabKey::Changed);
        assert!(keymap.chords(Action::Split).is_empty());
        // The loser reads unbound.
        let rows = shortcut_rows(keymap);
        let split = rows
            .iter()
            .find(|row| row.action == Some(Action::Split))
            .unwrap();
        assert!(split.is_unbound() && split.changed);
    }

    #[test]
    fn any_other_key_cancels_the_confirm() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Merge clips", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        press(&mut tab, Key::E, CMD, keymap);
        assert_eq!(press(&mut tab, Key::G, CMD, keymap), TabKey::Taken);
        assert_eq!(tab.recording, None);
        assert!(keymap.is_all_default());
    }

    #[test]
    fn backspace_unbinds_and_command_backspace_resets() {
        let keymap = &mut Keymap::default();
        // The list has the keyboard: the keys are the row's, not the search's.
        let mut tab = tab_on("Undo", "undo", keymap);
        assert_eq!(
            press(&mut tab, Key::Backspace, NONE, keymap),
            TabKey::Changed
        );
        assert!(keymap.chords(Action::Undo).is_empty());
        assert_eq!(tab.search.text, "undo");
        assert_eq!(
            press(&mut tab, Key::Backspace, CMD, keymap),
            TabKey::Changed
        );
        assert!(keymap.is_all_default());
        // A fixed key's row has nothing to unbind.
        let mut tab = tab_on("Save", "", keymap);
        assert_eq!(press(&mut tab, Key::Backspace, NONE, keymap), TabKey::Taken);
        assert!(keymap.is_all_default());
    }

    #[test]
    fn a_click_on_the_keys_records_and_anywhere_else_cancels() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab::default();
        tab.click_row(0, false, keymap);
        assert_eq!(tab.recording, None);
        tab.click_row(0, true, keymap);
        assert_eq!(
            tab.recording.map(|recording| recording.action),
            Some(ACTIONS[0].action)
        );
        tab.click_away();
        assert_eq!(tab.recording, None);
    }

    #[test]
    fn reset_all_takes_a_second_click() {
        let keymap = &mut Keymap::default();
        keymap.unbind(Action::Undo);
        let mut tab = KeyboardTab::default();
        assert_eq!(tab.click_reset_all(keymap), TabKey::Taken);
        assert!(!keymap.is_all_default());
        // A key in between disarms it.
        press(&mut tab, Key::ArrowDown, NONE, keymap);
        assert_eq!(tab.click_reset_all(keymap), TabKey::Taken);
        assert_eq!(tab.click_reset_all(keymap), TabKey::Changed);
        assert!(keymap.is_all_default());
    }

    #[test]
    fn the_rows_show_the_live_keymap() {
        let mut keymap = Keymap::default();
        keymap.bind(Action::Quantize, Chord::from_press(Key::G, CMD));
        let rows = shortcut_rows(&keymap);
        let quantize = rows
            .iter()
            .find(|row| row.action == Some(Action::Quantize))
            .unwrap();
        assert!(quantize.changed && !quantize.is_unbound());
        assert!(quantize.keys.ends_with("+G"));
        // Searching "unbound" finds the unbound commands.
        keymap.unbind(Action::Undo);
        let found = shortcut_lines("unbound", &keymap);
        assert!(labels(&found).contains(&"Undo"));
    }

    #[test]
    fn a_piano_layout_chord_is_noted() {
        let rows = shortcut_rows(&Keymap::default());
        let piano = |action| {
            rows.iter()
                .find(|row| row.action == Some(action))
                .unwrap()
                .piano
        };
        assert!(piano(Action::TapTempo));
        assert!(!piano(Action::Quantize));
        assert!(!piano(Action::Undo));
    }

    #[test]
    fn the_tab_opens_in_the_search_field() {
        assert_eq!(KeyboardTab::default().focus, KeyboardFocus::Search);
    }

    /// The field types: ⌫ and the letters are left to it, and Enter on no
    /// row records nothing.
    #[test]
    fn the_search_field_keeps_its_keys() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Undo", "undo", keymap);
        tab.focus = KeyboardFocus::Search;
        assert_eq!(press(&mut tab, Key::Backspace, NONE, keymap), TabKey::Left);
        assert_eq!(press(&mut tab, Key::A, NONE, keymap), TabKey::Left);
        assert!(keymap.is_all_default());
        assert_eq!(tab.recording, None);
    }

    #[test]
    fn down_or_enter_leaves_the_field_and_up_on_the_first_row_returns() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab::default();
        assert_eq!(press(&mut tab, Key::ArrowDown, NONE, keymap), TabKey::Taken);
        assert_eq!((tab.focus, tab.selection), (KeyboardFocus::List, 0));
        press(&mut tab, Key::ArrowDown, NONE, keymap);
        press(&mut tab, Key::ArrowUp, NONE, keymap);
        assert_eq!((tab.focus, tab.selection), (KeyboardFocus::List, 0));
        press(&mut tab, Key::ArrowUp, NONE, keymap);
        assert_eq!(tab.focus, KeyboardFocus::Search);
        assert_eq!(press(&mut tab, Key::Enter, NONE, keymap), TabKey::Taken);
        assert_eq!(tab.focus, KeyboardFocus::List);
        assert_eq!(tab.recording, None, "Enter from the field only moves");
    }

    /// With nothing matched there is no list to move into.
    #[test]
    fn an_empty_list_keeps_the_field_focused() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab {
            search: TextField::new("no such shortcut".into()),
            ..KeyboardTab::default()
        };
        press(&mut tab, Key::ArrowDown, NONE, keymap);
        assert_eq!(tab.focus, KeyboardFocus::Search);
    }

    /// The list takes every other key, so nothing reaches the field; Esc
    /// still clears a search, back in the field.
    #[test]
    fn the_list_swallows_typing_and_escape_returns_to_the_field() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Undo", "undo", keymap);
        assert_eq!(press(&mut tab, Key::A, NONE, keymap), TabKey::Taken);
        assert_eq!(press(&mut tab, Key::Escape, NONE, keymap), TabKey::Taken);
        assert_eq!(tab.focus, KeyboardFocus::Search);
        assert!(tab.search.text.is_empty());
    }

    #[test]
    fn clicks_move_the_focus() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab::default();
        tab.click_row(2, false, keymap);
        assert_eq!((tab.focus, tab.selection), (KeyboardFocus::List, 2));
        tab.click_search();
        assert_eq!(tab.focus, KeyboardFocus::Search);
    }

    /// Tab and ⇧Tab both reach the other part: two stops, wrapping.
    #[test]
    fn tab_and_shift_tab_switch_between_the_field_and_the_list() {
        let keymap = &mut Keymap::default();
        let shift = KeyModifiers {
            shift: true,
            ..NONE
        };
        let mut tab = KeyboardTab::default();
        for modifiers in [NONE, shift] {
            assert_eq!(press(&mut tab, Key::Tab, modifiers, keymap), TabKey::Taken);
            assert_eq!(tab.focus, KeyboardFocus::List);
            assert_eq!(press(&mut tab, Key::Tab, modifiers, keymap), TabKey::Taken);
            assert_eq!(tab.focus, KeyboardFocus::Search);
        }
        // Nothing matched: no list to go to.
        tab.search = TextField::new("no such shortcut".into());
        press(&mut tab, Key::Tab, NONE, keymap);
        assert_eq!(tab.focus, KeyboardFocus::Search);
    }

    /// A selection left past the matches by a rebinding lands on a row.
    #[test]
    fn moving_into_the_list_keeps_the_selection_on_a_row() {
        let keymap = &mut Keymap::default();
        let mut tab = KeyboardTab {
            search: TextField::new("undo".into()),
            selection: 40,
            ..KeyboardTab::default()
        };
        press(&mut tab, Key::Tab, NONE, keymap);
        let rows = row_lines(&shortcut_lines("undo", keymap)).len();
        assert_eq!(tab.selection, rows - 1);
    }

    /// Regression: holding ⌘ arrives as `SuperRight` before the key, and
    /// was recorded as the chord. The row waits past it for `⌘P`.
    #[test]
    fn a_lone_modifier_press_keeps_the_row_waiting() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Play notes on the keyboard", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        assert_eq!(
            press(&mut tab, Key::SuperRight, NONE, keymap),
            TabKey::Taken
        );
        assert!(tab.recording.is_some());
        assert_eq!(press(&mut tab, Key::P, CMD, keymap), TabKey::Changed);
        assert_eq!(
            keymap.chords(Action::ToggleComputerKeyboard),
            &[Chord::from_press(Key::P, CMD)]
        );
    }

    /// Holding ⌘ again for the confirming second press doesn't cancel it.
    #[test]
    fn a_lone_modifier_press_keeps_the_confirm_waiting() {
        let keymap = &mut Keymap::default();
        let mut tab = tab_on("Merge clips", "", keymap);
        press(&mut tab, Key::Enter, NONE, keymap);
        press(&mut tab, Key::SuperLeft, NONE, keymap);
        press(&mut tab, Key::E, CMD, keymap);
        press(&mut tab, Key::SuperLeft, NONE, keymap);
        assert!(
            tab.recording
                .is_some_and(|recording| recording.replacing.is_some())
        );
        assert_eq!(press(&mut tab, Key::E, CMD, keymap), TabKey::Changed);
        assert_eq!(
            keymap.chords(Action::Merge),
            &[Chord::from_press(Key::E, CMD)]
        );
    }
}
