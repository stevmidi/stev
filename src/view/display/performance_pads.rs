//! The performance lane's pads (`520-performance-capture.md` decisions 3 and
//! 6): with the lane selected, the knob panel's column, which has no plugin
//! to show then, holds the lane's eight performances as pads instead, in the
//! knobs' own cells ([`knob_layout`]), numbered as the knobs are. A pad is
//! lit when it holds a performance and unlit when it is empty; exactly one is
//! selected, the one a capture goes to. Hovering one names it in the footer
//! ("Pad 6 · 6 bars"). A click selects it and gives the column the keyboard
//! (`KeyFocus::Pads`): `Delete` empties the selected pad, `Esc` leaves, a
//! pane command hands the keyboard back and acts there, as from a knob. Mouse
//! only. The drawing is `rendering/knob_panel.rs`.

use egui::pos2;

use crate::core::input_event::InputEvent;
use crate::core::time::Meter;
use crate::models::performance_lane::{PAD_COUNT, length_label};

use super::Display;
use super::input::{ColumnKey, column_key};
use super::knob_panel::{KnobRects, knob_hit_at, knob_layout};
use super::pane::KeyFocus;

/// What the pads hold, as the sequencer last said (`UiEvent::PerformancePads`),
/// and which is selected: the view's own, set by a click and told to the
/// sequencer, pad 1 again on a load as there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PadsShown {
    /// Each pad's length in ticks, `None` for an empty one.
    pub(super) lengths: [Option<i32>; PAD_COUNT],
    /// The selected pad.
    pub(super) selected: usize,
}

/// The footer's line for pad `pad` holding a performance `length` ticks long
/// (`None` empty): "Pad 6 · 6 bars", "Pad 6 · empty". Pads count from 1.
fn pad_hint(pad: usize, length: Option<i32>, meter: Meter) -> String {
    let what = length.map_or_else(|| "empty".to_owned(), |ticks| length_label(ticks, meter));
    format!("Pad {} · {what}", pad + 1)
}

impl Display {
    /// The pads' layout this frame: the knobs' cells, while the lane is
    /// selected and the panel shows. `None` otherwise.
    pub(super) fn pad_rects(&self) -> Option<[KnobRects; PAD_COUNT]> {
        if !self.performance_lane_selected() {
            return None;
        }
        self.knob_panel_rect().map(knob_layout)
    }

    /// The pad under `(x, y)`, its dial's cell or its label's.
    fn pad_at(&self, x: f32, y: f32) -> Option<usize> {
        knob_hit_at(&self.pad_rects()?, pos2(x, y)).map(|hit| hit.knob())
    }

    /// The pad under the pointer.
    pub(super) fn pad_under_pointer(&self) -> Option<usize> {
        let pos = self.canvas_pointer()?;
        self.pad_at(pos.x, pos.y)
    }

    /// A press in the column with the lane selected: the pad there becomes
    /// the selected one, lit or not, and the column takes the keyboard, as a
    /// clicked knob does. Returns whether the press was on the pads at all.
    pub(super) fn handle_pad_click(&mut self, x: f32, y: f32) -> bool {
        if self.pad_rects().is_none() {
            return false;
        }
        let Some(pad) = self.pad_at(x, y) else {
            return true;
        };
        if self.key_focus == KeyFocus::Browser {
            self.leave_browser();
        }
        self.key_focus = KeyFocus::Pads;
        if self.performance_pads.selected != pad {
            self.performance_pads.selected = pad;
            self.input_event_tx
                .send(InputEvent::SelectPerformancePad(pad))
                .ok();
        }
        true
    }

    /// The footer's line for the pad under the pointer ([`pad_hint`]).
    pub(super) fn pad_footer_hint(&self) -> Option<String> {
        let pad = self.pad_under_pointer()?;
        Some(pad_hint(
            pad,
            self.performance_pads.lengths[pad],
            self.meter(),
        ))
    }

    /// What the clip pane says with the lane selected on an empty pad, in
    /// place of a piano roll ([`pad_hint`]): "Pad 6 · empty".
    pub(super) fn empty_pad_note(&self) -> String {
        pad_hint(self.performance_pads.selected, None, self.meter())
    }

    /// Whether the pad column has the keyboard: focused by a click, the lane
    /// still selected and the panel showing.
    pub(super) fn pads_have_keyboard(&self) -> bool {
        self.key_focus == KeyFocus::Pads && self.pad_rects().is_some()
    }

    /// The pad column's keys while it has the keyboard ([`column_key`], the
    /// knobs' own: `Delete` empties the selected pad).
    /// Returns whether it consumed `event`.
    pub(super) fn handle_pad_column_key(&mut self, event: &InputEvent) -> bool {
        let Some((key, action)) = self.focus_key_press(event) else {
            return false;
        };
        match column_key(key, action) {
            Some(ColumnKey::Delete) => {
                self.input_event_tx
                    .send(InputEvent::ClearPerformancePad)
                    .ok();
                true
            }
            Some(ColumnKey::Leave) => {
                self.key_focus = KeyFocus::Pane;
                true
            }
            Some(ColumnKey::HandBack) => {
                self.key_focus = KeyFocus::Pane;
                false
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pad_hint;
    use crate::core::time::Meter;

    #[test]
    fn the_hint_names_the_pad_and_its_length() {
        let four = Meter::FOUR_FOUR;
        assert_eq!(
            pad_hint(5, Some(6 * four.bar_ticks()), four),
            "Pad 6 · 6 bars"
        );
        assert_eq!(pad_hint(0, None, four), "Pad 1 · empty");
    }
}
