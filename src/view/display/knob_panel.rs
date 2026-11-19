//! The knob panel (`400-parameter-knobs.md`, the panel itself from
//! `archive/390-knob-panel.md`): eight virtual knobs in a column of their own left
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
//! A right-click on a knob (dial or label) arms it and selects it: the next
//! control turned in the plugin's editor becomes its parameter
//! (`knob_params.rs`), an undoable `SetKnobParamEdit`; `Esc` or a press
//! elsewhere disarms. An unassigned knob turns nothing. A press on a knob,
//! its dial or its label, selects it on the press, a drag after it
//! included (a ⌘-click reset and a scroll select nothing): the panel takes
//! the keyboard, `Delete`
//! removing the knob's mapping, and with a clip open its lane shows under
//! the piano roll (`ClipLane::Knob`; `Display::select_knob`,
//! `input/knob_focus.rs`). See `400-parameter-knobs.md` decisions 3 and 14
//! for the whole grammar. A track whose knobs
//! can't drive anything (no plugin, or one with no parameters) shows a one-line
//! hint instead of knobs. A scroll anywhere else on the panel does nothing.
//!
//! The geometry is pure and unit-tested here; the drawing is
//! `rendering/knob_panel.rs`. See `030-ui-design.md` § Knob Panel.

use std::f32::consts::{FRAC_1_SQRT_2, PI};
use std::time::{Duration, Instant};

use egui::{Key, Pos2, Rect, TouchPhase, pos2, vec2};

use crate::core::input_event::InputEvent;
use crate::core::time::monotonic_nanos;
use crate::models::knobs::{KNOB_COUNT, ParamId, PluginParam, snap_to_steps};

use super::Display;
use super::clip_lane::ClipLane;
use super::status_message::StatusMessage;

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

/// How far a selected knob's corner brackets stand off its dial, room
/// allowing.
const FRAME_GAP: f32 = 4.0;
/// The least clear space kept between the brackets' bottom and the label,
/// in a cell too tight for the whole [`FRAME_GAP`].
const FRAME_LABEL_CLEAR: f32 = 2.0;
/// The width of a dial's trough and value arc.
pub(super) const ARC_W: f32 = 3.0;

/// Where one knob is drawn and hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KnobRects {
    /// The dial's centre.
    pub(super) center: Pos2,
    /// The dial's radius.
    pub(super) radius: f32,
    /// The label under it: the parameter's name.
    pub(super) label: Rect,
    /// Where the selected knob's corner brackets go: [`FRAME_GAP`] out
    /// from what the dial draws ([`dial_ink`]), so it sits centred in
    /// them, but at least [`FRAME_LABEL_CLEAR`] short of the label under
    /// it, and never past its cell's top or across into the next column.
    pub(super) frame: Rect,
}

impl KnobRects {
    /// The square the dial is drawn in.
    pub(super) fn dial(&self) -> Rect {
        Rect::from_center_size(self.center, vec2(self.radius, self.radius) * 2.0)
    }

    /// The whole knob as one solid target: the label's width, from the top
    /// of the selection brackets down to the label's bottom, with no gap
    /// between dial and label. Above the label it is the dial's part.
    fn hit(&self) -> Rect {
        Rect::from_min_max(
            pos2(self.label.min.x, self.frame.min.y.min(self.dial().min.y)),
            self.label.max,
        )
    }
}

/// What a press at `pos` lands on among `knobs`: a knob's [`hit`] area,
/// its label's part from the label's top down, its dial's above.
///
/// [`hit`]: KnobRects::hit
fn knob_hit_at(knobs: &[KnobRects], pos: Pos2) -> Option<KnobHit> {
    knobs.iter().enumerate().find_map(|(i, knob)| {
        if !knob.hit().contains(pos) {
            None
        } else if pos.y < knob.label.min.y {
            Some(KnobHit::Dial(i))
        } else {
            Some(KnobHit::Label(i))
        }
    })
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
        let cell_top = inner.min.y + cell_h * row as f32;
        let half_w = (dial_d * 0.5 + FRAME_GAP).min(cell_w * 0.5);
        let ink = dial_ink(center, dial_d * 0.5);
        KnobRects {
            center,
            radius: dial_d * 0.5,
            label: Rect::from_min_size(
                pos2(center_x - cell_w * 0.5, label_top),
                vec2(cell_w, LABEL_H),
            ),
            frame: Rect::from_min_max(
                pos2(
                    center_x - half_w,
                    (ink.min.y - FRAME_GAP).max(cell_top).min(ink.min.y),
                ),
                pos2(
                    center_x + half_w,
                    (ink.max.y + FRAME_GAP).min(label_top - FRAME_LABEL_CLEAR),
                ),
            ),
        }
    })
}

/// What a dial of `radius` about `center` draws: its full square across and
/// at the top, but only down to the ends of its sweep at the bottom, where
/// the three-quarter sweep is open (the arc's ends at 45° below the
/// centre, plus half the stroke).
pub(super) fn dial_ink(center: Pos2, radius: f32) -> Rect {
    Rect::from_min_max(
        pos2(center.x - radius, center.y - radius),
        pos2(
            center.x + radius,
            center.y + radius * FRAC_1_SQRT_2 + ARC_W * 0.5,
        ),
    )
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
/// tempo chip's drag's shape — `fields/tempo_field.rs`).
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
    /// Where it has reached, shown in the dial while the drag runs (the hand
    /// wins).
    position: KnobPosition,
    /// Points travelled upward while still inside the drag threshold
    /// ([`Display::DRAG_THRESHOLD_PX`], every press's click-or-drag line);
    /// `None` once the drag has left it and turns the knob.
    slop: Option<f32>,
    /// Whether it has turned the knob, and so holds it against playback:
    /// a press that stays a click holds nothing.
    holding: bool,
}

impl KnobDrag {
    /// A drag of a knob standing at `value`, not yet out of the dead zone.
    fn new(track_idx: usize, knob: usize, param: Option<ParamId>, position: KnobPosition) -> Self {
        Self {
            track_idx,
            knob,
            param,
            anchor: position.raw,
            travel: 0.0,
            fine: false,
            position,
            slop: Some(0.0),
            holding: false,
        }
    }

    /// Raw pointer motion `dy` (downward positive): the new value, when it
    /// changed. Nothing turns until the travel leaves the drag threshold
    /// ([`Display::DRAG_THRESHOLD_PX`]: within it the press is a click, which
    /// selects the knob, and a hand's jitter never sends a step), and the
    /// turn then counts from there, so the knob never jumps. ⇧ going down or
    /// up re-anchors at the travel reached, and so does hitting either end.
    /// A stepped parameter changes only on reaching its next position.
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
            self.anchor = self.position.raw;
        }
        self.travel -= dy;
        let (raw, clamped) = turned_knob_value(self.anchor, self.travel, fine);
        if clamped {
            self.travel = 0.0;
            self.anchor = raw;
        }
        self.position.reach(raw)
    }
}

/// Where a hand turning a knob has reached.
#[derive(Clone, Copy, Debug)]
struct KnobPosition {
    /// The steps its parameter moves in, 0 when it moves smoothly.
    steps: u32,
    /// Where the turn has reached, between a stepped parameter's positions
    /// too, so a slow turn still gets to the next one.
    raw: f64,
    /// The value that is: `raw`, at a stepped parameter's position.
    value: f64,
}

impl KnobPosition {
    /// A knob standing at `value`, on a parameter of `steps` steps.
    fn new(steps: u32, value: f64) -> Self {
        Self {
            steps,
            raw: value,
            value,
        }
    }

    /// The turn reached `raw`: the new value, when that changed it.
    fn reach(&mut self, raw: f64) -> Option<f64> {
        self.raw = raw;
        let value = snap_to_steps(raw, self.steps);
        (value != self.value).then(|| {
            self.value = value;
            value
        })
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
    /// Where it has reached.
    position: KnobPosition,
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
    /// The knob a right-click armed — its track and which —
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
        if self
            .knobs_track()
            .is_some_and(|track_idx| !self.knobs_drive_plugin(track_idx))
        {
            return None;
        }
        self.knob_panel_rect().map(knob_layout)
    }

    /// The line the panel shows instead of knobs, when the selected track's
    /// knobs can't drive anything (`400-parameter-knobs.md` decision 2).
    pub(super) fn knob_hint(&self) -> Option<&'static str> {
        self.knob_panel_hint(self.knobs_track()?)
    }

    /// The dial or label under `(x, y)` ([`knob_hit_at`]).
    pub(super) fn knob_at(&self, x: f32, y: f32) -> Option<KnobHit> {
        knob_hit_at(&self.knob_rects()?, pos2(x, y))
    }

    /// The dial under `(x, y)`, if any.
    fn dial_at(&self, x: f32, y: f32) -> Option<usize> {
        match self.knob_at(x, y)? {
            KnobHit::Dial(knob) => Some(knob),
            KnobHit::Label(_) => None,
        }
    }

    /// Whether the pointer is on a dial that turns — the vertical arrows,
    /// as on the track header's bars. An unassigned knob's dial keeps the
    /// plain pointer: it turns nothing, and the arrows would promise it.
    pub(super) fn is_pointer_on_knob_dial(&self) -> bool {
        self.canvas_pointer()
            .and_then(|pos| self.dial_at(pos.x, pos.y))
            .is_some_and(|knob| self.knob_param(knob).is_some())
    }

    /// The track the knobs act on: the selected one, if there is one.
    pub(super) fn knobs_track(&self) -> Option<usize> {
        self.selected_track_idx
            .filter(|&track_idx| track_idx < self.tracks.len())
    }

    /// The parameter knob `knob` of the selected track drives, if any.
    pub(super) fn knob_param(&self, knob: usize) -> Option<ParamId> {
        self.knob_controller(knob).map(|param| param.id)
    }

    /// That parameter as clips know it, with its plugin.
    pub(super) fn knob_controller(&self, knob: usize) -> Option<PluginParam> {
        *self.tracks[self.knobs_track()?].knob_params.get(knob)?
    }

    /// Knob `knob` of the selected track as drawn — see [`KnobShown`].
    /// `None` with no such knob.
    pub(super) fn knob_shown(&self, knob: usize) -> Option<KnobShown> {
        let track_idx = self.knobs_track()?;
        if knob >= KNOB_COUNT {
            return None;
        }
        let param = self.knob_param(knob);
        let value = if let Some(drag) = self.knob_panel.drag
            && (drag.track_idx, drag.knob) == (track_idx, knob)
            && drag.param.is_some()
        {
            Some(drag.position.value)
        } else if let Some(turn) = self.knob_panel.wheel_turn
            && turn.holds(track_idx, knob, Instant::now())
        {
            Some(turn.position.value)
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

    /// A press in the knob panel selects the knob there on the press
    /// (`400-parameter-knobs.md` decision 14), the panel taking the
    /// keyboard, and on the dial starts its drag too; with ⌘/Ctrl a dial
    /// instead goes back to its parameter's default, selecting nothing and
    /// starting no drag — the track header bars' reset gesture.
    pub(super) fn handle_knob_panel_click(&mut self, x: f32, y: f32, command: bool) {
        match self.knob_at(x, y) {
            Some(KnobHit::Dial(knob)) if command => self.reset_knob(knob),
            Some(KnobHit::Dial(knob)) => {
                self.select_knob(knob);
                self.begin_knob_drag(knob);
            }
            Some(KnobHit::Label(knob)) => self.select_knob(knob),
            None => {}
        }
    }

    /// Whether a click on a knob also switches the piano roll's lower lane:
    /// only with a clip open, since there is no lane without one.
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
                    && self.knob_param(knob).is_some()
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
            None => "Right-click, then turn a control in the plugin".to_owned(),
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
        self.end_wheel_turn();
        let at_ns = monotonic_nanos();
        let start = self.knob_param_value(track_idx, param);
        self.hold_knob(track_idx, knob, start, at_ns);
        self.send_knob_value(track_idx, knob, param, default);
        self.release_knob(track_idx, knob);
    }

    /// Starts dragging knob `knob` of the selected track, from where it
    /// stands. An unassigned knob's drag turns nothing, but a click on it
    /// still selects it.
    fn begin_knob_drag(&mut self, knob: usize) {
        let (Some(track_idx), Some(shown)) = (self.knobs_track(), self.knob_shown(knob)) else {
            return;
        };
        let steps = shown
            .param
            .map_or(0, |param| self.knob_param_steps(track_idx, param));
        self.knob_panel.drag = Some(KnobDrag::new(
            track_idx,
            knob,
            shown.param,
            KnobPosition::new(steps, shown.value.unwrap_or(0.0)),
        ));
    }

    /// Raw pointer motion `dy` (downward positive) with the button held: the
    /// value for the travel since the anchor, set on the parameter when it
    /// changed ([`KnobDrag::extend`]).
    pub(super) fn extend_knob_drag(&mut self, dy: f32, fine: bool) {
        let Some(drag) = self.knob_panel.drag.as_mut() else {
            return;
        };
        // Where the turn starts: the knob, before this first step moves it.
        let start = drag.position.value;
        let changed = drag.extend(dy, fine);
        let begins = changed.is_some() && !std::mem::replace(&mut drag.holding, true);
        let KnobDrag {
            track_idx,
            knob,
            param,
            slop,
            ..
        } = *drag;
        if begins && param.is_some() {
            self.hold_knob(track_idx, knob, Some(start), monotonic_nanos());
        }
        match (param, changed) {
            (Some(param), Some(value)) => self.send_knob_value(track_idx, knob, param, value),
            // Out of the dead zone on an unassigned knob: a turn that turns
            // nothing, whichever way.
            (None, _) if slop.is_none() => self.show_unassigned_knob_turned(knob),
            _ => {}
        }
    }

    /// A turn of unassigned knob `knob`, which turns nothing: the footer
    /// says why and how to assign it, at the moment the knob didn't move.
    fn show_unassigned_knob_turned(&mut self, knob: usize) {
        self.render.status = Some(StatusMessage::new(unassigned_turn_message(knob)));
    }

    /// Knob `knob` of `track_idx`, on parameter `param`, now stands at
    /// `value`: the plugin gets it, and the sequencer counts the hand turn.
    fn send_knob_value(&mut self, track_idx: usize, knob: usize, param: ParamId, value: f64) {
        self.set_knob_param_value(track_idx, param, value);
        self.count_knob_turn(track_idx, knob, value, monotonic_nanos());
    }

    /// Tells the sequencer the hand took hold of knob `knob` of `track_idx`
    /// at `at_ns`, where it stood at `start` — from the panel or the plugin's
    /// own editor: playback lets go of it until
    /// [`release_knob`](Self::release_knob) (`400-parameter-knobs.md`
    /// decision 6).
    pub(super) fn hold_knob(&self, track_idx: usize, knob: usize, start: Option<f64>, at_ns: u64) {
        self.input_event_tx
            .send(InputEvent::HoldKnob {
                track_idx,
                knob,
                start,
                tick: self.input_clock.tick_at(at_ns),
            })
            .ok();
    }

    /// Tells the sequencer the hand let go of knob `knob` of `track_idx`.
    /// The plugin's state then goes to the crash recovery copy: a sound
    /// shaped with the knobs alone, the editor never opened, would otherwise
    /// never reach it (`410-crash-recovery.md` decision 6).
    pub(super) fn release_knob(&mut self, track_idx: usize, knob: usize) {
        self.input_event_tx
            .send(InputEvent::ReleaseKnob { track_idx, knob })
            .ok();
        #[cfg(target_os = "macos")]
        if let Some(slot) = self.tracks.get(track_idx).map(|lane| lane.slot) {
            self.capture_soon(slot);
        }
    }

    /// Tells the sequencer the hand turned knob `knob` of `track_idx` to
    /// `value` at `at_ns` — from the panel or the plugin's own editor — for
    /// the unsaved prompt and a take (`400-parameter-knobs.md` decisions 9
    /// and 10).
    pub(super) fn count_knob_turn(&self, track_idx: usize, knob: usize, value: f64, at_ns: u64) {
        self.input_event_tx
            .send(InputEvent::SetKnobValue {
                track_idx,
                knob,
                value,
                tick: self.input_clock.tick_at(at_ns),
            })
            .ok();
    }

    /// The button came up: the drag is done, its values already set, the
    /// knob selected on the press, and a drag that turned it lets go.
    pub(super) fn finish_knob_drag(&mut self) {
        if let Some(drag) = self.knob_panel.drag.take()
            && drag.holding
        {
            self.release_knob(drag.track_idx, drag.knob);
        }
    }

    /// Ends the scroll turn, if one runs: playback has its parameter back.
    fn end_wheel_turn(&mut self) {
        if let Some(turn) = self.knob_panel.wheel_turn.take() {
            self.release_knob(turn.track_idx, turn.knob);
        }
    }

    /// Per frame: a scroll has no release, so its turn ends once the
    /// scrolling has stopped for a moment ([`WHEEL_TURN_HOLD`]). With the
    /// transport stopped the next frame may be a while off, but then no
    /// playback is waiting to have the parameter back.
    pub(super) fn end_finished_wheel_turn(&mut self) {
        let now = Instant::now();
        if let Some(turn) = self.knob_panel.wheel_turn
            && !turn.holds(turn.track_idx, turn.knob, now)
        {
            self.end_wheel_turn();
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
                self.end_wheel_turn();
                let Some(KnobShown { param, value }) = self.knob_shown(knob) else {
                    return;
                };
                let Some(param) = param else {
                    self.show_unassigned_knob_turned(knob);
                    return;
                };
                self.hold_knob(track_idx, knob, value, monotonic_nanos());
                let steps = self.knob_param_steps(track_idx, param);
                WheelTurn {
                    track_idx,
                    knob,
                    param,
                    position: KnobPosition::new(steps, value.unwrap_or(0.0)),
                    last: now,
                }
            }
        };
        turn.last = now;
        let (raw, _) = turned_knob_value(turn.position.raw, -delta_y * scale, fine);
        let changed = turn.position.reach(raw);
        self.knob_panel.wheel_turn = Some(turn);
        if let Some(value) = changed {
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

    /// A right-click in the knob panel: on a dial or a label, arms its knob
    /// for the next control turned in the plugin's editor and selects it
    /// (`400-parameter-knobs.md` decision 3), so a `Delete` then removes the
    /// mapping instead. An assigned knob re-arms, to be reassigned. Not
    /// during a drag.
    pub(super) fn handle_knob_panel_right_click(&mut self, x: f32, y: f32) {
        if self.knob_panel.dragging() {
            return;
        }
        let (Some(KnobHit::Dial(knob) | KnobHit::Label(knob)), Some(track_idx)) =
            (self.knob_at(x, y), self.knobs_track())
        else {
            return;
        };
        self.close_output_menu();
        self.knob_panel.armed = Some((track_idx, knob));
        self.select_knob(knob);
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
    /// disarms, and a press anywhere — a right-click on another knob
    /// re-arms there — disarms and goes on to act there. Everything else
    /// passes, the keys to the computer keyboard among them, and `Delete`
    /// to the knob panel, which has the keyboard and removes the mapping
    /// (`input/knob_focus.rs`). An arming whose knob is gone (another track
    /// selected, the panel hidden) just ends. Returns whether the event was
    /// consumed.
    pub(super) fn handle_knob_arming_input_event(&mut self, event: &InputEvent) -> bool {
        if self.knob_panel.armed.is_none() {
            return false;
        }
        if self.armed_knob().is_none() {
            self.disarm_knob();
            return false;
        }
        match *event {
            InputEvent::KeyPressed {
                key: Key::Escape, ..
            } => {
                self.disarm_knob();
                true
            }
            InputEvent::MouseClicked { .. } | InputEvent::MouseRightClicked { .. } => {
                self.disarm_knob();
                false
            }
            _ => false,
        }
    }
}

/// The footer's line for a turn of unassigned knob `knob` (counted from 1,
/// as its placeholder label is).
fn unassigned_turn_message(knob: usize) -> String {
    format!(
        "Knob {} has no parameter: right-click it, then turn a control in the plugin",
        knob + 1
    )
}

/// The size of a knob's label.
pub(super) const LABEL_FONT_SIZE: f32 = 11.0;

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::time::{Duration, Instant};

    use egui::{Rect, TouchPhase, pos2};

    use super::{
        DIAL_MAX_D, DIAL_MIN_D, DRAG_PX_PER_RANGE, Display, FRAME_GAP, FRAME_LABEL_CLEAR, KnobDrag,
        KnobHit, KnobPosition, MOMENTUM_SCALE, ROWS, SWEEP, SWEEP_START, WheelLatch, dial_ink,
        knob_angle, knob_hit_at, knob_layout, turned_knob_value, unassigned_turn_message,
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
        // The selection brackets stand off the dial with room to spare, and
        // never reach into the label or the cell above.
        // With room, the dial's ink sits centred in them, the same gap on
        // every side.
        let ink = dial_ink(roomy[0].center, roomy[0].radius);
        let frame = roomy[0].frame;
        for gap in [
            ink.min.x - frame.min.x,
            frame.max.x - ink.max.x,
            ink.min.y - frame.min.y,
            frame.max.y - ink.max.y,
        ] {
            assert!((gap - FRAME_GAP).abs() < 1e-4, "{gap}");
        }
        assert!(frame.max.y <= roomy[0].label.min.y - FRAME_LABEL_CLEAR);
        for pair in knobs.windows(2).take(ROWS - 1) {
            assert!(pair[1].frame.min.y >= pair[0].label.max.y);
            let (frame, dial) = (pair[1].frame, pair[1].dial());
            assert!(frame.min.y <= dial.min.y && frame.x_range().contains(dial.center().x));
            assert!(frame.min.y >= pair[0].label.max.y);
            assert!(frame.max.y <= pair[1].label.min.y - FRAME_LABEL_CLEAR);
        }
        // And a tiny one never goes below the smallest dial.
        assert_eq!(knob_layout(panel(40.0))[0].radius * 2.0, DIAL_MIN_D);
    }

    /// A knob is one solid target: the gap between dial and label, the
    /// space beside the dial and the brackets' standoff above it all hit
    /// it, the dial's part down to the label's top; nothing between two
    /// knobs' areas belongs to a third.
    #[test]
    fn a_knob_is_one_solid_target() {
        let knobs = knob_layout(panel(800.0));
        let knob = knobs[0];
        let at = |x: f32, y: f32| knob_hit_at(&knobs, pos2(x, y));
        let gap_y = (knob.dial().max.y + knob.label.min.y) * 0.5;
        assert_eq!(at(knob.center.x, gap_y), Some(KnobHit::Dial(0)));
        assert_eq!(
            at(knob.label.min.x + 1.0, knob.center.y),
            Some(KnobHit::Dial(0))
        );
        assert_eq!(
            at(knob.center.x, knob.frame.min.y + 0.5),
            Some(KnobHit::Dial(0))
        );
        assert_eq!(at(knob.center.x, knob.label.min.y), Some(KnobHit::Label(0)));
        assert_eq!(at(knob.center.x, knob.frame.min.y - 0.5), None);
        // The second column's first knob, beside it.
        let right = knobs[ROWS];
        assert_eq!(
            at(right.label.min.x + 1.0, right.center.y),
            Some(KnobHit::Dial(ROWS))
        );
    }

    /// A turn of an unassigned knob names it, counted from 1, and says how
    /// to assign it.
    #[test]
    fn an_unassigned_turn_says_how_to_assign() {
        assert_eq!(
            unassigned_turn_message(2),
            "Knob 3 has no parameter: right-click it, then turn a control in the plugin"
        );
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
        let mut drag = KnobDrag::new(0, 3, Some(714), KnobPosition::new(0, 0.5));
        let jitter = Display::DRAG_THRESHOLD_PX * 0.6;
        // Up and back down: the travel is net, so it never leaves the zone.
        assert_eq!(drag.extend(-jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert_eq!(drag.extend(jitter, false), None);
        assert!(drag.slop.is_some());
        assert_eq!(drag.position.value, 0.5);
    }

    #[test]
    fn a_drag_out_of_the_dead_zone_turns_from_its_edge_without_a_jump() {
        let mut drag = KnobDrag::new(0, 0, Some(714), KnobPosition::new(0, 0.5));
        // Leaving the zone turns nothing yet, and the press is no click.
        assert_eq!(drag.extend(-Display::DRAG_THRESHOLD_PX, false), None);
        assert!(drag.slop.is_none());
        // A tenth of the range's travel from the edge is a tenth.
        let tenth = drag.extend(-DRAG_PX_PER_RANGE / 10.0, false).unwrap();
        assert!((tenth - 0.6).abs() < 1e-6, "{tenth}");
    }

    /// Only the crossing sends anything, so a take holds one clean step, and
    /// the travel between positions isn't lost.
    #[test]
    fn a_drag_on_a_switch_changes_it_once_at_the_middle() {
        let mut drag = KnobDrag::new(0, 0, Some(714), KnobPosition::new(1, 0.0));
        drag.extend(-Display::DRAG_THRESHOLD_PX, false);
        let quarter = -DRAG_PX_PER_RANGE / 4.0;
        assert_eq!(drag.extend(quarter, false), None);
        assert_eq!(drag.position.value, 0.0);
        assert_eq!(drag.extend(quarter, false), Some(1.0));
        assert_eq!(drag.extend(quarter, false), None);
        assert_eq!(drag.extend(-quarter, false), None);
        assert_eq!(drag.extend(-quarter, false), Some(0.0));
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
