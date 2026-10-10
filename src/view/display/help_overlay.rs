//! The keyboard help overlay (`?`, or the header's `?` chip): one page of
//! the bindings a stranger needs in a first session, an overlay over the
//! view like the settings modal. The content is the [`HELP`] table — data, not
//! code, so documenting a binding is adding a row. Its keymap is
//! `input/help.rs`; drawing and the scroll range share one layout in
//! `rendering/modals/help.rs`. See `020-views-and-state.md` and
//! `030-ui-design.md` § Help overlay.
//!
//! The table is not generated from the key matchers (they are spread over
//! `input_handler.rs` and `Display`): changing a binding updates `010` and
//! this table in the same change.

use egui::Key;

use crate::core::input_event::KeyModifiers;
use crate::core::view_state::Pane;

use super::state::scrolled_offset;
use super::{Display, Overlay};

/// The help overlay's view-local state. Session-only. Whether it is up is
/// `Display::overlay`.
#[derive(Default)]
pub(super) struct HelpOverlay {
    /// How far the page is scrolled down, in points — only ever non-zero in
    /// a window too short to show it whole.
    pub(super) scroll: f32,
}

/// One titled group of bindings.
pub(super) struct HelpSection {
    /// The heading, drawn uppercase.
    pub(super) title: &'static str,
    /// The pane the section is about, whose title is accented while that
    /// pane has the keyboard; `None` for the app-wide sections.
    pub(super) pane: Option<Pane>,
    /// `(keys, action)` rows. The keys are written with macOS's `⌘` `⌥` `⇧`
    /// and go through [`key_label`] before they are shown.
    pub(super) rows: &'static [(&'static str, &'static str)],
}

/// The help page: columns, left to right, each a stack of sections.
pub(super) const HELP: [&[HelpSection]; 3] = [
    &[
        HelpSection {
            title: "Transport & capture",
            pane: None,
            rows: &[
                ("Space", "Play / stop"),
                ("⌥Space", "Restart from the cursor"),
                ("0", "Stop and return to the cursor"),
                ("R", "Live record on / off"),
                ("\\  /", "Commit the take to a clip"),
                ("[  ]", "Clip start / end to the cursor"),
                ("Enter", "Fit the tempo to the only clip"),
                ("T", "Tap tempo"),
                ("K", "Metronome on / off"),
                ("⌘L", "Set the loop, or loop on / off"),
            ],
        },
        HelpSection {
            title: "Project & app",
            pane: None,
            rows: &[
                ("⌘S", "Save"),
                ("⌘⇧S", "Save as"),
                ("⌘N", "New project"),
                ("⌘O", "Open (the browser)"),
                ("⌘⌥B", "Browser panel on / off"),
                ("⌘R", "Rename the file (browser)"),
                ("⌘Z", "Undo"),
                ("⌘⇧Z", "Redo"),
                ("⇧Tab", "Clip panel on / off"),
                ("⌘⌥E", "Dock / maximize the clip panel"),
                ("+  -", "Zoom in / out"),
                ("Z  X", "Zoom to the selection / back"),
                ("⌘⇧E", "Export the clip as MIDI"),
                ("⌘,", "Settings"),
                ("?", "This page"),
            ],
        },
    ],
    &[HelpSection {
        title: "Arranger",
        pane: Some(Pane::Arranger),
        rows: &[
            ("←  →", "Move the cursor"),
            ("↑  ↓", "Select the track"),
            ("⇧ arrows", "Extend the selection"),
            ("⌥←  →", "Jump to the next clip edge"),
            ("⇧⌥←  →", "Extend to the next clip edge"),
            ("⌘←  →", "Move the selected block"),
            ("⌘A", "Select every clip"),
            ("Delete", "Delete the selection"),
            ("M", "Mute the selection"),
            ("⌘E", "Split at the cursor"),
            ("⌘J", "Merge clips"),
            ("⌘D", "Duplicate clips"),
            ("⌘⇧D", "Duplicate time"),
            ("⌘I", "Insert silence"),
            ("⌘Delete", "Delete time"),
            ("⌘⇧M", "Insert an empty clip"),
            ("⌘T", "Add a track"),
            ("⌘R", "Rename the track"),
        ],
    }],
    &[
        HelpSection {
            title: "Clip",
            pane: Some(Pane::Clip),
            rows: &[
                ("←  →", "Cursor, or nudge notes"),
                ("⌘←  →", "Nudge notes finely"),
                ("⇧←  →", "Shorten / lengthen notes"),
                ("↑  ↓", "Transpose (Shift: an octave)"),
                ("⌘A", "Select every note"),
                ("Esc", "Deselect"),
                ("Delete", "Delete notes"),
                ("M", "Mute notes"),
                ("Q", "Quantize (all if none selected)"),
                ("⌘C  X  V", "Copy / cut / paste notes"),
                ("⌘D", "Duplicate notes"),
                ("⌥=  ⌥-", "Stretch the clip a bar"),
            ],
        },
        HelpSection {
            title: "Clip mouse",
            pane: Some(Pane::Clip),
            rows: &[
                ("Double-click", "Insert a note"),
                ("Drag", "Move, or resize by an edge"),
                ("⌘ drag", "Change the velocity"),
                ("Legend drag", "Zoom / scroll the note rows"),
            ],
        },
    ],
];

/// `?` with no ⌘/Ctrl or ⌥ — opens the help overlay, or closes it when it is
/// up. ⇧ is ignored: it is part of typing `?` on most layouts, and egui-winit
/// reports the logical key, so this is `⇧/` on US layouts and `⇧+` on Nordic
/// ones.
pub(super) fn is_help_key(key: Key, modifiers: KeyModifiers) -> bool {
    key == Key::Questionmark && !modifiers.command && !modifiers.alt
}

/// `keys` as shown on this platform ([`key_label_for`]).
pub(super) fn key_label(keys: &str) -> String {
    key_label_for(keys, cfg!(target_os = "macos"))
}

/// `keys` with its modifier glyphs spelled out: `⌘` as `Cmd+` on macOS and
/// `Ctrl+` elsewhere, `⌥` as `Opt+` / `Alt+`, `⇧` as `Shift+`. Spelled out on
/// macOS too: egui's bundled fonts have no `⌥`, and a mix of glyphs and
/// words reads worse than words.
pub(super) fn key_label_for(keys: &str, mac: bool) -> String {
    let mut label = String::with_capacity(keys.len() + 8);
    let mut chars = keys.chars().peekable();
    while let Some(c) = chars.next() {
        let name = match c {
            '⌘' if mac => "Cmd",
            '⌘' => "Ctrl",
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

impl Display {
    /// Opens the help overlay, scrolled to the top.
    pub(super) fn open_help(&mut self) {
        self.overlay = Some(Overlay::Help);
        self.help.scroll = 0.0;
    }

    /// A wheel or trackpad scroll over the help overlay: moves the page,
    /// kept inside the room it overflows the window by (`scrolled_offset`).
    pub(super) fn scroll_help(&mut self, delta_y: f32) {
        self.help.scroll = scrolled_offset(self.help.scroll, delta_y, self.help_overflow());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::Key;

    use super::{HELP, is_help_key, key_label_for};
    use crate::core::input_event::KeyModifiers;

    #[test]
    fn a_question_mark_opens_help_with_or_without_shift() {
        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        assert!(is_help_key(Key::Questionmark, KeyModifiers::default()));
        assert!(is_help_key(Key::Questionmark, shift));
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        assert!(!is_help_key(Key::Questionmark, command));
        assert!(!is_help_key(Key::Questionmark, alt));
        assert!(!is_help_key(Key::Slash, shift));
    }

    #[test]
    fn modifier_glyphs_are_spelled_out_per_platform() {
        assert_eq!(key_label_for("⌘⇧S", true), "Cmd+Shift+S");
        assert_eq!(key_label_for("⌘⇧S", false), "Ctrl+Shift+S");
        assert_eq!(key_label_for("⇧⌥←  →", true), "Shift+Opt+←  →");
        assert_eq!(key_label_for("⌥Space", false), "Alt+Space");
        assert_eq!(key_label_for("⇧ arrows", false), "Shift arrows");
        assert_eq!(key_label_for("⌘ drag", true), "Cmd drag");
        assert_eq!(key_label_for("Q", true), "Q");
        assert_eq!(key_label_for("+  -", true), "+  -");
    }

    #[test]
    fn every_row_has_keys_and_an_action_and_no_unfontable_glyph() {
        for section in HELP.iter().flat_map(|column| column.iter()) {
            assert!(!section.title.is_empty());
            assert!(!section.rows.is_empty(), "{}", section.title);
            for &(keys, action) in section.rows {
                assert!(!keys.trim().is_empty() && !action.trim().is_empty());
                for mac in [true, false] {
                    let label = key_label_for(keys, mac);
                    assert!(
                        !label.contains(['⌘', '⌥', '⇧', '⌫']),
                        "{label} has a glyph the bundled fonts lack"
                    );
                }
            }
        }
    }

    #[test]
    fn no_keys_are_listed_twice_in_a_section() {
        for section in HELP.iter().flat_map(|column| column.iter()) {
            let mut seen = HashSet::new();
            for &(keys, _) in section.rows {
                assert!(seen.insert(keys), "{keys} twice in {}", section.title);
            }
        }
    }
}
