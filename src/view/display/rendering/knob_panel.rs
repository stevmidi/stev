//! Painting the knob panel (`knob_panel.rs`): its column fill, under the
//! track headers and in their tone, and each knob — a trough along the
//! dial's sweep, the value in the selected track's colour along it, a
//! pointer to the value, and the parameter's name under it, shortened to
//! fit — with a clip open, a lane switch coloured as the gutter's labels
//! are. The selected knob, while the panel has the keyboard, gets corner
//! brackets around its dial. An unassigned knob has an empty dial and a dim placeholder label; an
//! armed one reads `Turn…` in `accent`. A track whose knobs can't drive
//! anything shows the panel's hint instead. See `030-ui-design.md` § Knob
//! Panel.

use std::f32::consts::TAU;

use egui::text::LayoutJob;
use egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::output_menu::draw_truncated_at;
use super::*;
use crate::core::midi::message::Controller;
use crate::models::knobs::KNOB_COUNT;
use crate::view::display::knob_panel::{ARC_W, KnobRects, LABEL_FONT_SIZE, SWEEP, knob_angle};

/// The stroke width of a selected knob's corner brackets: a hairline, one
/// physical pixel on a Retina screen, so the focus edge across the panel's
/// top stays the strong mark.
const FRAME_W: f32 = 0.5;
/// Opacity of the `fg_dim` a dial's trough is drawn in: light enough that
/// the whole dial reads against the panel at any value, so a turned-down
/// knob doesn't look like its value arc alone (and sit off-centre in its
/// selection brackets), well under the value arc.
const TROUGH_ALPHA: f32 = 0.35;
/// How long each bracket's arms are, as a share of the frame's shorter side.
const FRAME_ARM: f32 = 0.2;
/// The arc's points per full turn — smooth at the largest dial.
const ARC_SEGMENTS_PER_TURN: f32 = 48.0;
/// What an armed knob's label reads while it waits for a control turned in
/// the plugin.
const ARMED_LABEL: &str = "Turn…";
/// An unassigned knob's dim label, by knob (counted from 1).
const PLACEHOLDER_LABELS: [&str; KNOB_COUNT] = [
    "Knob 1", "Knob 2", "Knob 3", "Knob 4", "Knob 5", "Knob 6", "Knob 7", "Knob 8",
];
/// Points kept clear at each end of a label, so names in neighbouring
/// columns never touch.
const LABEL_INSET_X: f32 = 2.0;

impl Display {
    /// Paints the knob panel beside the clip view, when it shows. `rect` is
    /// the canvas.
    pub(super) fn draw_knob_panel(&self, painter: &Painter, rect: Rect) {
        let Some(panel) = self.knob_panel_rect() else {
            return;
        };
        let offset = rect.min.to_vec2();
        painter.rect_filled(
            panel.translate(offset),
            CornerRadius::ZERO,
            track_column_fill(),
        );
        // The column's lip on its grid side, as the track headers'.
        painter.vline(
            panel.max.x + offset.x,
            (panel.min.y + offset.y)..=(panel.max.y + offset.y),
            Stroke::new(1.0, grid_seam_color()),
        );
        if let Some(hint) = self.knob_hint() {
            painter.text(
                panel.translate(offset).center(),
                Align2::CENTER_CENTER,
                hint,
                FontId::proportional(LABEL_FONT_SIZE),
                theme::fg_dim(),
            );
            return;
        }
        let Some(knobs) = self.knob_rects() else {
            return;
        };
        let accent = self.lead_clip_color();
        let switches = self.knob_labels_switch_lanes();
        let hovered = self.knob_under_pointer();
        let armed = self.armed_knob();
        let selected = self.selected_knob();
        let font = FontId::proportional(LABEL_FONT_SIZE);
        for (knob, rects) in knobs.iter().enumerate() {
            let Some(shown) = self.knob_shown(knob) else {
                continue;
            };
            let rects = KnobRects {
                center: rects.center + offset,
                label: rects.label.translate(offset),
                frame: rects.frame.translate(offset),
                ..*rects
            };
            draw_knob(painter, &rects, shown.value, accent);
            // The gutter's toggle colours, the knob's label being its lane's
            // switch: `accent` while its lane is up, so the lane stays named
            // once the panel loses the keyboard and the brackets go; `fg`
            // under the pointer, since a press selects any knob. An
            // unassigned knob has no lane to name, so it only lights up.
            let lane_up =
                switches && shown.param.is_some() && self.render.clip_lane == ClipLane::Knob(knob);
            let name_color = toggle_icon_color(lane_up, hovered == Some(knob));
            let (text, color) = if armed == Some(knob) {
                (ARMED_LABEL, theme::accent())
            } else if shown.param.is_some() {
                (self.knob_name(knob).unwrap_or("?"), name_color)
            } else {
                (PLACEHOLDER_LABELS[knob], name_color)
            };
            // The gutter's data marker, for a clip with moves of the knob's
            // parameter (`400-parameter-knobs.md` decision 12).
            let marked = switches
                && self
                    .knob_controller(knob)
                    .is_some_and(|param| self.lead_clip_moves(Controller::Param(param)));
            // Cut short with an ellipsis when it doesn't fit
            // (`400-parameter-knobs.md` decision 5 — the footer shows the
            // full name on hover).
            let name = draw_truncated_at(
                painter,
                rects.label.center(),
                Align2::CENTER_CENTER,
                LayoutJob::simple_singleline(text.to_owned(), font.clone(), color),
                label_width(rects.label.width(), marked),
            );
            if marked {
                self.draw_lane_marker(painter, name, name_color);
            }
            // The selected knob, while the panel has the keyboard: quiet
            // corner brackets around its dial, neutral, since the focus edge
            // already says the panel has the keyboard and the label may be
            // `accent` for its lane (`400-parameter-knobs.md` decision 14).
            if selected == Some(knob) {
                let stroke = Stroke::new(FRAME_W, theme::fg_dim());
                // On pixel centres, so a one-pixel line lights one row or
                // column instead of greying two: crisp, and the same tone
                // across and down.
                let snap = |p: Pos2| {
                    pos2(
                        painter.round_to_pixel_center(p.x),
                        painter.round_to_pixel_center(p.y),
                    )
                };
                for corner in frame_brackets(rects.frame) {
                    painter.add(Shape::line(corner.map(snap).to_vec(), stroke));
                }
            }
        }
    }
}

/// The four L-shaped corners of `frame`, each a polyline from the end of
/// one arm through the corner to the end of the other, the arms
/// [`FRAME_ARM`] of the shorter side long.
fn frame_brackets(frame: Rect) -> [[Pos2; 3]; 4] {
    let arm = frame.width().min(frame.height()) * FRAME_ARM;
    let Rect { min, max } = frame;
    [
        [pos2(min.x, min.y + arm), min, pos2(min.x + arm, min.y)],
        [
            pos2(max.x - arm, min.y),
            pos2(max.x, min.y),
            pos2(max.x, min.y + arm),
        ],
        [pos2(max.x, max.y - arm), max, pos2(max.x - arm, max.y)],
        [
            pos2(min.x + arm, max.y),
            pos2(min.x, max.y),
            pos2(min.x, max.y - arm),
        ],
    ]
}

/// The room a knob's name has in a label `cell_w` wide. A marked name, being
/// centred, gives up the marker's reach on both sides, so the dot beside it
/// stays in the knob's own cell.
fn label_width(cell_w: f32, marked: bool) -> f32 {
    let marker = if marked {
        2.0 * Display::LANE_MARKER_REACH
    } else {
        0.0
    };
    cell_w - 2.0 * LABEL_INSET_X - marker
}

/// One knob's dial: the trough, the value arc in `accent` and the pointer;
/// an empty dial — trough and a dim pointer at 0 — with no value.
fn draw_knob(painter: &Painter, rects: &KnobRects, value: Option<f64>, accent: Color32) {
    let arc_r = (rects.radius - ARC_W * 0.5).max(1.0);
    let start = knob_angle(0.0);
    painter.add(arc(
        rects.center,
        arc_r,
        start,
        start + SWEEP,
        theme::fg_dim().gamma_multiply(TROUGH_ALPHA),
    ));
    if let Some(value) = value {
        let end = knob_angle(value);
        if end > start {
            painter.add(arc(rects.center, arc_r, start, end, accent));
        }
    }
    let angle = knob_angle(value.unwrap_or(0.0));
    let tip = rects.center + vec2(angle.cos(), angle.sin()) * arc_r;
    let pointer = if value.is_some() {
        theme::fg()
    } else {
        theme::fg_dim()
    };
    painter.line_segment([rects.center, tip], Stroke::new(1.5, pointer));
}

/// An arc of radius `r` about `center` from screen angle `from` to `to`
/// (clockwise), stroked in `color`.
fn arc(center: Pos2, r: f32, from: f32, to: f32, color: Color32) -> Shape {
    let segments = ((to - from) / TAU * ARC_SEGMENTS_PER_TURN).ceil().max(1.0) as usize;
    let points = (0..=segments)
        .map(|i| {
            let angle = from + (to - from) * i as f32 / segments as f32;
            pos2(center.x + r * angle.cos(), center.y + r * angle.sin())
        })
        .collect();
    Shape::line(points, Stroke::new(ARC_W, color))
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    use super::{Display, FRAME_ARM, LABEL_INSET_X, frame_brackets, label_width};

    /// Each bracket turns at its own corner of the frame, its two arms a
    /// quarter of the shorter side long, along the frame's edges.
    #[test]
    fn the_brackets_hug_the_frame_corners() {
        let frame = Rect::from_min_max(pos2(10.0, 20.0), pos2(58.0, 66.0));
        let arm = 46.0 * FRAME_ARM;
        let corners = [
            frame.left_top(),
            frame.right_top(),
            frame.right_bottom(),
            frame.left_bottom(),
        ];
        for (bracket, corner) in frame_brackets(frame).iter().zip(corners) {
            assert_eq!(bracket[1], corner);
            for end in [bracket[0], bracket[2]] {
                let (dx, dy) = ((end.x - corner.x).abs(), (end.y - corner.y).abs());
                assert!(dx == 0.0 || dy == 0.0, "an arm runs along an edge");
                assert!((dx + dy - arm).abs() < 1e-4, "{dx} + {dy} vs {arm}");
                assert!(frame.contains(end));
            }
        }
    }

    /// A name as wide as its room, centred in its cell, leaves the marker
    /// inside the cell: it used to land in the next column, beside the
    /// neighbouring knob's name.
    #[test]
    fn a_marked_label_keeps_its_dot_in_the_cell() {
        let cell_w = 60.0;
        let name_end = cell_w / 2.0 + label_width(cell_w, true) / 2.0;
        assert!(name_end + Display::LANE_MARKER_REACH <= cell_w);
        assert_eq!(label_width(cell_w, false), cell_w - 2.0 * LABEL_INSET_X);
    }
}
