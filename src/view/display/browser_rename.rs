//! The browser panel's rename field: an inline text field over a project's
//! or `.mid` file's row, opened by ⌘R while the panel has the keyboard.
//! While it is open it takes the keyboard — every key, nothing falls
//! through. Enter renames the file on disk, Esc cancels, a click away
//! renames (and then acts as usual). A blank or unchanged name renames
//! nothing; a name the file system would trip over, or one already taken,
//! is refused with a footer message. Renaming the open project renames what
//! ⌘S saves to. See `020-views-and-state.md` § Views.
//!
//! The field and its keymap are the track rename's ([`show_name_field`],
//! [`field_event`]); this module decides where it goes and what a commit
//! does.

use egui::{FontId, Id, Margin, Rect, TextEdit, Ui, UiBuilder, pos2, vec2};

use crate::core::config::PROJECT_NAME_MAX_CHARS;
use crate::core::input_event::InputEvent;
use crate::core::project::{project_name_from_input, rename_midi_file, rename_project};
use crate::view::theme;

use super::Display;
use super::browser::{
    BROWSER_DISCLOSURE_W, BROWSER_INDENT_X, BROWSER_LIST_TOP, BROWSER_ROW_H, BROWSER_W, BrowserItem,
};
use super::status_message::StatusMessage;
use super::track_rename::{FieldEvent, field_event, show_name_field};

/// Right inset of the field from the panel's edge.
const FIELD_RIGHT_X: f32 = 6.0;

/// The open rename field.
pub(super) struct BrowserRename {
    /// The project or `.mid` row being renamed.
    item: BrowserItem,
    /// The name as typed so far — the file's to start with, selected whole.
    text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text.
    shown: bool,
}

impl Display {
    /// ⌘R in the panel: opens the field over the selected row, when that is
    /// a project or a `.mid` file; nothing on any other row.
    pub(super) fn open_browser_rename(&mut self) {
        let tree = &mut self.browser.tree;
        let Some(item) = tree.selected().filter(|item| item.file_name().is_some()) else {
            return;
        };
        let item = item.clone();
        tree.delete_armed = false;
        self.browser.rename = Some(BrowserRename {
            text: item.file_name().unwrap_or_default().to_owned(),
            item,
            shown: false,
        });
    }

    /// Where the field goes, in window space (the panel isn't shifted) —
    /// `None` when its row isn't on screen (the panel hidden, the row
    /// scrolled away or gone after a reload). The one validity rule, shared
    /// by drawing, the click test and
    /// [`drop_stale_browser_rename`](Self::drop_stale_browser_rename).
    fn browser_rename_rect(&self) -> Option<Rect> {
        let rename = self.browser.rename.as_ref()?;
        if !self.browser.visible {
            return None;
        }
        let rows = self.browser.tree.rows();
        let idx = rows.iter().position(|row| row.item == rename.item)?;
        let shown = idx.checked_sub(self.browser.scroll)?;
        if shown >= self.browser_visible_rows() {
            return None;
        }
        let origin = self.render.canvas_rect.min;
        let top = origin.y + BROWSER_LIST_TOP + shown as f32 * BROWSER_ROW_H;
        let left = origin.x + f32::from(rows[idx].depth) * BROWSER_INDENT_X + BROWSER_DISCLOSURE_W;
        Some(Rect::from_min_max(
            pos2(left - 4.0, top + 1.0),
            pos2(
                origin.x + BROWSER_W - FIELD_RIGHT_X,
                top + BROWSER_ROW_H - 1.0,
            ),
        ))
    }

    /// Once a frame, before input: closes the field, renaming nothing, when
    /// its row is off screen — so it never takes the keyboard unseen.
    pub(super) fn drop_stale_browser_rename(&mut self) {
        if self.browser.rename.is_some() && self.browser_rename_rect().is_none() {
            self.browser.rename = None;
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
            .map(|rect| rect.translate(vec2(-BROWSER_W, 0.0)));
        let action = field_event(event, field);
        match action {
            FieldEvent::Commit | FieldEvent::CommitAndPass => self.commit_browser_rename(),
            FieldEvent::Cancel => self.browser.rename = None,
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Closes the field, renaming the file to what was typed — trimmed and
    /// checked by [`project_name_from_input`], the rule Save As names a
    /// project by. A blank or unchanged name renames nothing. The renamed
    /// row stays selected; the open project, renamed, is what ⌘S saves to.
    fn commit_browser_rename(&mut self) {
        let Some(rename) = self.browser.rename.take() else {
            return;
        };
        let old = rename.item.file_name().unwrap_or_default().to_owned();
        if rename.text.trim().is_empty() || rename.text.trim() == old {
            return;
        }
        let Some(new) = project_name_from_input(&rename.text) else {
            self.render.status = Some(StatusMessage::new(format!(
                "Could not rename {old}: not a valid name"
            )));
            return;
        };
        let result = match &rename.item {
            BrowserItem::Project { folder, .. } => rename_project(folder.as_deref(), &old, &new),
            BrowserItem::MidiFile { folder, .. } => rename_midi_file(folder.as_deref(), &old, &new),
            _ => return,
        };
        let message = match result {
            Ok(()) => {
                if let BrowserItem::Project { folder, .. } = &rename.item
                    && *folder == self.project.project_current_folder
                    && self.project.project_current_name.as_deref() == Some(old.as_str())
                {
                    self.project.project_current_name = Some(new.clone());
                }
                self.reload_browser();
                if let Some(item) = rename.item.renamed(new.clone()) {
                    self.browser.tree.select(item);
                }
                format!("Renamed {old} to {new}")
            }
            Err(e) => format!("Could not rename {old}: {e}"),
        };
        self.render.status = Some(StatusMessage::new(message));
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
        let field = TextEdit::singleline(&mut rename.text)
            .char_limit(PROJECT_NAME_MAX_CHARS)
            .font(FontId::proportional(theme::FONT_SIZE_LABEL));
        show_name_field(
            &mut ui.new_child(UiBuilder::new().layer_id(Self::browser_layer())),
            field,
            Id::new("browser-rename"),
            rect,
            Margin::symmetric(3, 0),
            &mut rename.shown,
        );
    }
}
