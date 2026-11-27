//! The marker strip's rename field: an inline text field over a marker's
//! flag (`600-markers.md` decision 7), opened by `⌘R` with the strip focused
//! or a double-click on the flag. The track rename's rules: while it is open
//! it takes every key, Enter commits, Esc cancels, a click away commits (and
//! then acts as usual). A blank entry clears the name, leaving the number.
//! The commit is one undoable `SetMarkerEdit` on the sequencer thread.

use egui::{FontId, Id, Rect, Ui, vec2};

use crate::core::config::MARKER_NAME_MAX_CHARS;
use crate::core::input_event::InputEvent;
use crate::models::markers::MarkerChange;
use crate::view::display::pane::KeyFocus;
use crate::view::display::{Display, Pane};
use crate::view::theme;

use super::{FieldEvent, FieldStyle, ROW_MARGIN, TextField, field_event, field_rect};

/// The narrowest the field opens, so a name has room to be typed over a
/// flag that shows only its number.
const MIN_FIELD_W: f32 = 120.0;

/// The open rename field.
pub(in crate::view::display) struct MarkerRename {
    /// The marker being renamed.
    number: u8,
    /// The field — the marker's name to start with.
    field: TextField,
    /// Who had the keyboard before the field opened, to hand it back.
    return_focus: KeyFocus,
}

impl Display {
    /// Opens the rename field over marker `number`'s flag: its name,
    /// selected, or empty with its number as the hint. Nothing when the flag
    /// isn't on screen ([`marker_rename_rect`](Self::marker_rename_rect)).
    pub(in crate::view::display) fn open_marker_rename(&mut self, number: u8) {
        if self.marker_rename_rect(number).is_none() {
            return;
        }
        let name = self
            .markers
            .get(number)
            .and_then(|marker| marker.name.clone());
        let return_focus = match self.marker_rename.take() {
            Some(open) => open.return_focus,
            None => self.key_focus,
        };
        self.marker_rename = Some(MarkerRename {
            number,
            field: TextField::new(name.unwrap_or_default()),
            return_focus,
        });
    }

    /// The marker the rename field is open on, if any.
    fn renaming_marker(&self) -> Option<u8> {
        self.marker_rename.as_ref().map(|rename| rename.number)
    }

    /// Where the field over marker `number`'s flag goes: the flag, widened
    /// to [`MIN_FIELD_W`] and grown to fit its caret ([`field_rect`]).
    /// `None` when the flag isn't on screen (no such marker, scrolled away,
    /// the arranger hidden) — the one validity rule, shared by opening,
    /// drawing, the click test and
    /// [`drop_stale_marker_rename`](Self::drop_stale_marker_rename).
    fn marker_rename_rect(&mut self, number: u8) -> Option<Rect> {
        if !self.is_pane_visible(Pane::Arranger) {
            return None;
        }
        let flag = self.in_pane(Pane::Arranger, |display| display.marker_flag_rect(number))?;
        let flag =
            Rect::from_min_size(flag.min, vec2(flag.width().max(MIN_FIELD_W), flag.height()));
        Some(field_rect(flag, theme::FONT_SIZE_TL))
    }

    /// Once a frame, before input: closes the field, keeping the old name,
    /// when its flag is gone or off screen or the arranger lost the keyboard
    /// this frame (`left_arranger`).
    pub(in crate::view::display) fn drop_stale_marker_rename(&mut self, left_arranger: bool) {
        if let Some(number) = self.renaming_marker()
            && (left_arranger || self.marker_rename_rect(number).is_none())
        {
            self.close_marker_rename();
        }
    }

    /// Closes the field and hands the keyboard back to whoever had it. On
    /// its own: a cancel, keeping the old name.
    fn close_marker_rename(&mut self) -> Option<MarkerRename> {
        let rename = self.marker_rename.take()?;
        self.key_focus = rename.return_focus;
        Some(rename)
    }

    /// While the field is open it gets first look at every event
    /// ([`field_event`]). Enter and a click away name the marker what was
    /// typed (`MarkerChange::Rename`; blank clears it, an unchanged name
    /// makes no edit). Returns whether the event was consumed.
    pub(in crate::view::display) fn handle_marker_rename_input_event(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(number) = self.renaming_marker() else {
            return false;
        };
        let action = field_event(event, self.marker_rename_rect(number));
        match action {
            FieldEvent::Commit | FieldEvent::CommitAndPass => {
                if let Some(rename) = self.close_marker_rename() {
                    let name = rename.field.text.trim();
                    let name = (!name.is_empty()).then(|| name.to_owned());
                    self.change_marker(number, MarkerChange::Rename(name));
                }
            }
            FieldEvent::Cancel => {
                self.close_marker_rename();
            }
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Shows the open field over its marker's flag, in the canvas `ui`.
    pub(in crate::view::display) fn show_marker_rename(&mut self, ui: &mut Ui) {
        let Some(number) = self.renaming_marker() else {
            return;
        };
        let Some(rect) = self.marker_rename_rect(number) else {
            return;
        };
        let Some(rename) = self.marker_rename.as_mut() else {
            return;
        };
        let hint = number.to_string();
        let style = FieldStyle::new(FontId::proportional(theme::FONT_SIZE_TL), ROW_MARGIN)
            .hint(&hint)
            .max_chars(MARKER_NAME_MAX_CHARS);
        rename
            .field
            .show(ui, Id::new("marker-rename"), rect, style, true);
    }
}
