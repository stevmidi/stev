//! The knob panel (`400-parameter-knobs.md`, the panel itself from
//! `390-knob-panel.md`): eight virtual knobs in a column of their own left
//! of the clip view, under the track headers, each driving one parameter of
//! the selected track's plugin. Mouse only — a turn never takes the
//! keyboard. A vertical drag on a dial turns it (⇧ for fine steps) once it
//! has moved past a small dead zone, a ⌘/Ctrl-click puts it back to the
//! parameter's default: each new value goes straight to the plugin, its
//! sound and its editor alike (`Display::set_knob_param_value`), and to the
//! sequencer as a hand turn for the unsaved prompt
//! (`InputEvent::SetKnobValue`, not undoable). A scroll on a dial turns it
//! the same way — a scroll that began on that dial, its momentum included;
//! momentum coasting onto the panel from elsewhere turns nothing
//! ([`WheelLatch`]). The knob shows the parameter's value wherever it came
//! from: a drag, the plugin's editor, a preset.
//!
//! A double-click on a knob's label arms it: the next control turned in the
//! plugin's editor becomes its parameter (`knob_params.rs`), an undoable
//! `SetKnobParamEdit`; `Esc` or a press elsewhere disarms, `Delete`
//! unassigns. An unassigned knob turns nothing. A click on a dial — a press
//! and release that never left the dead zone — or on the label under it,
//! with a clip open, selects the knob: its lane shows under the piano roll
//! (`ClipLane::Knob`) and the panel takes the keyboard
//! (`Display::select_knob`, `input/knob_focus.rs`). A track whose knobs
//! can't drive anything (no plugin, or a CLAP one for now) shows a one-line
//! hint instead of knobs. A scroll anywhere else on the panel does nothing.
//!
//! The geometry is pure and unit-tested here; the drawing is
//! `rendering/knob_panel.rs`. See `030-ui-design.md` § Knob Panel.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use egui::{Key, Pos2, Rect, TouchPhase, pos2, vec2};

use crate::core::input_event::InputEvent;
use crate::models::knobs::{KNOB_COUNT, ParamId};

use super::Display;
use super::clip_lane::ClipLane;

/// The panel's inset from its column's edges, around the knob grid.
const PANEL_PAD: f32 = 8.0;
/// How many rows each of the two columns of knobs has.
const ROWS: usize = KNOB_COUNT / 2;
/// The height of the label under each dial.
pub(super) const LABEL_H: f32 = 14.0;
/// The gap between a dial and its label.
const LABEL_GAP: f32 = 2.0;
/// The largest a dial is drawn, across: with room to spare it stays this
/// size rather than filling its cell.
const DIAL_MAX_D: f32 = 40.0;
/// The smallest, in a panel too short for the rest: the dials shrink to fit
/// rather than clip (`390` decision 3).
const DIAL_MIN_D: f32 = 6.0;
/// Points of vertical travel across a knob's whole range, dragged or
/// scrolled.
const DRAG_PX_PER_RANGE: f32 = 190.0;
/// How much slower a ⇧-drag turns.
const FINE_FACTOR: f32 = 5.0;
/// Where a dial's sweep starts, the value 0: down-left, in screen radians
/// (0 = right, growing clockwise since screen y points down).
const SWEEP_START: f32 = 0.75 * PI;
/// How far the sweep turns from 0 to 1: three quarters of a circle, through
/// the top.
pub(super) const SWEEP: f32 = 1.5 * PI;
/// A trackpad gesture that starts this soon after the last one ended carries
/// it on: macOS sends the coasting as a second gesture right after the
/// fingers lift, and ends it the moment a touch cuts it short, with the
/// touch's own gesture right behind.
const MOMENTUM_GAP: Duration = Duration::from_millis(150);
/// How much of a coasting gesture's travel turns the dial: the OS's inertia
/// is tuned for scrolling pages, far too lively for a dial. A faster flick
/// still coasts further, in proportion. The fingers' own travel counts in
/// full.
const MOMENTUM_SCALE: f32 = 0.35;
/// How long a wheel turn's value stays the one shown and counted from after
/// its last event, ahead of the plugin's value catching up. A turn on the
/// same dial after a longer pause starts again from the plugin's value.
const WHEEL_TURN_HOLD: Duration = Duration::from_millis(500);

/// Where one knob is drawn and hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KnobRects {
    /// The dial's centre.
    pub(super) center: Pos2,
    /// The dial's radius.
    pub(super) radius: f32,
    /// The label under it: the parameter's name.
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

/// The screen angle of a dial's pointer at `value` (normalized, clamped)
/// along [`SWEEP`] from [`SWEEP_START`].
pub(super) fn knob_angle(value: f64) -> f32 {
    SWEEP_START + SWEEP * value.clamp(0.0, 1.0) as f32
}

/// The value `travel` points (upward positive) turn a knob from `anchor`,
/// [`FINE_FACTOR`] slower with `fine`, and whether it ran into either end of
/// the range.
pub(super) fn turned_knob_value(anchor: f64, travel: f32, fine: bool) -> (f64, bool) {
    let raw = anchor + f64::from(travel / px_per_range(fine));
    let value = raw.clamp(0.0, 1.0);
    (value, raw != value)
}

/// Points of travel across the whole range, [`FINE_FACTOR`] more with
/// `fine`.
fn px_per_range(fine: bool) -> f32 {
    if fine {
        DRAG_PX_PER_RANGE * FINE_FACTOR
    } else {
        DRAG_PX_PER_RANGE
    }
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
    /// Knob `n`'s label.
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
    /// The parameter it drives; `None` for an unassigned knob, whose drag
    /// turns nothing but can still be a click.
    param: Option<ParamId>,
    /// The value the travel counts from.
    anchor: f64,
    /// Points travelled upward since the anchor.
    travel: f32,
    /// Whether ⇧ was down at the last step.
    fine: bool,
    /// The value reached, shown in the dial while the drag runs (the hand
    /// wins).
    value: f64,
    /// Points travelled upward while still inside the drag threshold
    /// ([`Display::DRAG_THRESHOLD_PX`], every press's click-or-drag line);
    /// `None` once the drag has left it and turns the knob.
    slop: Option<f32>,
}

impl KnobDrag {
    /// A drag of a knob standing at `value`, not yet out of the dead zone.
    fn new(track_idx: usize, knob: usize, param: Option<ParamId>, value: f64) -> Self {
        Self {
            track_idx,
            knob,
            param,
            anchor: value,
            travel: 0.0,
            fine: false,
            value,
            slop: Some(0.0),
        }
    }

    /// Raw pointer motion `dy` (downward positive): the new value, when it
    /// changed. Nothing turns until the travel leaves the drag threshold
    /// ([`Display::DRAG_THRESHOLD_PX`]: within it the press is a click, which
    /// selects the knob, and a hand's jitter never sends a step), and the
    /// turn then counts from there, so the knob never jumps. ⇧ going down or
    /// up re-anchors at the value reached, and so does hitting either end.
    fn extend(&mut self, dy: f32, fine: bool) -> Option<f64> {
        if let Some(slop) = self.slop.as_mut() {
            *slop -= dy;
            if slop.abs() >= Display::DRAG_THRESHOLD_PX {
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
        let (value, clamped) = turned_knob_value(self.anchor, self.travel, fine);
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

/// A dial being turned by scrolling: the value it has reached, ahead of the
/// plugin's.
#[derive(Clone, Copy, Debug)]
struct WheelTurn {
    /// The knob's track.
    track_idx: usize,
    /// Which knob.
    knob: usize,
    /// The parameter it drives.
    param: ParamId,
    /// The value reached.
    value: f64,
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

/// One knob of the selected track as drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KnobShown {
    /// The parameter it drives; `None` while unassigned.
    pub(super) param: Option<ParamId>,
    /// Where it stands, normalized: the drag's value while dragged, else the
    /// plugin's (a wheel turn's, for a moment after it). `None` while
    /// unassigned.
    pub(super) value: Option<f64>,
}

/// The knob panel's view state: the drag, the wheel and the armed knob.
/// Where the knobs are is derived from the pane layout every frame.
#[derive(Default)]
pub(super) struct KnobPanel {
    /// The dial being dragged.
    drag: Option<KnobDrag>,
    /// Which dial the scroll events turn.
    wheel: WheelLatch,
    /// The dial being turned by scrolling.
    wheel_turn: Option<WheelTurn>,
    /// The knob a double-click on its label armed — its track and which —
    /// waiting for a control turned in the plugin's editor
    /// (`400-parameter-knobs.md` decision 3).
    armed: Option<(usize, usize)>,
}

impl KnobPanel {
    /// Whether a dial is being dragged — a drag that holds the pointer.
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
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

    /// The knobs' layout this frame — `None` while the panel is hidden or
    /// shows its hint instead ([`knob_hint`](Self::knob_hint)).
    pub(super) fn knob_rects(&self) -> Option<[KnobRects; KNOB_COUNT]> {
        if self.knob_hint().is_some() {
            return None;
        }
        self.knob_panel_rect().map(knob_layout)
    }

    /// The line the panel shows instead of knobs, when the selected track's
    /// knobs can't drive anything (`400-parameter-knobs.md` decision 2).
    pub(super) fn knob_hint(&self) -> Option<String> {
        self.knob_panel_hint(self.knobs_track()?)
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

    /// The parameter knob `knob` of the selected track drives, if any.
    fn knob_param(&self, knob: usize) -> Option<ParamId> {
        *self.tracks[self.knobs_track()?].knob_params.get(knob)?
    }

    /// Knob `knob` of the selected track as drawn — see [`KnobShown`].
    /// `None` with no such knob.
    pub(super) fn knob_shown(&self, knob: usize) -> Option<KnobShown> {
        let track_idx = self.knobs_track()?;
        let param = *self.tracks[track_idx].knob_params.get(knob)?;
        let value = if let Some(drag) = self.knob_panel.drag
            && (drag.track_idx, drag.knob) == (track_idx, knob)
            && drag.param.is_some()
        {
            Some(drag.value)
        } else if let Some(turn) = self.knob_panel.wheel_turn
            && turn.holds(track_idx, knob, Instant::now())
        {
            Some(turn.value)
        } else {
            param.and_then(|param| self.knob_param_value(track_idx, param))
        };
        Some(KnobShown { param, value })
    }

    /// The name of knob `knob`'s parameter on the selected track, if it has
    /// one.
    pub(super) fn knob_name(&self, knob: usize) -> Option<&str> {
        self.knob_param_name(self.knobs_track()?, self.knob_param(knob)?)
    }

    /// A press in the knob panel: a dial starts its drag (a click if it
    /// never turns, shown on the release — [`Display::finish_knob_drag`]),
    /// or with ⌘/Ctrl goes back to its parameter's default and starts none —
    /// the track header bars' reset gesture; a label, with a clip open,
    /// selects the knob, its lane under the piano roll (`390-knob-panel.md`
    /// decisions 11 and 14). Only that selection takes the keyboard.
    pub(super) fn handle_knob_panel_click(&mut self, x: f32, y: f32, command: bool) {
        match self.knob_at(x, y) {
            Some(KnobHit::Dial(knob)) if command => self.reset_knob(knob),
            Some(KnobHit::Dial(knob)) => self.begin_knob_drag(knob),
            Some(KnobHit::Label(knob)) => self.select_knob(knob),
            None => {}
        }
    }

    /// Whether a knob's label switches the piano roll's lower lane: only
    /// with a clip open, since there is no lane without one.
    pub(super) fn knob_labels_switch_lanes(&self) -> bool {
        self.lead_clip_time.is_some()
    }

    /// The dial or label under the pointer.
    fn knob_hit_under_pointer(&self) -> Option<KnobHit> {
        let pos = self.canvas_pointer()?;
        self.knob_at(pos.x, pos.y)
    }

    /// The knob whose dial or label is under the pointer — the label lit as
    /// a hover, since a click on either shows its lane.
    pub(super) fn knob_under_pointer(&self) -> Option<usize> {
        match self.knob_hit_under_pointer()? {
            KnobHit::Dial(knob) | KnobHit::Label(knob) => Some(knob),
        }
    }

    /// The knob whose label is under the pointer, when a click there would
    /// switch the lane — not the lane already shown. The pointing hand; a
    /// dial keeps its vertical arrows, a drag being its main use.
    pub(super) fn knob_lane_switch_under_pointer(&self) -> Option<usize> {
        match self.knob_hit_under_pointer()? {
            KnobHit::Label(knob)
                if self.knob_labels_switch_lanes()
                    && self.render.clip_lane != ClipLane::Knob(knob) =>
            {
                Some(knob)
            }
            _ => None,
        }
    }

    /// The footer's line for the knob panel: the arming prompt while a knob
    /// is armed, else the full name of the parameter under the pointer — a
    /// label shortens it — or how to assign an unassigned knob.
    pub(super) fn knob_footer_hint(&self) -> Option<String> {
        if self.armed_knob().is_some() {
            return Some("Turn a control in the plugin".to_owned());
        }
        let knob = self.knob_under_pointer()?;
        Some(match self.knob_name(knob) {
            Some(name) => name.to_owned(),
            None if self.knob_param(knob).is_some() => "Unknown parameter".to_owned(),
            None => "Double-click the label, then turn a control in the plugin".to_owned(),
        })
    }

    /// Turns knob `knob` of the selected track back to its parameter's
    /// default — sent even when it shows that already, so a plugin moved
    /// some other way follows. Nothing for an unassigned knob.
    fn reset_knob(&mut self, knob: usize) {
        let (Some(track_idx), Some(param)) = (self.knobs_track(), self.knob_param(knob)) else {
            return;
        };
        let Some(default) = self.knob_param_default(track_idx, param) else {
            return;
        };
        self.knob_panel.wheel_turn = None;
        self.send_knob_value(track_idx, knob, param, default);
    }

    /// Starts dragging knob `knob` of the selected track, from where it
    /// stands. An unassigned knob's drag turns nothing, but a click on it
    /// still selects it.
    fn begin_knob_drag(&mut self, knob: usize) {
        let (Some(track_idx), Some(shown)) = (self.knobs_track(), self.knob_shown(knob)) else {
            return;
        };
        self.knob_panel.drag = Some(KnobDrag::new(
            track_idx,
            knob,
            shown.param,
            shown.value.unwrap_or(0.0),
        ));
    }

    /// Raw pointer motion `dy` (downward positive) with the button held: the
    /// value for the travel since the anchor, set on the parameter when it
    /// changed ([`KnobDrag::extend`]).
    pub(super) fn extend_knob_drag(&mut self, dy: f32, fine: bool) {
        let Some(drag) = self.knob_panel.drag.as_mut() else {
            return;
        };
        let Some(value) = drag.extend(dy, fine) else {
            return;
        };
        let KnobDrag {
            track_idx,
            knob,
            param,
            ..
        } = *drag;
        if let Some(param) = param {
            self.send_knob_value(track_idx, knob, param, value);
        }
    }

    /// Knob `knob` of `track_idx`, on parameter `param`, now stands at
    /// `value`: the plugin gets it, and the sequencer counts the hand turn
    /// for the unsaved prompt.
    fn send_knob_value(&mut self, track_idx: usize, knob: usize, param: ParamId, value: f64) {
        self.set_knob_param_value(track_idx, param, value);
        self.input_event_tx
            .send(InputEvent::SetKnobValue {
                track_idx,
                knob,
                value,
            })
            .ok();
    }

    /// The button came up: the drag is done, its values already set. A drag
    /// that never left the dead zone was a click: with a clip open it
    /// selects the knob, as a click on its label does (`390-knob-panel.md`
    /// decision 11) — on the release, so a turn never switches the lane.
    pub(super) fn finish_knob_drag(&mut self) {
        let Some(drag) = self.knob_panel.drag.take() else {
            return;
        };
        if drag.is_click() && self.knobs_track() == Some(drag.track_idx) {
            self.select_knob(drag.knob);
        }
    }

    /// One raw scroll event (`InputEvent::WheelScrolled`), the pointer at
    /// `pointer`: turns the dial the [`WheelLatch`] says it turns, if any,
    /// by `delta_y` points (content down positive, so the knob turns up as
    /// the content would move up — fingers up on a trackpad), damped while
    /// it coasts ([`WheelLatch::travel_scale`]). An unassigned knob turns
    /// nothing.
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
                let Some(KnobShown {
                    param: Some(param),
                    value,
                }) = self.knob_shown(knob)
                else {
                    return;
                };
                WheelTurn {
                    track_idx,
                    knob,
                    param,
                    value: value.unwrap_or(0.0),
                    last: now,
                }
            }
        };
        turn.last = now;
        let (value, _) = turned_knob_value(turn.value, -delta_y * scale, fine);
        let changed = value != turn.value;
        turn.value = value;
        self.knob_panel.wheel_turn = Some(turn);
        if changed {
            self.send_knob_value(track_idx, knob, turn.param, value);
        }
    }

    /// Whether the panes' scroll (`InputEvent::TimelineScroll`) is the knob
    /// panel's: over the panel, or while a trackpad gesture or its momentum
    /// turns a dial (it may have left the panel). The panel scrolls nothing
    /// of its own.
    pub(super) fn knob_panel_takes_scroll(&self, pointer: Option<(f32, f32)>) -> bool {
        self.knob_panel.wheel.knob().is_some()
            || pointer.is_some_and(|(x, y)| self.is_over_knob_panel(x, y))
    }

    /// A double-click in the knob panel: on a label, arms its knob for the
    /// next control turned in the plugin's editor (`400-parameter-knobs.md`
    /// decision 3).
    pub(super) fn handle_knob_panel_double_click(&mut self, x: f32, y: f32) {
        if let (Some(KnobHit::Label(knob)), Some(track_idx)) =
            (self.knob_at(x, y), self.knobs_track())
        {
            self.close_output_menu();
            self.knob_panel.armed = Some((track_idx, knob));
        }
    }

    /// The armed knob, while it is still the selected track's and the panel
    /// shows it.
    pub(super) fn armed_knob(&self) -> Option<usize> {
        let (track_idx, knob) = self.knob_panel.armed?;
        (self.knobs_track() == Some(track_idx) && self.knob_rects().is_some()).then_some(knob)
    }

    /// Ends arming, nothing assigned.
    pub(super) fn disarm_knob(&mut self) {
        self.knob_panel.armed = None;
    }

    /// While a knob is armed it gets first look at every event: `Esc`
    /// disarms, `Delete` (or `⌫`) unassigns it, and a press anywhere but on
    /// its own label disarms and goes on to act there. Everything else
    /// passes, the keys to the computer keyboard among them. An arming whose
    /// knob is gone (another track selected, the panel hidden) just ends.
    /// Returns whether the event was consumed.
    pub(super) fn handle_knob_arming_input_event(&mut self, event: &InputEvent) -> bool {
        let Some((track_idx, _)) = self.knob_panel.armed else {
            return false;
        };
        let Some(knob) = self.armed_knob() else {
            self.disarm_knob();
            return false;
        };
        match *event {
            InputEvent::KeyPressed {
                key: Key::Escape, ..
            } => {
                self.disarm_knob();
                true
            }
            InputEvent::KeyPressed {
                key: Key::Delete | Key::Backspace,
                ..
            } => {
                self.disarm_knob();
                self.input_event_tx
                    .send(InputEvent::SetKnobParam {
                        track_idx,
                        knob,
                        param: None,
                    })
                    .ok();
                true
            }
            InputEvent::MouseClicked { x, y, .. }
                if self.knob_at(x, y) != Some(KnobHit::Label(knob)) =>
            {
                self.disarm_knob();
                false
            }
            _ => false,
        }
    }
}

/// The size of a knob's label.
pub(super) const LABEL_FONT_SIZE: f32 = 11.0;

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::time::{Duration, Instant};

    use egui::{Rect, TouchPhase, pos2};

    use super::{
        DIAL_MAX_D, DIAL_MIN_D, DRAG_PX_PER_RANGE, Display, KnobDrag, MOMENTUM_SCALE, SWEEP,
        SWEEP_START, WheelLatch, knob_angle, knob_layout, turned_knob_value,
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
        assert_eq!(knob_angle(0.0), SWEEP_START);
        assert_eq!(knob_angle(1.0), SWEEP_START + SWEEP);
        // Halfway is straight up (-π/2, or 3π/2 a turn on).
        assert!((knob_angle(0.5) - 1.5 * PI).abs() < 1e-5);
        assert_eq!(knob_angle(2.0), knob_angle(1.0));
        assert_eq!(knob_angle(-1.0), knob_angle(0.0));
    }

    #[test]
    fn a_press_that_wanders_inside_the_dead_zone_is_a_click_and_turns_nothing() {
        let mut drag = KnobDrag::new(0, 3, Some(714), 0.5);
        let jitter = Display::DRAG_THRESHOLD_PX * 0.6;
        // Up and back down: the travel is net, so it never leaves the zone.
        assert_eq!(drag.extend(-jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert!(drag.is_click());
        assert_eq!(drag.value, 0.5);
    }

    #[test]
    fn a_drag_out_of_the_dead_zone_turns_from_its_edge_without_a_jump() {
        let mut drag = KnobDrag::new(0, 0, Some(714), 0.5);
        // Leaving the zone turns nothing yet, and the press is no click.
        assert_eq!(drag.extend(-Display::DRAG_THRESHOLD_PX, false), None);
        assert!(!drag.is_click());
        // A tenth of the range's travel from the edge is a tenth.
        let tenth = drag.extend(-DRAG_PX_PER_RANGE / 10.0, false).unwrap();
        assert!((tenth - 0.6).abs() < 1e-6, "{tenth}");
    }

    #[test]
    fn a_turn_up_turns_the_knob_up_and_stops_at_either_end() {
        assert_eq!(turned_knob_value(0.5, 0.0, false), (0.5, false));
        let (up, clamped) = turned_knob_value(0.5, DRAG_PX_PER_RANGE / 4.0, false);
        assert!((up - 0.75).abs() < 1e-6 && !clamped);
        let (down, _) = turned_knob_value(0.5, -DRAG_PX_PER_RANGE / 4.0, false);
        assert!((down - 0.25).abs() < 1e-6);
        // Fine is five times slower.
        let (fine, _) = turned_knob_value(0.5, DRAG_PX_PER_RANGE / 4.0, true);
        assert!((fine - 0.55).abs() < 1e-6);
        assert_eq!(
            turned_knob_value(0.9, DRAG_PX_PER_RANGE, false),
            (1.0, true)
        );
        assert_eq!(
            turned_knob_value(0.1, -DRAG_PX_PER_RANGE, false),
            (0.0, true)
        );
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
