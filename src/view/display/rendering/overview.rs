//! The arranger's overview strip (`590-arranger-overview.md`): the whole
//! arrangement in miniature above the timeline — every clip in its track
//! colour, the cursor, the playhead — with the strip outside the part on
//! screen shaded. No loop band: the timeline's own is right under it. The
//! geometry is the pure `overview` module's; the drag that moves the view is
//! in `input/gestures.rs`.

use egui::{CornerRadius, Painter, Rect, Stroke, pos2};

use crate::view::display::overview::overview_row;

use super::*;

/// Opacity of a clip's fill in the overview: the lanes' clip header strength
/// (`draw_clip_body`), so the miniature reads as the arranger from afar and
/// no louder (0.8 until 2026-11-27, when a full project turned the strip into
/// the brightest band on the screen).
const CLIP_ALPHA: f32 = 0.45;
/// Opacity of a muted clip's fill, the lanes' muted header strength.
const MUTED_CLIP_ALPHA: f32 = 0.2;
/// Opacity of the shade over the strip outside the view box: the
/// window's darkest background (`bg`), so the off-screen part sinks back.
const OUTSIDE_SHADE_ALPHA: f32 = 0.6;
/// Width of the cursor's line.
const CURSOR_W: f32 = 1.0;
/// Width of the groove between the overview and the timeline strip.
const GROOVE_W: f32 = 1.0;
/// Gap above and below the clip rows, the cursor and the playhead, inside
/// the strip.
const ROWS_INSET_Y: f32 = 2.0;

impl Display {
    /// The overview's tick ↔ x mapping for this frame: across the content
    /// area, over the fully zoomed-out view (`arranger_zoom_out_view_ticks`),
    /// as Ableton's: the strip doesn't rescale while the view zooms, only
    /// the box does, and at full zoom-out the box fills it. It changes only
    /// with the arrangement's length (and the window's width). Arranger pane
    /// only.
    pub(in crate::view::display) fn overview_map(&self) -> OverviewMap {
        OverviewMap::new(
            self.content_origin_x(),
            self.content_right_x(),
            self.arranger_zoom_out_view_ticks(),
        )
    }

    /// Paints the overview strip into the top `OVERVIEW_H` of the arranger
    /// pane, back to front: the background, the clips, the shade outside the
    /// view box, the cursor, the playhead, and the groove to the timeline
    /// strip under it. The track column's corner to its left is the column's
    /// (`draw_arranger_backgrounds` fills it).
    pub(super) fn draw_overview(&self, painter: &Painter, rect: Rect) {
        let top = rect.min.y + self.track_area_top();
        let bottom = top + OVERVIEW_H;
        let left = self.content_origin_x();
        let map = self.overview_map();
        let strip = Rect::from_min_max(pos2(left, top), pos2(rect.max.x, bottom));
        let painter = painter.with_clip_rect(strip.intersect(painter.clip_rect()));
        painter.rect_filled(strip, CornerRadius::ZERO, theme::bg_timeline());

        // Clips, one row per track, their edges on whole pixels so rows and
        // neighbouring clips don't smear together, never under a pixel wide.
        let ppp = painter.pixels_per_point();
        let snap = |x: f32| (x * ppp).round() / ppp;
        let track_count = self.track_count();
        let rows_top = top + ROWS_INSET_Y;
        let rows_h = OVERVIEW_H - 2.0 * ROWS_INSET_Y;
        for shape in &self.render.clip_shapes {
            let track_idx = shape.track_idx();
            if track_idx >= track_count {
                continue;
            }
            let (row_top, row_bottom) = overview_row(track_idx, track_count, rows_top, rows_h, ppp);
            let x0 = snap(map.x_of(shape.start_tick() as f32));
            let x1 = snap(map.x_of(shape.end_tick() as f32)).max(x0 + 1.0 / ppp);
            let alpha = if shape.is_muted() {
                MUTED_CLIP_ALPHA
            } else {
                CLIP_ALPHA
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, row_top), pos2(x1, row_bottom)),
                CornerRadius::ZERO,
                self.track_color(track_idx).gamma_multiply(alpha),
            );
        }

        // The view box: the strip outside the view shaded down, no outline
        // (an outline framed the whole strip at full zoom-out), held inside
        // the strip. Under the cursor and the playhead, which stay crisp.
        let (view_start, view_end) = self.arranger_view_ticks();
        let (box_left, box_right) = map.view_box(view_start, view_end);
        shade_outside(
            &painter,
            strip,
            (box_left.round(), box_right.round()),
            OUTSIDE_SHADE_ALPHA,
        );

        // The cursor (where playback starts), in the lanes' cursor colour,
        // so you can find your way back to it. It and the playhead span the
        // clip rows, clear of the pane's focus edge above.
        let lines_y = rows_top..=rows_top + rows_h;
        let cursor_x = map.x_of(self.render_cursor_tick() as f32).round();
        painter.vline(
            cursor_x,
            lines_y.clone(),
            Stroke::new(CURSOR_W, self.arranger_cursor_color()),
        );

        // The playhead, while the transport runs.
        if self.render_running()
            && let Some(playback) = self.render_playback_tick()
        {
            let x = map.x_of(playback as f32).round();
            painter.vline(x, lines_y, Stroke::new(1.0, theme::playhead()));
        }

        // The groove between the overview and the timeline strip.
        painter.hline(
            left..=strip.max.x,
            bottom - GROOVE_W * 0.5,
            Stroke::new(GROOVE_W, grid_seam_color()),
        );
    }
}
