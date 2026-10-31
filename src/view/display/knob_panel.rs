//! The knob panel (`390-knob-panel.md`): eight virtual knobs in a column of
//! their own left of the clip view, under the track headers, acting on the
//! selected track. Mouse only — a turn never takes the keyboard. A
//! vertical drag on a dial turns it (⇧ for fine steps) once it has moved
//! past a small dead zone, a ⌘/Ctrl-click puts it back to 0: each new value
//! goes
//! out live as the knob's CC through the `"computerkeys"` thread, routed
//! like a played note to the selected track's output, and to the sequencer
//! to be kept (`InputEvent::SetKnobValue`, not undoable). A scroll on a dial
//! turns it the same way — a scroll that began on that dial, its momentum
//! included; momentum coasting onto the panel from elsewhere turns nothing
//! ([`WheelLatch`]).
//! A click on a dial — a press and release that never left the dead zone —
//! or on the label under it, with a clip open, selects the knob: its CC
//! lane shows under the piano roll (`ClipLane::Knob`) and the panel takes
//! the keyboard, so `Delete` clears the CC from the clip
//! (`Display::select_knob`, `input/knob_focus.rs`). The label names the CC; a double-click
//! there opens a field to type another, an undoable `SetKnobCcEdit`. A scroll
//! anywhere else on the panel does nothing.
//!
//! The geometry is pure and unit-tested here; the drawing is
//! `rendering/knob_panel.rs`. See `030-ui-design.md` § Knob Panel.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use egui::{FontId, Id, Pos2, Rect, TouchPhase, Ui, pos2, vec2};

use crate::core::input_event::InputEvent;
use crate::core::midi::message::control_change;
use crate::models::knobs::{CC_VALUE_MAX, KNOB_COUNT};

use super::Display;
use super::clip_lane::ClipLane;
use super::text_field::{CHIP_MARGIN, FieldEvent, FieldStyle, TextField, field_event};

/// The panel's inset from its column's edges, around the knob grid.
const PANEL_PAD: f32 = 8.0;
/// How many rows each of the two columns of knobs has.
const ROWS: usize = KNOB_COUNT / 2;
/// The height of the CC label under each dial.
pub(super) const LABEL_H: f32 = 14.0;
/// The gap between a dial and its label.
const LABEL_GAP: f32 = 2.0;
/// The largest a dial is drawn, across: with room to spare it stays this
/// size rather than filling its cell.
const DIAL_MAX_D: f32 = 40.0;
/// The smallest, in a panel too short for the rest: the dials shrink to fit
/// rather than clip (`390` decision 3).
const DIAL_MIN_D: f32 = 6.0;
/// Points of vertical travel per CC step, dragged or scrolled — the whole
/// range in about 190.
const DRAG_PX_PER_STEP: f32 = 1.5;
/// Points of vertical travel a press on a dial may wander before it turns
/// the knob: within it the press is a click, which selects the knob,
/// and a hand's jitter never sends (or records) a stray step.
const DRAG_DEAD_ZONE: f32 = 4.0;
/// How much slower a ⇧-drag turns.
const FINE_FACTOR: f32 = 5.0;
/// Where a dial's sweep starts, the value 0: down-left, in screen radians
/// (0 = right, growing clockwise since screen y points down).
const SWEEP_START: f32 = 0.75 * PI;
/// How far the sweep turns from 0 to 127: three quarters of a circle,
/// through the top.
pub(super) const SWEEP: f32 = 1.5 * PI;
/// A trackpad gesture that starts this soon after the last one ended carries
/// it on: macOS sends the coasting as a second gesture right after the
/// fingers lift, and ends it the moment a touch cuts it short, with the
/// touch's own gesture right behind.
const MOMENTUM_GAP: Duration = Duration::from_millis(150);
/// How much of a coasting gesture's travel turns the dial: the OS's inertia
/// is tuned for scrolling pages, far too lively for a dial's 128 steps. A
/// faster flick still coasts further, in proportion. The fingers' own travel
/// counts in full.
const MOMENTUM_SCALE: f32 = 0.35;
/// How long a wheel turn's value stays the one shown and counted from after
/// its last event, ahead of the published value catching up. A turn on the
/// same dial after a longer pause starts again from the published value.
const WHEEL_TURN_HOLD: Duration = Duration::from_millis(500);

/// Where one knob is drawn and hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KnobRects {
    /// The dial's centre.
    pub(super) center: Pos2,
    /// The dial's radius.
    pub(super) radius: f32,
    /// The CC label under it.
    pub(super) label: Rect,
}

impl KnobRects {
    /// The square the dial is drawn in — where a press starts a drag.
    pub(super) fn dial(&self) -> Rect {
        Rect::from_center_size(self.center, vec2(self.radius, self.radius) * 2.0)
    }
}

/// Lays the eight knobs out in `panel`: two columns of four, in reading
/// order (the first column top to bottom, then the second), each dial
/// centred in its cell above its label, as large as the cell allows up to
/// [`DIAL_MAX_D`].
pub(super) fn knob_layout(panel: Rect) -> [KnobRects; KNOB_COUNT] {
    let inner = panel.shrink(PANEL_PAD);
    let cell_w = inner.width() / 2.0;
    let cell_h = inner.height() / ROWS as f32;
    let dial_d = cell_w
        .min(cell_h - LABEL_H - LABEL_GAP)
        .clamp(DIAL_MIN_D, DIAL_MAX_D);
    let block_h = dial_d + LABEL_GAP + LABEL_H;
    std::array::from_fn(|i| {
        let (col, row) = (i / ROWS, i % ROWS);
        let center_x = inner.min.x + cell_w * (col as f32 + 0.5);
        let block_top = inner.min.y + cell_h * row as f32 + ((cell_h - block_h) * 0.5).max(0.0);
        let center = pos2(center_x, block_top + dial_d * 0.5);
        let label_top = block_top + dial_d + LABEL_GAP;
        KnobRects {
            center,
            radius: dial_d * 0.5,
            label: Rect::from_min_size(
                pos2(center_x - cell_w * 0.5, label_top),
                vec2(cell_w, LABEL_H),
            ),
        }
    })
}

/// The screen angle of a dial's pointer at `value`, `0..=127` along
/// [`SWEEP`] from [`SWEEP_START`].
pub(super) fn knob_angle(value: u8) -> f32 {
    SWEEP_START + SWEEP * f32::from(value.min(CC_VALUE_MAX)) / f32::from(CC_VALUE_MAX)
}

/// The value a drag reaches: `anchor` moved by `travel` points (upward
/// positive), [`FINE_FACTOR`] slower with `fine`, and whether it ran into
/// either end of the range.
pub(super) fn dragged_knob_value(anchor: u8, travel: f32, fine: bool) -> (u8, bool) {
    let raw = f32::from(anchor) + (travel / px_per_step(fine)).trunc();
    let value = raw.clamp(0.0, f32::from(CC_VALUE_MAX));
    (value as u8, raw != value)
}

/// Points of travel per CC step, [`FINE_FACTOR`] more with `fine`.
fn px_per_step(fine: bool) -> f32 {
    if fine {
        DRAG_PX_PER_STEP * FINE_FACTOR
    } else {
        DRAG_PX_PER_STEP
    }
}

/// Takes the whole CC steps out of `pending` points of scroll travel
/// (upward positive), leaving the rest for the next event.
pub(super) fn take_steps(pending: &mut f32, fine: bool) -> i32 {
    let per_step = px_per_step(fine);
    let steps = (*pending / per_step).trunc();
    *pending -= steps * per_step;
    steps as i32
}

/// Which dial a stream of scroll events turns. A trackpad gesture (`Start`
/// … `End`) turns the dial it started on, wherever the pointer goes after,
/// and nothing if it started off a dial. A gesture that starts within
/// [`MOMENTUM_GAP`] of the last one's end carries that one on, whatever is
/// under the pointer: it is the last one's momentum (macOS sends the
/// coasting as a gesture of its own), or a touch that cut the coasting
/// short. So a flick on a dial coasts on it and a quick re-flick keeps
/// turning it, and a scroll of the clip view coasting onto the panel turns
/// nothing. A mouse wheel has no phases (every event `Move`, no gesture
/// open): each event turns the dial under the pointer. Pure, unit-tested.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WheelLatch {
    /// The open trackpad gesture.
    gesture: Option<WheelGesture>,
    /// Whether it carries the last one on — coasting, mostly.
    carried: bool,
    /// When the last one ended, and what it turned.
    last_end: Option<(Instant, WheelGesture)>,
}

/// What a trackpad gesture turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelGesture {
    /// Knob `n`'s dial, where it started.
    Knob(usize),
    /// Nothing: it started off a dial.
    Elsewhere,
}

impl WheelLatch {
    /// The knob a scroll event of `phase` at `now` turns, `dial` the dial
    /// under the pointer, and how much of its travel counts
    /// ([`travel_scale`](Self::travel_scale), of the gesture the event
    /// belongs to — an `End`'s is the one it closes).
    pub(super) fn on_wheel(
        &mut self,
        phase: TouchPhase,
        now: Instant,
        dial: Option<usize>,
    ) -> Option<(usize, f32)> {
        match phase {
            TouchPhase::Start => {
                let carried = self
                    .last_end
                    .filter(|(end, _)| now.duration_since(*end) < MOMENTUM_GAP)
                    .map(|(_, gesture)| gesture);
                self.carried = carried.is_some();
                self.gesture = Some(carried.unwrap_or(match dial {
                    Some(knob) => WheelGesture::Knob(knob),
                    None => WheelGesture::Elsewhere,
                }));
                self.turned()
            }
            TouchPhase::Move => match self.gesture {
                Some(_) => self.turned(),
                None => dial.map(|knob| (knob, 1.0)),
            },
            TouchPhase::End | TouchPhase::Cancel => {
                let turned = self.turned();
                self.last_end = self.gesture.take().map(|gesture| (now, gesture));
                turned
            }
        }
    }

    /// The open gesture's knob and travel scale, if it turns one.
    fn turned(&self) -> Option<(usize, f32)> {
        self.knob().map(|knob| (knob, self.travel_scale()))
    }

    /// How much of the open gesture's travel turns the dial:
    /// [`MOMENTUM_SCALE`] while it carries the last one on, else all of it.
    /// A touch that cut the coasting short counts as carried too (the OS
    /// shows no difference); it soon coasts itself.
    fn travel_scale(&self) -> f32 {
        if self.gesture.is_some() && self.carried {
            MOMENTUM_SCALE
        } else {
            1.0
        }
    }

    /// The knob the open gesture turns, if any — the panes don't scroll on
    /// it meanwhile.
    pub(super) fn knob(&self) -> Option<usize> {
        match self.gesture {
            Some(WheelGesture::Knob(knob)) => Some(knob),
            _ => None,
        }
    }
}

/// What a press in the panel landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KnobHit {
    /// Knob `n`'s dial.
    Dial(usize),
    /// Knob `n`'s CC label.
    Label(usize),
}

/// A drag on a dial: which knob, and the travel since its anchor (the
/// tempo chip's drag's shape — `tempo_field.rs`).
#[derive(Clone, Copy, Debug)]
struct KnobDrag {
    /// The knob's track.
    track_idx: usize,
    /// Which knob.
    knob: usize,
    /// The CC it sends.
    cc: u8,
    /// The value the travel counts from.
    anchor: u8,
    /// Points travelled upward since the anchor.
    travel: f32,
    /// Whether ⇧ was down at the last step.
    fine: bool,
    /// The value reached, shown in the dial while the drag runs (the hand
    /// wins).
    value: u8,
    /// Where the knob stood when the drag began, until its first move has
    /// gone out ahead of it ([`Display::send_knob_value`]).
    from: Option<u8>,
    /// Points travelled upward while still inside [`DRAG_DEAD_ZONE`];
    /// `None` once the drag has left it and turns the knob.
    slop: Option<f32>,
}

impl KnobDrag {
    /// A drag of a knob standing at `value`, not yet out of the dead zone.
    fn new(track_idx: usize, knob: usize, cc: u8, value: u8) -> Self {
        Self {
            track_idx,
            knob,
            cc,
            anchor: value,
            travel: 0.0,
            fine: false,
            value,
            from: Some(value),
            slop: Some(0.0),
        }
    }

    /// Raw pointer motion `dy` (downward positive): the new value, when it
    /// changed. Nothing turns until the travel leaves [`DRAG_DEAD_ZONE`],
    /// and the turn then counts from there, so the knob never jumps. ⇧
    /// going down or up re-anchors at the value reached, and so does
    /// hitting either end.
    fn extend(&mut self, dy: f32, fine: bool) -> Option<u8> {
        if let Some(slop) = self.slop.as_mut() {
            *slop -= dy;
            if slop.abs() >= DRAG_DEAD_ZONE {
                self.slop = None;
                self.fine = fine;
            }
            return None;
        }
        if fine != self.fine {
            self.fine = fine;
            self.travel = 0.0;
            self.anchor = self.value;
        }
        self.travel -= dy;
        let (value, clamped) = dragged_knob_value(self.anchor, self.travel, fine);
        if clamped {
            self.travel = 0.0;
            self.anchor = value;
        }
        (value != self.value).then(|| {
            self.value = value;
            value
        })
    }

    /// Whether the drag never left the dead zone: a click.
    fn is_click(&self) -> bool {
        self.slop.is_some()
    }
}

/// A dial being turned by scrolling: the value it has reached, ahead of
/// the published one, and the travel not yet worth a step.
#[derive(Clone, Copy, Debug)]
struct WheelTurn {
    /// The knob's track.
    track_idx: usize,
    /// Which knob.
    knob: usize,
    /// The CC it sends.
    cc: u8,
    /// The value reached.
    value: u8,
    /// Where the knob stood when the turn began, until its first step has
    /// gone out ahead of it ([`Display::send_knob_value`]).
    from: Option<u8>,
    /// Points of travel (upward positive) short of a step.
    pending: f32,
    /// When its last scroll event came.
    last: Instant,
}

impl WheelTurn {
    /// Whether this turn is still knob `knob` of `track_idx`'s, at `now`.
    fn holds(&self, track_idx: usize, knob: usize, now: Instant) -> bool {
        (self.track_idx, self.knob) == (track_idx, knob)
            && now.duration_since(self.last) < WHEEL_TURN_HOLD
    }
}

/// The open CC field on a label.
struct CcField {
    /// The knob's track.
    track_idx: usize,
    /// Which knob.
    knob: usize,
    /// The field itself.
    field: TextField,
}

/// The knob panel's view state: the drag, the wheel and the CC field. Where the knobs are is derived from the pane layout every frame.
#[derive(Default)]
pub(super) struct KnobPanel {
    /// The dial being dragged.
    drag: Option<KnobDrag>,
    /// Which dial the scroll events turn.
    wheel: WheelLatch,
    /// The dial being turned by scrolling.
    wheel_turn: Option<WheelTurn>,
    /// The open CC field.
    field: Option<CcField>,
}

impl KnobPanel {
    /// Whether a dial is being dragged — a drag that holds the pointer.
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Whether the CC field is open; it has the keyboard while it is.
    pub(super) fn editing(&self) -> bool {
        self.field.is_some()
    }
}

impl Display {
    /// The knob panel's fill this frame, when the clip view shows: its
    /// column less the strip of bare canvas at its right, as the
    /// track-header column's.
    pub(super) fn knob_panel_rect(&self) -> Option<Rect> {
        let mut panel = self.pane_rects().knobs?;
        panel.max.x -= Self::TRACK_COLUMN_GRID_GAP_X;
        Some(panel)
    }

    /// Whether `(x, y)` is in the knob panel's column.
    pub(super) fn is_over_knob_panel(&self, x: f32, y: f32) -> bool {
        self.pane_rects()
            .knobs
            .is_some_and(|column| column.contains(pos2(x, y)))
    }

    /// The knobs' layout this frame.
    pub(super) fn knob_rects(&self) -> Option<[KnobRects; KNOB_COUNT]> {
        self.knob_panel_rect().map(knob_layout)
    }

    /// The dial or label under `(x, y)`.
    pub(super) fn knob_at(&self, x: f32, y: f32) -> Option<KnobHit> {
        let pos = pos2(x, y);
        self.knob_rects()?.iter().enumerate().find_map(|(i, knob)| {
            if knob.dial().contains(pos) {
                Some(KnobHit::Dial(i))
            } else {
                knob.label.contains(pos).then_some(KnobHit::Label(i))
            }
        })
    }

    /// The dial under `(x, y)`, if any.
    fn dial_at(&self, x: f32, y: f32) -> Option<usize> {
        match self.knob_at(x, y)? {
            KnobHit::Dial(knob) => Some(knob),
            KnobHit::Label(_) => None,
        }
    }

    /// Whether the pointer is on a dial — the vertical arrows, as on the
    /// track header's bars.
    pub(super) fn is_pointer_on_knob_dial(&self) -> bool {
        self.canvas_pointer()
            .is_some_and(|pos| self.dial_at(pos.x, pos.y).is_some())
    }

    /// The track the knobs act on: the selected one, if there is one.
    pub(super) fn knobs_track(&self) -> Option<usize> {
        (self.selected_track_idx < self.tracks.len()).then_some(self.selected_track_idx)
    }

    /// Knob `knob` of the selected track as drawn: its CC and where it
    /// stands — the drag's value while it is dragged, else the published
    /// one (a wheel turn's, for a moment after it), `None` while never
    /// turned.
    pub(super) fn knob_shown(&self, knob: usize) -> Option<(u8, Option<u8>)> {
        let track_idx = self.knobs_track()?;
        let lane = &self.tracks[track_idx];
        if let Some(drag) = self.knob_panel.drag
            && (drag.track_idx, drag.knob) == (track_idx, knob)
        {
            return Some((drag.cc, Some(drag.value)));
        }
        if let Some(turn) = self.knob_panel.wheel_turn
            && turn.holds(track_idx, knob, Instant::now())
        {
            return Some((turn.cc, Some(turn.value)));
        }
        let cc = *lane.knob_ccs.get(knob)?;
        Some((cc, self.track_mix.knob_value(lane.slot, knob)))
    }

    /// A press in the knob panel: a dial starts its drag (a click if it
    /// never turns, shown on the release — [`Display::finish_knob_drag`]),
    /// or with ⌘/Ctrl goes back to 0 and starts none — the track header
    /// bars' reset gesture; a label, with a clip open, selects the
    /// knob, its CC's lane under the piano roll (`390-knob-panel.md` decisions 11 and 14).
    /// Only that selection takes the keyboard.
    pub(super) fn handle_knob_panel_click(&mut self, x: f32, y: f32, command: bool) {
        match self.knob_at(x, y) {
            Some(KnobHit::Dial(knob)) if command => self.reset_knob(knob),
            Some(KnobHit::Dial(knob)) => self.begin_knob_drag(knob),
            Some(KnobHit::Label(knob)) if self.knob_labels_switch_lanes() => {
                self.select_knob(knob);
            }
            Some(KnobHit::Label(_)) | None => {}
        }
    }

    /// Whether a knob's label switches the piano roll's lower lane: only
    /// with a clip open, since there is no lane without one.
    pub(super) fn knob_labels_switch_lanes(&self) -> bool {
        self.lead_clip_time.is_some()
    }

    /// The knob whose dial or label is under the pointer — the label lit as
    /// a hover, since a click on either shows its lane.
    pub(super) fn knob_under_pointer(&self) -> Option<usize> {
        let pos = self.canvas_pointer()?;
        match self.knob_at(pos.x, pos.y)? {
            KnobHit::Dial(knob) | KnobHit::Label(knob) => Some(knob),
        }
    }

    /// The knob whose label is under the pointer — the pointing hand; a
    /// dial keeps its vertical arrows, a drag being its main use.
    fn knob_label_under_pointer(&self) -> Option<usize> {
        let pos = self.canvas_pointer()?;
        match self.knob_at(pos.x, pos.y)? {
            KnobHit::Label(knob) => Some(knob),
            KnobHit::Dial(_) => None,
        }
    }

    /// The knob whose label is under the pointer, when a click there would
    /// switch the lane — not the lane already shown.
    pub(super) fn knob_lane_switch_under_pointer(&self) -> Option<usize> {
        self.knob_label_under_pointer().filter(|&knob| {
            self.knob_labels_switch_lanes() && self.render.clip_lane != ClipLane::Knob(knob)
        })
    }

    /// Turns knob `knob` of the selected track back to 0, where a knob
    /// never turned is drawn — a macro's rest. Sent even when it shows 0
    /// already, so a plugin moved some other way follows.
    fn reset_knob(&mut self, knob: usize) {
        let (Some(track_idx), Some((cc, from))) = (self.knobs_track(), self.knob_shown(knob))
        else {
            return;
        };
        self.knob_panel.wheel_turn = None;
        self.send_knob_value(track_idx, knob, cc, Some(from.unwrap_or(0)), 0);
    }

    /// A double-click in the knob panel: on a label, opens its CC field.
    pub(super) fn handle_knob_panel_double_click(&mut self, x: f32, y: f32) {
        if let Some(KnobHit::Label(knob)) = self.knob_at(x, y) {
            self.open_knob_cc_field(knob);
        }
    }

    /// Starts dragging knob `knob` of the selected track, from where it
    /// stands (0 for a knob never turned), and takes hold of it: until the
    /// release, playback leaves its CC to the hand (`390-knob-panel.md`
    /// decision 7).
    fn begin_knob_drag(&mut self, knob: usize) {
        let Some(track_idx) = self.knobs_track() else {
            return;
        };
        let Some((cc, value)) = self.knob_shown(knob) else {
            return;
        };
        self.knob_panel.drag = Some(KnobDrag::new(track_idx, knob, cc, value.unwrap_or(0)));
        self.input_event_tx
            .send(InputEvent::HoldKnob { track_idx, knob })
            .ok();
    }

    /// Raw pointer motion `dy` (downward positive) with the button held: the
    /// value for the travel since the anchor, sent live and to the
    /// sequencer when it changed ([`KnobDrag::extend`]).
    pub(super) fn extend_knob_drag(&mut self, dy: f32, fine: bool) {
        let Some(drag) = self.knob_panel.drag.as_mut() else {
            return;
        };
        let Some(value) = drag.extend(dy, fine) else {
            return;
        };
        let from = drag.from.take();
        let KnobDrag {
            track_idx,
            knob,
            cc,
            ..
        } = *drag;
        self.send_knob_value(track_idx, knob, cc, from, value);
    }

    /// Knob `knob` of `track_idx`, sending `cc`, now stands at `value`: its
    /// CC goes out live, and the sequencer keeps the position. A gesture's
    /// first move passes `from`, where the knob stood, sent just ahead of it
    /// — the same value again, so nothing is heard, but the take records
    /// where the turn started, which a clip with no earlier data for the CC
    /// plays before it (`Clip::hold_cc_start_values`, `390-knob-panel.md`
    /// decision 9). Every later move passes `None`.
    fn send_knob_value(&self, track_idx: usize, knob: usize, cc: u8, from: Option<u8>, value: u8) {
        let mut messages = Vec::with_capacity(2);
        if let Some(from) = from.filter(|&from| from != value) {
            messages.push(control_change(cc, from));
        }
        messages.push(control_change(cc, value));
        self.send_computer_keys(messages);
        self.input_event_tx
            .send(InputEvent::SetKnobValue {
                track_idx,
                knob,
                value,
            })
            .ok();
    }

    /// The button came up: the drag is done, its values already sent, and
    /// playback takes the knob over again at its CC's next move. A drag
    /// that never left the dead zone was a click: with a clip open it
    /// selects the knob, as a click on its label does (`390-knob-panel.md`
    /// decision 11) — on the release, so a turn never switches the lane.
    pub(super) fn finish_knob_drag(&mut self) {
        let Some(drag) = self.knob_panel.drag.take() else {
            return;
        };
        self.input_event_tx.send(InputEvent::ReleaseKnobs).ok();
        if drag.is_click()
            && self.knobs_track() == Some(drag.track_idx)
            && self.knob_labels_switch_lanes()
        {
            self.select_knob(drag.knob);
        }
    }

    /// One raw scroll event (`InputEvent::WheelScrolled`), the pointer at
    /// `pointer`: turns the dial the [`WheelLatch`] says it turns, if any,
    /// by `delta_y` points (content down positive, so the knob turns up as
    /// the content would move up — fingers up on a trackpad), damped while
    /// it coasts ([`WheelLatch::travel_scale`]).
    pub(super) fn handle_knob_wheel(
        &mut self,
        delta_y: f32,
        fine: bool,
        phase: TouchPhase,
        pointer: Option<(f32, f32)>,
    ) {
        let now = Instant::now();
        let dial = pointer.and_then(|(x, y)| self.dial_at(x, y));
        let Some((knob, scale)) = self.knob_panel.wheel.on_wheel(phase, now, dial) else {
            return;
        };
        let Some(track_idx) = self.knobs_track() else {
            return;
        };
        let mut turn = match self.knob_panel.wheel_turn {
            Some(turn) if turn.holds(track_idx, knob, now) => turn,
            _ => {
                let Some((cc, value)) = self.knob_shown(knob) else {
                    return;
                };
                let value = value.unwrap_or(0);
                WheelTurn {
                    track_idx,
                    knob,
                    cc,
                    value,
                    from: Some(value),
                    pending: 0.0,
                    last: now,
                }
            }
        };
        turn.pending -= delta_y * scale;
        turn.last = now;
        let steps = take_steps(&mut turn.pending, fine);
        let value = (i32::from(turn.value) + steps).clamp(0, i32::from(CC_VALUE_MAX)) as u8;
        if value == turn.value {
            self.knob_panel.wheel_turn = Some(turn);
            return;
        }
        turn.value = value;
        let from = turn.from.take();
        self.knob_panel.wheel_turn = Some(turn);
        self.send_knob_value(track_idx, knob, turn.cc, from, value);
    }

    /// Whether the panes' scroll (`InputEvent::TimelineScroll`) is the knob
    /// panel's: over the panel, or while a trackpad gesture or its momentum
    /// turns a dial (it may have left the panel). The panel scrolls nothing
    /// of its own.
    pub(super) fn knob_panel_takes_scroll(&self, pointer: Option<(f32, f32)>) -> bool {
        self.knob_panel.wheel.knob().is_some()
            || pointer.is_some_and(|(x, y)| self.is_over_knob_panel(x, y))
    }

    /// Opens the CC field on knob `knob`'s label, its number in it selected.
    fn open_knob_cc_field(&mut self, knob: usize) {
        let Some((cc, _)) = self.knob_shown(knob) else {
            return;
        };
        let Some(track_idx) = self.knobs_track() else {
            return;
        };
        self.close_output_menu();
        self.knob_panel.field = Some(CcField {
            track_idx,
            knob,
            field: TextField::new(cc.to_string()),
        });
    }

    /// Whether the CC field is open on knob `knob`'s label — the label
    /// isn't drawn under it.
    pub(super) fn knob_cc_field_open_on(&self, knob: usize) -> bool {
        self.knob_panel
            .field
            .as_ref()
            .is_some_and(|field| field.knob == knob)
    }

    /// Where the open CC field is: over its label.
    fn knob_cc_field_rect(&self) -> Option<Rect> {
        let field = self.knob_panel.field.as_ref()?;
        Some(self.knob_rects()?[field.knob].label)
    }

    /// The number the open field holds, if it is one. Whether a knob may
    /// send it is the sequencer's to answer (a refusal says why in the
    /// footer).
    fn typed_knob_cc(&self) -> Option<u8> {
        self.knob_panel
            .field
            .as_ref()?
            .field
            .text
            .trim()
            .parse()
            .ok()
    }

    /// Closes the CC field, putting its knob on `cc` unless that is `None`.
    fn close_knob_cc_field(&mut self, cc: Option<u8>) {
        let Some(field) = self.knob_panel.field.take() else {
            return;
        };
        if let Some(cc) = cc {
            self.input_event_tx
                .send(InputEvent::SetKnobCc {
                    track_idx: field.track_idx,
                    knob: field.knob,
                    cc,
                })
                .ok();
        }
    }

    /// While the CC field is open it gets first look at every event, by the
    /// inline text fields' rule ([`field_event`]); Enter on text that isn't
    /// a number leaves it open. Returns whether the event was consumed.
    pub(super) fn handle_knob_cc_field_input_event(&mut self, event: &InputEvent) -> bool {
        if !self.knob_panel.editing() {
            return false;
        }
        // The panel can go (⌘⌥E hides the clip view): the field goes with it.
        let Some(rect) = self.knob_cc_field_rect() else {
            self.close_knob_cc_field(None);
            return false;
        };
        let action = field_event(event, Some(rect));
        match action {
            FieldEvent::Commit => {
                if let Some(cc) = self.typed_knob_cc() {
                    self.close_knob_cc_field(Some(cc));
                }
            }
            FieldEvent::CommitAndPass => self.close_knob_cc_field(self.typed_knob_cc()),
            FieldEvent::Cancel => self.close_knob_cc_field(None),
            FieldEvent::Swallow | FieldEvent::Pass => {}
        }
        action.consumed()
    }

    /// Shows the open CC field over its label, in the canvas `ui`. Its first
    /// frame takes the egui focus and selects the number.
    pub(super) fn show_knob_cc_field(&mut self, ui: &mut Ui) {
        let Some(rect) = self.knob_cc_field_rect() else {
            return;
        };
        if let Some(open) = self.knob_panel.field.as_mut() {
            let style =
                FieldStyle::new(FontId::proportional(LABEL_FONT_SIZE), CHIP_MARGIN).max_chars(3);
            open.field
                .show(ui, Id::new("knob-cc-field"), rect, style, true);
        }
    }
}

/// The size of a knob's label, and of its CC field.
pub(super) const LABEL_FONT_SIZE: f32 = 11.0;

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::time::{Duration, Instant};

    use egui::{Rect, TouchPhase, pos2};

    use super::{
        CC_VALUE_MAX, DIAL_MAX_D, DIAL_MIN_D, DRAG_DEAD_ZONE, DRAG_PX_PER_STEP, KnobDrag,
        MOMENTUM_SCALE, SWEEP, SWEEP_START, WheelLatch, dragged_knob_value, knob_angle,
        knob_layout, take_steps,
    };

    fn panel(height: f32) -> Rect {
        Rect::from_min_max(pos2(0.0, 500.0), pos2(160.0, 500.0 + height))
    }

    #[test]
    fn the_knobs_read_down_the_first_column_then_the_second() {
        let knobs = knob_layout(panel(320.0));
        for i in 0..3 {
            assert!(knobs[i + 1].center.y > knobs[i].center.y);
            assert_eq!(knobs[i + 1].center.x, knobs[i].center.x);
        }
        assert!(knobs[4].center.x > knobs[0].center.x);
        assert_eq!(knobs[4].center.y, knobs[0].center.y);
        // Each label sits under its dial.
        for knob in knobs {
            assert!(knob.label.min.y >= knob.center.y + knob.radius);
            assert!(
                knob.label
                    .contains(pos2(knob.center.x, knob.label.center().y))
            );
        }
    }

    #[test]
    fn the_dials_stop_growing_at_their_largest_and_shrink_to_fit() {
        let roomy = knob_layout(panel(800.0));
        assert_eq!(roomy[0].radius * 2.0, DIAL_MAX_D);

        // A short panel: every dial and label stays inside it.
        let short = panel(200.0);
        let knobs = knob_layout(short);
        assert!(knobs[0].radius * 2.0 < DIAL_MAX_D);
        for knob in knobs {
            assert!(short.contains_rect(knob.dial()));
            assert!(short.contains_rect(knob.label));
        }
        // And a tiny one never goes below the smallest dial.
        assert_eq!(knob_layout(panel(40.0))[0].radius * 2.0, DIAL_MIN_D);
    }

    #[test]
    fn the_pointer_sweeps_three_quarters_through_the_top() {
        assert_eq!(knob_angle(0), SWEEP_START);
        assert_eq!(knob_angle(CC_VALUE_MAX), SWEEP_START + SWEEP);
        // Halfway is straight up (-π/2, or 3π/2 a turn on).
        let mid = (f32::from(CC_VALUE_MAX) / 2.0) / f32::from(CC_VALUE_MAX);
        let up = SWEEP_START + SWEEP * mid;
        assert!((up - 1.5 * PI).abs() < 1e-5);
        assert_eq!(knob_angle(200), knob_angle(CC_VALUE_MAX));
    }

    #[test]
    fn a_press_that_wanders_inside_the_dead_zone_is_a_click_and_turns_nothing() {
        let mut drag = KnobDrag::new(0, 3, 14, 64);
        let jitter = DRAG_DEAD_ZONE * 0.6;
        // Up and back down: the travel is net, so it never leaves the zone.
        assert_eq!(drag.extend(-jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert!(drag.is_click());
        assert_eq!(drag.value, 64);
    }

    #[test]
    fn a_drag_out_of_the_dead_zone_turns_from_its_edge_without_a_jump() {
        let mut drag = KnobDrag::new(0, 0, 14, 64);
        // Leaving the zone turns nothing yet, and the press is no click.
        assert_eq!(drag.extend(-DRAG_DEAD_ZONE, false), None);
        assert!(!drag.is_click());
        // One step's travel from the edge is one step.
        assert_eq!(drag.extend(-DRAG_PX_PER_STEP, false), Some(65));
        assert_eq!(drag.extend(DRAG_PX_PER_STEP * 2.0, false), Some(63));
    }

    #[test]
    fn a_drag_up_turns_the_knob_up_and_stops_at_either_end() {
        assert_eq!(dragged_knob_value(64, 0.0, false), (64, false));
        assert_eq!(dragged_knob_value(64, 15.0, false), (74, false));
        assert_eq!(dragged_knob_value(64, -15.0, false), (54, false));
        // Fine steps are five times slower.
        assert_eq!(dragged_knob_value(64, 15.0, true), (66, false));
        assert_eq!(dragged_knob_value(120, 300.0, false), (127, true));
        assert_eq!(dragged_knob_value(5, -300.0, false), (0, true));
    }

    #[test]
    fn scroll_travel_turns_in_whole_steps_and_keeps_the_rest() {
        let mut pending = 4.0;
        assert_eq!(take_steps(&mut pending, false), 2);
        assert!((pending - 1.0).abs() < 1e-5);
        let mut pending = -4.0;
        assert_eq!(take_steps(&mut pending, false), -2);
        // Fine: five times the travel per step.
        let mut pending = 4.0;
        assert_eq!(take_steps(&mut pending, true), 0);
        assert_eq!(pending, 4.0);
    }

    /// The knob `latch` turns for a scroll event, its travel scale aside.
    fn knob_on(
        latch: &mut WheelLatch,
        phase: TouchPhase,
        now: Instant,
        dial: Option<usize>,
    ) -> Option<usize> {
        latch.on_wheel(phase, now, dial).map(|(knob, _)| knob)
    }

    /// `ms` milliseconds after `t0`.
    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn a_trackpad_gesture_turns_the_dial_it_started_on_wherever_it_goes() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        assert_eq!(knob_on(&mut latch, TouchPhase::Start, t0, Some(2)), Some(2));
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 10), Some(2)),
            Some(2)
        );
        // Off the dial, onto another: still knob 2.
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 20), None),
            Some(2)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 30), Some(5)),
            Some(2)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::End, at(t0, 40), None),
            Some(2)
        );
        assert_eq!(latch.knob(), None);
    }

    #[test]
    fn a_flick_on_a_dial_coasts_on_it() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        knob_on(&mut latch, TouchPhase::Start, t0, Some(1));
        knob_on(&mut latch, TouchPhase::End, at(t0, 80), Some(1));
        // The coasting starts at once, the pointer drifting off the dial.
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Start, at(t0, 85), None),
            Some(1)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 120), Some(6)),
            Some(1)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::End, at(t0, 600), None),
            Some(1)
        );
        // A touch long after is a gesture of its own.
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Start, at(t0, 1500), Some(6)),
            Some(6)
        );
    }

    #[test]
    fn only_the_coasting_is_damped() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        let mut scale = |phase, ms| latch.on_wheel(phase, at(t0, ms), Some(1)).map(|(_, s)| s);
        assert_eq!(
            scale(TouchPhase::Start, 0),
            Some(1.0),
            "the fingers' own travel"
        );
        assert_eq!(scale(TouchPhase::End, 80), Some(1.0));
        assert_eq!(scale(TouchPhase::Start, 85), Some(MOMENTUM_SCALE));
        assert_eq!(scale(TouchPhase::Move, 100), Some(MOMENTUM_SCALE));
        // The `End` closing the coasting is still the coasting's.
        assert_eq!(scale(TouchPhase::End, 600), Some(MOMENTUM_SCALE));
        // A mouse wheel, with no gesture open.
        assert_eq!(scale(TouchPhase::Move, 700), Some(1.0));
        assert_eq!(
            scale(TouchPhase::Start, 2000),
            Some(1.0),
            "a touch of its own"
        );
    }

    #[test]
    fn a_quick_reflick_cutting_the_coasting_short_keeps_turning_the_dial() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        knob_on(&mut latch, TouchPhase::Start, t0, Some(2));
        knob_on(&mut latch, TouchPhase::End, at(t0, 60), Some(2));
        knob_on(&mut latch, TouchPhase::Start, at(t0, 62), Some(2));
        // The fingers touch down mid-coast: macOS ends it, the touch starts.
        knob_on(&mut latch, TouchPhase::End, at(t0, 200), Some(2));
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Start, at(t0, 201), Some(2)),
            Some(2)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 220), Some(2)),
            Some(2)
        );
    }

    #[test]
    fn a_gesture_coasting_onto_the_panel_turns_nothing() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        // A clip-view scroll: started off the dials.
        assert_eq!(knob_on(&mut latch, TouchPhase::Start, t0, None), None);
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 50), Some(3)),
            None
        );
        knob_on(&mut latch, TouchPhase::End, at(t0, 100), Some(3));
        // Its momentum, over a dial, still turns nothing.
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Start, at(t0, 110), Some(3)),
            None
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 130), Some(3)),
            None
        );
    }

    #[test]
    fn a_mouse_wheel_turns_the_dial_under_the_pointer() {
        let t0 = Instant::now();
        let mut latch = WheelLatch::default();
        assert_eq!(knob_on(&mut latch, TouchPhase::Move, t0, Some(4)), Some(4));
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 30), Some(6)),
            Some(6)
        );
        assert_eq!(
            knob_on(&mut latch, TouchPhase::Move, at(t0, 60), None),
            None
        );
        // No gesture is ever open: over the panel its scroll is swallowed
        // (`knob_panel_takes_scroll`), elsewhere the panes keep it.
        assert_eq!(latch.knob(), None);
    }
}
