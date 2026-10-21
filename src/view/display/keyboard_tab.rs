//! The settings modal's Keyboard tab (`?`, the header's `?` chip, or a tab
//! step inside Settings): every command in the keymap with its chords,
//! then the keys that are not commands as read-only groups, so the tab is
//! the whole reference. A search field at the top filters the list as you
//! type, on the label and the key label both. Read-only until `320` phase 3
//! adds the recorder.
//!
//! The rows are generated: the commands from [`ACTIONS`], the fixed keys
//! from [`FIXED`]. Drawing, the field and the click hit-test share one
//! layout in `rendering/modals/settings.rs`; the keys are `input/modal.rs`.
//! See `docs/320-keyboard-shortcuts.md` and `010-keybindings.md`
//! § Keyboard Tab.

use egui::Key;

use crate::core::input_event::KeyModifiers;
use crate::core::keymap::{ACTIONS, Chord};

use super::Display;
use super::browser::scroll_to_show;
use super::modal_focus::SettingsTab;
use super::settings_modal::{scroll_by, window_start};

/// The Keyboard tab's view-local state. Session-only; a fresh search each
/// time the modal opens.
#[derive(Default)]
pub(super) struct KeyboardTab {
    /// The search field's text.
    pub(super) search: String,
    /// The selected row, counted among the rows the search matches.
    pub(super) selection: usize,
    /// The first visible line of the list (headers count).
    pub(super) scroll: usize,
    /// Whether the search field has shown since the modal opened: its first
    /// frame selects the text.
    pub(super) field_shown: bool,
}

/// `keys` as the modal opens: no search, the first row selected, the list
/// at the top, the field due to select its text again.
pub(super) fn reset_keyboard_tab(keys: &mut KeyboardTab) {
    *keys = KeyboardTab::default();
}

/// One row of the tab: a label and the keys that do it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ShortcutRow {
    /// The group the row is listed under.
    pub(super) category: &'static str,
    /// What the keys do.
    pub(super) label: &'static str,
    /// The keys, spelled out for this platform ([`key_label`]).
    pub(super) keys: String,
    /// A key that is not a command: listed, never rebindable.
    pub(super) fixed: bool,
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

/// Every row, in the tab's order: each command on its chords, then the
/// fixed keys. The commands show their defaults — the only keymap until
/// `320` phase 3.
fn shortcut_rows() -> Vec<ShortcutRow> {
    let commands = ACTIONS.iter().map(|info| ShortcutRow {
        category: info.category,
        label: info.label,
        keys: info
            .defaults
            .iter()
            .map(|&chord| key_label(&chord_glyphs(chord)))
            .collect::<Vec<_>>()
            .join("  "),
        fixed: false,
    });
    let fixed = FIXED.iter().flat_map(|&(category, rows)| {
        rows.iter().map(move |&(keys, label)| ShortcutRow {
            category,
            label,
            keys: key_label(keys),
            fixed: true,
        })
    });
    commands.chain(fixed).collect()
}

/// Whether `row` matches the search `query`: a case-blind substring of its
/// label or of its keys (so "cmd+l" finds Set loop, and so does "loop").
fn row_matches(row: &ShortcutRow, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || row.label.to_lowercase().contains(&query)
        || row.keys.to_lowercase().contains(&query)
}

/// The list the tab shows for `query`: the matching rows, each group's
/// heading over its first match. Empty when nothing matches.
pub(super) fn shortcut_lines(query: &str) -> Vec<ShortcutLine> {
    let mut lines = Vec::new();
    let mut category = None;
    for row in shortcut_rows()
        .into_iter()
        .filter(|row| row_matches(row, query))
    {
        if category != Some(row.category) {
            category = Some(row.category);
            lines.push(ShortcutLine::Header(row.category));
        }
        lines.push(ShortcutLine::Row(row));
    }
    lines
}

/// How many rows `lines` holds (its headers aside).
pub(super) fn row_count(lines: &[ShortcutLine]) -> usize {
    lines
        .iter()
        .filter(|line| matches!(line, ShortcutLine::Row(_)))
        .count()
}

/// The line `lines` shows row `row` on, if there is such a row.
pub(super) fn line_of_row(lines: &[ShortcutLine], row: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| matches!(line, ShortcutLine::Row(_)))
        .nth(row)
        .map(|(i, _)| i)
}

/// The row line `line` of `lines` is, counted among the rows — `None` on a
/// heading or past the end.
pub(super) fn row_of_line(lines: &[ShortcutLine], line: usize) -> Option<usize> {
    matches!(lines.get(line)?, ShortcutLine::Row(_)).then(|| row_count(&lines[..line]))
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

    /// The Keyboard tab's keys: ↑/↓ move the selection through the rows the
    /// search matches, scrolling to keep it in view. Every other key is the
    /// search field's.
    pub(super) fn handle_keyboard_tab_key(&mut self, key: Key) {
        let step: isize = match key {
            Key::ArrowUp => -1,
            Key::ArrowDown => 1,
            _ => return,
        };
        let lines = shortcut_lines(&self.settings.keys.search);
        let count = row_count(&lines);
        if count == 0 {
            return;
        }
        let keys = &mut self.settings.keys;
        keys.selection = keys
            .selection
            .min(count - 1)
            .saturating_add_signed(step)
            .min(count - 1);
        if let Some(line) = line_of_row(&lines, keys.selection) {
            let visible = Self::keyboard_visible_lines();
            let scroll = window_start(keys.scroll, lines.len(), visible);
            keys.scroll = scroll_to_show_line(scroll, line, &lines, visible);
        }
    }

    /// Esc on the Keyboard tab: clears a non-empty search and returns
    /// `true`; `false` when there is none, and Esc closes the modal.
    pub(super) fn clear_keyboard_search(&mut self) -> bool {
        if self.settings.keys.search.is_empty() {
            return false;
        }
        self.reset_keyboard_list();
        self.settings.keys.search.clear();
        true
    }

    /// The selection and the scroll back at the top — when the search
    /// changes what the list holds.
    pub(super) fn reset_keyboard_list(&mut self) {
        self.settings.keys.selection = 0;
        self.settings.keys.scroll = 0;
    }

    /// A wheel scroll over the Keyboard tab: `rows` lines down (up when
    /// negative), kept inside the list.
    pub(super) fn scroll_keyboard_list(&mut self, rows: isize) {
        let len = shortcut_lines(&self.settings.keys.search).len();
        let keys = &mut self.settings.keys;
        keys.scroll = scroll_by(keys.scroll, rows, len, Self::keyboard_visible_lines());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::Key;

    use super::{
        FIXED, ShortcutLine, chord_glyphs, is_keyboard_tab_key, key_label_for, line_of_row,
        row_count, row_of_line, scroll_to_show_line, shortcut_lines, shortcut_rows,
    };
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::{ACTIONS, Chord};

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
        let rows = shortcut_rows();
        for info in ACTIONS {
            let listed: Vec<_> = rows
                .iter()
                .filter(|row| !row.fixed && row.label == info.label)
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
        for row in shortcut_rows() {
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
        for row in shortcut_rows() {
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
        let lines = shortcut_lines("");
        assert_eq!(row_count(&lines), shortcut_rows().len());
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
            let lines = shortcut_lines(query);
            assert!(
                labels(&lines).contains(&"Set the loop, or loop on / off"),
                "{query}"
            );
            // Only the groups with a match keep their heading.
            assert!(matches!(lines[0], ShortcutLine::Header(_)));
        }
        assert!(shortcut_lines("no such shortcut").is_empty());
    }

    #[test]
    fn rows_and_lines_convert_both_ways() {
        let lines = shortcut_lines("");
        for row in 0..row_count(&lines) {
            let line = line_of_row(&lines, row).unwrap();
            assert_eq!(row_of_line(&lines, line), Some(row));
        }
        assert_eq!(row_of_line(&lines, 0), None, "a heading is no row");
        assert_eq!(line_of_row(&lines, row_count(&lines)), None);
        assert_eq!(row_of_line(&lines, lines.len()), None);
    }

    #[test]
    fn scrolling_to_a_groups_first_row_shows_its_heading_too() {
        let lines = shortcut_lines("");
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
}
