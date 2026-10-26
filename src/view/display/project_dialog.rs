//! The two project dialogs, each centred over the canvas and holding the
//! keyboard while open — every key, nothing falls through:
//!
//! - **Save As** (⌘/Ctrl+⇧+S, and ⌘/Ctrl+S on a project never saved): a name
//!   field, the open project's name (or [`UNTITLED`]) selected, and a
//!   "Save in" dropdown for the destination folder (a click, or ↑/↓, opens
//!   its list; Enter or a click picks, Esc closes it). It starts at the
//!   browser's selected folder (see [`save_folder_default`]), else the current
//!   folder. Enter saves under the typed name there, makes it the open
//!   project and its folder the current one; Esc cancels.
//! - **Unsaved changes**: asked for by the sequencer (`UiEvent::UnsavedChanges`)
//!   when ⌘/Ctrl+N, opening a project from the browser or quitting would
//!   throw away changes. ←/→ move the selection between its buttons (Save
//!   to start with) and Enter presses the selected one; Save saves first (as
//!   ⌘/Ctrl+S would — through Save As for a project never saved, the action
//!   waiting on it). `D` (or ⌘/Ctrl+D) goes on without saving and Esc
//!   cancels, whatever is selected; each is also a button.
//!
//! Quitting goes through the same check: a window close is held
//! ([`sync_window_close`](Display::sync_window_close)) until the sequencer
//! approves it.
//!
//! This module decides what the keys and buttons do (Save As's are the text
//! fields' [`field_key`]; [`unsaved_key`], unit-tested);
//! `rendering/modals/project_dialog.rs` draws the dialogs and lays out their
//! buttons. See `060-persistence.md` § Unsaved changes.

use std::iter::once;
use std::ops::Range;

use egui::{Context, Key, Pos2, ViewportCommand, pos2};

use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::project::{ProjectAction, list_project_folders, project_name_from_input};
#[cfg(target_os = "macos")]
use crate::view::appkit::take_open_document;

use super::Display;
#[cfg(target_os = "macos")]
use super::Overlay;
use super::browser::BrowserItem;
use super::text_field::{FieldEvent, TextField, field_key};

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
        /// The button Enter presses, moved with ←/→.
        selected: UnsavedButton,
    },
}

/// Save As's name field and destination folder.
pub(super) struct SaveAs {
    /// The name field — the open project's name (or [`UNTITLED`]) to start
    /// with.
    pub(super) field: TextField,
    /// The folders ↑/↓ step through: the projects root (`None`), then each
    /// project folder. Read from disk when the dialog opens.
    folders: Vec<Option<String>>,
    /// Which of `folders` the project is saved into.
    folder_idx: usize,
    /// The folder dropdown's highlighted row while its list is open, `None`
    /// while it is closed.
    list: Option<usize>,
    /// What runs once the save is sent: the action the unsaved-changes
    /// prompt's Save was answered for, on a project never saved. Esc drops it
    /// with the dialog.
    then: Option<ProjectAction>,
}

impl SaveAs {
    /// The folder the project will be saved into, `None` for the projects
    /// root.
    pub(super) fn folder(&self) -> Option<&str> {
        self.folders[self.folder_idx].as_deref()
    }

    /// How many folders the dropdown offers.
    pub(super) fn folder_count(&self) -> usize {
        self.folders.len()
    }

    /// The dropdown's `idx`th entry as shown.
    pub(super) fn folder_label(&self, idx: usize) -> &str {
        self.folders[idx].as_deref().unwrap_or(ROOT_LABEL)
    }

    /// The chosen entry's index in the dropdown.
    pub(super) fn folder_idx(&self) -> usize {
        self.folder_idx
    }

    /// The open list's highlighted row, `None` while it is closed.
    pub(super) fn list_highlight(&self) -> Option<usize> {
        self.list
    }

    /// A press on the dropdown's box: opens the list on the chosen folder,
    /// or closes it.
    fn toggle_list(&mut self) {
        self.list = match self.list {
            Some(_) => None,
            None => Some(self.folder_idx),
        };
    }

    /// ↑/↓: opens a closed list on the chosen folder; on an open one moves
    /// the highlight `step` rows, stopping at either end.
    fn move_highlight(&mut self, step: isize) {
        let last = self.folders.len() as isize - 1;
        self.list = Some(match self.list {
            Some(row) => (row as isize + step).clamp(0, last) as usize,
            None => self.folder_idx,
        });
    }

    /// Chooses the dropdown's `idx`th folder and closes the list.
    fn choose_folder(&mut self, idx: usize) {
        self.folder_idx = idx.min(self.folders.len() - 1);
        self.list = None;
    }
}

/// What a press in Save As lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveAsHit {
    /// The dropdown's box.
    Box,
    /// This entry of the open list.
    Row(usize),
}

/// How the root of the projects folder reads in the dropdown.
const ROOT_LABEL: &str = "Projects";

/// Most rows the open dropdown list shows at once.
pub(super) const LIST_MAX_ROWS: usize = 8;

/// The rows of a `len`-row list shown while `highlight` is highlighted: at
/// most [`LIST_MAX_ROWS`], the window sliding to keep the highlight in view.
pub(super) fn list_window(highlight: usize, len: usize) -> Range<usize> {
    let start = (highlight + 1).saturating_sub(LIST_MAX_ROWS);
    start..(start + LIST_MAX_ROWS).min(len)
}

/// Where Save As starts saving: the folder the browser has selected — the
/// folder itself, or the one holding the selected project — while the panel
/// is shown (`selected` is `None` otherwise); anything else selected, or
/// nothing, leaves `current`, the open project's folder. `None` is the
/// projects root.
fn save_folder_default(selected: Option<&BrowserItem>, current: Option<&str>) -> Option<String> {
    match selected {
        Some(BrowserItem::Folder(name)) => Some(name.clone()),
        Some(BrowserItem::Project { folder, .. }) => folder.clone(),
        _ => current.map(str::to_owned),
    }
}

/// Save As's keymap: ↑/↓ open the folder list and move through it, Enter
/// saves (or picks the highlighted folder while the list is open), Esc
/// cancels (or closes the list), the rest is typing ([`field_key`]).
fn save_as_key(key: Key) -> DialogKey {
    match key {
        Key::ArrowUp => DialogKey::Folder(-1),
        Key::ArrowDown => DialogKey::Folder(1),
        _ => field_key(key).into(),
    }
}

/// What a key does while a project dialog is open.
#[derive(Debug, PartialEq)]
enum DialogKey {
    /// Save As: ↑/↓ — opens the folder list, then moves its highlight this
    /// many rows.
    Folder(isize),
    /// Save As: a press at this canvas point, on the folder dropdown or
    /// away from it.
    Click(Pos2),
    /// Save As: save under the typed name. The prompt: save, then go on.
    Confirm,
    /// The prompt: go on without saving.
    Discard,
    /// Close the dialog, doing nothing.
    Cancel,
    /// The prompt: select this button.
    Select(UnsavedButton),
    /// Typing, or a key with no meaning here: nothing else gets it.
    Swallow,
}

impl From<FieldEvent> for DialogKey {
    /// Save As's key, from the text fields' keymap ([`field_key`]): Enter
    /// saves, Esc cancels, the rest is typing.
    fn from(event: FieldEvent) -> Self {
        match event {
            FieldEvent::Commit => DialogKey::Confirm,
            FieldEvent::Cancel => DialogKey::Cancel,
            _ => DialogKey::Swallow,
        }
    }
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

    /// The button `step` places along the row, stopping at either end
    /// rather than wrapping (cf. `SettingsTab::stepped`).
    fn stepped(self, step: isize) -> UnsavedButton {
        let last = Self::ALL.len() as isize - 1;
        let idx = Self::ALL.iter().position(|&b| b == self).unwrap_or(0) as isize;
        Self::ALL[(idx + step).clamp(0, last) as usize]
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

/// The prompt's keymap with `selected` the selected button: ←/→ select the
/// one beside it, Enter presses it, `D` — bare, or ⌘/Ctrl+D as in the macOS
/// sheet — doesn't save, Esc cancels; nothing else does anything.
fn unsaved_key(key: Key, modifiers: KeyModifiers, selected: UnsavedButton) -> DialogKey {
    match key {
        Key::ArrowLeft => DialogKey::Select(selected.stepped(-1)),
        Key::ArrowRight => DialogKey::Select(selected.stepped(1)),
        Key::Enter => selected.key(),
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

    /// Once a frame (macOS): asks to open the `.stev` Finder handed over
    /// (`appkit::take_open_document`), through the unsaved-changes check,
    /// like the browser's Enter. It waits while a project dialog, a field or
    /// the plugin restore has the keyboard; Settings stays up over the
    /// opened project, as for the app menu's items. At launch it waits for
    /// the first frame, and replaces the empty startup project.
    #[cfg(target_os = "macos")]
    pub(super) fn sync_open_document(&mut self) {
        if self.keyboard_taken() && self.overlay != Some(Overlay::Settings) {
            return;
        }
        if let Some(path) = take_open_document() {
            self.request_project_action(ProjectAction::OpenFile { path });
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
        let selected = self
            .browser
            .visible
            .then(|| self.browser.tree.selected())
            .flatten();
        let default = save_folder_default(selected, self.project.project_current_folder.as_deref());
        let mut folders: Vec<Option<String>> = once(None)
            .chain(list_project_folders().into_iter().map(Some))
            .collect();
        let folder_idx = folders
            .iter()
            .position(|f| *f == default)
            .unwrap_or_else(|| {
                folders.push(default);
                folders.len() - 1
            });
        self.project_dialog = Some(ProjectDialog::SaveAs(SaveAs {
            field: TextField::new(text),
            folders,
            folder_idx,
            list: None,
            then,
        }));
    }

    /// Opens the unsaved-changes prompt before `action` — unless a dialog is
    /// already open, which can't have asked for it.
    pub(super) fn open_unsaved_prompt(&mut self, action: ProjectAction) {
        if self.project_dialog.is_none() {
            self.project_dialog = Some(ProjectDialog::Unsaved {
                action,
                selected: UnsavedButton::Save,
            });
        }
    }

    /// Saves the project under `name` in `folder`, which becomes the current
    /// folder and `name` the open project, then asks for `then`. Each live plugin's
    /// preset is captured first: those events queue ahead of
    /// `ConfirmFilename` on the same channel, so the sequencer applies them
    /// before it serializes the project — and `then` queues after it, so the
    /// sequencer runs it now that nothing is unsaved, or prompts again if the
    /// save failed.
    fn save_project_as(
        &mut self,
        name: String,
        folder: Option<String>,
        then: Option<ProjectAction>,
    ) {
        self.project.project_current_name = Some(name.clone());
        self.set_current_project_folder(folder.clone());
        #[cfg(target_os = "macos")]
        self.capture_instrument_states();
        self.input_event_tx
            .send(InputEvent::ConfirmFilename {
                filename: name,
                folder,
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
            Some(name) => {
                let folder = self.project.project_current_folder.clone();
                self.save_project_as(name, folder, then);
            }
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
            // The text fields' keymap, but no click-away commit: a modal
            // takes every press.
            (&InputEvent::KeyPressed { key, .. }, ProjectDialog::SaveAs(_)) => save_as_key(key),
            (
                &InputEvent::KeyPressed { key, modifiers },
                &ProjectDialog::Unsaved { selected, .. },
            ) => unsaved_key(key, modifiers, selected),
            (&InputEvent::MouseClicked { x, y, .. }, ProjectDialog::Unsaved { .. }) => self
                .unsaved_button_at(pos2(x, y))
                .map_or(DialogKey::Swallow, UnsavedButton::key),
            (&InputEvent::MouseClicked { x, y, .. }, ProjectDialog::SaveAs(_)) => {
                DialogKey::Click(pos2(x, y))
            }
            (
                InputEvent::Copy { .. }
                | InputEvent::Cut { .. }
                | InputEvent::Paste
                | InputEvent::MouseDoubleClicked { .. },
                _,
            ) => DialogKey::Swallow,
            _ => return false,
        };
        match key {
            DialogKey::Confirm => match self.open_folder_list() {
                Some((save_as, row)) => save_as.choose_folder(row),
                None => self.confirm_project_dialog(),
            },
            DialogKey::Discard => self.discard_project_changes(),
            DialogKey::Cancel => match self.open_folder_list() {
                Some((save_as, _)) => save_as.list = None,
                None => self.project_dialog = None,
            },
            DialogKey::Folder(step) => {
                if let Some(ProjectDialog::SaveAs(save_as)) = &mut self.project_dialog {
                    save_as.move_highlight(step);
                }
            }
            DialogKey::Click(pos) => self.click_save_as(pos),
            DialogKey::Select(button) => {
                if let Some(ProjectDialog::Unsaved { selected, .. }) = &mut self.project_dialog {
                    *selected = button;
                }
            }
            DialogKey::Swallow => {}
        }
        true
    }

    /// Save As and its highlighted row, while its folder list is open.
    fn open_folder_list(&mut self) -> Option<(&mut SaveAs, usize)> {
        match &mut self.project_dialog {
            Some(ProjectDialog::SaveAs(save_as)) => save_as.list.map(|row| (save_as, row)),
            _ => None,
        }
    }

    /// A press in Save As: the dropdown's box opens or closes the list, a row
    /// of the open list chooses that folder, anywhere else closes the list.
    fn click_save_as(&mut self, pos: Pos2) {
        let hit = self.save_as_hit_at(pos);
        let Some(ProjectDialog::SaveAs(save_as)) = &mut self.project_dialog else {
            return;
        };
        match hit {
            Some(SaveAsHit::Box) => save_as.toggle_list(),
            Some(SaveAsHit::Row(idx)) => save_as.choose_folder(idx),
            None => save_as.list = None,
        }
    }

    /// Enter, or the prompt's Save. Save As with a name that isn't one
    /// stays open (its hint says why); with one, it saves and asks for the
    /// action waiting on it, if any. The prompt saves and asks for its
    /// action again — through Save As for a project never saved.
    fn confirm_project_dialog(&mut self) {
        match self.project_dialog.take() {
            Some(ProjectDialog::SaveAs(save_as)) => {
                match project_name_from_input(&save_as.field.text) {
                    Some(name) => {
                        let folder = save_as.folder().map(str::to_owned);
                        self.save_project_as(name, folder, save_as.then);
                    }
                    None => self.project_dialog = Some(ProjectDialog::SaveAs(save_as)),
                }
            }
            Some(ProjectDialog::Unsaved { action, .. }) => self.save_project(Some(action)),
            None => {}
        }
    }

    /// `D`, or the prompt's Don't Save: runs its action, changes and all.
    fn discard_project_changes(&mut self) {
        if let Some(ProjectDialog::Unsaved { action, .. }) = self.project_dialog.take() {
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
    use crate::view::display::browser::{BrowserCategory, BrowserItem};
    use crate::view::display::text_field::TextField;

    use super::{
        DialogKey, LIST_MAX_ROWS, SaveAs, UnsavedButton, field_key, list_window, save_as_key,
        save_folder_default, unsaved_key,
    };

    /// A Save As over the root and folders `a`, `b`, `c`, starting at `idx`.
    fn save_as(idx: usize) -> SaveAs {
        SaveAs {
            field: TextField::new(String::new()),
            folders: vec![None, Some("a".into()), Some("b".into()), Some("c".into())],
            folder_idx: idx,
            list: None,
            then: None,
        }
    }

    /// The list opens on the chosen folder, ↑/↓ move the highlight (stopping
    /// at the ends) without choosing, and picking a row closes the list.
    #[test]
    fn the_folder_list_opens_moves_and_picks() {
        let mut save_as = save_as(2);
        assert_eq!(save_as.folder(), Some("b"));
        save_as.move_highlight(1);
        assert_eq!(save_as.list_highlight(), Some(2));
        save_as.move_highlight(5);
        assert_eq!(save_as.list_highlight(), Some(3));
        assert_eq!(save_as.folder(), Some("b"));
        save_as.choose_folder(0);
        assert_eq!((save_as.folder(), save_as.list_highlight()), (None, None));
        assert_eq!(save_as.folder_label(0), "Projects");
        save_as.toggle_list();
        assert_eq!(save_as.list_highlight(), Some(0));
        save_as.toggle_list();
        assert_eq!(save_as.list_highlight(), None);
    }

    /// A long list shows a window of rows that follows the highlight.
    #[test]
    fn the_list_window_follows_the_highlight() {
        assert_eq!(list_window(0, 3), 0..3);
        assert_eq!(list_window(2, 3), 0..3);
        assert_eq!(list_window(0, 20), 0..LIST_MAX_ROWS);
        assert_eq!(list_window(LIST_MAX_ROWS - 1, 20), 0..LIST_MAX_ROWS);
        assert_eq!(list_window(LIST_MAX_ROWS, 20), 1..LIST_MAX_ROWS + 1);
        assert_eq!(list_window(19, 20), 20 - LIST_MAX_ROWS..20);
    }

    /// The selected folder, or the folder of the selected project, is where
    /// Save As starts; anything else selected leaves the current folder.
    #[test]
    fn save_as_starts_in_the_selected_folder() {
        let folder = BrowserItem::Folder("b".into());
        let in_a = BrowserItem::Project {
            folder: Some("a".into()),
            name: "song".into(),
        };
        let loose = BrowserItem::Project {
            folder: None,
            name: "song".into(),
        };
        let category = BrowserItem::Category(BrowserCategory::Projects);
        let clip = BrowserItem::Clip("riff".into());
        assert_eq!(
            save_folder_default(Some(&folder), Some("a")),
            Some("b".into())
        );
        assert_eq!(
            save_folder_default(Some(&in_a), Some("c")),
            Some("a".into())
        );
        assert_eq!(save_folder_default(Some(&loose), Some("c")), None);
        assert_eq!(
            save_folder_default(Some(&category), Some("c")),
            Some("c".into())
        );
        assert_eq!(save_folder_default(Some(&clip), None), None);
        assert_eq!(save_folder_default(None, Some("a")), Some("a".into()));
        assert_eq!(save_folder_default(None, None), None);
    }

    /// ↑/↓ drive the folder list; the rest of Save As's keys are unchanged.
    #[test]
    fn save_as_arrows_drive_the_folder_list() {
        assert_eq!(save_as_key(Key::ArrowUp), DialogKey::Folder(-1));
        assert_eq!(save_as_key(Key::ArrowDown), DialogKey::Folder(1));
        assert_eq!(save_as_key(Key::Enter), DialogKey::Confirm);
        assert_eq!(save_as_key(Key::Escape), DialogKey::Cancel);
        assert_eq!(save_as_key(Key::D), DialogKey::Swallow);
    }

    /// Save As reads the text fields' keymap: Enter saves, Esc cancels, and
    /// nothing typed reaches the app — not `D`, which the prompt reads as
    /// Don't Save.
    #[test]
    fn save_as_saves_on_enter_and_cancels_on_escape() {
        assert_eq!(DialogKey::from(field_key(Key::Enter)), DialogKey::Confirm);
        assert_eq!(DialogKey::from(field_key(Key::Escape)), DialogKey::Cancel);
        for key in [Key::D, Key::Space, Key::Delete] {
            assert_eq!(DialogKey::from(field_key(key)), DialogKey::Swallow);
        }
    }

    /// Enter presses whichever button is selected; `D` — bare or ⌘/Ctrl+D —
    /// and Esc don't care which.
    #[test]
    fn enter_presses_the_selected_button() {
        let none = KeyModifiers::default();
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        for (selected, pressed) in [
            (UnsavedButton::Cancel, DialogKey::Cancel),
            (UnsavedButton::DontSave, DialogKey::Discard),
            (UnsavedButton::Save, DialogKey::Confirm),
        ] {
            assert_eq!(unsaved_key(Key::Enter, none, selected), pressed);
            assert_eq!(unsaved_key(Key::D, none, selected), DialogKey::Discard);
            assert_eq!(unsaved_key(Key::D, command, selected), DialogKey::Discard);
            assert_eq!(unsaved_key(Key::Escape, none, selected), DialogKey::Cancel);
        }
    }

    /// ←/→ step through the buttons left to right and stop at either end.
    #[test]
    fn arrows_step_the_selection_without_wrapping() {
        use UnsavedButton::{Cancel, DontSave, Save};
        for (key, from, to) in [
            (Key::ArrowLeft, Save, DontSave),
            (Key::ArrowLeft, DontSave, Cancel),
            (Key::ArrowLeft, Cancel, Cancel),
            (Key::ArrowRight, Cancel, DontSave),
            (Key::ArrowRight, DontSave, Save),
            (Key::ArrowRight, Save, Save),
        ] {
            assert_eq!(
                unsaved_key(key, KeyModifiers::default(), from),
                DialogKey::Select(to)
            );
        }
    }

    /// A stray key does nothing — above all no other key discards the
    /// changes, and ⇧D (duplicate elsewhere) isn't Don't Save.
    #[test]
    fn no_other_prompt_key_acts() {
        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        let save = UnsavedButton::Save;
        assert_eq!(unsaved_key(Key::D, shift, save), DialogKey::Swallow);
        for key in [Key::Space, Key::Delete, Key::N, Key::S] {
            assert_eq!(
                unsaved_key(key, KeyModifiers::default(), save),
                DialogKey::Swallow
            );
        }
    }
}
