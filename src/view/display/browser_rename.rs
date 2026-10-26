//! The browser panel's rename field: an inline text field over a project's,
//! clip's or folder's row, opened by ⌘R while the panel has the keyboard —
//! and, empty, over the new folder's row, opened by ⌘⇧N from anywhere or the
//! `+` on the Projects row. While it is open it takes the keyboard — every
//! key, nothing falls through. Enter renames the file or folder on disk (or
//! makes the new folder), Esc cancels, a click away commits (and then acts
//! as usual). A blank or unchanged name changes nothing; a name the file
//! system would trip over, or one already taken, is refused with a footer
//! message. Renaming the open project, or the current folder, renames what
//! ⌘S saves to. See `020-views-and-state.md` § Views.
//!
//! The field and its keymap are the app's one text field ([`TextField`],
//! [`field_event`]); this module decides where it goes and what a commit
//! does.

use egui::{FontId, Id, Rect, Ui, UiBuilder, pos2, vec2};

use crate::core::config::PROJECT_NAME_MAX_CHARS;
use crate::core::input_event::InputEvent;
use crate::core::project::{
    create_project_folder, project_name_from_input, rename_clip, rename_project,
    rename_project_folder,
};

use super::Display;
use super::browser::{BROWSER_FONT_SIZE, BrowserItem, browser_text_x};
use super::pane::KeyFocus;
use super::status_message::StatusMessage;
use super::text_field::{FieldEvent, FieldStyle, ROW_MARGIN, TextField, field_event, field_rect};

/// Right inset of the field from the panel's edge.
const FIELD_RIGHT_X: f32 = 6.0;

/// The open rename field.
pub(super) struct BrowserRename {
    /// The project, `.mid` or folder row being renamed, or the new folder's.
    item: BrowserItem,
    /// The field — the row's name to start with, empty for a new folder.
    field: TextField,
}

impl Display {
    /// ⌘R in the panel: opens the field over the selected row, when that is
    /// a project, a `.mid` file or a folder; nothing on any other row.
    pub(super) fn open_browser_rename(&mut self) {
        let tree = &mut self.browser.tree;
        let Some(item) = tree.selected().cloned() else {
            return;
        };
        let Some(text) = item.renamable_name().map(str::to_owned) else {
            return;
        };
        tree.delete_armed = false;
        self.browser.rename = Some(BrowserRename {
            item,
            field: TextField::new(text),
        });
    }

    /// ⌘⇧N, or a press on the `+` of the Projects row: shows the panel with
    /// the keyboard, lists the new folder's row first under Projects and
    /// opens the empty field over it. The folder is made on Enter; Esc, a
    /// blank name or the row leaving the screen makes nothing.
    pub(super) fn open_new_folder_field(&mut self) {
        if self.browser.visible {
            self.key_focus = KeyFocus::Browser;
        } else {
            self.show_browser();
        }
        let tree = &mut self.browser.tree;
        tree.delete_armed = false;
        tree.set_new_folder(true);
        let row = tree.index_of(&BrowserItem::NewFolder);
        self.browser.rename = Some(BrowserRename {
            item: BrowserItem::NewFolder,
            field: TextField::new(String::new()),
        });
        if let Some(row) = row {
            self.scroll_browser_to_row(row);
        }
    }

    /// Closes the field, taking the new folder's row away with it.
    fn close_browser_rename(&mut self) -> Option<BrowserRename> {
        self.browser.tree.set_new_folder(false);
        self.browser.rename.take()
    }

    /// Where the field goes, in window space (the panel isn't shifted), grown
    /// to fit its caret ([`field_rect`]) —
    /// `None` when its row isn't on screen (the panel hidden, the row
    /// scrolled away or gone after a reload). The one validity rule, shared
    /// by drawing, the click test and
    /// [`drop_stale_browser_rename`](Self::drop_stale_browser_rename).
    fn browser_rename_rect(&self) -> Option<Rect> {
        let rename = self.browser.rename.as_ref()?;
        // Only while its row is wholly in the list: the field isn't clipped.
        let (idx, row) = self.browser_visible_row(&rename.item)?;
        let field = Rect::from_min_max(
            pos2(
                row.min.x + browser_text_x(&self.browser.tree.rows()[idx]) - 4.0,
                row.min.y + 1.0,
            ),
            pos2(row.max.x - FIELD_RIGHT_X, row.max.y - 1.0),
        );
        Some(field_rect(field, BROWSER_FONT_SIZE))
    }

    /// Once a frame, before input: closes the field, renaming nothing, when
    /// its row is off screen — so it never takes the keyboard unseen.
    pub(super) fn drop_stale_browser_rename(&mut self) {
        if self.browser.rename.is_some() && self.browser_rename_rect().is_none() {
            self.close_browser_rename();
        }
    }

    /// While the field is open it gets first look at every event
    /// ([`field_event`]; the typing itself egui's text field reads on its
    /// own). Returns whether the event was consumed.
    pub(super) fn handle_browser_rename_input_event(&mut self, event: &InputEvent) -> bool {
        if self.browser.rename.is_none() {
            return false;
        }
        // Events are in canvas space, where the panel sits left of x = 0.
        let field = self
            .browser_rename_rect()
            .map(|rect| rect.translate(vec2(-self.browser_width(), 0.0)));
        let action = field_event(event, field);
        match action {
            FieldEvent::Commit | FieldEvent::CommitAndPass => self.commit_browser_rename(),
            FieldEvent::Cancel => {
                self.close_browser_rename();
            }
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Closes the field, renaming the file or folder to what was typed —
    /// or making the new folder — trimmed and checked by
    /// [`project_name_from_input`], the rule Save As names a project by. A
    /// blank or unchanged name changes nothing. The renamed or new row is
    /// selected.
    fn commit_browser_rename(&mut self) {
        let Some(rename) = self.close_browser_rename() else {
            return;
        };
        if rename.field.text.trim().is_empty() {
            return;
        }
        // `None` for the new folder, which has no name yet.
        let old = rename.item.renamable_name();
        let failed = |why: &str| match old {
            Some(old) => format!("Could not rename {old}: {why}"),
            None => format!("Could not create the folder: {why}"),
        };
        let Some(new) = project_name_from_input(&rename.field.text) else {
            self.render.status = Some(StatusMessage::new(failed("not a valid name")));
            return;
        };
        if old == Some(new.as_str()) {
            return;
        }
        let result = match &rename.item {
            BrowserItem::NewFolder => create_project_folder(&new),
            BrowserItem::Folder(old) => rename_project_folder(old, &new),
            BrowserItem::Project { folder, name } => rename_project(folder.as_deref(), name, &new),
            BrowserItem::Clip(old) => rename_clip(old, &new),
            _ => return,
        };
        let message = match result {
            Ok(()) => {
                self.follow_browser_rename(&rename.item, &new);
                match old {
                    Some(old) => format!("Renamed {old} to {new}"),
                    None => format!("Created the folder {new}"),
                }
            }
            Err(e) => failed(&e.to_string()),
        };
        self.render.status = Some(StatusMessage::new(message));
    }

    /// After `item` was renamed to `new` on disk (or the new folder made):
    /// the open project, renamed, is what ⌘S saves to; the current folder,
    /// renamed, stays current; a renamed folder stays expanded. Then
    /// re-lists the tree and selects the row under its new name.
    fn follow_browser_rename(&mut self, item: &BrowserItem, new: &str) {
        match item {
            BrowserItem::Project { folder, name }
                if *folder == self.project.project_current_folder
                    && self.project.project_current_name.as_ref() == Some(name) =>
            {
                self.project.project_current_name = Some(new.to_owned());
            }
            BrowserItem::Folder(old) => {
                if self.project.project_current_folder.as_ref() == Some(old) {
                    self.set_current_project_folder(Some(new.to_owned()));
                }
                self.browser.tree.carry_folder_expansion(old, new);
            }
            _ => {}
        }
        self.reload_browser();
        if let Some(renamed) = item.renamed(new.to_owned()) {
            self.browser.tree.select(renamed);
            self.scroll_browser_to_selection();
        }
    }

    /// Shows the open field over its row, on the panel's layer. Its first
    /// frame takes the egui focus and selects the text.
    pub(super) fn show_browser_rename(&mut self, ui: &mut Ui) {
        let Some(rect) = self.browser_rename_rect() else {
            return;
        };
        let Some(rename) = self.browser.rename.as_mut() else {
            return;
        };
        let style = FieldStyle::new(FontId::proportional(BROWSER_FONT_SIZE), ROW_MARGIN)
            .max_chars(PROJECT_NAME_MAX_CHARS);
        rename.field.show(
            &mut ui.new_child(UiBuilder::new().layer_id(Self::browser_layer())),
            Id::new("browser-rename"),
            rect,
            style,
            true,
        );
    }
}
