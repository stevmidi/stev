//! The track header's rename field: an inline text field over a track's name
//! row, opened by ⌘R (the selected track), a double-click on the name or the
//! output menu's `Rename`. While it is open it takes the keyboard — every
//! key, nothing falls through. Enter commits, Esc cancels, a click away
//! commits (and then acts as usual); a blank entry gives the track its
//! number back. The commit is one undoable `RenameTrackEdit` on the
//! sequencer thread. See `020-views-and-state.md` § Track rename.
//!
//! The field and its keymap are the app's one text field
//! (`text_field.rs`); this module decides where it goes and what a commit
//! does.

use egui::{FontId, Id, Rect, Ui};

use crate::core::config::TRACK_NAME_MAX_CHARS;
use crate::core::input_event::InputEvent;
use crate::models::track::track_name_from_input;

use super::pane::KeyFocus;
use super::text_field::{FieldEvent, FieldStyle, ROW_MARGIN, TextField, field_event, field_rect};
use super::{Display, Pane};

/// Text size of a track's name — on the header's name row and in the rename
/// field over it, so the text doesn't jump when the field opens.
pub(super) const TRACK_NAME_FONT_SIZE: f32 = 12.0;

/// The open rename field.
pub(super) struct TrackRename {
    /// The track being renamed, by position.
    track_idx: usize,
    /// The field — the track's name to start with.
    field: TextField,
    /// Who had the keyboard before the field opened, to hand it back.
    return_focus: KeyFocus,
}

/// The track's number — what its header shows while it is unnamed, and the
/// rename field's hint.
pub(super) fn track_number_label(track_idx: usize) -> String {
    (track_idx + 1).to_string()
}

impl Display {
    /// Opens the rename field over `track_idx`'s name row (closing the output
    /// menu, whose `Rename` may have asked): the track's name, selected, or
    /// empty with its number as the hint while unnamed. Nothing when the row
    /// isn't on screen ([`rename_field_rect`](Self::rename_field_rect)).
    pub(super) fn open_track_rename(&mut self, track_idx: usize) {
        if self.rename_field_rect(track_idx).is_none() {
            return;
        }
        self.close_output_menu();
        let return_focus = match self.track_rename.take() {
            Some(open) => open.return_focus,
            None => self.key_focus,
        };
        self.track_rename = Some(TrackRename {
            track_idx,
            field: TextField::new(self.tracks[track_idx].name.clone().unwrap_or_default()),
            return_focus,
        });
    }

    /// The track the rename field is open on, if any.
    pub(super) fn renaming_track(&self) -> Option<usize> {
        self.track_rename.as_ref().map(|rename| rename.track_idx)
    }

    /// Where the rename field over `track_idx`'s name row goes, grown to fit
    /// its caret ([`field_rect`]) — `None` when that row isn't on screen (no such track, the arranger hidden). The one
    /// validity rule, shared by opening, drawing, the click test and
    /// [`drop_stale_track_rename`](Self::drop_stale_track_rename), so the
    /// field never takes the keyboard while it can't be seen.
    fn rename_field_rect(&mut self, track_idx: usize) -> Option<Rect> {
        if !self.is_pane_visible(Pane::Arranger) {
            return None;
        }
        self.in_pane(Pane::Arranger, |display| display.track_name_rect(track_idx))
            .map(|row| field_rect(row, TRACK_NAME_FONT_SIZE))
    }

    /// Once a frame, before input: closes the field, keeping the old name,
    /// when its row is off screen or the arranger lost the keyboard this
    /// frame (`left_arranger` — the caller then drops the header focus the
    /// field handed back, too).
    pub(super) fn drop_stale_track_rename(&mut self, left_arranger: bool) {
        if let Some(track_idx) = self.renaming_track()
            && (left_arranger || self.rename_field_rect(track_idx).is_none())
        {
            self.close_track_rename();
        }
    }

    /// Closes the field, renaming the track to what was typed — through
    /// [`track_name_from_input`], so a blank entry goes back to the number.
    /// An unchanged name makes no edit (`RenameTrackEdit::new`).
    fn commit_track_rename(&mut self) {
        if let Some(rename) = self.close_track_rename() {
            self.input_event_tx
                .send(InputEvent::RenameTrack {
                    track_idx: rename.track_idx,
                    name: track_name_from_input(&rename.field.text),
                })
                .ok();
        }
    }

    /// Closes the field and hands the keyboard back to whoever had it. On
    /// its own: a cancel, keeping the old name (Esc, a track add / remove).
    pub(super) fn close_track_rename(&mut self) -> Option<TrackRename> {
        let rename = self.track_rename.take()?;
        self.key_focus = rename.return_focus;
        Some(rename)
    }

    /// While the rename field is open it gets first look at every event
    /// ([`field_event`]; the typing itself egui's text field reads on its
    /// own). Returns whether the event was consumed.
    pub(super) fn handle_track_rename_input_event(&mut self, event: &InputEvent) -> bool {
        let Some(track_idx) = self.renaming_track() else {
            return false;
        };
        let action = field_event(event, self.rename_field_rect(track_idx));
        match action {
            FieldEvent::Commit | FieldEvent::CommitAndPass => self.commit_track_rename(),
            FieldEvent::Cancel => {
                self.close_track_rename();
            }
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Shows the open rename field over its track's name row, in the canvas
    /// `ui`. Its first frame takes the egui focus and selects the text.
    pub(super) fn show_track_rename(&mut self, ui: &mut Ui) {
        let Some(track_idx) = self.renaming_track() else {
            return;
        };
        let Some(rect) = self.rename_field_rect(track_idx) else {
            return;
        };
        let Some(rename) = self.track_rename.as_mut() else {
            return;
        };
        let hint = track_number_label(track_idx);
        let style = FieldStyle::new(FontId::proportional(TRACK_NAME_FONT_SIZE), ROW_MARGIN)
            .hint(&hint)
            .max_chars(TRACK_NAME_MAX_CHARS);
        rename.field.show(ui, Id::new("track-rename"), rect, style);
    }
}
