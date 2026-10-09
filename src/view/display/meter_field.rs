//! The header's meter chip as a control: a double-click opens an inline field
//! over its value, the BPM field's twin (`header_chip.rs`) — Enter commits,
//! Esc cancels, a click away commits. The text goes through [`Meter::parse`]; Enter on text that
//! isn't a supported meter leaves the field open as it is, and a click away
//! from it changes nothing. Each typed meter is one undoable `SetMeterEdit`
//! on the sequencer thread. No drag. See `030-ui-design.md` § Header & Footer.

use egui::{FontId, Ui};

use crate::core::input_event::InputEvent;
use crate::core::time::Meter;

use super::Display;
use super::header_chip::{HeaderChipRects, HeaderTextField, show_header_field};
use super::track_rename::{FieldEvent, field_event};

/// The meter chip's state: where it is and the open field.
#[derive(Default)]
pub(super) struct MeterChip {
    /// Where it was painted last frame; `None` before the first frame.
    pub(super) rects: Option<HeaderChipRects>,
    /// The open field.
    field: Option<HeaderTextField>,
}

impl MeterChip {
    /// Whether the field is open.
    pub(super) fn editing(&self) -> bool {
        self.field.is_some()
    }

    /// Whether `(x, y)` is on the chip.
    pub(super) fn contains(&self, x: f32, y: f32) -> bool {
        self.rects.is_some_and(|rects| rects.contains(x, y))
    }
}

impl Display {
    /// A double-click on the chip: opens the field over its value, the meter
    /// in it selected.
    pub(super) fn open_meter_field(&mut self) {
        self.close_output_menu();
        self.meter_chip.field = Some(HeaderTextField::new(self.meter().to_string()));
    }

    /// The meter the open field's text means, if it is one.
    fn typed_meter(&self) -> Option<Meter> {
        Meter::parse(&self.meter_chip.field.as_ref()?.text)
    }

    /// Closes the field, setting the meter to `meter` unless it is `None` or
    /// the meter already.
    fn close_meter_field(&mut self, meter: Option<Meter>) {
        self.meter_chip.field = None;
        if let Some(meter) = meter.filter(|&meter| meter != self.meter()) {
            self.input_event_tx
                .send(InputEvent::SetMeter { meter })
                .ok();
        }
    }

    /// While the field is open it gets first look at every event, by the
    /// track rename field's rule ([`field_event`]). Returns whether the
    /// event was consumed.
    pub(super) fn handle_meter_field_input_event(&mut self, event: &InputEvent) -> bool {
        if !self.meter_chip.editing() {
            return false;
        }
        let action = field_event(event, self.meter_chip.rects.map(|rects| rects.value));
        match action {
            FieldEvent::Commit => {
                if let Some(meter) = self.typed_meter() {
                    self.close_meter_field(Some(meter));
                }
            }
            FieldEvent::CommitAndPass => self.close_meter_field(self.typed_meter()),
            FieldEvent::Cancel => self.close_meter_field(None),
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Shows the open field over the chip's value, in the canvas `ui`, in
    /// the header's value font. Its first frame takes the egui focus and
    /// selects the text.
    pub(super) fn show_meter_field(&mut self, ui: &mut Ui, font: FontId) {
        if let Some(field) = self.meter_chip.field.as_mut() {
            show_header_field(ui, font, self.meter_chip.rects, field, "meter-field");
        }
    }
}
