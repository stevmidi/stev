//! What the header's control chips — BPM (`tempo_field.rs`) and meter
//! (`meter_field.rs`) — share: where a chip was painted, and the inline text
//! field a double-click opens over its value (a [`TextField`]). Each chip
//! keeps its own parse and commit. See `030-ui-design.md` § Header & Footer.

use egui::{FontId, Id, Rect, Ui, pos2};

use super::text_field::{CHIP_MARGIN, FieldStyle, TextField};

/// The longest text a header field takes — `300.0` or `16 / 8`, and a spare.
const FIELD_MAX_CHARS: usize = 6;

/// Where a header control chip was painted this frame, for the pointer and
/// its field.
#[derive(Clone, Copy)]
pub(super) struct HeaderChipRects {
    /// The whole chip, label and value — what a press or a double-click hits.
    pub(super) chip: Rect,
    /// Where the field goes: the value's part of the chip, the label left
    /// showing, grown to fit the caret ([`field_rect`](super::text_field::field_rect))
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

/// Shows `field` over `rects`' value, in the canvas `ui`, in the header's
/// value `font`. Nothing when the chip hasn't been painted yet.
pub(super) fn show_header_field(
    ui: &mut Ui,
    font: FontId,
    rects: Option<HeaderChipRects>,
    field: &mut TextField,
    id: &str,
) {
    let Some(rects) = rects else {
        return;
    };
    let style = FieldStyle::new(font, CHIP_MARGIN).max_chars(FIELD_MAX_CHARS);
    field.show(ui, Id::new(id), rects.value, style, true);
}
