//! What the header's control chips — BPM (`tempo_field.rs`) and meter
//! (`meter_field.rs`) — share: where a chip was painted, and the inline text
//! field a double-click opens over its value. Each chip keeps its own parse
//! and commit. See `030-ui-design.md` § Header & Footer.

use egui::{FontId, Id, Margin, Rect, TextEdit, Ui, pos2};

use super::track_rename::show_name_field;

/// The longest text a header field takes — `300.0` or `16 / 8`, and a spare.
const FIELD_MAX_CHARS: usize = 6;

/// Where a header control chip was painted this frame, for the pointer and
/// its field.
#[derive(Clone, Copy)]
pub(super) struct HeaderChipRects {
    /// The whole chip, label and value — what a press or a double-click hits.
    pub(super) chip: Rect,
    /// Where the field goes: the value's part of the chip, the label left
    /// showing, grown to fit the caret ([`field_rect`](super::track_rename::field_rect))
    /// — past the chip's top and bottom, the value's font being nearly the
    /// chip's height. What the field is drawn in and what a press is tested
    /// against.
    pub(super) value: Rect,
}

impl HeaderChipRects {
    /// Whether `(x, y)` is on the chip.
    pub(super) fn contains(&self, x: f32, y: f32) -> bool {
        self.chip.contains(pos2(x, y))
    }
}

/// A header chip's open field.
pub(super) struct HeaderTextField {
    /// The text as typed so far — the chip's value to start with, selected
    /// whole.
    pub(super) text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text.
    shown: bool,
}

impl HeaderTextField {
    /// A field opening on `text`.
    pub(super) fn new(text: String) -> Self {
        HeaderTextField { text, shown: false }
    }
}

/// Shows `field` over `rects`' value, in the canvas `ui`, in the header's
/// value `font`. Nothing when the chip hasn't been painted yet.
pub(super) fn show_header_field(
    ui: &mut Ui,
    font: FontId,
    rects: Option<HeaderChipRects>,
    field: &mut HeaderTextField,
    id: &str,
) {
    let Some(rects) = rects else {
        return;
    };
    let edit = TextEdit::singleline(&mut field.text).char_limit(FIELD_MAX_CHARS);
    show_name_field(
        ui,
        edit,
        font,
        None,
        Id::new(id),
        rects.value,
        Margin::symmetric(4, 0),
        &mut field.shown,
    );
}
