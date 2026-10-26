//! The two project dialogs, each centred over the canvas and holding the
//! keyboard while open — every key, nothing falls through:
//!
//! - **Save As** (⌘/Ctrl+⇧+S, and ⌘/Ctrl+S on a project never saved): a name
//!   field, the open project's name (or [`UNTITLED`]) selected, and a
//!   "Save in" dropdown for the destination folder (a click, or ↑/↓ or Space
//!   while it has the keyboard, opens its list; Enter or a click picks, Esc
//!   closes it), with a `+` beside it
//!   (or the new-folder chord, ⌘/Ctrl+⇧+N) that turns the box into a field
//!   for a new folder's name (Enter chooses it, Esc goes back; the folder is
//!   made by the save, so a cancelled Save As leaves nothing on disk). Tab
//!   moves the keyboard name → dropdown → `+` (Enter or Space presses it),
//!   or between the name and the new folder's field while that is open. It
//!   starts at the browser's selected folder (see [`save_folder_default`]),
//!   else the current folder. Enter saves under the typed name there, makes
//!   it the open project and its folder the current one; Esc cancels.
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
use crate::core::keymap::Action;
use crate::core::project::{
    ProjectAction, list_project_folders, project_folder_name_from_input, project_name_from_input,
};
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
    /// project folder. Read from disk when the dialog opens; a new folder
    /// chosen in [`folder_field`](Self::folder_field) is added at the end,
    /// made only when the project is saved into it.
    folders: Vec<Option<String>>,
    /// Which of `folders` the project is saved into.
    folder_idx: usize,
    /// The folder dropdown's highlighted row while its list is open, `None`
    /// while it is closed.
    list: Option<usize>,
    /// The new folder's name field over the dropdown's box, while it is
    /// open.
    pub(super) folder_field: Option<TextField>,
    /// What has the keyboard: moved by Tab, a press, and the new folder's
    /// field opening (it takes it) and closing (the name gets it back).
    pub(super) focus: SaveAsFocus,
    /// What runs once the save is sent: the action the unsaved-changes
    /// prompt's Save was answered for, on a project never saved. Esc drops it
    /// with the dialog.
    then: Option<ProjectAction>,
}

/// What has Save As's keyboard — its Tab stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveAsFocus {
    /// The name field.
    Name,
    /// The folder dropdown, which ↑/↓ and Space drive. Not a stop while the
    /// new folder's field covers it.
    Dropdown,
    /// The `+`, which Enter and Space press. Not a stop while the new
    /// folder's field is open.
    NewFolder,
    /// The new folder's field, while it is open.
    Folder,
}

/// Save As's Tab stops, in order: the name, the dropdown and the `+` — or,
/// while the new folder's field is open, the name and that field.
fn focus_stops(folder_field_open: bool) -> &'static [SaveAsFocus] {
    if folder_field_open {
        &[SaveAsFocus::Name, SaveAsFocus::Folder]
    } else {
        &[
            SaveAsFocus::Name,
            SaveAsFocus::Dropdown,
            SaveAsFocus::NewFolder,
        ]
    }
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
    /// the highlight `step` rows, stopping at either end. Only while the
    /// dropdown has the keyboard.
    fn move_highlight(&mut self, step: isize) {
        if self.focus != SaveAsFocus::Dropdown {
            return;
        }
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

    /// The `+`, or the new-folder chord: closes the list and opens the new
    /// folder's field, empty, with the keyboard — or, already open, gives it
    /// the keyboard.
    fn open_folder_field(&mut self) {
        self.list = None;
        self.folder_field
            .get_or_insert_with(|| TextField::new(String::new()));
        self.focus = SaveAsFocus::Folder;
    }

    /// Closes the new folder's field, choosing nothing; the name gets the
    /// keyboard.
    fn close_folder_field(&mut self) {
        self.folder_field = None;
        self.focus = SaveAsFocus::Name;
    }

    /// Tab (`step` 1) or ⇧Tab (-1): moves the keyboard to the next or
    /// previous of the [`focus_stops`], round from the last to the first.
    /// Closes the list.
    fn switch_focus(&mut self, step: isize) {
        self.list = None;
        let stops = focus_stops(self.folder_field.is_some());
        let idx = stops.iter().position(|&f| f == self.focus).unwrap_or(0) as isize;
        self.focus = stops[(idx + step).rem_euclid(stops.len() as isize) as usize];
    }

    /// Space: opens or closes the list while the dropdown has the keyboard,
    /// presses the `+` while it has it; nothing on a field, which types it.
    fn press(&mut self) {
        match self.focus {
            SaveAsFocus::Dropdown => self.toggle_list(),
            SaveAsFocus::NewFolder => self.open_folder_field(),
            SaveAsFocus::Name | SaveAsFocus::Folder => {}
        }
    }

    /// Closes the new folder's field, choosing the folder typed in it: one
    /// already listed under that name (in any case — the volume may not tell
    /// them apart), else a new entry. A blank name chooses nothing; one that
    /// isn't a folder name ([`project_folder_name_from_input`]) keeps the
    /// field open, its note saying why.
    fn commit_folder_field(&mut self) {
        let Some(field) = &self.folder_field else {
            return;
        };
        if !field.text.trim().is_empty() {
            let Some(name) = project_folder_name_from_input(&field.text) else {
                return;
            };
            let key = name.to_lowercase();
            let listed = self
                .folders
                .iter()
                .position(|f| f.as_ref().is_some_and(|f| f.to_lowercase() == key));
            self.folder_idx = listed.unwrap_or_else(|| {
                self.folders.push(Some(name));
                self.folders.len() - 1
            });
        }
        self.close_folder_field();
    }

    /// Enter, while the list is open, the `+` has the keyboard or the new
    /// folder's field is open: picks the highlighted folder, presses the `+`,
    /// or takes the typed folder. Returns whether that was all — else Enter
    /// saves. From the name field, the typed folder is taken and the project
    /// saved into it; a refused folder name stops the save and hands the
    /// folder's field the keyboard.
    fn enter(&mut self) -> bool {
        if let Some(row) = self.list {
            self.choose_folder(row);
        } else if self.focus == SaveAsFocus::NewFolder {
            self.open_folder_field();
        } else if self.folder_field.is_some() {
            let from_name = self.focus == SaveAsFocus::Name;
            self.commit_folder_field();
            if self.folder_field.is_some() {
                self.focus = SaveAsFocus::Folder;
            } else if from_name {
                return false;
            }
        } else {
            return false;
        }
        true
    }

    /// Esc, while the new folder's field or the list is open: closes it,
    /// choosing nothing. Returns whether it did — else Esc cancels.
    fn escape(&mut self) -> bool {
        if self.folder_field.is_some() {
            self.close_folder_field();
            true
        } else {
            self.list.take().is_some()
        }
    }
}

/// What a press in Save As lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveAsHit {
    /// The dropdown's box.
    Box,
    /// This entry of the open list.
    Row(usize),
    /// The `+` beside the box.
    NewFolder,
    /// The name field.
    Name,
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

/// Save As's keymap: ↑/↓ open the folder list and move through it, the
/// new-folder chord (`new_folder`, the keymap's [`Action::NewProjectFolder`])
/// opens the new folder's field, Tab (⇧Tab) moves the keyboard to the next
/// (previous) stop ([`focus_stops`]), Space presses the dropdown or the `+`,
/// Enter saves (or picks the highlighted folder, presses the `+`, or takes
/// the typed folder), Esc
/// cancels (or closes the list or the field), the rest is typing
/// ([`field_key`]).
fn save_as_key(key: Key, modifiers: KeyModifiers, new_folder: bool) -> DialogKey {
    match key {
        _ if new_folder => DialogKey::NewFolder,
        Key::Tab => DialogKey::SwitchFocus(if modifiers.shift { -1 } else { 1 }),
        Key::Space => DialogKey::Press,
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
    /// Save As: open the new folder's field.
    NewFolder,
    /// Save As: move the keyboard this many Tab stops on.
    SwitchFocus(isize),
    /// Save As: Space — press the dropdown or the `+`, whichever has the
    /// keyboard.
    Press,
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
            folder_field: None,
            focus: SaveAsFocus::Name,
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
            (&InputEvent::KeyPressed { key, modifiers }, ProjectDialog::SaveAs(_)) => {
                let action = self
                    .keymap
                    .resolve_press(key, modifiers, self.focused_pane());
                save_as_key(key, modifiers, action == Some(Action::NewProjectFolder))
            }
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
            DialogKey::Confirm => {
                if !self.save_as_mut().is_some_and(SaveAs::enter) {
                    self.confirm_project_dialog();
                }
            }
            DialogKey::Discard => self.discard_project_changes(),
            DialogKey::Cancel => {
                if !self.save_as_mut().is_some_and(SaveAs::escape) {
                    self.project_dialog = None;
                }
            }
            DialogKey::Folder(step) => {
                if let Some(save_as) = self.save_as_mut() {
                    save_as.move_highlight(step);
                }
            }
            DialogKey::NewFolder => {
                if let Some(save_as) = self.save_as_mut() {
                    save_as.open_folder_field();
                }
            }
            DialogKey::SwitchFocus(step) => {
                if let Some(save_as) = self.save_as_mut() {
                    save_as.switch_focus(step);
                }
            }
            DialogKey::Press => {
                if let Some(save_as) = self.save_as_mut() {
                    save_as.press();
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

    /// The open Save As, if that is the dialog.
    fn save_as_mut(&mut self) -> Option<&mut SaveAs> {
        match &mut self.project_dialog {
            Some(ProjectDialog::SaveAs(save_as)) => Some(save_as),
            _ => None,
        }
    }

    /// A press in Save As: the dropdown's box opens or closes the list, a row
    /// of the open list chooses that folder — either giving the dropdown the
    /// keyboard — the `+` opens the new folder's field, the name field gets
    /// the keyboard back, anywhere else closes the list. While that field is open, a press on it or on the name field
    /// gives that field the keyboard, and a press anywhere else takes the
    /// typed folder.
    fn click_save_as(&mut self, pos: Pos2) {
        let hit = self.save_as_hit_at(pos);
        let Some(save_as) = self.save_as_mut() else {
            return;
        };
        match hit {
            Some(SaveAsHit::Box) if save_as.folder_field.is_some() => {
                save_as.focus = SaveAsFocus::Folder
            }
            Some(SaveAsHit::Name) => {
                save_as.list = None;
                save_as.focus = SaveAsFocus::Name;
            }
            _ if save_as.folder_field.is_some() => save_as.commit_folder_field(),
            Some(SaveAsHit::Box) => {
                save_as.focus = SaveAsFocus::Dropdown;
                save_as.toggle_list();
            }
            Some(SaveAsHit::Row(idx)) => {
                save_as.focus = SaveAsFocus::Dropdown;
                save_as.choose_folder(idx);
            }
            Some(SaveAsHit::NewFolder) => save_as.open_folder_field(),
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
        DialogKey, LIST_MAX_ROWS, SaveAs, SaveAsFocus, UnsavedButton, field_key, list_window,
        save_as_key, save_folder_default, unsaved_key,
    };

    /// A Save As over the root and folders `a`, `b`, `c`, starting at `idx`.
    fn save_as(idx: usize) -> SaveAs {
        SaveAs {
            field: TextField::new(String::new()),
            folders: vec![None, Some("a".into()), Some("b".into()), Some("c".into())],
            folder_idx: idx,
            list: None,
            folder_field: None,
            focus: SaveAsFocus::Name,
            then: None,
        }
    }

    /// `save_as` with `text` typed in its new folder's field.
    fn typed_folder(mut save_as: SaveAs, text: &str) -> SaveAs {
        save_as.open_folder_field();
        save_as.folder_field.as_mut().unwrap().text = text.to_owned();
        save_as
    }

    /// A new folder's name is chosen as a new entry at the end of the list,
    /// to be made by the save; a listed folder's name, in any case, chooses
    /// that folder rather than adding it again.
    #[test]
    fn a_typed_folder_is_chosen_new_or_listed() {
        let mut new = typed_folder(save_as(0), "  Album ");
        assert!(new.enter());
        assert_eq!(new.folder(), Some("Album"));
        assert_eq!(new.folder_count(), 5);
        assert!(new.folder_field.is_none());

        let mut listed = typed_folder(save_as(0), "B");
        assert!(listed.enter());
        assert_eq!((listed.folder(), listed.folder_count()), (Some("b"), 4));
    }

    /// A blank name closes the field choosing nothing; a name that isn't a
    /// folder name — the clips folder's too — keeps it open; Esc closes it
    /// and keeps the folder that was chosen.
    #[test]
    fn a_blank_or_refused_folder_name_chooses_nothing() {
        let mut blank = typed_folder(save_as(2), "  ");
        assert!(blank.enter());
        assert_eq!((blank.folder(), blank.folder_count()), (Some("b"), 4));
        assert!(blank.folder_field.is_none());

        for refused in ["a/b", "Clips", ".hidden"] {
            let mut save_as = typed_folder(save_as(2), refused);
            assert!(save_as.enter());
            assert!(save_as.folder_field.is_some(), "{refused}");
            assert_eq!(save_as.folder(), Some("b"));
        }

        let mut escaped = typed_folder(save_as(2), "Album");
        assert!(escaped.escape());
        assert_eq!((escaped.folder(), escaped.folder_count()), (Some("b"), 4));
        assert!(!escaped.escape());
    }

    /// ↑/↓ and Space drive the list only while the dropdown has the
    /// keyboard, and Tab away closes it; Space presses the `+` too.
    #[test]
    fn only_the_dropdown_with_the_keyboard_opens_its_list() {
        let mut save_as = save_as(1);
        save_as.move_highlight(1);
        save_as.press();
        assert_eq!(save_as.list_highlight(), None);
        save_as.switch_focus(1);
        save_as.move_highlight(1);
        assert_eq!(save_as.list_highlight(), Some(1));
        save_as.switch_focus(1);
        assert_eq!(save_as.list_highlight(), None);
        save_as.switch_focus(-1);
        save_as.press();
        assert_eq!(save_as.list_highlight(), Some(1));
        save_as.press();
        assert_eq!(save_as.list_highlight(), None);
        save_as.switch_focus(1);
        save_as.press();
        assert!(save_as.folder_field.is_some());
    }

    /// The field closes the list, and holds the arrows while it is open;
    /// with nothing open, Enter and Esc are the dialog's.
    #[test]
    fn the_folder_field_takes_enter_escape_and_the_arrows() {
        let mut save_as = save_as(1);
        assert!(!save_as.enter());
        save_as.focus = SaveAsFocus::Dropdown;
        save_as.move_highlight(1);
        save_as.open_folder_field();
        assert_eq!(save_as.list_highlight(), None);
        save_as.move_highlight(1);
        assert_eq!(save_as.list_highlight(), None);
        assert!(save_as.escape());
        assert!(!save_as.escape());
    }

    /// Tab goes name → dropdown → `+` and round, ⇧Tab the other way; the
    /// `+`'s Enter opens the new folder's field with the keyboard; while that
    /// is open, Tab moves between it and the name, and closing it gives the
    /// name the keyboard back.
    #[test]
    fn tab_moves_through_the_name_the_dropdown_and_the_plus() {
        use SaveAsFocus::{Dropdown, Folder, Name, NewFolder};
        let mut save_as = save_as(1);
        for focus in [Dropdown, NewFolder, Name] {
            save_as.switch_focus(1);
            assert_eq!(save_as.focus, focus);
        }
        for focus in [NewFolder, Dropdown, Name] {
            save_as.switch_focus(-1);
            assert_eq!(save_as.focus, focus);
        }
        save_as.switch_focus(-1);
        assert!(save_as.enter());
        assert!(save_as.folder_field.is_some());
        assert_eq!(save_as.focus, Folder);
        save_as.switch_focus(1);
        assert_eq!(save_as.focus, Name);
        save_as.switch_focus(1);
        assert_eq!(save_as.focus, Folder);
        save_as.switch_focus(1);
        save_as.open_folder_field();
        assert_eq!(save_as.focus, Folder);
        assert!(save_as.escape());
        assert_eq!(save_as.focus, Name);
    }

    /// Enter from the name field takes the typed folder and goes on to save;
    /// a refused folder name stops it and hands that field the keyboard. From
    /// the folder's field, Enter only takes the folder.
    #[test]
    fn enter_from_the_name_field_takes_the_folder_and_saves() {
        let mut from_name = typed_folder(save_as(0), "Album");
        from_name.switch_focus(1);
        assert!(!from_name.enter());
        assert_eq!(from_name.folder(), Some("Album"));

        let mut refused = typed_folder(save_as(0), "a/b");
        refused.switch_focus(1);
        assert!(refused.enter());
        assert!(refused.folder_field.is_some());
        assert_eq!(refused.focus, SaveAsFocus::Folder);

        let mut from_folder = typed_folder(save_as(0), "Album");
        assert!(from_folder.enter());
        assert_eq!(from_folder.folder(), Some("Album"));
    }

    /// The list opens on the chosen folder, ↑/↓ move the highlight (stopping
    /// at the ends) without choosing, and picking a row closes the list.
    #[test]
    fn the_folder_list_opens_moves_and_picks() {
        let mut save_as = save_as(2);
        save_as.focus = SaveAsFocus::Dropdown;
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

    /// ↑/↓ drive the folder list, the new-folder chord opens the new
    /// folder's field, Tab and ⇧Tab move the keyboard on and back and Space
    /// presses; the rest of Save As's keys are unchanged.
    #[test]
    fn save_as_arrows_drive_the_folder_list() {
        let none = KeyModifiers::default();
        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            save_as_key(Key::ArrowUp, none, false),
            DialogKey::Folder(-1)
        );
        assert_eq!(
            save_as_key(Key::ArrowDown, none, false),
            DialogKey::Folder(1)
        );
        assert_eq!(save_as_key(Key::Enter, none, false), DialogKey::Confirm);
        assert_eq!(save_as_key(Key::Escape, none, false), DialogKey::Cancel);
        assert_eq!(save_as_key(Key::D, none, false), DialogKey::Swallow);
        assert_eq!(save_as_key(Key::N, none, false), DialogKey::Swallow);
        assert_eq!(save_as_key(Key::N, none, true), DialogKey::NewFolder);
        assert_eq!(
            save_as_key(Key::Tab, none, false),
            DialogKey::SwitchFocus(1)
        );
        assert_eq!(
            save_as_key(Key::Tab, shift, false),
            DialogKey::SwitchFocus(-1)
        );
        assert_eq!(save_as_key(Key::Space, none, false), DialogKey::Press);
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
