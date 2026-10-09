//! The header's BPM chip as a control: a vertical drag on it scrubs the tempo
//! (1 BPM a step, 0.1 with ⇧; Esc puts it back) on raw pointer motion, the
//! pointer held and hidden as for the piano roll's octave-legend zoom, so the
//! window edge doesn't stop it; and a double-click opens an
//! inline number field over its value — Enter commits, Esc cancels, a click
//! away commits. Each typed value or drag is one undoable `SetTempoEdit` on
//! the sequencer thread. See `030-ui-design.md` § Header & Footer.
//!
//! The text editing itself is an egui [`TextEdit`], shown the way the track
//! rename field is ([`show_name_field`], [`field_event`]); the drag maths is
//! [`dragged_bpm_tenths`], unit-tested.

use std::sync::atomic::Ordering;

use egui::{FontId, Id, Margin, Rect, TextEdit, Ui, pos2};

use crate::core::input_event::InputEvent;
use crate::core::time::{
    bpm_tenths_to_tempo_us, clamp_bpm_tenths, format_bpm, parse_bpm_tenths, tempo_us_to_bpm_tenths,
};

use super::Display;
use super::track_rename::{FieldEvent, field_event, show_name_field};

/// Vertical pixels of drag per tempo step.
const DRAG_PX_PER_STEP: f32 = 3.0;

/// The longest text the BPM field takes — `300.0` and a spare.
const FIELD_MAX_CHARS: usize = 6;

/// Where the BPM chip was painted this frame, for the pointer and the field.
#[derive(Clone, Copy)]
pub(super) struct TempoChipRects {
    /// The whole chip, label and value — what a press or a double-click hits.
    pub(super) chip: Rect,
    /// The value's part of it — where the field goes, the `BPM` label left
    /// showing.
    pub(super) value: Rect,
}

/// A drag on the BPM chip, `Some` while the button is held.
struct TempoDrag {
    /// The drag's id, so its steps merge into one undo step.
    drag_id: u64,
    /// The tempo the drag found, exactly — what Esc puts back, and what a
    /// drag back to its starting value sends, so it leaves no undo step.
    start_tempo_us: i32,
    /// Upward pointer travel since the anchor, in pixels — summed from raw
    /// motion (`InputEvent::PointerMotion`), which keeps coming past the
    /// window edge. Back to zero when ⇧ goes down or up, so switching step
    /// size doesn't jump the tempo, and at the tempo range's ends, so turning
    /// round there acts at once.
    travel: f32,
    /// The tempo where `travel` counts from, in tenths of a BPM.
    anchor_tenths: i32,
    /// Whether ⇧ (0.1 BPM steps) was held at the last move.
    fine: bool,
    /// The last tempo sent, in tenths of a BPM.
    last_tenths: i32,
}

/// The open BPM field.
struct TempoTextField {
    /// The text as typed so far — the tempo to start with, selected whole.
    text: String,
    /// Whether the field has shown yet: its first frame takes the egui focus
    /// and selects the text.
    shown: bool,
}

/// The BPM chip's state: where it is, what the pointer is doing on it, and
/// the open field.
#[derive(Default)]
pub(super) struct TempoChip {
    /// Where it was painted last frame; `None` before the first frame.
    pub(super) rects: Option<TempoChipRects>,
    /// The pointer is over it with no button held — the resize cursor icon.
    pub(super) hover: bool,
    /// The drag in progress.
    drag: Option<TempoDrag>,
    /// The open field.
    field: Option<TempoTextField>,
}

impl TempoChip {
    /// Whether a drag is in progress.
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Whether the field is open.
    pub(super) fn editing(&self) -> bool {
        self.field.is_some()
    }

    /// Whether `(x, y)` is on the chip.
    pub(super) fn contains(&self, x: f32, y: f32) -> bool {
        self.rects
            .is_some_and(|rects| rects.chip.contains(pos2(x, y)))
    }
}

/// The tempo `dy_up` pixels of upward drag from a tempo of `anchor_tenths`
/// gives, in tenths of a BPM: a whole BPM a step, a tenth with `fine`,
/// clamped to the app's tempo range — and whether it was clamped. A partial
/// step counts for nothing.
fn dragged_bpm_tenths(anchor_tenths: i32, dy_up: f32, fine: bool) -> (i32, bool) {
    let step = if fine { 1 } else { 10 };
    let steps = (dy_up / DRAG_PX_PER_STEP).trunc() as i32;
    let tenths = anchor_tenths + steps * step;
    let clamped = clamp_bpm_tenths(tenths);
    (clamped, clamped != tenths)
}

impl Display {
    /// The project tempo, µs per quarter.
    pub(super) fn tempo_us(&self) -> i32 {
        self.tempo.load(Ordering::Relaxed)
    }

    /// A press on the chip: starts a drag from the current tempo. ⇧ is
    /// read on the first move (a change from `fine: false` re-anchors where
    /// nothing has moved yet).
    pub(super) fn begin_tempo_drag(&mut self) {
        let start_tempo_us = self.tempo_us();
        let start_tenths = tempo_us_to_bpm_tenths(start_tempo_us);
        self.tempo_chip.hover = false;
        self.tempo_chip.drag = Some(TempoDrag {
            drag_id: self.gesture.take_drag_id(),
            start_tempo_us,
            travel: 0.0,
            anchor_tenths: start_tenths,
            fine: false,
            last_tenths: start_tenths,
        });
    }

    /// Raw pointer motion `dy` (downward positive) with the button held: the
    /// tempo for the travel since the anchor, sent when it changed. ⇧ going
    /// down or up re-anchors at the tempo reached, and so does hitting the
    /// tempo range's end.
    pub(super) fn extend_tempo_drag(&mut self, dy: f32, fine: bool) {
        let Some(drag) = self.tempo_chip.drag.as_mut() else {
            return;
        };
        if fine != drag.fine {
            drag.fine = fine;
            drag.travel = 0.0;
            drag.anchor_tenths = drag.last_tenths;
        }
        drag.travel -= dy;
        let (tenths, clamped) = dragged_bpm_tenths(drag.anchor_tenths, drag.travel, fine);
        if clamped {
            drag.travel = 0.0;
            drag.anchor_tenths = tenths;
        }
        if tenths == drag.last_tenths {
            return;
        }
        drag.last_tenths = tenths;
        let tempo_us = if tenths == tempo_us_to_bpm_tenths(drag.start_tempo_us) {
            drag.start_tempo_us
        } else {
            bpm_tenths_to_tempo_us(tenths)
        };
        let drag_id = Some(drag.drag_id);
        self.input_event_tx
            .send(InputEvent::SetTempo { tempo_us, drag_id })
            .ok();
    }

    /// The button came up: the drag is done, its steps already sent.
    pub(super) fn finish_tempo_drag(&mut self) {
        self.tempo_chip.drag = None;
    }

    /// Esc mid drag: the tempo goes back to where the drag found it, and the
    /// gesture leaves no undo step (the edit merge annuls it).
    pub(super) fn cancel_tempo_drag(&mut self) {
        let Some(drag) = self.tempo_chip.drag.take() else {
            return;
        };
        if drag.last_tenths != tempo_us_to_bpm_tenths(drag.start_tempo_us) {
            self.input_event_tx
                .send(InputEvent::SetTempo {
                    tempo_us: drag.start_tempo_us,
                    drag_id: Some(drag.drag_id),
                })
                .ok();
        }
    }

    /// Updates whether the pointer is over the chip, for the cursor icon.
    pub(super) fn update_tempo_hover(&mut self, x: f32, y: f32) {
        self.tempo_chip.hover = self.tempo_chip.contains(x, y);
    }

    /// A double-click on the chip: opens the field over its value, the
    /// tempo in it selected. Ends the drag its second press started.
    pub(super) fn open_tempo_field(&mut self) {
        self.tempo_chip.drag = None;
        self.close_output_menu();
        self.tempo_chip.field = Some(TempoTextField {
            text: format_bpm(self.tempo_us()),
            shown: false,
        });
    }

    /// Closes the field, setting the tempo to what was typed. Nothing when it
    /// isn't a number, or is the tempo shown already.
    fn commit_tempo_field(&mut self) {
        let Some(field) = self.tempo_chip.field.take() else {
            return;
        };
        let Some(tenths) = parse_bpm_tenths(&field.text) else {
            return;
        };
        if tenths != tempo_us_to_bpm_tenths(self.tempo_us()) {
            self.input_event_tx
                .send(InputEvent::SetTempo {
                    tempo_us: bpm_tenths_to_tempo_us(tenths),
                    drag_id: None,
                })
                .ok();
        }
    }

    /// While the field is open it gets first look at every event, by the
    /// track rename field's rule ([`field_event`]). Returns whether the
    /// event was consumed.
    pub(super) fn handle_tempo_field_input_event(&mut self, event: &InputEvent) -> bool {
        if !self.tempo_chip.editing() {
            return false;
        }
        let action = field_event(event, self.tempo_chip.rects.map(|rects| rects.value));
        match action {
            FieldEvent::Commit | FieldEvent::CommitAndPass => self.commit_tempo_field(),
            FieldEvent::Cancel => self.tempo_chip.field = None,
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Shows the open field over the chip's value, in the canvas `ui`, in
    /// the header's value font. Its first frame takes the egui focus and
    /// selects the text.
    pub(super) fn show_tempo_field(&mut self, ui: &mut Ui, font: FontId) {
        let Some(rects) = self.tempo_chip.rects else {
            return;
        };
        let Some(field) = self.tempo_chip.field.as_mut() else {
            return;
        };
        let edit = TextEdit::singleline(&mut field.text)
            .char_limit(FIELD_MAX_CHARS)
            .font(font);
        show_name_field(
            ui,
            edit,
            Id::new("tempo-field"),
            rects.value,
            Margin::symmetric(4, 0),
            &mut field.shown,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{DRAG_PX_PER_STEP, dragged_bpm_tenths};

    #[test]
    fn a_drag_steps_whole_bpm_and_fine_steps_tenths() {
        let step = DRAG_PX_PER_STEP;
        assert_eq!(dragged_bpm_tenths(1203, 0.0, false), (1203, false));
        assert_eq!(dragged_bpm_tenths(1203, step * 2.0, false), (1223, false));
        assert_eq!(dragged_bpm_tenths(1203, -step * 3.0, false), (1173, false));
        assert_eq!(dragged_bpm_tenths(1203, step * 2.0, true), (1205, false));
    }

    /// Less than a step's distance, either way, changes nothing.
    #[test]
    fn a_partial_step_counts_for_nothing() {
        let almost = DRAG_PX_PER_STEP * 0.9;
        assert_eq!(dragged_bpm_tenths(1200, almost, false), (1200, false));
        assert_eq!(dragged_bpm_tenths(1200, -almost, false), (1200, false));
    }

    /// Past the range's end the tempo stops there, and says so — the drag
    /// re-anchors, so turning round acts at once.
    #[test]
    fn a_drag_stops_at_the_tempo_range() {
        assert_eq!(dragged_bpm_tenths(2990, 1000.0, false), (3000, true));
        assert_eq!(dragged_bpm_tenths(210, -1000.0, false), (200, true));
        assert_eq!(
            dragged_bpm_tenths(2990, DRAG_PX_PER_STEP, false),
            (3000, false)
        );
    }
}
