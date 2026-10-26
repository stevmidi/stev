//! The app's text fields — the track and browser renames, the header's BPM
//! and meter fields, Save As's name and the Keyboard tab's search: one state
//! type ([`TextField`]), one look ([`FieldStyle`], [`TextField::show`]), one
//! rule for the rect an inline field is laid over ([`field_rect`]) and one
//! keymap ([`field_key`], [`field_event`]). Each field keeps only where it
//! goes and what a commit does. See `020-views-and-state.md` § Track rename.
//!
//! The text editing itself — caret, selection, IME, the clipboard — is an
//! egui [`TextEdit`]; this module decides how it looks and what its keys and
//! presses mean to the rest of the app.

use egui::text_selection::CCursorRange;
use egui::{
    Align, FontId, Frame, Id, Key, Margin, Rect, Response, RichText, Stroke, TextEdit, Ui,
    UiBuilder, pos2, vec2,
};

use crate::core::input_event::InputEvent;
use crate::view::theme;

use super::rendering::focus_edge_stroke;

/// The inset of a field laid over a list row: a track's name, a browser row.
pub(super) const ROW_MARGIN: Margin = Margin::symmetric(3, 0);

/// The inset of a field over a header chip's value.
pub(super) const CHIP_MARGIN: Margin = Margin::symmetric(4, 0);

/// The inset of a field in a modal: Save As's name, the Keyboard tab's
/// search.
pub(super) const MODAL_MARGIN: Margin = Margin::symmetric(6, 0);

/// The UI font's line height over its size: 1.3 for Plex at every size the
/// fields use. Measured, and pinned by `the_line_height_matches_the_font`.
const LINE_H_PER_PT: f32 = 1.3;

/// How much taller than its line egui draws a text field's caret: 1.5 points
/// above and below (`cursor_rect`).
const CARET_OVERHANG: f32 = 3.0;

/// The room a text field leaves above and below its caret, per point of its
/// font: a fifth, about what a native macOS field leaves.
const CARET_ROOM_PER_PT: f32 = 0.2;

/// Where a text field in `font_size` text over `rect` goes: `rect`, grown
/// about its centre to at least the caret's height plus
/// [`CARET_ROOM_PER_PT`] of room above and below — so the caret never runs
/// the field's full height, or past it, in a field laid over a row barely
/// taller than its text (a track's name row, a header chip). Its text stays
/// centred where the row's was. Applied where an inline field's rect comes
/// from, so what it is drawn in ([`TextField::show`]) and what a press is
/// tested against ([`field_event`]) are one rect.
pub(super) fn field_rect(rect: Rect, font_size: f32) -> Rect {
    let min_h = font_size * (LINE_H_PER_PT + 2.0 * CARET_ROOM_PER_PT) + CARET_OVERHANG;
    rect.expand2(vec2(0.0, ((min_h - rect.height()) / 2.0).max(0.0)))
}

/// An open text field's state.
#[derive(Default)]
pub(super) struct TextField {
    /// The text as typed so far — what the field opened on to start with,
    /// selected whole.
    pub(super) text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text — every frame until the pointer is up, when a
    /// press opened it.
    shown: bool,
}

/// How a field looks and what it takes, beyond its text and where it goes.
pub(super) struct FieldStyle<'a> {
    /// The text's font, the hint's too.
    font: FontId,
    /// The inset of the text from the field's edge: [`ROW_MARGIN`],
    /// [`CHIP_MARGIN`] or [`MODAL_MARGIN`].
    margin: Margin,
    /// What shows, in the field's font, while it is empty.
    hint: Option<&'a str>,
    /// The longest text it takes, in characters.
    max_chars: usize,
    /// Its edge: the focus edge (`focus_edge_stroke`) unless given one — a
    /// field on screen without the keyboard (Save As's) drops the focus edge.
    edge: Option<Stroke>,
}

impl<'a> FieldStyle<'a> {
    /// A field in `font` inset by `margin`, with no hint and no length
    /// limit.
    pub(super) fn new(font: FontId, margin: Margin) -> Self {
        FieldStyle {
            font,
            margin,
            hint: None,
            max_chars: usize::MAX,
            edge: None,
        }
    }

    /// With `hint` showing while the field is empty.
    pub(super) fn hint(self, hint: &'a str) -> Self {
        FieldStyle {
            hint: Some(hint),
            ..self
        }
    }

    /// Taking at most `max_chars` characters.
    pub(super) fn max_chars(self, max_chars: usize) -> Self {
        FieldStyle { max_chars, ..self }
    }

    /// Edged with `edge` instead of the focus edge: for a field shown while
    /// something else has the keyboard.
    pub(super) fn edge(self, edge: Stroke) -> Self {
        FieldStyle {
            edge: Some(edge),
            ..self
        }
    }
}

impl TextField {
    /// A field opening on `text`.
    pub(super) fn new(text: String) -> Self {
        TextField { text, shown: false }
    }

    /// Shows the field as every text field in the app looks — `fg` text on
    /// a `bg` fill in the focus-edge stroke (or `style`'s edge), in `style` —
    /// filling `rect` of
    /// the canvas `ui`, its line centred vertically. Its first frame takes
    /// the egui focus and selects the whole text. Returns the field's
    /// response, for a caller that watches it change.
    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        id: Id,
        rect: Rect,
        style: FieldStyle<'_>,
    ) -> Response {
        let field = TextEdit::singleline(&mut self.text).char_limit(style.max_chars);
        // The hint in the field's font: egui draws a hint in its default font
        // otherwise, and places the empty field's caret against it.
        let field = match style.hint {
            Some(hint) => field.hint_text(RichText::new(hint).font(style.font.clone())),
            None => field,
        };
        let field = field
            .id(id)
            .font(style.font)
            .text_color(theme::fg())
            // A custom frame replaces `TextEdit::margin`: the inset lives on
            // the frame.
            .frame(
                Frame::NONE
                    .fill(theme::bg())
                    .stroke(style.edge.unwrap_or_else(focus_edge_stroke))
                    .corner_radius(2)
                    .inner_margin(style.margin),
            )
            .desired_width(rect.width())
            // Fill `rect` and centre the line in it, caret and hint included,
            // as a single-line field does everywhere; egui lays text out from
            // the top of the frame otherwise.
            .min_size(rect.size())
            .vertical_align(Align::Center);
        let mut output = field.show(&mut ui.new_child(UiBuilder::new().max_rect(rect)));
        if !self.shown {
            // A field opened by a press (the browser's `+`) takes the focus
            // again until that press is released: egui drops it on a click
            // ending off the field, and the release ends one.
            self.shown = !ui.input(|i| i.pointer.any_down());
            output
                .state
                .cursor
                .set_char_range(Some(CCursorRange::select_all(&output.galley)));
            output.state.store(ui.ctx(), id);
            output.response.request_focus();
        }
        output.response.response
    }
}

/// What an event does while a text field is open — [`field_event`].
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
/// text field's alone. Save As reads its keys from it too; the inline fields
/// through [`field_event`].
pub(super) fn field_key(key: Key) -> FieldEvent {
    match key {
        Key::Enter => FieldEvent::Commit,
        Key::Escape => FieldEvent::Cancel,
        _ => FieldEvent::Swallow,
    }
}

/// What `event` does to an open inline text field at `field` (`None` when it
/// isn't on screen): it takes every key ([`field_key`]) and the clipboard
/// chords; a press on the field is the field's, a press anywhere else
/// commits and then goes on to act as usual. The rule the inline fields
/// share — each maps the answer to its own commit and close.
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

#[cfg(test)]
mod tests {
    use egui::epaint::{Fonts, TextOptions};
    use egui::{FontId, Key, Rect, pos2};

    use crate::core::input_event::{InputEvent, KeyModifiers};
    use crate::view::display::browser::BROWSER_FONT_SIZE;
    use crate::view::display::track_rename::TRACK_NAME_FONT_SIZE;
    use crate::view::theme::FONT_SIZE_HEADER_VALUE;
    use crate::view::theme::fonts::font_definitions;

    use super::{CARET_OVERHANG, FieldEvent, LINE_H_PER_PT, field_event, field_key, field_rect};

    #[test]
    fn the_line_height_matches_the_font() {
        let mut fonts = Fonts::new(TextOptions::default(), font_definitions());
        let mut fonts = fonts.with_pixels_per_point(2.0);
        for size in [
            TRACK_NAME_FONT_SIZE,
            BROWSER_FONT_SIZE,
            FONT_SIZE_HEADER_VALUE,
        ] {
            let row_h = fonts.row_height(&FontId::proportional(size));
            assert!(
                (row_h - size * LINE_H_PER_PT).abs() <= 0.25,
                "{size}: {row_h}"
            );
        }
    }

    /// The track name's 16pt row is shorter than its caret: the field grows
    /// about the row's centre to fit it with room to spare.
    #[test]
    fn a_field_grows_past_its_caret_about_the_row_centre() {
        let row = Rect::from_min_max(pos2(10.0, 100.0), pos2(90.0, 116.0));
        let field = field_rect(row, TRACK_NAME_FONT_SIZE);
        let caret_h = TRACK_NAME_FONT_SIZE * LINE_H_PER_PT + CARET_OVERHANG;
        assert_eq!(field.center(), row.center());
        assert_eq!(field.x_range(), row.x_range());
        assert!(field.height() - caret_h >= 2.0 * 2.0, "{}", field.height());
    }

    /// A field already roomier than that (a modal's) is left as it is.
    #[test]
    fn a_roomy_field_keeps_its_rect() {
        let row = Rect::from_min_max(pos2(10.0, 100.0), pos2(90.0, 140.0));
        assert_eq!(field_rect(row, TRACK_NAME_FONT_SIZE), row);
    }

    #[test]
    fn enter_commits_and_escape_cancels() {
        assert_eq!(field_key(Key::Enter), FieldEvent::Commit);
        assert_eq!(field_key(Key::Escape), FieldEvent::Cancel);
    }

    /// Nothing typed into a field reaches the app — not Delete (remove the
    /// track), Space (play), `R` (record) or `D` (which the unsaved-changes
    /// prompt reads as Don't Save).
    #[test]
    fn every_other_key_is_typing() {
        for key in [
            Key::Delete,
            Key::Backspace,
            Key::Space,
            Key::R,
            Key::D,
            Key::S,
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
