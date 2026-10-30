//! The piano roll's lower lane when it shows a wheel, and the gutter switch
//! that picks the lane (`ClipLane`): the `VEL` / `PB` / `MW` labels left of
//! the lane, and the lead clip's pitch bend or mod wheel drawn as a line,
//! read-only: a ride smooth, a held value flat until the next move steps off
//! it ([`wheel_path`]). The moves are folded per pixel column
//! ([`fold_wheel_columns`]) so the cost follows the lane's width, not the
//! event count. See `380-wheel-lane.md`.

use egui::{Align2, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::piano_keys::NoteAreaGeom;
use super::*;
use crate::core::midi::message::Wheel;
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

    /// The lower lane under the note grid: the velocity bars or a wheel's
    /// curve, across the content width.
    pub(super) fn lower_lane_rect(&self, geom: &NoteAreaGeom) -> Rect {
        let content_x = self.content_origin_x();
        Rect::from_min_size(
            pos2(content_x, geom.top + geom.h),
            vec2(self.content_right_x() - content_x, Self::VELOCITY_PANEL_H),
        )
    }

    /// The gutter left of the lower lane, under the octave legend and the
    /// keys, holding the lane switch.
    fn lane_gutter_rect(&self, geom: &NoteAreaGeom) -> Rect {
        let content_x = self.content_origin_x();
        Rect::from_min_max(
            pos2(content_x - 2.0 * Self::PIANO_MARGIN_W, geom.top + geom.h),
            pos2(content_x, geom.top + geom.h + Self::VELOCITY_PANEL_H),
        )
    }

    /// The gutter label under `(x, y)`: its whole row, the gutter's width.
    /// Clip pane only, and only with a lead clip shown.
    pub(in crate::view::display) fn clip_lane_label_at(&self, x: f32, y: f32) -> Option<ClipLane> {
        if !self.is_pane_visible(Pane::Clip) || self.lead_clip_time.is_none() {
            return None;
        }
        let gutter = self.lane_gutter_rect(&self.note_area_geom());
        if !gutter.contains(pos2(x, y)) {
            return None;
        }
        ClipLane::ALL
            .into_iter()
            .enumerate()
            .find(|&(i, _)| lane_label_row(gutter, i).contains(pos2(x, y)))
            .map(|(_, lane)| lane)
    }

    /// Whether `(x, y)` is in the lower lane. Clip pane only.
    pub(in crate::view::display) fn is_in_lower_lane(&self, x: f32, y: f32) -> bool {
        self.lower_lane_rect(&self.note_area_geom())
            .contains(pos2(x, y))
    }

    /// A gutter label under the pointer that a click would switch to — not
    /// the lane already shown. Clip pane only.
    pub(in crate::view::display) fn hovered_clip_lane_label(&mut self) -> Option<ClipLane> {
        let shown = self.render.clip_lane;
        self.in_pane(Pane::Clip, |display| {
            display
                .canvas_pointer()
                .and_then(|p| display.clip_lane_label_at(p.x, p.y))
        })
        .filter(|&lane| lane != shown)
    }

    /// Draws the lane switch: `VEL`, `PB`, `MW` stacked in the gutter,
    /// coloured as the header's toggles are (`toggle_icon_color`): the shown
    /// one `accent`, one under the pointer `fg`, the rest `fg_dim` — so a
    /// hover is never mistaken for the shown lane. A dot, in the label's
    /// colour, beside a wheel's label when the lead clip has moves for it.
    pub(super) fn draw_lane_gutter(&self, painter: &Painter, geom: &NoteAreaGeom) {
        let gutter = self.lane_gutter_rect(geom);
        let hovered = self
            .canvas_pointer()
            .and_then(|p| self.clip_lane_label_at(p.x, p.y));
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
                .is_some_and(|wheel| !self.render.wheel_moves[wheel.index()].is_empty())
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
    /// rest. Nothing else with no moves.
    pub(super) fn draw_wheel_lane(&self, painter: &Painter, lane: Rect, wheel: Wheel) {
        let inner_top = lane.top() + Self::WHEEL_LANE_PAD;
        let inner_h = lane.height() - 2.0 * Self::WHEEL_LANE_PAD;
        let y_of = |value| inner_top + (1.0 - wheel_fraction(wheel, value)) * inner_h;

        if wheel == Wheel::PitchBend {
            painter.hline(
                lane.x_range(),
                y_of(wheel.neutral()).round(),
                Stroke::new(1.0_f32, theme::fg_dim().gamma_multiply(0.3)),
            );
        }

        let moves = &self.render.wheel_moves[wheel.index()];
        if moves.is_empty() {
            return;
        }

        let ticks_per_col = self.ticks_per_pixel();
        let first_tick = (lane.left() - self.tick_to_x(0)) * ticks_per_col;
        let cols = lane.width().max(0.0);
        let (start, knots) = fold_wheel_columns(
            moves,
            wheel.neutral(),
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

/// Where `value` sits in the lane, `0.0` at the bottom to `1.0` at the top:
/// pitch bend centred, its rest (`0x2000`) exactly halfway; the mod wheel
/// from 0 to 127.
fn wheel_fraction(wheel: Wheel, value: u16) -> f32 {
    match wheel {
        Wheel::PitchBend => {
            let centre = f32::from(Wheel::PitchBend.neutral());
            0.5 + (f32::from(value) - centre) / (2.0 * centre)
        }
        Wheel::Modulation => f32::from(value.min(127)) / 127.0,
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

/// The value a wheel's line holds and where it got it: the x of the move that
/// set it and that move's tick, `None` for rest before any move.
#[derive(Copy, Clone, Debug, PartialEq)]
struct PathStart {
    /// Where the value was set, column units (negative left of the lane).
    x: f32,
    /// The value.
    value: u16,
    /// The move's tick, `None` at rest.
    tick: Option<i32>,
}

/// The moves inside one pixel column (or, past the lane's right edge, the
/// first move after it), folded: where they sit, when they start and end,
/// and the values they reach.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Knot {
    /// The column's centre, or the move's own x past the right edge.
    x: f32,
    /// The first move's tick.
    first_tick: i32,
    /// The last move's tick.
    last_tick: i32,
    /// The lowest value moved to.
    lo: u16,
    /// The highest value moved to.
    hi: u16,
    /// The last value moved to — held after the column.
    last: u16,
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
/// edge, so a ride crossing either edge still joins up. The cost follows the
/// columns, not the moves.
fn fold_wheel_columns(
    moves: &[WheelMove],
    neutral: u16,
    first_tick: f32,
    ticks_per_col: f32,
    cols: usize,
) -> (PathStart, Vec<Knot>) {
    let x_of = |tick: i32| (tick as f32 - first_tick) / ticks_per_col;
    let first = moves.partition_point(|&(tick, _)| (tick as f32) < first_tick);
    let start = match first.checked_sub(1).map(|i| moves[i]) {
        Some((tick, value)) => PathStart {
            x: x_of(tick),
            value,
            tick: Some(tick),
        },
        None => PathStart {
            x: 0.0,
            value: neutral,
            tick: None,
        },
    };

    let mut knots: Vec<Knot> = Vec::new();
    let mut last_col = None;
    for &(tick, value) in &moves[first..] {
        let col = x_of(tick).floor() as usize;
        if col >= cols {
            knots.push(Knot {
                x: x_of(tick),
                first_tick: tick,
                last_tick: tick,
                lo: value,
                hi: value,
                last: value,
            });
            break;
        }
        match knots.last_mut() {
            Some(knot) if last_col == Some(col) => {
                knot.last_tick = tick;
                knot.lo = knot.lo.min(value);
                knot.hi = knot.hi.max(value);
                knot.last = value;
            }
            _ => knots.push(Knot {
                x: col as f32 + 0.5,
                first_tick: tick,
                last_tick: tick,
                lo: value,
                hi: value,
                last: value,
            }),
        }
        last_col = Some(col);
    }
    (start, knots)
}

/// The wheel's line from `start` through `knots` to the lane's right edge
/// at `cols`: into a knot within [`RIDE_GAP_TICKS`] of the move before it, a
/// diagonal (a ride); otherwise flat at the held value, then a vertical step
/// at the knot. A column whose moves reach past the values the line passes
/// through there gets a span over them.
fn wheel_path(start: PathStart, knots: &[Knot], cols: f32) -> WheelPath {
    let mut line = Vec::with_capacity(knots.len() * 2 + 2);
    let mut push = |point: PathPoint| {
        if line.last() != Some(&point) {
            line.push(point);
        }
    };
    let mut spans = Vec::new();
    let (mut x, mut value, mut tick) = (start.x, start.value, start.tick);
    push((x, value));
    for knot in knots {
        let ride = tick.is_some_and(|tick| knot.first_tick - tick <= RIDE_GAP_TICKS);
        if !ride {
            push((knot.x, value));
        }
        push((knot.x, knot.last));
        if knot.lo < value.min(knot.last) || knot.hi > value.max(knot.last) {
            spans.push(Segment {
                from: (knot.x, knot.lo),
                to: (knot.x, knot.hi),
            });
        }
        (x, value, tick) = (knot.x, knot.last, Some(knot.last_tick));
    }
    if x < cols {
        push((cols, value));
    }
    WheelPath { line, spans }
}

/// Drops the points of `points` that lie within `tolerance` of the line
/// through the points kept either side (Ramer–Douglas–Peucker), so a ride
/// of the mod wheel's coarse steps — about half a pixel each — draws as the
/// slope it is rather than a wobble. A hold's corners and a step are far
/// off any such line and stay.
fn simplify(points: &[Pos2], tolerance: f32) -> Vec<Pos2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
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
/// `cuts`, into runs of two or more points that each lie on one side.
fn split_at_x(points: &[Pos2], cuts: [f32; 2]) -> Vec<Vec<Pos2>> {
    let mut runs = Vec::new();
    let Some(&first) = points.first() else {
        return runs;
    };
    let mut run = vec![first];
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let mut crossings: Vec<f32> = cuts.into_iter().filter(|&x| a.x < x && x < b.x).collect();
        crossings.sort_by(f32::total_cmp);
        for x in crossings {
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
    use egui::pos2;

    use super::{
        Knot, PathStart, RIDE_GAP_TICKS, Segment, fold_wheel_columns, simplify, split_at_x,
        wheel_fraction, wheel_path,
    };
    use crate::core::midi::message::Wheel;

    #[test]
    fn a_column_with_many_moves_folds_to_one_knot() {
        // Ten ticks a column; column 1 holds four moves.
        let moves = [(10, 50), (12, 90), (15, 20), (19, 60)];
        let (start, knots) = fold_wheel_columns(&moves, 0, 0.0, 10.0, 3);
        assert_eq!(start.value, 0, "at rest before the first move");
        assert_eq!(
            knots,
            vec![Knot {
                x: 1.5,
                first_tick: 10,
                last_tick: 19,
                lo: 20,
                hi: 90,
                last: 60,
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
        assert_eq!(
            start,
            PathStart {
                x: -56.0,
                value: 80,
                tick: Some(400),
            }
        );
        assert_eq!(knots.len(), 1);
        assert_eq!((knots[0].x, knots[0].last), (0.5, 120));

        let (start, knots) = fold_wheel_columns(&moves, 0x2000, 0.0, 10.0, 1);
        assert_eq!((start.x, start.value, start.tick), (0.0, 0x2000, None));
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
    fn pitch_bend_rests_at_the_centre() {
        assert_eq!(wheel_fraction(Wheel::PitchBend, 0x2000), 0.5);
        assert_eq!(wheel_fraction(Wheel::PitchBend, 0), 0.0);
        assert!((wheel_fraction(Wheel::PitchBend, 0x3FFF) - 1.0).abs() < 0.001);
    }

    #[test]
    fn the_mod_wheel_runs_from_bottom_to_top() {
        assert_eq!(wheel_fraction(Wheel::Modulation, 0), 0.0);
        assert_eq!(wheel_fraction(Wheel::Modulation, 127), 1.0);
    }
}
