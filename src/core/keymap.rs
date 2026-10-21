//! The command keymap: every rebindable command ([`Action`]), the chords that
//! trigger it, and the one lookup that turns a key press into a command.
//!
//! A *command* is a discrete action with a name (`Space`, `\`, `⌘L`, …). The
//! navigation grammar (the arrows with their modifier family), `Esc` /
//! `Enter` / `Tab`, the keys inside dialogs, text fields, the browser and the
//! track headers, and the computer keyboard's piano layout are not commands:
//! they stay hand-matched where they are handled. `Display` resolves a
//! `KeyPressed` through [`Keymap::resolve`] once, after the handlers that take
//! every key, and the hit travels on as `InputEvent::Action`. See
//! `docs/320-keyboard-shortcuts.md` and `010-keybindings.md`.
//!
//! [`ACTIONS`] is the single source: the defaults are built from it, and so
//! is the settings modal's Keyboard tab. Adding a command is adding a row
//! there and an arm where it is handled.

use std::collections::HashMap;

use egui::Key;

use crate::core::{input_event::KeyModifiers, view_state::Pane};

/// A rebindable command. One meaning per action: a chord that means
/// different things in the two panes is two actions with the same default
/// (`M` is [`MuteRange`](Self::MuteRange) in the arranger and
/// [`MuteNotes`](Self::MuteNotes) in the clip view), so each can be rebound
/// on its own.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Action {
    /// Start if stopped, stop if running (`TransportCommand::TogglePlayback`).
    TogglePlayback,
    /// Cubase's "Start": in the clip view, restart from the clip cursor; in
    /// the arranger, the same as [`TogglePlayback`](Self::TogglePlayback).
    PlayFromCursor,
    /// Stop and return to the cursor (`TransportCommand::Stop`).
    Stop,
    /// Freeze the capture into a clip, or into the lead clip
    /// (`SequencerCommand::Commit`).
    Commit,
    /// Move the start edge of a clip to the cursor.
    ClipStartToCursor,
    /// Move the end edge of a clip to the cursor.
    ClipEndToCursor,
    /// Live recording on / off (arranger).
    ToggleLiveRecord,
    /// Tap tempo.
    TapTempo,
    /// The built-in metronome on / off.
    ToggleMetronome,
    /// The computer keyboard (the letter keys as a piano) on / off.
    ToggleComputerKeyboard,
    /// Snap the loop to the time selection, or toggle looping.
    SetLoop,
    /// Save the project.
    Save,
    /// Save the project under a typed name.
    SaveAs,
    /// A new project, through the unsaved-changes check.
    NewProject,
    /// Show the browser panel with the Projects tree focused.
    ShowProjects,
    /// Show and focus the browser panel, or hide it.
    ToggleBrowser,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Select every clip (arranger) or every note (clip view).
    SelectAll,
    /// The clip panel between docked and maximized.
    ToggleClipPanelSize,
    /// Zoom the focused pane in.
    ZoomIn,
    /// Zoom the focused pane out.
    ZoomOut,
    /// Zoom to the time selection (arranger) or the selected notes (clip).
    ZoomToFit,
    /// Step back to the framing the last [`ZoomToFit`](Self::ZoomToFit) replaced.
    ZoomBack,
    /// Export the lead clip as a Standard MIDI File.
    ExportClip,
    /// Export the arranger's time selection as audio.
    ExportAudio,
    /// Step to the previous colour theme.
    PreviousTheme,
    /// Step to the next colour theme.
    NextTheme,
    /// Carve the marquee out (arranger).
    DeleteSelection,
    /// Mute the marquee's range (arranger).
    MuteRange,
    /// Split clips at the cursor (arranger).
    Split,
    /// Merge the marquee's clips into one per track (arranger).
    Merge,
    /// Duplicate the marquee's clips after themselves (arranger).
    DuplicateClips,
    /// Duplicate the marquee's time across every track (arranger).
    DuplicateTime,
    /// Insert silence at the selection start (arranger).
    InsertSilence,
    /// Delete the selection's time, closing the gap (arranger).
    DeleteTime,
    /// Insert an empty clip over the marquee, or a bar at the cursor.
    InsertEmptyClip,
    /// Add an empty MIDI track after the selected one.
    AddTrack,
    /// Rename the selected track.
    RenameTrack,
    /// Show / hide the selected track's plugin editor (macOS).
    TogglePluginEditor,
    /// Delete the selected notes (clip view).
    DeleteNotes,
    /// Mute the selected notes (clip view).
    MuteNotes,
    /// Quantize the selected notes, or every note with none selected.
    Quantize,
    /// Duplicate the selected notes after themselves (clip view).
    DuplicateNotes,
    /// Stretch the clip a bar longer, retiming its notes (clip view).
    StretchClipLonger,
    /// Stretch the clip a bar shorter, retiming its notes (clip view).
    StretchClipShorter,
}

/// Which pane must have the keyboard for an action's chord to trigger it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Either pane.
    Global,
    /// The arranger has the keyboard.
    Arranger,
    /// The clip view has the keyboard.
    Clip,
}

impl Scope {
    /// Whether two actions in these scopes could both be live at once, so
    /// must not share a chord: either is global, or both are the same pane.
    /// The rebinding recorder's conflict check (`320` phase 3).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn overlaps(self, other: Scope) -> bool {
        self == Scope::Global || other == Scope::Global || self == other
    }

    /// Whether an action in this scope is live while `pane` has the keyboard.
    fn contains(self, pane: Pane) -> bool {
        match self {
            Scope::Global => true,
            Scope::Arranger => pane == Pane::Arranger,
            Scope::Clip => pane == Pane::Clip,
        }
    }
}

/// A key with the modifiers held, as the keymap stores and compares it.
///
/// `command` is ⌘ on macOS and Ctrl elsewhere (egui's `command`), so a stored
/// chord is portable. A press becomes a chord through
/// [`from_press`](Self::from_press), which drops what a keyboard layout adds
/// to type the key at all, so chords compare exactly.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Chord {
    /// The logical key (egui-winit's, so `?` is `Questionmark` on every
    /// layout).
    pub(crate) key: Key,
    /// ⌘ on macOS, Ctrl elsewhere.
    pub(crate) command: bool,
    /// ⌥ on macOS, Alt elsewhere.
    pub(crate) alt: bool,
    /// Shift.
    pub(crate) shift: bool,
    /// The Control key on macOS (⌃); always `false` elsewhere, where Control
    /// is `command`.
    pub(crate) ctrl: bool,
}

impl Chord {
    /// `key` with no modifier.
    const fn plain(key: Key) -> Self {
        Chord {
            key,
            command: false,
            alt: false,
            shift: false,
            ctrl: false,
        }
    }

    /// `⌘/Ctrl+key`.
    const fn cmd(key: Key) -> Self {
        Chord {
            command: true,
            ..Chord::plain(key)
        }
    }

    /// This chord with Shift added.
    const fn shift(self) -> Self {
        Chord {
            shift: true,
            ..self
        }
    }

    /// This chord with ⌥/Alt added.
    const fn alt(self) -> Self {
        Chord { alt: true, ..self }
    }

    /// The chord a press of `key` with `modifiers` is, on this platform.
    pub(crate) fn from_press(key: Key, modifiers: KeyModifiers) -> Self {
        Chord {
            key,
            command: modifiers.command,
            alt: modifiers.alt,
            shift: modifiers.shift,
            ctrl: modifiers.ctrl,
        }
        .normalized(cfg!(target_os = "macos"))
    }

    /// This chord with what a keyboard layout adds dropped, `mac` choosing
    /// the platform's rules:
    ///
    /// - Off macOS, Control *is* `command`, so `ctrl` folds into it.
    /// - A punctuation key ignores Shift: egui-winit reports the logical
    ///   key, so the key already says what Shift made of it (`?` is `⇧/` on
    ///   US layouts and `⇧+` on Nordic ones, `+` is `⇧=` on US ones, `/` is
    ///   `⇧7` on Nordic ones).
    /// - The brackets, `\` and `|` ignore ⌥ too: Nordic and German layouts
    ///   type them with ⌥ on macOS and AltGr elsewhere — and AltGr arrives
    ///   as Ctrl+Alt there, so with ⌥ held `command` goes as well.
    fn normalized(mut self, mac: bool) -> Self {
        if !mac {
            self.ctrl = false;
        }
        if is_punctuation(self.key) {
            self.shift = false;
        }
        if is_alt_typed(self.key) {
            if !mac && self.alt {
                self.command = false;
            }
            self.alt = false;
        }
        self
    }

    /// Whether no modifier is held — the chords the plugin-editor key guard
    /// can reserve (`plugin_host/key_guard.rs`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn is_bare(self) -> bool {
        !self.command && !self.alt && !self.shift && !self.ctrl
    }
}

/// Whether `key` is a punctuation key, whose logical identity already
/// reflects Shift (see [`Chord::normalized`]).
fn is_punctuation(key: Key) -> bool {
    matches!(
        key,
        Key::Colon
            | Key::Comma
            | Key::Backslash
            | Key::Slash
            | Key::Pipe
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
            | Key::Backtick
            | Key::Minus
            | Key::Period
            | Key::Plus
            | Key::Equals
            | Key::Semicolon
            | Key::Quote
    )
}

/// Whether some common layout types `key` with ⌥ (macOS) or AltGr.
fn is_alt_typed(key: Key) -> bool {
    matches!(
        key,
        Key::Backslash
            | Key::Pipe
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
    )
}

/// One row of [`ACTIONS`]: everything the keymap and the Keyboard tab know
/// about an action. The id, label and category are for the Keyboard tab
/// and `settings.json` (`320` phases 2 and 3); until then only the tests
/// read them.
pub(crate) struct ActionInfo {
    /// The action.
    pub(crate) action: Action,
    /// Stable id, stored in `settings.json`: never renamed once released.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) id: &'static str,
    /// What the Keyboard tab calls it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) label: &'static str,
    /// The Keyboard tab's group.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) category: &'static str,
    /// Where its chords are live.
    pub(crate) scope: Scope,
    /// The chords it has out of the box — several are layout fallbacks (`\`
    /// and `/` for Commit). Written normalized ([`Chord::normalized`]).
    pub(crate) defaults: &'static [Chord],
}

/// The Keyboard tab's group for the transport and capture commands.
const TRANSPORT: &str = "Transport & capture";
/// The Keyboard tab's group for the project and app commands.
const PROJECT: &str = "Project & app";
/// The Keyboard tab's group for the arranger's commands.
const ARRANGER: &str = "Arranger";
/// The Keyboard tab's group for the clip view's commands.
const CLIP: &str = "Clip";

/// Shorthand for an [`ACTIONS`] row.
const fn row(
    action: Action,
    id: &'static str,
    label: &'static str,
    category: &'static str,
    scope: Scope,
    defaults: &'static [Chord],
) -> ActionInfo {
    ActionInfo {
        action,
        id,
        label,
        category,
        scope,
        defaults,
    }
}

/// Every action, in the Keyboard tab's order. The defaults are conflict-free
/// (pinned by a test): no two overlapping actions share a chord.
pub(crate) const ACTIONS: &[ActionInfo] = &[
    // --- Transport & capture ---
    row(
        Action::TogglePlayback,
        "transport.play",
        "Play / stop",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::Space)],
    ),
    row(
        Action::PlayFromCursor,
        "transport.play_from_cursor",
        "Restart from the cursor",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::Space).alt()],
    ),
    row(
        Action::Stop,
        "transport.stop",
        "Stop and return to the cursor",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::Num0)],
    ),
    row(
        Action::ToggleLiveRecord,
        "transport.live_record",
        "Live record on / off",
        TRANSPORT,
        Scope::Arranger,
        &[Chord::plain(Key::R)],
    ),
    row(
        Action::Commit,
        "capture.commit",
        "Commit the take to a clip",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::Backslash), Chord::plain(Key::Slash)],
    ),
    row(
        Action::ClipStartToCursor,
        "capture.clip_start",
        "Clip start to the cursor",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::OpenBracket)],
    ),
    row(
        Action::ClipEndToCursor,
        "capture.clip_end",
        "Clip end to the cursor",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::CloseBracket)],
    ),
    row(
        Action::TapTempo,
        "transport.tap_tempo",
        "Tap tempo",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::T)],
    ),
    row(
        Action::ToggleMetronome,
        "transport.metronome",
        "Metronome on / off",
        TRANSPORT,
        Scope::Global,
        &[Chord::plain(Key::K)],
    ),
    row(
        Action::ToggleComputerKeyboard,
        "transport.computer_keyboard",
        "Play notes on the keyboard",
        TRANSPORT,
        Scope::Global,
        &[Chord::cmd(Key::K)],
    ),
    row(
        Action::SetLoop,
        "transport.set_loop",
        "Set the loop, or loop on / off",
        TRANSPORT,
        Scope::Arranger,
        &[Chord::cmd(Key::L)],
    ),
    // --- Project & app ---
    row(
        Action::Save,
        "project.save",
        "Save",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::S)],
    ),
    row(
        Action::SaveAs,
        "project.save_as",
        "Save as",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::S).shift()],
    ),
    row(
        Action::NewProject,
        "project.new",
        "New project",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::N)],
    ),
    row(
        Action::ShowProjects,
        "project.open",
        "Open (the browser)",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::O)],
    ),
    row(
        Action::ToggleBrowser,
        "view.browser",
        "Browser panel on / off",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::B).alt()],
    ),
    row(
        Action::Undo,
        "edit.undo",
        "Undo",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::Z)],
    ),
    row(
        Action::Redo,
        "edit.redo",
        "Redo",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::Z).shift()],
    ),
    row(
        Action::SelectAll,
        "edit.select_all",
        "Select every clip / note",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::A)],
    ),
    row(
        Action::ToggleClipPanelSize,
        "view.clip_panel_size",
        "Dock / maximize the clip panel",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::E).alt()],
    ),
    row(
        Action::ZoomIn,
        "view.zoom_in",
        "Zoom in",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::Plus), Chord::plain(Key::Equals)],
    ),
    row(
        Action::ZoomOut,
        "view.zoom_out",
        "Zoom out",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::Minus)],
    ),
    row(
        Action::ZoomToFit,
        "view.zoom_to_fit",
        "Zoom to the selection",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::Z)],
    ),
    row(
        Action::ZoomBack,
        "view.zoom_back",
        "Zoom back",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::X)],
    ),
    row(
        Action::ExportClip,
        "project.export_clip",
        "Export the clip as MIDI",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::E).shift()],
    ),
    row(
        Action::ExportAudio,
        "project.export_audio",
        "Export the selection as audio",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::R).shift()],
    ),
    row(
        Action::PreviousTheme,
        "view.previous_theme",
        "Previous theme",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::F5).shift()],
    ),
    row(
        Action::NextTheme,
        "view.next_theme",
        "Next theme",
        PROJECT,
        Scope::Global,
        &[Chord::plain(Key::F6).shift()],
    ),
    // --- Arranger ---
    row(
        Action::DeleteSelection,
        "arranger.delete",
        "Delete the selection",
        ARRANGER,
        Scope::Arranger,
        &[Chord::plain(Key::Delete), Chord::plain(Key::Backspace)],
    ),
    row(
        Action::MuteRange,
        "arranger.mute",
        "Mute the selection",
        ARRANGER,
        Scope::Arranger,
        &[Chord::plain(Key::M)],
    ),
    row(
        Action::Split,
        "arranger.split",
        "Split at the cursor",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::E)],
    ),
    row(
        Action::Merge,
        "arranger.merge",
        "Merge clips",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::J)],
    ),
    row(
        Action::DuplicateClips,
        "arranger.duplicate_clips",
        "Duplicate clips",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::D)],
    ),
    row(
        Action::DuplicateTime,
        "arranger.duplicate_time",
        "Duplicate time",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::D).shift()],
    ),
    row(
        Action::InsertSilence,
        "arranger.insert_silence",
        "Insert silence",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::I)],
    ),
    row(
        Action::DeleteTime,
        "arranger.delete_time",
        "Delete time",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::Delete)],
    ),
    row(
        Action::InsertEmptyClip,
        "arranger.insert_clip",
        "Insert an empty clip",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::M).shift()],
    ),
    row(
        Action::AddTrack,
        "arranger.add_track",
        "Add a track",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::T)],
    ),
    row(
        Action::RenameTrack,
        "arranger.rename_track",
        "Rename the track",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::R)],
    ),
    row(
        Action::TogglePluginEditor,
        "arranger.plugin_editor",
        "Show / hide the plugin",
        ARRANGER,
        Scope::Arranger,
        &[Chord::plain(Key::V)],
    ),
    // --- Clip ---
    row(
        Action::DeleteNotes,
        "clip.delete",
        "Delete notes",
        CLIP,
        Scope::Clip,
        &[Chord::plain(Key::Delete), Chord::plain(Key::Backspace)],
    ),
    row(
        Action::MuteNotes,
        "clip.mute",
        "Mute notes",
        CLIP,
        Scope::Clip,
        &[Chord::plain(Key::M)],
    ),
    row(
        Action::Quantize,
        "clip.quantize",
        "Quantize (all if none selected)",
        CLIP,
        Scope::Clip,
        &[Chord::plain(Key::Q)],
    ),
    row(
        Action::DuplicateNotes,
        "clip.duplicate",
        "Duplicate notes",
        CLIP,
        Scope::Clip,
        &[Chord::cmd(Key::D)],
    ),
    row(
        Action::StretchClipLonger,
        "clip.stretch_longer",
        "Stretch the clip a bar longer",
        CLIP,
        Scope::Clip,
        &[
            Chord::plain(Key::Equals).alt(),
            Chord::plain(Key::Plus).alt(),
        ],
    ),
    row(
        Action::StretchClipShorter,
        "clip.stretch_shorter",
        "Stretch the clip a bar shorter",
        CLIP,
        Scope::Clip,
        &[Chord::plain(Key::Minus).alt()],
    ),
];

impl Action {
    /// This action's row in [`ACTIONS`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn info(self) -> &'static ActionInfo {
        ACTIONS
            .iter()
            .find(|info| info.action == self)
            .expect("every action has a row in ACTIONS")
    }
}

/// The live keymap: each action's chords — the defaults, today — indexed by
/// chord for [`resolve`](Self::resolve). Main-thread, view-local plain data
/// held by `Display`.
pub(crate) struct Keymap {
    /// Every bound chord and the actions on it, at most one per scope.
    by_chord: HashMap<Chord, Vec<(Scope, Action)>>,
}

impl Default for Keymap {
    /// The out-of-the-box keymap: every action on its default chords.
    fn default() -> Self {
        let mut by_chord: HashMap<Chord, Vec<(Scope, Action)>> = HashMap::new();
        for info in ACTIONS {
            for &chord in info.defaults {
                by_chord
                    .entry(chord)
                    .or_default()
                    .push((info.scope, info.action));
            }
        }
        Keymap { by_chord }
    }
}

impl Keymap {
    /// The action `chord` triggers while `pane` has the keyboard: an action
    /// scoped to that pane ahead of a global one.
    pub(crate) fn resolve(&self, chord: Chord, pane: Pane) -> Option<Action> {
        let candidates = self.by_chord.get(&chord)?;
        let live = || candidates.iter().filter(|(scope, _)| scope.contains(pane));
        live()
            .find(|(scope, _)| *scope != Scope::Global)
            .or_else(|| live().next())
            .map(|&(_, action)| action)
    }

    /// The action a press of `key` with `modifiers` triggers while `pane` has
    /// the keyboard.
    pub(crate) fn resolve_press(
        &self,
        key: Key,
        modifiers: KeyModifiers,
        pane: Pane,
    ) -> Option<Action> {
        self.resolve(Chord::from_press(key, modifiers), pane)
    }

    /// The keys `action` is bound to with no modifier — what the
    /// plugin-editor key guard reserves for it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn bare_keys(&self, action: Action) -> Vec<Key> {
        let mut keys: Vec<Key> = self
            .by_chord
            .iter()
            .filter(|(chord, actions)| {
                chord.is_bare() && actions.iter().any(|&(_, bound)| bound == action)
            })
            .map(|(chord, _)| chord.key)
            .collect();
        keys.sort_unstable();
        keys
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::Key;

    use super::{ACTIONS, Action, Chord, Keymap, Scope};
    use crate::core::{input_event::KeyModifiers, view_state::Pane};

    fn mods(command: bool, alt: bool, shift: bool, ctrl: bool) -> KeyModifiers {
        KeyModifiers {
            shift,
            command,
            alt,
            ctrl,
        }
    }

    fn press(key: Key, modifiers: KeyModifiers, mac: bool) -> Chord {
        Chord {
            key,
            command: modifiers.command,
            alt: modifiers.alt,
            shift: modifiers.shift,
            ctrl: modifiers.ctrl,
        }
        .normalized(mac)
    }

    #[test]
    fn ids_are_unique_and_every_action_has_a_label_and_category() {
        let mut ids = HashSet::new();
        for info in ACTIONS {
            assert!(ids.insert(info.id), "duplicate id {}", info.id);
            assert!(!info.label.is_empty(), "{:?} has no label", info.action);
            assert!(
                !info.category.is_empty(),
                "{:?} has no category",
                info.action
            );
            assert!(!info.defaults.is_empty(), "{:?} has no key", info.action);
        }
    }

    #[test]
    fn every_action_has_exactly_one_row() {
        let mut seen = HashSet::new();
        for info in ACTIONS {
            assert!(seen.insert(info.action), "{:?} listed twice", info.action);
            assert_eq!(info.action.info().id, info.id);
        }
    }

    #[test]
    fn the_defaults_do_not_conflict() {
        for (i, a) in ACTIONS.iter().enumerate() {
            for b in &ACTIONS[i + 1..] {
                if !a.scope.overlaps(b.scope) {
                    continue;
                }
                for chord in a.defaults {
                    assert!(
                        !b.defaults.contains(chord),
                        "{:?} and {:?} share {chord:?}",
                        a.action,
                        b.action
                    );
                }
            }
        }
    }

    /// A default a press could never produce would be dead.
    #[test]
    fn the_defaults_are_written_normalized() {
        for info in ACTIONS {
            for &chord in info.defaults {
                for mac in [true, false] {
                    assert_eq!(chord.normalized(mac), chord, "{:?}", info.action);
                }
            }
        }
    }

    #[test]
    fn matching_is_exact() {
        let keymap = Keymap::default();
        let resolve = |key, modifiers| keymap.resolve_press(key, modifiers, Pane::Arranger);
        assert_eq!(
            resolve(Key::Space, KeyModifiers::default()),
            Some(Action::TogglePlayback)
        );
        assert_eq!(resolve(Key::Space, mods(true, false, false, false)), None);
        assert_eq!(resolve(Key::Space, mods(false, false, true, false)), None);
        assert_eq!(resolve(Key::L, KeyModifiers::default()), None);
        assert_eq!(
            resolve(Key::L, mods(true, false, false, false)),
            Some(Action::SetLoop)
        );
        assert_eq!(resolve(Key::L, mods(true, true, false, false)), None);
        assert_eq!(
            resolve(Key::E, mods(true, true, false, false)),
            Some(Action::ToggleClipPanelSize)
        );
        assert_eq!(
            resolve(Key::E, mods(true, false, true, false)),
            Some(Action::ExportClip)
        );
        assert_eq!(
            resolve(Key::E, mods(true, false, false, false)),
            Some(Action::Split)
        );
    }

    /// `+` is Shift+`=` on US layouts and `/` is Shift+7 on Nordic ones: the
    /// logical key already says so, and the Shift is dropped.
    #[test]
    fn punctuation_ignores_shift() {
        let shift = mods(false, false, true, false);
        for mac in [true, false] {
            assert_eq!(press(Key::Plus, shift, mac), Chord::plain(Key::Plus));
            assert_eq!(press(Key::Slash, shift, mac), Chord::plain(Key::Slash));
            assert_eq!(press(Key::Minus, shift, mac), Chord::plain(Key::Minus));
            // A letter keeps it.
            assert_eq!(press(Key::S, shift, mac), Chord::plain(Key::S).shift());
        }
    }

    /// Nordic and German layouts type `\` and the brackets with ⌥ (macOS)
    /// or AltGr, which arrives as Ctrl+Alt elsewhere.
    #[test]
    fn brackets_and_backslash_ignore_the_layouts_alt() {
        let keymap = Keymap::default();
        let shift_alt = mods(false, true, true, false);
        assert_eq!(
            press(Key::Backslash, shift_alt, true),
            Chord::plain(Key::Backslash)
        );
        assert_eq!(
            press(Key::OpenBracket, mods(false, true, false, false), true),
            Chord::plain(Key::OpenBracket)
        );
        let alt_gr = mods(true, true, false, true);
        assert_eq!(
            press(Key::CloseBracket, alt_gr, false),
            Chord::plain(Key::CloseBracket)
        );
        // ⌘[ on macOS is a chord of its own, not `[`.
        assert_eq!(
            press(Key::OpenBracket, mods(true, false, false, false), true),
            Chord::cmd(Key::OpenBracket)
        );
        // `⌥=` is not typed with ⌥: it stays the clip stretch.
        assert_eq!(
            keymap.resolve(
                press(Key::Equals, mods(false, true, false, false), true),
                Pane::Clip
            ),
            Some(Action::StretchClipLonger)
        );
    }

    /// Off macOS Control is `command`, so egui reports both for Ctrl.
    #[test]
    fn control_folds_into_command_off_macos() {
        let ctrl = mods(true, false, false, true);
        assert_eq!(press(Key::S, ctrl, false), Chord::cmd(Key::S));
        assert_ne!(press(Key::S, ctrl, true), Chord::cmd(Key::S));
    }

    #[test]
    fn a_scoped_action_only_resolves_in_its_pane() {
        let keymap = Keymap::default();
        let m = Chord::plain(Key::M);
        assert_eq!(keymap.resolve(m, Pane::Arranger), Some(Action::MuteRange));
        assert_eq!(keymap.resolve(m, Pane::Clip), Some(Action::MuteNotes));
        let q = Chord::plain(Key::Q);
        assert_eq!(keymap.resolve(q, Pane::Arranger), None);
        assert_eq!(keymap.resolve(q, Pane::Clip), Some(Action::Quantize));
    }

    /// The defaults never put a scoped and a global action on one chord, so
    /// the precedence is pinned on a hand-built map.
    #[test]
    fn a_scoped_action_wins_over_a_global_one() {
        let chord = Chord::plain(Key::U);
        let keymap = Keymap {
            by_chord: [(
                chord,
                vec![
                    (Scope::Global, Action::TapTempo),
                    (Scope::Clip, Action::Quantize),
                ],
            )]
            .into(),
        };
        assert_eq!(keymap.resolve(chord, Pane::Clip), Some(Action::Quantize));
        assert_eq!(
            keymap.resolve(chord, Pane::Arranger),
            Some(Action::TapTempo)
        );
    }

    #[test]
    fn bare_keys_are_the_unmodified_chords() {
        let keymap = Keymap::default();
        assert_eq!(keymap.bare_keys(Action::TogglePlayback), vec![Key::Space]);
        assert_eq!(keymap.bare_keys(Action::Stop), vec![Key::Num0]);
        assert_eq!(keymap.bare_keys(Action::TogglePluginEditor), vec![Key::V]);
        assert!(keymap.bare_keys(Action::Undo).is_empty());
    }
}
