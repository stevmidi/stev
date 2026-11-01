//! Painting the knob panel (`knob_panel.rs`): its column fill, under the
//! track headers and in their tone, and each knob — a trough along the
//! dial's sweep, the value in the selected track's colour along it, a
//! pointer to the value, and the parameter's name under it, shortened to
//! fit — with a clip open, a lane switch coloured as the gutter's labels
//! are. An unassigned knob has an empty dial and a dim placeholder label; an
//! armed one reads `Turn…` in `accent`. A track whose knobs can't drive
//! anything shows the panel's hint instead. See `030-ui-design.md` § Knob
//! Panel.

use std::f32::consts::TAU;

use egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::*;
use crate::view::display::knob_panel::{KnobRects, LABEL_FONT_SIZE, SWEEP, knob_angle};

/// Points between a selected knob's label and the focus edge under it.
const SELECTED_UNDERLINE_GAP: f32 = 2.0;
/// The width of a dial's trough and value arc.
const ARC_W: f32 = 3.0;
/// The arc's points per full turn — smooth at the largest dial.
const ARC_SEGMENTS_PER_TURN: f32 = 48.0;
/// What an armed knob's label reads while it waits for a control turned in
/// the plugin.
const ARMED_LABEL: &str = "Turn…";
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
        let accent = self.track_color(self.selected_track_idx);
        let switches = self.knob_labels_switch_lanes();
        let hovered = self.knob_under_pointer();
        let armed = self.armed_knob();
        let selected = match self.render.clip_lane {
            ClipLane::Knob(knob) if self.knob_panel_has_keyboard() => Some(knob),
            _ => None,
        };
        let font = FontId::proportional(LABEL_FONT_SIZE);
        for (knob, rects) in knobs.iter().enumerate() {
            let Some(shown) = self.knob_shown(knob) else {
                continue;
            };
            let rects = KnobRects {
                center: rects.center + offset,
                label: rects.label.translate(offset),
                ..*rects
            };
            draw_knob(painter, &rects, shown.value, accent);
            let lane_color = if switches {
                toggle_icon_color(
                    self.render.clip_lane == ClipLane::Knob(knob),
                    hovered == Some(knob),
                )
            } else {
                theme::fg_dim()
            };
            let (text, color) = if armed == Some(knob) {
                (ARMED_LABEL.to_owned(), theme::accent())
            } else if shown.param.is_some() {
                let name = self.knob_name(knob).unwrap_or("?");
                let max_w = rects.label.width() - 2.0 * LABEL_INSET_X;
                (fit_label(painter, name, &font, max_w), lane_color)
            } else {
                (format!("Knob {}", knob + 1), theme::fg_dim())
            };
            let label = painter.text(
                rects.label.center(),
                Align2::CENTER_CENTER,
                text,
                font.clone(),
                color,
            );
            // The selected knob, while the panel has the keyboard: the focus
            // edge under its label (`390-knob-panel.md` decision 14).
            if selected == Some(knob) {
                painter.hline(
                    label.x_range(),
                    label.max.y + SELECTED_UNDERLINE_GAP,
                    focus_edge_stroke(),
                );
            }
        }
    }
}

/// `text` as it fits in `max_w` points of `font`: whole when it does, else
/// cut short with an ellipsis (`400-parameter-knobs.md` decision 5 — the
/// footer shows the full name on hover).
fn fit_label(painter: &Painter, text: &str, font: &FontId, max_w: f32) -> String {
    let width = |s: String| {
        painter
            .layout_no_wrap(s, font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text.to_owned()) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    (1..chars.len())
        .rev()
        .map(|n| format!("{}…", chars[..n].iter().collect::<String>().trim_end()))
        .find(|cut| width(cut.clone()) <= max_w)
        .unwrap_or_else(|| "…".to_owned())
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
        grid_seam_color(),
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
