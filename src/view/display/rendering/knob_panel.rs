//! Painting the knob panel (`knob_panel.rs`): its column fill, under the
//! track headers and in their tone, and each knob — a trough along the
//! dial's sweep, the value in the selected track's colour along it, a
//! pointer to the value, and the CC label under it. A knob never turned has
//! no value arc and a dim pointer at 0. See `030-ui-design.md` § Knob Panel.

use std::f32::consts::TAU;

use egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::*;
use crate::view::display::knob_panel::{KnobRects, LABEL_FONT_SIZE, SWEEP, knob_angle};

/// The width of a dial's trough and value arc.
const ARC_W: f32 = 3.0;
/// The arc's points per full turn — smooth at the largest dial.
const ARC_SEGMENTS_PER_TURN: f32 = 48.0;

impl Display {
    /// Paints the knob panel beside the clip view, when it shows. `rect` is
    /// the canvas.
    pub(super) fn draw_knob_panel(&self, painter: &Painter, rect: Rect) {
        let (Some(panel), Some(knobs)) = (self.knob_panel_rect(), self.knob_rects()) else {
            return;
        };
        let offset = rect.min.to_vec2();
        painter.rect_filled(
            panel.translate(offset),
            CornerRadius::ZERO,
            theme::bg_panel().gamma_multiply(0.8),
        );
        // The column's lip on its grid side, as the track headers'.
        painter.vline(
            panel.max.x + offset.x,
            (panel.min.y + offset.y)..=(panel.max.y + offset.y),
            Stroke::new(1.0, grid_seam_color()),
        );
        let accent = self.track_color(self.selected_track_idx);
        for (knob, rects) in knobs.iter().enumerate() {
            let Some((cc, value)) = self.knob_shown(knob) else {
                continue;
            };
            let rects = KnobRects {
                center: rects.center + offset,
                label: rects.label.translate(offset),
                ..*rects
            };
            draw_knob(painter, &rects, value, accent);
            if !self.knob_cc_field_open_on(knob) {
                painter.text(
                    rects.label.center(),
                    Align2::CENTER_CENTER,
                    format!("CC{cc}"),
                    FontId::proportional(LABEL_FONT_SIZE),
                    theme::fg_dim(),
                );
            }
        }
    }
}

/// One knob's dial: the trough, the value arc in `accent` and the pointer.
fn draw_knob(painter: &Painter, rects: &KnobRects, value: Option<u8>, accent: Color32) {
    let arc_r = (rects.radius - ARC_W * 0.5).max(1.0);
    let start = knob_angle(0);
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
    let angle = knob_angle(value.unwrap_or(0));
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
