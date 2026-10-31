//! The piano roll's lower lane when it shows a wheel, and the gutter switch
//! that picks the lane (`ClipLane`): the `VEL` / `PB` / `MW` labels left of
//! the lane, and the lead clip's pitch bend or mod wheel drawn as a line,
//! a ride smooth, a held value flat until the next move steps off it
//! ([`wheel_path`]), under the span `Delete` would return to rest. The moves
//! are folded per pixel column ([`fold_wheel_columns`]) so what is drawn
//! follows the lane's width, not the event count. See `380-wheel-lane.md`.

use egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::piano_keys::NoteAreaGeom;
use super::*;
use crate::core::midi::message::{Controller, PITCH_BEND_CENTRE};
use crate::core::time::PPQN;
use crate::metadata::clip_view::WheelMove;

impl Display {
    /// Space above and below a wheel's curve inside the lane, so its
    /// extremes clear the divider and the lane's bottom edge.
    const WHEEL_LANE_PAD: f32 = 4.0;
    /// Thickness of a wheel's line.
    const WHEEL_LINE_W: f32 = 1.5;
    /// How far a point of a wheel's line may lie off the straight line
    /// through its neighbours and still be dropped (`simplify`): half a
    /// pixel, under what the eye sees as a bend.
    const WHEEL_SIMPLIFY_PX: f32 = 0.5;
    /// Space left of a gutter label.
    const LANE_LABEL_INSET_X: f32 = 12.0;
    /// Radius of the data marker beside `PB` / `MW`.
    const LANE_MARKER_R: f32 = 2.0;
    /// Gap between a gutter label and its data marker's centre.
    const LANE_MARKER_GAP: f32 = 6.0;

    /// The gutter left of the lower lane, under the octave legend and the
    /// keys, holding the lane switch.
    fn lane_gutter_rect(&self, geom: &NoteAreaGeom) -> Rect {
        let lane = self.lower_lane_rect(geom);
        Rect::from_x_y_ranges(
            lane.left() - 2.0 * Self::PIANO_MARGIN_W..=lane.left(),
            lane.y_range(),
        )
    }

    /// The gutter label under `(x, y)`: its whole row, the gutter's width.
    /// Clip pane only, and only with a lead clip shown.
    pub(in crate::view::display) fn clip_lane_label_at(&self, x: f32, y: f32) -> Option<ClipLane> {
        if !self.is_pane_visible(Pane::Clip) || self.lead_clip_time.is_none() {
            return None;
        }
        lane_label_in(self.lane_gutter_rect(&self.note_area_geom()), pos2(x, y))
    }

    /// The gutter label under the pointer. Clip pane only.
    fn clip_lane_label_under_pointer(&self) -> Option<ClipLane> {
        self.canvas_pointer()
            .and_then(|p| self.clip_lane_label_at(p.x, p.y))
    }

    /// Whether `(x, y)` is in the lower lane. Clip pane only.
    pub(in crate::view::display) fn is_in_lower_lane(&self, x: f32, y: f32) -> bool {
        self.lower_lane_rect(&self.note_area_geom())
            .contains(pos2(x, y))
    }

    /// Whether the pointer is on a gutter label a click would switch to —
    /// not the lane already shown.
    pub(in crate::view::display) fn is_pointer_on_clip_lane_switch(&mut self) -> bool {
        let shown = self.render.clip_lane;
        self.in_pane(Pane::Clip, |display| {
            display.clip_lane_label_under_pointer()
        })
        .is_some_and(|lane| lane != shown)
    }

    /// Draws the lane switch: `VEL`, `PB`, `MW` stacked in the gutter,
    /// coloured as the header's toggles are (`toggle_icon_color`): the shown
    /// one `accent`, one under the pointer `fg`, the rest `fg_dim` — so a
    /// hover is never mistaken for the shown lane. A dot, in the label's
    /// colour, beside a wheel's label when the lead clip has moves for it.
    pub(super) fn draw_lane_gutter(&self, painter: &Painter, geom: &NoteAreaGeom) {
        let gutter = self.lane_gutter_rect(geom);
        let hovered = self.canvas_pointer().and_then(|p| lane_label_in(gutter, p));
        let font = FontId::proportional(theme::FONT_SIZE_TL);
        for (i, lane) in ClipLane::ALL.into_iter().enumerate() {
            let row = lane_label_row(gutter, i);
            let color = toggle_icon_color(lane == self.render.clip_lane, hovered == Some(lane));
            let label = painter.text(
                pos2(row.left() + Self::LANE_LABEL_INSET_X, row.center().y),
                Align2::LEFT_CENTER,
                lane.label(),
                font.clone(),
                color,
            );
            if lane
                .wheel()
                .is_some_and(|wheel| !self.render.wheel_moves.of(wheel).is_empty())
            {
                painter.circle_filled(
                    pos2(label.right() + Self::LANE_MARKER_GAP, row.center().y),
                    Self::LANE_MARKER_R,
                    color,
                );
            }
        }
    }

    /// Draws `wheel`'s curve for the lead clip into the lower lane `lane`
    /// ([`wheel_path`]): a ride joined smoothly, a held value flat until the
    /// next move steps off it, from rest before the first move, dimmed
    /// outside the clip's window. Pitch bend gets a faint centre line at
    /// rest; the span, if one is marked, a tint across the lane's height
    /// under the line, filled as the note marquee is. Nothing else with no
    /// moves.
    pub(super) fn draw_wheel_lane(&self, painter: &Painter, lane: Rect, wheel: Controller) {
        if let Some((start, end)) = self.gesture.wheel_span {
            let x_range = self.tick_to_screen_x(start).max(lane.left())
                ..=self.tick_to_screen_x(end).min(lane.right());
            if x_range.start() < x_range.end() {
                painter.rect_filled(
                    Rect::from_x_y_ranges(x_range, lane.y_range()),
                    CornerRadius::ZERO,
                    theme::accent().gamma_multiply(Self::MARQUEE_FILL_ALPHA),
                );
            }
        }

        let inner_top = lane.top() + Self::WHEEL_LANE_PAD;
        let inner_h = lane.height() - 2.0 * Self::WHEEL_LANE_PAD;
        let y_of = |value| inner_top + (1.0 - wheel_fraction(wheel, value)) * inner_h;

        if wheel == Controller::PitchBend {
            painter.hline(
                lane.x_range(),
                y_of(wheel.rest().unwrap_or(0)).round(),
                Stroke::new(1.0_f32, theme::fg_dim().gamma_multiply(0.3)),
            );
        }

        let moves = self.render.wheel_moves.of(wheel);
        if moves.is_empty() {
            return;
        }

        let ticks_per_col = self.ticks_per_pixel();
        let first_tick = (lane.left() - self.tick_to_x(0)) * ticks_per_col;
        let cols = lane.width().max(0.0);
        let (start, knots) = fold_wheel_columns(
            moves,
            wheel.rest().unwrap_or(0),
            first_tick,
            ticks_per_col,
            cols as usize,
        );
        let path = wheel_path(start, &knots, cols);
        let to_screen = |&(x, value): &PathPoint| pos2(lane.left() + x, y_of(value));
        let line: Vec<Pos2> = path.line.iter().map(to_screen).collect();
        let line = simplify(&line, Self::WHEEL_SIMPLIFY_PX);

        let (window_start, window_end) = self.clip_window_for_render();
        let window_x = [self.tick_to_x(window_start), self.tick_to_x(window_end)];
        let color = self.lead_clip_color();
        let stroke = |x: f32| {
            let inside = (window_x[0]..window_x[1]).contains(&x);
            let color = if inside {
                color
            } else {
                color.gamma_multiply(Self::SILENT_NOTE_SCALE)
            };
            Stroke::new(Self::WHEEL_LINE_W, color)
        };
        let p = painter.with_clip_rect(lane);

        // One path per side of the window's edges, so egui joins its corners
        // and feathers it once — segments drawn apart overlap their soft
        // ends at every join, and a ride reads frizzy.
        for run in split_at_x(&line, window_x) {
            let mid = (run[0].x + run[1].x) * 0.5;
            p.add(Shape::line(run, stroke(mid)));
        }
        for Segment { from, to } in path.spans {
            p.line_segment(
                [to_screen(&from), to_screen(&to)],
                stroke(lane.left() + from.0),
            );
        }
    }
}

/// Row `i` of the gutter's three label rows, top to bottom.
fn lane_label_row(gutter: Rect, i: usize) -> Rect {
    let row_h = gutter.height() / ClipLane::ALL.len() as f32;
    Rect::from_min_size(
        pos2(gutter.left(), gutter.top() + i as f32 * row_h),
        vec2(gutter.width(), row_h),
    )
}

/// The lane whose label row of `gutter` holds `pos`.
fn lane_label_in(gutter: Rect, pos: Pos2) -> Option<ClipLane> {
    if !gutter.contains(pos) {
        return None;
    }
    let row_h = gutter.height() / ClipLane::ALL.len() as f32;
    ClipLane::ALL
        .get(((pos.y - gutter.top()) / row_h) as usize)
        .copied()
}

/// Where `value` sits in the lane, `0.0` at the bottom to `1.0` at the top:
/// pitch bend centred, its rest (`0x2000`) exactly halfway; the mod wheel,
/// as any CC, from 0 to 127.
fn wheel_fraction(wheel: Controller, value: u16) -> f32 {
    match wheel {
        Controller::PitchBend => {
            let centre = f32::from(PITCH_BEND_CENTRE);
            0.5 + (f32::from(value) - centre) / (2.0 * centre)
        }
        Controller::Cc(_) => f32::from(value.min(127)) / 127.0,
    }
}

/// The longest gap between two moves that the lane joins as one ride: a
/// 32nd note. A wheel being moved sends every few milliseconds, so moves
/// closer than this are drawn as a smooth line between them; a longer gap is
/// a value held, drawn flat until the next move steps off it — as the synth
/// hears it, since Stev never sends a value in between.
const RIDE_GAP_TICKS: i32 = PPQN / 8;

/// A point the wheel's line passes through, in column units across the lane
/// (`0.0` its left edge) and wheel value.
type PathPoint = (f32, u16);

/// The moves inside one pixel column (or, past the lane's right edge, the
/// first move after it), folded: where they sit, the values they reach, and
/// whether the line rides into them.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Knot {
    /// The column's centre, or the move's own x past the right edge.
    x: f32,
    /// The lowest value moved to.
    lo: u16,
    /// The highest value moved to.
    hi: u16,
    /// The last value moved to — held after the column.
    last: u16,
    /// Whether the first move came within [`RIDE_GAP_TICKS`] of the move
    /// before it: the line joins it with a diagonal rather than a step.
    ride: bool,
}

impl Knot {
    /// A knot of one move to `value`.
    fn at(x: f32, value: u16, ride: bool) -> Self {
        Knot {
            x,
            lo: value,
            hi: value,
            last: value,
            ride,
        }
    }
}

/// A vertical stroke over the values one column reaches beyond the line
/// through it — a spike inside a single pixel.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Segment {
    /// Where it starts.
    from: PathPoint,
    /// Where it ends.
    to: PathPoint,
}

/// A wheel's drawing: one continuous line, and the spikes it would miss.
#[derive(Clone, Debug, PartialEq)]
struct WheelPath {
    /// The line, left to right.
    line: Vec<PathPoint>,
    /// The columns whose moves reach past the line.
    spans: Vec<Segment>,
}

/// Folds `moves` (tick order) into `cols` pixel columns of `ticks_per_col`
/// ticks each, the first starting at `first_tick`. Returns where the line
/// starts — the last move before the lane, at its own x left of it, as
/// playback chases it, or `neutral` at the lane's edge with none — and one
/// [`Knot`] per column holding moves, plus the first move past the right
/// edge, so a ride crossing either edge still joins up. Visits each visible
/// move once; its output follows the columns.
fn fold_wheel_columns(
    moves: &[WheelMove],
    neutral: u16,
    first_tick: f32,
    ticks_per_col: f32,
    cols: usize,
) -> (PathPoint, Vec<Knot>) {
    let x_of = |tick: i32| (tick as f32 - first_tick) / ticks_per_col;
    let first = moves.partition_point(|&(tick, _)| (tick as f32) < first_tick);
    let mut prev_tick = first.checked_sub(1).map(|i| moves[i].0);
    let start = match first.checked_sub(1).map(|i| moves[i]) {
        Some((tick, value)) => (x_of(tick), value),
        None => (0.0, neutral),
    };

    let mut knots: Vec<Knot> = Vec::new();
    let mut last_col = None;
    for &(tick, value) in &moves[first..] {
        let ride = prev_tick.is_some_and(|prev| tick - prev <= RIDE_GAP_TICKS);
        prev_tick = Some(tick);
        let col = x_of(tick).floor() as usize;
        if col >= cols {
            knots.push(Knot::at(x_of(tick), value, ride));
            break;
        }
        match knots.last_mut() {
            Some(knot) if last_col == Some(col) => {
                knot.lo = knot.lo.min(value);
                knot.hi = knot.hi.max(value);
                knot.last = value;
            }
            _ => knots.push(Knot::at(col as f32 + 0.5, value, ride)),
        }
        last_col = Some(col);
    }
    (start, knots)
}

/// The wheel's line from `start` through `knots` to the lane's right edge
/// at `cols`: into a riding knot, a diagonal; otherwise flat at the held
/// value, then a vertical step at the knot. A column whose moves reach past
/// the values the line passes through there gets a span over them.
fn wheel_path(start: PathPoint, knots: &[Knot], cols: f32) -> WheelPath {
    let mut line = Vec::with_capacity(knots.len() * 2 + 2);
    let mut push = |point: PathPoint| {
        if line.last() != Some(&point) {
            line.push(point);
        }
    };
    let mut spans = Vec::new();
    let mut value = start.1;
    push(start);
    for knot in knots {
        if !knot.ride {
            push((knot.x, value));
        }
        push((knot.x, knot.last));
        if knot.lo < value.min(knot.last) || knot.hi > value.max(knot.last) {
            spans.push(Segment {
                from: (knot.x, knot.lo),
                to: (knot.x, knot.hi),
            });
        }
        value = knot.last;
    }
    if line.last().is_some_and(|&(x, _)| x < cols) {
        line.push((cols, value));
    }
    WheelPath { line, spans }
}

/// Drops the points of `points` that lie within `tolerance` of the line
/// through the points kept either side (Ramer–Douglas–Peucker), so a ride
/// of the mod wheel's coarse steps — about half a pixel each — draws as the
/// slope it is rather than a wobble. Both ends of every vertical step (a
/// hold's corner and the move it steps to) are kept outright and split the
/// line into the rides between them, each simplified alone: a staircase of
/// holds costs one pass, never the algorithm's quadratic worst case.
fn simplify(points: &[Pos2], tolerance: f32) -> Vec<Pos2> {
    let n = points.len();
    let mut keep = vec![false; n];
    if let (Some(first), Some(last)) = (keep.first_mut(), n.checked_sub(1)) {
        *first = true;
        keep[last] = true;
    }
    for (i, pair) in points.windows(2).enumerate() {
        if pair[0].x == pair[1].x {
            keep[i] = true;
            keep[i + 1] = true;
        }
    }
    let anchors: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
    let mut stack: Vec<(usize, usize)> = anchors.windows(2).map(|w| (w[0], w[1])).collect();
    while let Some((first, last)) = stack.pop() {
        let (a, b) = (points[first], points[last]);
        let farthest = (first + 1..last)
            .map(|i| (i, distance_to_segment(points[i], a, b)))
            .max_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((i, distance)) = farthest
            && distance > tolerance
        {
            keep[i] = true;
            stack.push((first, i));
            stack.push((i, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(&point, kept)| kept.then_some(point))
        .collect()
}

/// How far `point` is from the segment `a`–`b`.
fn distance_to_segment(point: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    let t = if len_sq == 0.0 {
        0.0
    } else {
        ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0)
    };
    (point - (a + ab * t)).length()
}

/// Cuts the line `points` (left to right) where it crosses either x in
/// `cuts` (in order, left first), into runs of two or more points that each
/// lie on one side.
fn split_at_x(points: &[Pos2], cuts: [f32; 2]) -> Vec<Vec<Pos2>> {
    let mut runs = Vec::new();
    let Some(&first) = points.first() else {
        return runs;
    };
    let mut run = vec![first];
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        for x in cuts.into_iter().filter(|&x| a.x < x && x < b.x) {
            let cut = a + (b - a) * ((x - a.x) / (b.x - a.x));
            run.push(cut);
            runs.push(std::mem::replace(&mut run, vec![cut]));
        }
        run.push(b);
    }
    if run.len() > 1 {
        runs.push(run);
    }
    runs
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};

    use super::{
        Knot, RIDE_GAP_TICKS, Segment, fold_wheel_columns, lane_label_in, simplify, split_at_x,
        wheel_fraction, wheel_path,
    };
    use crate::core::midi::message::Controller;
    use crate::view::display::clip_lane::ClipLane;

    #[test]
    fn a_column_with_many_moves_folds_to_one_knot() {
        // Ten ticks a column; column 1 holds four moves.
        let moves = [(10, 50), (12, 90), (15, 20), (19, 60)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 3);
        assert_eq!(start, (0.0, 0), "at rest before the first move");
        assert_eq!(
            knots,
            vec![Knot {
                x: 1.5,
                lo: 20,
                hi: 90,
                last: 60,
                ride: false,
            }]
        );
    }

    /// The line enters the lane with what playback chases there: the last
    /// move before it, never neutral, at its own x; a move exactly at the
    /// lane's left edge is the first knot.
    #[test]
    fn the_line_starts_with_the_value_in_force() {
        let moves = [(100, 30), (400, 80), (960, 120)];
        let (start, knots) = fold_wheel_columns(&moves, 0x2000, 960.0, 10.0, 2);
        assert_eq!(start, (-56.0, 80));
        assert_eq!(knots, vec![Knot::at(0.5, 120, false)]);

        let (start, knots) = fold_wheel_columns(&moves, 0x2000, 0.0, 10.0, 1);
        assert_eq!(start, (0.0, 0x2000));
        assert_eq!(
            knots[0].x, 10.0,
            "the first move past the edge, at its own x"
        );
    }

    #[test]
    fn folding_stops_at_the_first_move_past_the_right_edge() {
        let moves = [(5, 1), (25, 2), (35, 3)];
        let (_, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 2);
        assert_eq!(knots.iter().map(|k| k.last).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// Moves a few ticks apart are a ride: one diagonal into each.
    #[test]
    fn a_ride_is_joined_smoothly() {
        let moves = [(0, 10), (10, 20), (20, 30)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 3);
        let path = wheel_path(start, &knots, 3.0);
        assert_eq!(
            path.line,
            vec![
                (0.0, 0),
                (0.5, 0),
                (0.5, 10),
                (1.5, 20),
                (2.5, 30),
                (3.0, 30)
            ],
            "the first move steps off rest; the rest of the ride is diagonal"
        );
        assert!(path.spans.is_empty());
    }

    /// A value held longer than a 32nd stays flat, then steps.
    #[test]
    fn a_held_value_steps_to_the_next_move() {
        let gap = RIDE_GAP_TICKS + 1;
        let moves = [(0, 10), (gap, 50)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 1.0, gap as usize + 2);
        let x = knots[1].x;
        let path = wheel_path(start, &knots, x + 1.5);
        assert_eq!(
            path.line[3..],
            [(x, 10), (x, 50), (x + 1.5, 50)],
            "{:?}",
            path.line
        );
    }

    #[test]
    fn a_ride_crosses_the_left_edge() {
        let moves = [(95, 40), (105, 60)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 100.0, 10.0, 2);
        let path = wheel_path(start, &knots, 2.0);
        assert_eq!(path.line[..2], [(-0.5, 40), (0.5, 60)]);
    }

    /// A spike inside one column reaches past the line, so it gets a span;
    /// a column whose moves stay on the line doesn't.
    #[test]
    fn a_spike_inside_a_column_gets_a_span() {
        let moves = [(10, 50), (12, 90), (15, 20), (19, 60)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 3);
        let path = wheel_path(start, &knots, 3.0);
        assert_eq!(
            path.spans,
            vec![Segment {
                from: (1.5, 20),
                to: (1.5, 90),
            }]
        );

        let moves = [(10, 50), (12, 55), (15, 60)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 3);
        assert!(wheel_path(start, &knots, 3.0).spans.is_empty());
    }

    #[test]
    fn simplifying_drops_sub_pixel_wobble_and_keeps_corners() {
        // A slope with half-pixel wobble, then a hold and a step.
        let points = [
            pos2(0.0, 10.0),
            pos2(1.0, 9.3),
            pos2(2.0, 8.0),
            pos2(3.0, 7.2),
            pos2(4.0, 6.0),
            pos2(20.0, 6.0),
            pos2(20.0, 30.0),
        ];
        assert_eq!(
            simplify(&points, 0.5),
            vec![
                pos2(0.0, 10.0),
                pos2(4.0, 6.0),
                pos2(20.0, 6.0),
                pos2(20.0, 30.0)
            ]
        );
    }

    #[test]
    fn the_line_splits_where_it_crosses_the_window() {
        let points = [pos2(0.0, 0.0), pos2(10.0, 10.0), pos2(30.0, 10.0)];
        let runs = split_at_x(&points, [5.0, 20.0]);
        assert_eq!(
            runs,
            vec![
                vec![pos2(0.0, 0.0), pos2(5.0, 5.0)],
                vec![pos2(5.0, 5.0), pos2(10.0, 10.0), pos2(20.0, 10.0)],
                vec![pos2(20.0, 10.0), pos2(30.0, 10.0)],
            ]
        );
        assert_eq!(split_at_x(&points, [-1.0, 99.0]), vec![points.to_vec()]);
    }

    #[test]
    fn a_gutter_row_picks_its_lane() {
        let gutter = Rect::from_min_size(pos2(0.0, 100.0), vec2(88.0, 81.0));
        assert_eq!(
            lane_label_in(gutter, pos2(10.0, 101.0)),
            Some(ClipLane::Velocity)
        );
        assert_eq!(
            lane_label_in(gutter, pos2(10.0, 140.0)),
            Some(ClipLane::PitchBend)
        );
        assert_eq!(
            lane_label_in(gutter, pos2(87.0, 180.0)),
            Some(ClipLane::Modulation)
        );
        assert_eq!(lane_label_in(gutter, pos2(10.0, 99.0)), None);
        assert_eq!(lane_label_in(gutter, pos2(89.0, 140.0)), None);
    }

    #[test]
    fn pitch_bend_rests_at_the_centre() {
        assert_eq!(wheel_fraction(Controller::PitchBend, 0x2000), 0.5);
        assert_eq!(wheel_fraction(Controller::PitchBend, 0), 0.0);
        assert!((wheel_fraction(Controller::PitchBend, 0x3FFF) - 1.0).abs() < 0.001);
    }

    #[test]
    fn the_mod_wheel_runs_from_bottom_to_top() {
        assert_eq!(wheel_fraction(Controller::MOD_WHEEL, 0), 0.0);
        assert_eq!(wheel_fraction(Controller::MOD_WHEEL, 127), 1.0);
    }
}
