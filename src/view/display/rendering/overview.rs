//! The arranger's overview strip (`590-arranger-overview.md`): the whole
//! arrangement in miniature above the timeline — every clip in its track
//! colour, the loop band, the playhead — and a box around the part on
//! screen. The geometry is the pure `overview` module's; the drag that moves
//! the view is in `input/gestures.rs`.

use egui::{CornerRadius, Painter, Rect, Stroke, StrokeKind, pos2};

use crate::view::display::overview::overview_row;

use super::*;

/// Opacity of a clip's fill in the overview.
const CLIP_ALPHA: f32 = 0.8;
/// Opacity of a muted clip's fill, dimmer as in the lanes.
const MUTED_CLIP_ALPHA: f32 = 0.3;
/// Opacity of the loop band's fill, before `loop_dim`.
const LOOP_ALPHA: f32 = 0.22;
/// Opacity of the wash inside the view box.
const BOX_WASH_ALPHA: f32 = 0.06;
/// Width of the cursor's line.
const CURSOR_W: f32 = 1.0;
/// Width of the view box's outline.
const BOX_STROKE_W: f32 = 1.5;
/// Width of the groove between the overview and the timeline strip.
const GROOVE_W: f32 = 1.0;
/// Gap above and below the clip rows, inside the strip.
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
    /// pane, back to front: the background, the clips, the loop band, the
    /// cursor, the playhead, the view box, and the groove to the timeline strip under it.
    /// The track column's corner to its left is the column's
    /// (`draw_arranger_backgrounds` fills it).
    pub(super) fn draw_overview(&self, painter: &Painter, rect: Rect) {
        let top = rect.min.y + self.track_area_top();
        let bottom = top + OVERVIEW_H;
        let left = self.content_origin_x();
        let map = self.overview_map();
        let strip = Rect::from_min_max(pos2(left, top), pos2(rect.max.x, bottom));
        let painter = painter.with_clip_rect(strip.intersect(painter.clip_rect()));
        painter.rect_filled(strip, CornerRadius::ZERO, theme::bg_timeline());

        // Clips, one row per track, never under a pixel wide.
        let track_count = self.track_count();
        let rows_top = top + ROWS_INSET_Y;
        let rows_h = OVERVIEW_H - 2.0 * ROWS_INSET_Y;
        for shape in &self.render.clip_shapes {
            let track_idx = shape.track_idx();
            if track_idx >= track_count {
                continue;
            }
            let (row_top, row_bottom) = overview_row(track_idx, track_count, rows_top, rows_h);
            let x0 = map.x_of(shape.start_tick() as f32);
            let x1 = map.x_of(shape.end_tick() as f32).max(x0 + 1.0);
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

        // The loop band over the strip's height, dimmed as the timeline's is.
        let (region_start, region_end) = self.region_bounds_for_render();
        if region_end > region_start {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(map.x_of(region_start as f32), top),
                    pos2(map.x_of(region_end as f32), bottom),
                ),
                CornerRadius::ZERO,
                theme::region().gamma_multiply(LOOP_ALPHA * self.loop_dim()),
            );
        }

        // The cursor (where playback starts), in the lanes' cursor colour,
        // so you can find your way back to it.
        let cursor_x = map.x_of(self.render_cursor_tick() as f32).round();
        painter.vline(
            cursor_x,
            top..=bottom,
            Stroke::new(CURSOR_W, self.arranger_cursor_color()),
        );

        // The playhead, while the transport runs.
        if self.render_running()
            && let Some(playback) = self.render_playback_tick()
        {
            let x = map.x_of(playback as f32).round();
            painter.vline(x, top..=bottom, Stroke::new(1.0, theme::playhead()));
        }

        // The view box: a faint wash and an outline drawn inside it, held
        // inside the strip and above the groove at its bottom, which would
        // otherwise paint over the box's bottom edge.
        let (view_start, view_end) = self.arranger_view_ticks();
        let (box_left, box_right) = map.view_box(view_start, view_end);
        let view_box = Rect::from_min_max(
            pos2(box_left.round(), top),
            pos2(box_right.round(), bottom - GROOVE_W),
        );
        painter.rect_filled(
            view_box,
            CornerRadius::ZERO,
            theme::fg().gamma_multiply(BOX_WASH_ALPHA),
        );
        painter.rect_stroke(
            view_box,
            CornerRadius::ZERO,
            Stroke::new(BOX_STROKE_W, theme::fg_dim()),
            StrokeKind::Inside,
        );

        // The groove between the overview and the timeline strip.
        painter.hline(
            left..=strip.max.x,
            bottom - GROOVE_W * 0.5,
            Stroke::new(GROOVE_W, grid_seam_color()),
        );
    }
}
