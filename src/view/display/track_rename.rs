//! The track header's rename field: an inline text field over a track's name
//! row, opened by ⌘R (the selected track), a double-click on the name or the
//! output menu's `Rename`. While it is open it takes the keyboard — every
//! key, nothing falls through. Enter commits, Esc cancels, a click away
//! commits (and then acts as usual); a blank entry gives the track its
//! number back. The commit is one undoable `RenameTrackEdit` on the
//! sequencer thread. See `020-views-and-state.md` § Track rename.
//!
//! The text editing itself — caret, selection, IME, the clipboard — is an
//! egui [`TextEdit`]; this module decides when it shows and what its keys
//! and presses mean to the rest of the app ([`field_event`], unit-tested,
//! shared with the header's BPM field).

use egui::text_selection::CCursorRange;
use egui::{FontId, Frame, Id, Key, Margin, Rect, TextEdit, Ui, UiBuilder, pos2};

use crate::core::config::TRACK_NAME_MAX_CHARS;
use crate::core::input_event::InputEvent;
use crate::models::track::track_name_from_input;
use crate::view::theme;

use super::pane::KeyFocus;
use super::rendering::focus_edge_stroke;
use super::{Display, Pane};

/// Text size of a track's name — on the header's name row and in the rename
/// field over it, so the text doesn't jump when the field opens.
pub(super) const TRACK_NAME_FONT_SIZE: f32 = 12.0;

/// The open rename field.
pub(super) struct TrackRename {
    /// The track being renamed, by position.
    track_idx: usize,
    /// The text as typed so far — the track's name to start with, selected
    /// whole.
    text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text.
    shown: bool,
    /// Who had the keyboard before the field opened, to hand it back.
    return_focus: KeyFocus,
}

/// What an event does while a text field (the rename field, the header's BPM
/// field) is open — [`field_event`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FieldEvent {
    /// Keep what was typed (Enter).
    Commit,
    /// Drop it (Esc).
    Cancel,
    /// The field's alone: typing, a clipboard chord, a press on the field.
    Swallow,
    /// A press off the field: keep what was typed, then the press acts as
    /// usual.
    CommitAndPass,
    /// Not the field's business.
    Pass,
}

impl FieldEvent {
    /// Whether the field consumed the event, so nothing else sees it.
    pub(super) fn consumed(&self) -> bool {
        !matches!(self, FieldEvent::CommitAndPass | FieldEvent::Pass)
    }
}

/// A text field's keymap: Enter commits, Esc cancels, every other key is the
/// text field's alone.
fn field_key(key: Key) -> FieldEvent {
    match key {
        Key::Enter => FieldEvent::Commit,
        Key::Escape => FieldEvent::Cancel,
        _ => FieldEvent::Swallow,
    }
}

/// What `event` does to an open text field at `field` (`None` when it isn't
/// on screen): it takes every key ([`field_key`]) and the clipboard chords; a
/// press on the field is the field's, a press anywhere else commits and then
/// goes on to act as usual. The rule both fields share — each maps the
/// answer to its own commit and close.
pub(super) fn field_event(event: &InputEvent, field: Option<Rect>) -> FieldEvent {
    match *event {
        InputEvent::KeyPressed { key, .. } => field_key(key),
        InputEvent::Copy { .. } | InputEvent::Cut { .. } | InputEvent::Paste => FieldEvent::Swallow,
        InputEvent::MouseClicked { x, y, .. } | InputEvent::MouseDoubleClicked { x, y } => {
            if field.is_some_and(|field| field.contains(pos2(x, y))) {
                FieldEvent::Swallow
            } else {
                FieldEvent::CommitAndPass
            }
        }
        _ => FieldEvent::Pass,
    }
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
            text: self.tracks[track_idx].name.clone().unwrap_or_default(),
            shown: false,
            return_focus,
        });
    }

    /// The track the rename field is open on, if any.
    pub(super) fn renaming_track(&self) -> Option<usize> {
        self.track_rename.as_ref().map(|rename| rename.track_idx)
    }

    /// Where the rename field over `track_idx`'s name row goes — `None` when
    /// that row isn't on screen (no such track, the arranger hidden). The one
    /// validity rule, shared by opening, drawing, the click test and
    /// [`drop_stale_track_rename`](Self::drop_stale_track_rename), so the
    /// field never takes the keyboard while it can't be seen.
    fn rename_field_rect(&mut self, track_idx: usize) -> Option<Rect> {
        if !self.is_pane_visible(Pane::Arranger) {
            return None;
        }
        self.in_pane(Pane::Arranger, |display| display.track_name_rect(track_idx))
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
                    name: track_name_from_input(&rename.text),
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
        let field = TextEdit::singleline(&mut rename.text)
            .char_limit(TRACK_NAME_MAX_CHARS)
            .font(FontId::proportional(TRACK_NAME_FONT_SIZE))
            .hint_text(track_number_label(track_idx));
        show_name_field(
            ui,
            field,
            Id::new("track-rename"),
            rect,
            Margin::symmetric(3, 0),
            &mut rename.shown,
        );
    }
}

/// Shows `field` as the app's name fields look — `fg` text on a `bg` fill in
/// the focus-edge stroke, inset by `margin` — filling `rect` of the canvas
/// `ui`. Its first frame (`*shown` false) takes the egui focus and selects
/// the whole text. The track rename field and Save As's
/// (`rendering/modals/project_dialog.rs`) both use it.
pub(super) fn show_name_field(
    ui: &mut Ui,
    field: TextEdit<'_>,
    id: Id,
    rect: Rect,
    margin: Margin,
    shown: &mut bool,
) {
    let field = field
        .id(id)
        .text_color(theme::fg())
        // A custom frame replaces `TextEdit::margin`: the inset lives on the
        // frame.
        .frame(
            Frame::NONE
                .fill(theme::bg())
                .stroke(focus_edge_stroke())
                .corner_radius(2)
                .inner_margin(margin),
        )
        .desired_width(rect.width());
    let mut output = field.show(&mut ui.new_child(UiBuilder::new().max_rect(rect)));
    if !*shown {
        *shown = true;
        output
            .state
            .cursor
            .set_char_range(Some(CCursorRange::select_all(&output.galley)));
        output.state.store(ui.ctx(), id);
        output.response.request_focus();
    }
}

#[cfg(test)]
mod tests {
    use egui::{Key, Rect, pos2};

    use crate::core::input_event::{InputEvent, KeyModifiers};

    use super::{FieldEvent, field_event, field_key};

    #[test]
    fn enter_commits_and_escape_cancels() {
        assert_eq!(field_key(Key::Enter), FieldEvent::Commit);
        assert_eq!(field_key(Key::Escape), FieldEvent::Cancel);
    }

    /// Nothing typed into the field reaches the arranger — not Delete
    /// (remove the track), Space (play) or `R` (record).
    #[test]
    fn every_other_key_is_typing() {
        for key in [
            Key::Delete,
            Key::Backspace,
            Key::Space,
            Key::R,
            Key::ArrowUp,
        ] {
            assert_eq!(field_key(key), FieldEvent::Swallow);
        }
    }

    /// A press on the field is the field's; one anywhere else commits and
    /// goes on — and with the field off screen every press is "elsewhere".
    #[test]
    fn a_press_off_the_field_commits_and_passes_on() {
        let field = Rect::from_min_max(pos2(10.0, 10.0), pos2(50.0, 30.0));
        let press = |x, y| InputEvent::MouseClicked {
            x,
            y,
            modifiers: KeyModifiers::default(),
        };
        assert_eq!(
            field_event(&press(20.0, 20.0), Some(field)),
            FieldEvent::Swallow
        );
        assert_eq!(
            field_event(&press(100.0, 20.0), Some(field)),
            FieldEvent::CommitAndPass
        );
        assert_eq!(
            field_event(&press(20.0, 20.0), None),
            FieldEvent::CommitAndPass
        );
        assert!(!FieldEvent::CommitAndPass.consumed());
        assert_eq!(
            field_event(&InputEvent::Paste, Some(field)),
            FieldEvent::Swallow
        );
        assert_eq!(
            field_event(&InputEvent::MouseReleased, Some(field)),
            FieldEvent::Pass
        );
    }
}
