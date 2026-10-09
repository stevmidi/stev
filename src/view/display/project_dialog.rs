//! The two project dialogs, each centred over the canvas and holding the
//! keyboard while open — every key, nothing falls through:
//!
//! - **Save As** (⌘/Ctrl+⇧+S, and ⌘/Ctrl+S on a project never saved): a name
//!   field, the open project's name (or [`UNTITLED`]) selected. Enter saves
//!   under the typed name in the current folder and makes it the open
//!   project; Esc cancels.
//! - **Unsaved changes**: asked for by the sequencer (`UiEvent::UnsavedChanges`)
//!   when ⌘/Ctrl+N, opening a project from the browser or quitting would
//!   throw away changes. Enter saves first (as ⌘/Ctrl+S would — through
//!   Save As for a project never saved, the action waiting on it), `D` (or
//!   ⌘/Ctrl+D) goes on without saving, Esc cancels; each is also a button.
//!
//! Quitting goes through the same check: a window close is held
//! ([`sync_window_close`](Display::sync_window_close)) until the sequencer
//! approves it.
//!
//! This module decides what the keys and buttons do ([`save_as_key`],
//! [`unsaved_key`], unit-tested); `rendering/modals/project_dialog.rs` draws
//! the dialogs and lays out their buttons. See `060-persistence.md`
//! § Unsaved changes.

use egui::{Context, Key, ViewportCommand, pos2};

use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::project::{ProjectAction, project_name_from_input};

use super::Display;

/// What Save As starts with for a project that has never been saved.
const UNTITLED: &str = "Untitled";

/// The open project dialog.
pub(super) enum ProjectDialog {
    /// The Save As name field.
    SaveAs(SaveAs),
    /// The Save / Don't Save / Cancel prompt before `action` runs.
    Unsaved {
        /// What runs after Save or Don't Save.
        action: ProjectAction,
    },
}

/// Save As's name field.
pub(super) struct SaveAs {
    /// The name as typed so far — the open project's (or [`UNTITLED`]) to
    /// start with, selected whole.
    pub(super) text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text.
    pub(super) shown: bool,
    /// What runs once the save is sent: the action the unsaved-changes
    /// prompt's Save was answered for, on a project never saved. Esc drops it
    /// with the dialog.
    then: Option<ProjectAction>,
}

/// What a key does while a project dialog is open.
#[derive(Debug, PartialEq, Eq)]
enum DialogKey {
    /// Save As: save under the typed name. The prompt: save, then go on.
    Confirm,
    /// The prompt: go on without saving.
    Discard,
    /// Close the dialog, doing nothing.
    Cancel,
    /// Typing, or a key with no meaning here: nothing else gets it.
    Swallow,
}

/// One of the prompt's three buttons, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnsavedButton {
    /// Close the prompt, doing nothing.
    Cancel,
    /// Go on without saving.
    DontSave,
    /// Save, then go on.
    Save,
}

impl UnsavedButton {
    /// Every button, in the order they sit left to right.
    pub(super) const ALL: [UnsavedButton; 3] = [
        UnsavedButton::Cancel,
        UnsavedButton::DontSave,
        UnsavedButton::Save,
    ];

    /// The button's label.
    pub(super) fn label(self) -> &'static str {
        match self {
            UnsavedButton::Cancel => "Cancel",
            UnsavedButton::DontSave => "Don't Save",
            UnsavedButton::Save => "Save",
        }
    }

    /// What clicking it does — the same as its key.
    fn key(self) -> DialogKey {
        match self {
            UnsavedButton::Cancel => DialogKey::Cancel,
            UnsavedButton::DontSave => DialogKey::Discard,
            UnsavedButton::Save => DialogKey::Confirm,
        }
    }
}

/// Save As's keymap: Enter saves, Esc cancels, every other key is the text
/// field's alone.
fn save_as_key(key: Key) -> DialogKey {
    match key {
        Key::Enter => DialogKey::Confirm,
        Key::Escape => DialogKey::Cancel,
        _ => DialogKey::Swallow,
    }
}

/// The prompt's keymap: Enter saves, `D` — bare, or ⌘/Ctrl+D as in the
/// macOS sheet — doesn't, Esc cancels; nothing else does anything.
fn unsaved_key(key: Key, modifiers: KeyModifiers) -> DialogKey {
    match key {
        Key::Enter => DialogKey::Confirm,
        Key::D if !modifiers.shift && !modifiers.alt => DialogKey::Discard,
        Key::Escape => DialogKey::Cancel,
        _ => DialogKey::Swallow,
    }
}

impl Display {
    /// Asks the sequencer to run `action`, which it does at once unless the
    /// project has unsaved changes (then it asks for the prompt). Ignored
    /// while a project dialog is open. Plugin presets aren't captured first:
    /// the check leaves them out (`ProjectData::fingerprint`).
    pub(super) fn request_project_action(&mut self, action: ProjectAction) {
        if self.project_dialog.is_some() {
            return;
        }
        self.input_event_tx
            .send(InputEvent::ProjectAction {
                action,
                discard_changes: false,
            })
            .ok();
    }

    /// Once a frame, after the sequencer's replies: holds a window close —
    /// the close button, or ⌘Q, which macOS routes to it (`appkit.rs`) —
    /// for the unsaved-changes check, and closes the window once quitting
    /// is approved (`UiEvent::QuitApproved`). An open Save As gives way to
    /// the close; an open prompt keeps it held.
    pub(super) fn sync_window_close(&mut self, ctx: &Context) {
        if self.quit_approved {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if matches!(self.project_dialog, Some(ProjectDialog::SaveAs(_))) {
                self.project_dialog = None;
            }
            self.request_project_action(ProjectAction::Quit);
        }
    }

    /// Once a frame: when a project dialog opens, brings the main window to
    /// the front and gives it the keyboard — the dialog may have been asked
    /// for from a plugin editor (⌘Q) — and wakes the next frame either way
    /// it changes, whose plugin pump lowers the floating editors under the
    /// dialog, or floats them again (`InstrumentEditor::pump`, macOS). The raise
    /// waits for that next frame: this frame's pump ran before `ui`, so the
    /// editors still float now, and a window raised under them stays there.
    pub(super) fn sync_project_dialog_window(&mut self, ctx: &Context) {
        if std::mem::take(&mut self.raise_main_window) {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        let open = self.project_dialog.is_some();
        if open == self.project_dialog_shown {
            return;
        }
        self.project_dialog_shown = open;
        self.raise_main_window = open;
        ctx.request_repaint();
    }

    /// Opens Save As with the open project's name — or [`UNTITLED`] for one
    /// never saved — selected; `then` runs after the save.
    pub(super) fn open_save_as(&mut self, then: Option<ProjectAction>) {
        let text = self
            .project
            .project_current_name
            .clone()
            .unwrap_or_else(|| UNTITLED.to_owned());
        self.project_dialog = Some(ProjectDialog::SaveAs(SaveAs {
            text,
            shown: false,
            then,
        }));
    }

    /// Opens the unsaved-changes prompt before `action` — unless a dialog is
    /// already open, which can't have asked for it.
    pub(super) fn open_unsaved_prompt(&mut self, action: ProjectAction) {
        if self.project_dialog.is_none() {
            self.project_dialog = Some(ProjectDialog::Unsaved { action });
        }
    }

    /// Saves the project under `name` in the current folder, which makes
    /// `name` the open project, then asks for `then`. Each live plugin's
    /// preset is captured first: those events queue ahead of
    /// `ConfirmFilename` on the same channel, so the sequencer applies them
    /// before it serializes the project — and `then` queues after it, so the
    /// sequencer runs it now that nothing is unsaved, or prompts again if the
    /// save failed.
    fn save_project_as(&mut self, name: String, then: Option<ProjectAction>) {
        self.project.project_current_name = Some(name.clone());
        #[cfg(target_os = "macos")]
        self.capture_instrument_states();
        self.input_event_tx
            .send(InputEvent::ConfirmFilename {
                filename: name,
                folder: self.project.project_current_folder.clone(),
            })
            .ok();
        if let Some(action) = then {
            self.request_project_action(action);
        }
    }

    /// Saves under the open project's name — or opens Save As for a project
    /// never saved — then asks for `then`: ⌘/Ctrl+S (`None`), the prompt's
    /// Save (its action).
    pub(super) fn save_project(&mut self, then: Option<ProjectAction>) {
        match self.project.project_current_name.clone() {
            Some(name) => self.save_project_as(name, then),
            None => self.open_save_as(then),
        }
    }

    /// While a project dialog is open it gets first look at every event: it
    /// takes every key and the clipboard chords (Save As's text field reads
    /// them on its own), and every press — on a prompt button, that button's
    /// work. Returns whether the event was consumed.
    pub(super) fn handle_project_dialog_input_event(&mut self, event: &InputEvent) -> bool {
        let Some(dialog) = &self.project_dialog else {
            return false;
        };
        let key = match (event, dialog) {
            (&InputEvent::KeyPressed { key, .. }, ProjectDialog::SaveAs(_)) => save_as_key(key),
            (&InputEvent::KeyPressed { key, modifiers }, ProjectDialog::Unsaved { .. }) => {
                unsaved_key(key, modifiers)
            }
            (&InputEvent::MouseClicked { x, y, .. }, ProjectDialog::Unsaved { .. }) => self
                .unsaved_button_at(pos2(x, y))
                .map_or(DialogKey::Swallow, UnsavedButton::key),
            (InputEvent::MouseClicked { .. }, ProjectDialog::SaveAs(_))
            | (
                InputEvent::Copy { .. }
                | InputEvent::Cut { .. }
                | InputEvent::Paste
                | InputEvent::MouseDoubleClicked { .. },
                _,
            ) => DialogKey::Swallow,
            _ => return false,
        };
        match key {
            DialogKey::Confirm => self.confirm_project_dialog(),
            DialogKey::Discard => self.discard_project_changes(),
            DialogKey::Cancel => self.project_dialog = None,
            DialogKey::Swallow => {}
        }
        true
    }

    /// Enter, or the prompt's Save. Save As with a name that isn't one
    /// stays open (its hint says why); with one, it saves and asks for the
    /// action waiting on it, if any. The prompt saves and asks for its
    /// action again — through Save As for a project never saved.
    fn confirm_project_dialog(&mut self) {
        match self.project_dialog.take() {
            Some(ProjectDialog::SaveAs(save_as)) => match project_name_from_input(&save_as.text) {
                Some(name) => self.save_project_as(name, save_as.then),
                None => self.project_dialog = Some(ProjectDialog::SaveAs(save_as)),
            },
            Some(ProjectDialog::Unsaved { action }) => self.save_project(Some(action)),
            None => {}
        }
    }

    /// `D`, or the prompt's Don't Save: runs its action, changes and all.
    fn discard_project_changes(&mut self) {
        if let Some(ProjectDialog::Unsaved { action }) = self.project_dialog.take() {
            self.input_event_tx
                .send(InputEvent::ProjectAction {
                    action,
                    discard_changes: true,
                })
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use crate::core::input_event::KeyModifiers;

    use super::{DialogKey, save_as_key, unsaved_key};

    #[test]
    fn save_as_saves_on_enter_and_cancels_on_escape() {
        assert_eq!(save_as_key(Key::Enter), DialogKey::Confirm);
        assert_eq!(save_as_key(Key::Escape), DialogKey::Cancel);
    }

    /// Nothing typed into the name reaches the app — not `D` (which the
    /// prompt reads as Don't Save), Space (play) or Delete.
    #[test]
    fn every_other_save_as_key_is_typing() {
        for key in [Key::D, Key::Space, Key::Delete, Key::Backspace, Key::S] {
            assert_eq!(save_as_key(key), DialogKey::Swallow);
        }
    }

    #[test]
    fn the_prompt_saves_on_enter_discards_on_d_and_cancels_on_escape() {
        let none = KeyModifiers::default();
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        assert_eq!(unsaved_key(Key::Enter, none), DialogKey::Confirm);
        assert_eq!(unsaved_key(Key::D, none), DialogKey::Discard);
        assert_eq!(unsaved_key(Key::D, command), DialogKey::Discard);
        assert_eq!(unsaved_key(Key::Escape, none), DialogKey::Cancel);
    }

    /// A stray key does nothing — above all no other key discards the
    /// changes, and ⇧D (duplicate elsewhere) isn't Don't Save.
    #[test]
    fn no_other_prompt_key_acts() {
        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        assert_eq!(unsaved_key(Key::D, shift), DialogKey::Swallow);
        for key in [Key::Space, Key::Delete, Key::N, Key::S] {
            assert_eq!(
                unsaved_key(key, KeyModifiers::default()),
                DialogKey::Swallow
            );
        }
    }
}
