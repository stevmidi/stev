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
//! `docs/archive/320-keyboard-shortcuts.md` and `010-keybindings.md`.
//!
//! [`ACTIONS`] is the single source: the defaults are built from it, and so
//! is the settings modal's Keyboard tab. Adding a command is adding a row
//! there and an arm where it is handled.
//!
//! The user's rebindings are stored in `settings.json` as differences from
//! the defaults ([`Keymap::overrides`], [`ChordDto`]), so a new default in a
//! later version reaches every action the user never touched.

use std::collections::{BTreeMap, HashMap};

use egui::Key;
use serde::{Deserialize, Serialize};

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
    /// Tap tempo.
    TapTempo,
    /// The built-in metronome on / off.
    ToggleMetronome,
    /// The computer keyboard (the letter keys as a piano) on / off.
    ToggleComputerKeyboard,
    /// Snap the loop to the time selection, or toggle looping.
    SetLoop,
    /// Show the browser panel with the Projects tree focused, or hide it
    /// when it already has the keyboard.
    ShowProjects,
    /// Show and focus the browser panel, or hide it.
    ToggleBrowser,
    /// Name and make a new project folder in the browser panel.
    NewProjectFolder,
    /// Save the open project as a template (`archive/340-templates.md`).
    SaveAsTemplate,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Marquee every clip (arranger).
    SelectAllClips,
    /// Select every note (clip view).
    SelectAllNotes,
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
        Chord::from_press_on(key, modifiers, cfg!(target_os = "macos"))
    }

    /// The chord a press of `key` with `modifiers` is, on macOS when `mac`.
    fn from_press_on(key: Key, modifiers: KeyModifiers, mac: bool) -> Self {
        Chord {
            key,
            command: modifiers.command,
            alt: modifiers.alt,
            shift: modifiers.shift,
            ctrl: modifiers.ctrl,
        }
        .normalized(mac)
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

    /// Whether this is `?` with no ⌘/Ctrl or ⌥ — the fixed key that opens
    /// Settings on the Keyboard tab (⇧ is already dropped, being part of
    /// typing `?` on most layouts).
    pub(crate) fn opens_keyboard_tab(self) -> bool {
        self.key == Key::Questionmark && !self.command && !self.alt
    }

    /// Whether no modifier is held — the chords the plugin-editor key guard
    /// can reserve (`plugin_host/key_guard.rs`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn is_bare(self) -> bool {
        !self.command && !self.alt && !self.shift && !self.ctrl
    }
}

/// `⌘/Ctrl+S`: save. Fixed, like the other project chords — never an
/// [`Action`], so it can't be rebound and works from any key focus, ahead
/// of the overlays (`Display::handle_project_input_event`).
pub(crate) const SAVE: Chord = Chord::cmd(Key::S);
/// `⇧⌘/Ctrl+S`: save as. Fixed, like [`SAVE`].
pub(crate) const SAVE_AS: Chord = Chord::cmd(Key::S).shift();
/// `⌘/Ctrl+N`: a new project. Fixed, like [`SAVE`].
pub(crate) const NEW_PROJECT: Chord = Chord::cmd(Key::N);

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

/// Whether `key` is a modifier key itself (⇧, ⌃, ⌥, ⌘, either side).
/// egui reports a press of one as a key of its own, ahead of the key it
/// modifies; the input poller drops those, and [`refusal`] refuses one
/// read from `settings.json`.
pub(crate) fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
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

/// Why `chord` can't be bound to any action, as the end of a sentence that
/// starts with the chord ("⌘C *is the clipboard*"); `None` when it can be.
/// Refused: the clipboard (egui-winit turns `⌘C`/`⌘X`/`⌘V` into clipboard
/// events, so the key never arrives), egui's UI scale, the chords macOS
/// keeps, the ways into Settings and its Keyboard tab (always reachable),
/// the fixed project chords, the navigation and dialog keys, which stay
/// hand-matched (`docs/archive/320-keyboard-shortcuts.md` decision 1), and a
/// modifier key on its own.
pub(crate) fn refusal(chord: Chord) -> Option<&'static str> {
    let key = chord.key;
    if is_modifier(key) {
        return Some("is a modifier: hold it with another key");
    }
    if matches!(
        key,
        Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::Tab
            | Key::Enter
            | Key::Escape
    ) {
        return Some("is fixed: arrows, Tab, Enter and Esc navigate");
    }
    if chord.opens_keyboard_tab() {
        return Some("opens this tab");
    }
    if [SAVE, SAVE_AS, NEW_PROJECT].contains(&chord) {
        return Some("is fixed: save, save as and new project");
    }
    if !chord.command {
        return None;
    }
    match key {
        Key::C | Key::X | Key::V => Some("is the clipboard"),
        Key::Plus | Key::Equals | Key::Minus | Key::Num0 => Some("is the UI scale"),
        // Quit, hide (`⌥` hides the others) and minimize; `⇧⌘M` is free.
        Key::Q | Key::H | Key::M if !chord.shift => Some("is the system's"),
        Key::Comma => Some("opens Settings"),
        _ => None,
    }
}

/// A [`Chord`] as `settings.json` stores it: the key by egui's name
/// (`Key::name`, read back with `Key::from_name`) and the four modifier
/// flags, each `false` when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChordDto {
    /// The key's egui name (`"Space"`, `"Backslash"`, `"K"`).
    pub(crate) key: String,
    /// ⌘ on macOS, Ctrl elsewhere.
    #[serde(default)]
    pub(crate) command: bool,
    /// ⌥ / Alt.
    #[serde(default)]
    pub(crate) alt: bool,
    /// Shift.
    #[serde(default)]
    pub(crate) shift: bool,
    /// macOS's Control.
    #[serde(default)]
    pub(crate) ctrl: bool,
}

impl From<Chord> for ChordDto {
    fn from(chord: Chord) -> Self {
        ChordDto {
            key: chord.key.name().to_owned(),
            command: chord.command,
            alt: chord.alt,
            shift: chord.shift,
            ctrl: chord.ctrl,
        }
    }
}

impl ChordDto {
    /// The chord, normalized for this platform; `None` for a key name egui
    /// doesn't know (an edited file).
    fn to_chord(&self) -> Option<Chord> {
        let modifiers = KeyModifiers {
            command: self.command,
            alt: self.alt,
            shift: self.shift,
            ctrl: self.ctrl,
        };
        Some(Chord::from_press(Key::from_name(&self.key)?, modifiers))
    }
}

/// One row of [`ACTIONS`]: everything the keymap and the Keyboard tab know
/// about an action. The label and category are the Keyboard tab's
/// (`view/display/keyboard_tab.rs`); the id is `settings.json`'s.
pub(crate) struct ActionInfo {
    /// The action.
    pub(crate) action: Action,
    /// Stable id, stored in `settings.json`: never renamed once released.
    pub(crate) id: &'static str,
    /// What the Keyboard tab calls it.
    pub(crate) label: &'static str,
    /// The Keyboard tab's group.
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
        Action::ShowProjects,
        "project.open",
        "Open or focus the browser, or hide it",
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
        Action::NewProjectFolder,
        "project.new_folder",
        "New project folder",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::N).shift()],
    ),
    row(
        Action::SaveAsTemplate,
        "project.save_as_template",
        "Save as template",
        PROJECT,
        Scope::Global,
        &[Chord::cmd(Key::S).alt()],
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
    // --- Arranger ---
    row(
        Action::SelectAllClips,
        "arranger.select_all",
        "Select every clip",
        ARRANGER,
        Scope::Arranger,
        &[Chord::cmd(Key::A)],
    ),
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
        Action::SelectAllNotes,
        "clip.select_all",
        "Select every note",
        CLIP,
        Scope::Clip,
        &[Chord::cmd(Key::A)],
    ),
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
    pub(crate) fn info(self) -> &'static ActionInfo {
        &ACTIONS[self.index()]
    }

    /// Whether this action acts on a pane — scoped to the arranger or the
    /// clip view rather than global. Pressed while the browser has the
    /// keyboard, it takes the keyboard to the pane, so the key after it
    /// acts on what it changed (`Display::handle_browser_key`).
    pub(crate) fn acts_on_pane(self) -> bool {
        self.info().scope != Scope::Global
    }

    /// This action's index in [`ACTIONS`].
    fn index(self) -> usize {
        ACTIONS
            .iter()
            .position(|info| info.action == self)
            .expect("every action has a row in ACTIONS")
    }
}

/// The live keymap: each action's chords — the defaults with the user's
/// rebindings applied — indexed by chord for [`resolve`](Self::resolve).
/// Main-thread, view-local plain data held by `Display`. Never holds a
/// conflict: every change takes the chords it binds away from the
/// overlapping actions that had them.
pub(crate) struct Keymap {
    /// Each action's chords, index-parallel with [`ACTIONS`]. Empty is
    /// unbound.
    chords: Vec<Vec<Chord>>,
    /// Every bound chord and the actions on it, at most one per scope.
    by_chord: HashMap<Chord, Vec<(Scope, Action)>>,
}

impl Default for Keymap {
    /// The out-of-the-box keymap: every action on its default chords.
    fn default() -> Self {
        Keymap::from_chords(default_chords())
    }
}

/// Every action's default chords, index-parallel with [`ACTIONS`].
fn default_chords() -> Vec<Vec<Chord>> {
    ACTIONS.iter().map(|info| info.defaults.to_vec()).collect()
}

impl Keymap {
    /// The keymap holding `chords` (index-parallel with [`ACTIONS`]), as
    /// given: the caller keeps it conflict-free.
    fn from_chords(chords: Vec<Vec<Chord>>) -> Self {
        let mut keymap = Keymap {
            chords,
            by_chord: HashMap::new(),
        };
        keymap.reindex();
        keymap
    }

    /// The defaults with `overrides` (`settings.json`'s, by action id)
    /// applied. An unknown id, an unparseable chord or a refused one
    /// ([`refusal`]) is skipped and the rest kept — a downgrade (a key
    /// name or a rule this version doesn't know), a removed action, an
    /// edited file. An action whose stored chords are all skipped keeps its
    /// defaults rather than going unbound by accident; an empty list stored
    /// is unbound, as saved. Should what is left conflict, the action later in
    /// [`ACTIONS`] order loses the chord.
    pub(crate) fn from_overrides(overrides: &BTreeMap<String, Vec<ChordDto>>) -> Self {
        let mut chords = default_chords();
        for (id, dtos) in overrides {
            let Some(i) = ACTIONS.iter().position(|info| info.id == id) else {
                continue;
            };
            let mut list: Vec<Chord> = Vec::new();
            for chord in dtos.iter().filter_map(ChordDto::to_chord) {
                if refusal(chord).is_none() && !list.contains(&chord) {
                    list.push(chord);
                }
            }
            if list.is_empty() && !dtos.is_empty() {
                continue;
            }
            chords[i] = list;
        }
        for (i, info) in ACTIONS.iter().enumerate() {
            let (earlier, rest) = chords.split_at_mut(i);
            rest[0].retain(|chord| {
                !ACTIONS
                    .iter()
                    .zip(earlier.iter())
                    .any(|(other, held)| other.scope.overlaps(info.scope) && held.contains(chord))
            });
        }
        Keymap::from_chords(chords)
    }

    /// What `settings.json` stores: every action whose chords differ from
    /// its defaults, by id, an empty list for an unbound one.
    pub(crate) fn overrides(&self) -> BTreeMap<String, Vec<ChordDto>> {
        self.iter()
            .filter(|(info, chords)| info.defaults != *chords)
            .map(|(info, chords)| {
                let dtos = chords.iter().map(|&chord| ChordDto::from(chord)).collect();
                (info.id.to_owned(), dtos)
            })
            .collect()
    }

    /// `action`'s chords; empty when it is unbound.
    pub(crate) fn chords(&self, action: Action) -> &[Chord] {
        &self.chords[action.index()]
    }

    /// Whether `action` is on its default chords.
    #[cfg(test)]
    pub(crate) fn is_default(&self, action: Action) -> bool {
        self.chords(action) == action.info().defaults
    }

    /// Whether every action is on its default chords.
    pub(crate) fn is_all_default(&self) -> bool {
        self.iter().all(|(info, chords)| info.defaults == chords)
    }

    /// Every action's row in [`ACTIONS`] with its chords, in table order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static ActionInfo, &[Chord])> {
        ACTIONS.iter().zip(self.chords.iter().map(Vec::as_slice))
    }

    /// The other action binding `chord` would take it from: one in a scope
    /// overlapping `action`'s that has it.
    pub(crate) fn conflict(&self, action: Action, chord: Chord) -> Option<Action> {
        let scope = action.info().scope;
        self.by_chord
            .get(&chord)?
            .iter()
            .find(|&&(other_scope, other)| other != action && other_scope.overlaps(scope))
            .map(|&(_, other)| other)
    }

    /// Records `chord` for `action`: it becomes the action's only chord, and
    /// an overlapping action that had it loses it — left unbound if it was
    /// its only one.
    pub(crate) fn bind(&mut self, action: Action, chord: Chord) {
        self.set(action, vec![chord]);
    }

    /// Leaves `action` with no chord.
    pub(crate) fn unbind(&mut self, action: Action) {
        self.set(action, Vec::new());
    }

    /// Puts `action` back on its defaults, taking them from any overlapping
    /// action that was given one since.
    pub(crate) fn reset(&mut self, action: Action) {
        self.set(action, action.info().defaults.to_vec());
    }

    /// Puts every action back on its defaults.
    pub(crate) fn reset_all(&mut self) {
        *self = Keymap::default();
    }

    /// Makes `chords` `action`'s, taking each from the overlapping actions
    /// that had it, and rebuilds the index.
    fn set(&mut self, action: Action, chords: Vec<Chord>) {
        let scope = action.info().scope;
        for (info, held) in ACTIONS.iter().zip(&mut self.chords) {
            if info.action != action && info.scope.overlaps(scope) {
                held.retain(|chord| !chords.contains(chord));
            }
        }
        self.chords[action.index()] = chords;
        self.reindex();
    }

    /// Rebuilds [`by_chord`](Self::by_chord) from [`chords`](Self::chords).
    fn reindex(&mut self) {
        self.by_chord.clear();
        for (info, chords) in ACTIONS.iter().zip(&self.chords) {
            for &chord in chords {
                self.by_chord
                    .entry(chord)
                    .or_default()
                    .push((info.scope, info.action));
            }
        }
    }

    /// The action `chord` triggers while `pane` has the keyboard: an action
    /// scoped to that pane ahead of a global one.
    pub(crate) fn resolve(&self, chord: Chord, pane: Pane) -> Option<Action> {
        self.by_chord
            .get(&chord)?
            .iter()
            .filter(|(scope, _)| scope.contains(pane))
            .max_by_key(|(scope, _)| *scope != Scope::Global)
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
            .chords(action)
            .iter()
            .filter(|chord| chord.is_bare())
            .map(|chord| chord.key)
            .collect();
        keys.sort_unstable();
        keys
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use egui::Key;

    use super::{
        ACTIONS, Action, Chord, ChordDto, Keymap, NEW_PROJECT, SAVE, SAVE_AS, default_chords,
        is_modifier, refusal,
    };
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
        Chord::from_press_on(key, modifiers, mac)
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

    /// The project chords are fixed: no action may take them.
    #[test]
    fn no_action_takes_a_project_chord() {
        for info in ACTIONS {
            for chord in [SAVE, SAVE_AS, NEW_PROJECT] {
                assert!(!info.defaults.contains(&chord), "{:?}", info.action);
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
        let a = Chord::cmd(Key::A);
        assert_eq!(
            keymap.resolve(a, Pane::Arranger),
            Some(Action::SelectAllClips)
        );
        assert_eq!(keymap.resolve(a, Pane::Clip), Some(Action::SelectAllNotes));
        let q = Chord::plain(Key::Q);
        assert_eq!(keymap.resolve(q, Pane::Arranger), None);
        assert_eq!(keymap.resolve(q, Pane::Clip), Some(Action::Quantize));
    }

    /// ⌘A in the browser selected every clip but left the keyboard there,
    /// so the Delete after it armed a file delete: select-all acts on a
    /// pane and takes the keyboard there; the transport stays global, so
    /// Space previews a project without leaving the browser.
    #[test]
    fn select_all_acts_on_a_pane_and_playback_does_not() {
        assert!(Action::SelectAllClips.acts_on_pane());
        assert!(Action::SelectAllNotes.acts_on_pane());
        assert!(Action::DeleteSelection.acts_on_pane());
        assert!(!Action::TogglePlayback.acts_on_pane());
        assert!(!Action::ShowProjects.acts_on_pane());
    }

    /// The defaults never put a scoped and a global action on one chord, so
    /// the precedence is pinned on a hand-built map.
    #[test]
    fn a_scoped_action_wins_over_a_global_one() {
        let chord = Chord::plain(Key::U);
        let mut chords = default_chords();
        chords[Action::TapTempo.index()] = vec![chord];
        chords[Action::Quantize.index()] = vec![chord];
        let keymap = Keymap::from_chords(chords);
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

    /// Whether `keymap` puts no chord on two overlapping actions.
    fn conflict_free(keymap: &Keymap) -> bool {
        ACTIONS.iter().enumerate().all(|(i, a)| {
            ACTIONS[i + 1..].iter().all(|b| {
                !a.scope.overlaps(b.scope)
                    || !keymap
                        .chords(a.action)
                        .iter()
                        .any(|chord| keymap.chords(b.action).contains(chord))
            })
        })
    }

    #[test]
    fn binding_a_chord_replaces_the_whole_list() {
        let mut keymap = Keymap::default();
        let g = Chord::cmd(Key::G);
        keymap.bind(Action::Commit, g);
        assert_eq!(keymap.chords(Action::Commit), &[g]);
        assert_eq!(keymap.resolve(g, Pane::Arranger), Some(Action::Commit));
        // Both old chords are gone.
        assert_eq!(keymap.resolve(Chord::plain(Key::Slash), Pane::Clip), None);
        assert_eq!(
            keymap.resolve(Chord::plain(Key::Backslash), Pane::Clip),
            None
        );
        assert!(!keymap.is_default(Action::Commit));
    }

    #[test]
    fn a_conflict_leaves_the_loser_unbound() {
        let mut keymap = Keymap::default();
        let split = Chord::cmd(Key::E);
        assert_eq!(keymap.conflict(Action::Merge, split), Some(Action::Split));
        keymap.bind(Action::Merge, split);
        assert!(keymap.chords(Action::Split).is_empty());
        assert_eq!(keymap.resolve(split, Pane::Arranger), Some(Action::Merge));
        assert!(conflict_free(&keymap));
    }

    /// Only the chord taken goes: Commit keeps its other default.
    #[test]
    fn a_loser_with_another_chord_keeps_it() {
        let mut keymap = Keymap::default();
        keymap.bind(Action::TapTempo, Chord::plain(Key::Slash));
        assert_eq!(
            keymap.chords(Action::Commit),
            &[Chord::plain(Key::Backslash)]
        );
    }

    /// The panes don't overlap: `M` stays both mutes, and taking it in the
    /// clip view leaves the arranger's alone.
    #[test]
    fn only_overlapping_scopes_conflict() {
        let mut keymap = Keymap::default();
        let m = Chord::plain(Key::M);
        assert_eq!(
            keymap.conflict(Action::Quantize, m),
            Some(Action::MuteNotes)
        );
        keymap.bind(Action::Quantize, m);
        assert_eq!(keymap.chords(Action::MuteRange), &[m]);
        assert!(keymap.chords(Action::MuteNotes).is_empty());
        // A global action overlaps both.
        assert!(keymap.conflict(Action::TapTempo, m).is_some());
        // An action's own chord is no conflict.
        assert_eq!(keymap.conflict(Action::Quantize, m), None);
    }

    #[test]
    fn unbind_reset_and_reset_all() {
        let mut keymap = Keymap::default();
        keymap.unbind(Action::Undo);
        assert!(keymap.chords(Action::Undo).is_empty());
        assert_eq!(keymap.resolve(Chord::cmd(Key::Z), Pane::Arranger), None);
        // Resetting takes the default back from whoever got it since.
        keymap.bind(Action::Redo, Chord::cmd(Key::Z));
        keymap.reset(Action::Undo);
        assert!(keymap.is_default(Action::Undo));
        assert!(keymap.chords(Action::Redo).is_empty());
        assert!(conflict_free(&keymap));
        keymap.reset_all();
        assert!(keymap.is_all_default());
    }

    #[test]
    fn the_refused_chords() {
        for chord in [
            Chord::cmd(Key::C),
            Chord::cmd(Key::C).shift(),
            Chord::cmd(Key::X),
            Chord::cmd(Key::V),
            Chord::cmd(Key::Plus),
            Chord::cmd(Key::Minus),
            Chord::cmd(Key::Num0),
            Chord::cmd(Key::Q),
            Chord::cmd(Key::M),
            Chord::cmd(Key::H),
            Chord::cmd(Key::H).alt(),
            Chord::cmd(Key::Comma),
            Chord::plain(Key::Questionmark),
            SAVE,
            SAVE_AS,
            NEW_PROJECT,
            Chord::plain(Key::ArrowLeft),
            Chord::cmd(Key::ArrowUp),
            Chord::plain(Key::Enter),
            Chord::plain(Key::Tab),
            Chord::plain(Key::Escape),
            Chord::plain(Key::SuperRight),
            Chord::cmd(Key::ShiftLeft),
        ] {
            assert!(refusal(chord).is_some(), "{chord:?}");
        }
        for chord in [
            Chord::plain(Key::C),
            Chord::plain(Key::Comma),
            Chord::cmd(Key::G),
            Chord::cmd(Key::S).alt(),
            Chord::plain(Key::F5),
        ] {
            assert_eq!(refusal(chord), None, "{chord:?}");
        }
        // No default is refused.
        for info in ACTIONS {
            for &chord in info.defaults {
                assert_eq!(refusal(chord), None, "{:?}", info.action);
            }
        }
    }

    #[test]
    fn the_defaults_store_nothing() {
        assert!(Keymap::default().overrides().is_empty());
    }

    #[test]
    fn overrides_round_trip_through_json() {
        let mut keymap = Keymap::default();
        keymap.bind(Action::Merge, Chord::cmd(Key::E));
        keymap.bind(Action::Commit, Chord::plain(Key::Space).alt().shift());
        keymap.unbind(Action::Quantize);
        let overrides = keymap.overrides();
        // Merge, Split (it lost `⌘E`), Commit and Quantize; nothing else.
        let ids: Vec<_> = overrides.keys().map(String::as_str).collect();
        assert_eq!(
            ids,
            vec![
                "arranger.merge",
                "arranger.split",
                "capture.commit",
                "clip.quantize"
            ]
        );
        assert!(overrides["clip.quantize"].is_empty());
        let json = serde_json::to_string(&overrides).unwrap();
        let loaded: BTreeMap<String, Vec<ChordDto>> = serde_json::from_str(&json).unwrap();
        let reloaded = Keymap::from_overrides(&loaded);
        for info in ACTIONS {
            assert_eq!(
                reloaded.chords(info.action),
                keymap.chords(info.action),
                "{:?}",
                info.action
            );
        }
    }

    /// Every default key's name reads back as the key.
    #[test]
    fn every_default_key_name_parses_back() {
        for info in ACTIONS {
            for &chord in info.defaults {
                assert_eq!(ChordDto::from(chord).to_chord(), Some(chord));
            }
        }
    }

    #[test]
    fn unknown_ids_and_bad_chords_are_dropped() {
        let dto = |key: &str, command: bool| ChordDto {
            key: key.to_owned(),
            command,
            alt: false,
            shift: false,
            ctrl: false,
        };
        let overrides = BTreeMap::from([
            ("no.such.action".to_owned(), vec![dto("G", false)]),
            (
                "transport.tap_tempo".to_owned(),
                vec![dto("NoSuchKey", false), dto("C", true), dto("U", false)],
            ),
        ]);
        let keymap = Keymap::from_overrides(&overrides);
        // The unparseable and the refused chord go; the rest is kept.
        assert_eq!(keymap.chords(Action::TapTempo), &[Chord::plain(Key::U)]);
        assert_eq!(keymap.overrides().len(), 1);
    }

    /// Stored differences only: every action the user never touched gets
    /// whatever the defaults are now.
    #[test]
    fn untouched_actions_get_the_current_defaults() {
        let overrides = BTreeMap::from([(
            "transport.tap_tempo".to_owned(),
            vec![ChordDto::from(Chord::plain(Key::U))],
        )]);
        let keymap = Keymap::from_overrides(&overrides);
        for info in ACTIONS {
            if info.action != Action::TapTempo {
                assert!(keymap.is_default(info.action), "{:?}", info.action);
            }
        }
    }

    /// An edited file putting one chord on two overlapping actions: the
    /// later in table order loses it.
    #[test]
    fn a_conflicting_file_costs_the_later_action_its_chord() {
        // Merge (later) given Split's `⌘E`, Split left alone in the file.
        let overrides = BTreeMap::from([(
            "arranger.merge".to_owned(),
            vec![ChordDto::from(Chord::cmd(Key::E))],
        )]);
        let keymap = Keymap::from_overrides(&overrides);
        assert_eq!(keymap.chords(Action::Split), &[Chord::cmd(Key::E)]);
        assert!(keymap.chords(Action::Merge).is_empty());
        assert!(conflict_free(&keymap));
    }

    #[test]
    fn bare_keys_follow_a_rebinding() {
        let mut keymap = Keymap::default();
        keymap.bind(Action::TogglePlayback, Chord::plain(Key::P));
        assert_eq!(keymap.bare_keys(Action::TogglePlayback), vec![Key::P]);
        keymap.bind(Action::Stop, Chord::cmd(Key::P));
        assert!(keymap.bare_keys(Action::Stop).is_empty());
    }

    /// Every stored chord unusable (a lone modifier here, as an early
    /// recorder saved; a key a downgrade can't read): the action keeps its
    /// defaults rather than going unbound by accident.
    #[test]
    fn an_override_with_nothing_left_keeps_the_defaults() {
        let overrides = BTreeMap::from([
            (
                "transport.computer_keyboard".to_owned(),
                vec![ChordDto::from(Chord::plain(Key::SuperRight))],
            ),
            ("edit.undo".to_owned(), Vec::new()),
        ]);
        let keymap = Keymap::from_overrides(&overrides);
        assert!(keymap.is_default(Action::ToggleComputerKeyboard));
        // A list stored empty is still unbound.
        assert!(keymap.chords(Action::Undo).is_empty());
    }

    /// Each side of each of the four is a modifier; nothing else is.
    #[test]
    fn the_modifier_keys() {
        for key in [
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ControlLeft,
            Key::ControlRight,
            Key::AltLeft,
            Key::AltRight,
            Key::SuperLeft,
            Key::SuperRight,
        ] {
            assert!(is_modifier(key), "{key:?}");
        }
        assert!(!is_modifier(Key::P));
        assert!(!is_modifier(Key::Space));
    }
}
