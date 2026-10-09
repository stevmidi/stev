//! Painting the timeline strip above the lanes: the bar-number ruler, the
//! loop-region band, and the arranger time-selection markers. The `REGION_BAND_H`
//! constant is shared with the piano roll. See `030-ui-design.md`.

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Shape, Stroke, pos2, vec2};

use crate::view::display::grid::GridRole;

use super::*;

/// Height of the region band along the bottom of the timeline strip. Also the
/// baseline offset for the bar-number labels — they sit directly on top of the
/// band, so the two share this constant and can't drift into each other.
pub(super) const REGION_BAND_H: f32 = 10.0;

impl Display {
    /// Paints the bar-number ruler into `rect`.
    pub(super) fn draw_timeline(&self, painter: &Painter, rect: Rect) {
        /// Ruler bar-tick width, and the arranger's in-lane bar-line width.
        const BAR_STROKE_W: f32 = 1.0;
        let scroll_x = self.render_scroll_x();

        let sw = rect.width();
        let area_top = rect.min.y + self.track_area_top();
        let area_bottom = rect.min.y + self.track_area_bottom();
        let strip_bottom = area_top + Self::TIMELINE_H;
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();
        let ppt = self.pixels_per_tick();
        // Walk the finest visible tier; each line is styled by the coarsest
        // tier it sits on (`GridTiers::role_at`). See `grid.rs`.
        let tiers = self.grid_tiers();
        let step_ticks = tiers.snap_ticks;
        let pixels_per_step: f32 = ppt * step_ticks as f32;
        let (first_visible, num_visible) =
            timeline_step_span(scroll_x, content_x, self.content_w(), pixels_per_step);

        // The strip background covers the content area only — the corner to
        // the left of `content_x` belongs to the left-gutter panel in both
        // views: the arranger's track column (`draw_arranger_backgrounds`) or
        // the piano roll's legend + key columns (`draw_note_lane_backgrounds`),
        // each filled up from the strip top so the column reads as one panel
        // top to bottom. Both fills run before `draw_timeline`, so the strip
        // just stops short and lets the panel own the corner.
        let strip_left = content_x;
        painter.rect_filled(
            Rect::from_min_size(
                pos2(strip_left, area_top),
                vec2(rect.min.x + sw - strip_left, Self::TIMELINE_H),
            ),
            CornerRadius::ZERO,
            theme::bg_timeline(),
        );

        // Bar color: slightly brighter than grid_major
        let bar_color = shifted_rgb(theme::grid_major(), 26);

        // In-lane vertical grid strategy (see `030-ui-design.md` § Grid
        // Hierarchy). Lines are styled by **role** — the tier they sit on
        // (`GridTiers::role_at`) — never by rung or view, so a bar line looks
        // like a bar line at every zoom. The tiers themselves come from
        // `grid_tiers`, adaptive in every view (bar / beat / 8th / 16th
        // zooming in, every 2, 4, 8 … bars zooming out) — the piano roll's
        // surface in the clip views shows its sub-beats sooner
        // (`GridSurface`). The settings modal, drawn over either, changes
        // neither.
        //
        // Every tier is the *same* groove — `grid_seam_color()`, a few steps
        // darker than the canvas — at a falling opacity
        // (`GridTiers::line_strength`: bar 1.0 — 0.8 in the arranger, where
        // it crosses the full-strength lane seams and would otherwise tile the
        // view into a chessboard — beat 0.5, finest 0.28). One
        // hue, one polarity: the old beat / 16th tones were derived from
        // `grid_major` and sat *lighter* than the lane while bars sat darker,
        // so the eye read two competing grids, and in the near-black themes
        // the 16ths came out the brightest lines on screen. Translucency also
        // lets a line blend with whatever it crosses — the selected-track
        // accent tint, the piano roll's key rows. The ruler ticks hanging off
        // the strip are a separate surface and keep the crisp `bar_color` /
        // `grid_major`, graduated by height rather than colour.
        let groove = grid_seam_color();
        let crosses_track_seams = self.active_pane() == Pane::Arranger;
        let line_color =
            |role: GridRole| groove.gamma_multiply(tiers.line_strength(role, crosses_track_seams));
        // The piano roll's grid usually has three vertical tiers
        // (bar/beat/16th) under its bars, so its bar line carries more weight
        // than the arranger's: 2px there, 1px in the arranger. Same groove
        // tone, width scaled to the job.
        let in_lane_bar_w = if self.active_pane() == Pane::Clip {
            2.0
        } else {
            BAR_STROKE_W
        };

        let tl_font = FontId::proportional(theme::FONT_SIZE_TL);
        let ticks_per_bar = self.meter().bar_ticks();

        for i in first_visible..(first_visible + num_visible) {
            let tick = i * step_ticks;
            let x = grid_line_x(i, step_ticks, ppt, content_x, scroll_x);
            if x < content_x - 1.0 || x > content_right + 1.0 {
                continue;
            }

            // Each line: the in-lane line down into the clip area, then its
            // ruler tick hanging off the strip. The bar tick spans the full
            // region band (`REGION_BAND_H`) so its top meets the bar-number
            // baseline, reading as one graduation-and-label unit; the beat
            // tick is half that; the snap tick shortest of the three, but a
            // real graduation, not a near-dot — `grid_major` like the beat
            // tick, height carrying the hierarchy. Beat lines are the bar
            // groove at beat strength (or the snap strength when nothing
            // finer sits under them).
            let role = tiers.role_at(tick);
            let (lane_w, tick_h, tick_color) = match role {
                GridRole::Bar => (in_lane_bar_w, REGION_BAND_H, bar_color),
                GridRole::Beat => (1.0_f32, REGION_BAND_H * 0.5, theme::grid_major()),
                GridRole::Snap => (1.0_f32, REGION_BAND_H * 0.35, theme::grid_major()),
            };
            painter.vline(
                x,
                strip_bottom..=area_bottom,
                Stroke::new(lane_w, line_color(role)),
            );
            // The ruler is a readout, not the snap grid: fine snap lines keep
            // their in-lane groove but only every `ruler_ticks` gets a tick.
            if tiers.has_ruler_tick(tick) {
                painter.vline(
                    x,
                    (strip_bottom - tick_h)..=strip_bottom,
                    Stroke::new(BAR_STROKE_W, tick_color),
                );
            }

            // Bar number label — baseline sits on the region band's top edge
            // (shared `REGION_BAND_H`) so the two never overlap. Thinned to
            // `label_ticks` so numbers never collide zoomed out.
            let bar_num = tick / ticks_per_bar + 1;
            if role == GridRole::Bar && bar_num > 0 && tiers.is_labelled(tick) {
                painter.text(
                    pos2(x + 3.0, strip_bottom - REGION_BAND_H),
                    Align2::LEFT_BOTTOM,
                    bar_num,
                    tl_font.clone(),
                    theme::fg_dim(),
                );
            }
        }

        // Seam between the timeline strip and what's below it — `grid_seam_color()`
        // groove tone like every other horizontal divider, and the same width
        // as the performance-lane and track-to-track seams: in the arranger
        // this line is the performance lane's top edge, and a thinner one left
        // that row with a lighter top border than its bottom and than every
        // track. `ARRANGER_LANE_SEAM_W` in the arranger; 2px in the clip views,
        // matching their 2px octave dividers.
        let (strip_seam_y, strip_seam_w) = if self.active_pane() == Pane::Clip {
            (strip_bottom, 2.0_f32)
        } else {
            (
                seam_y(
                    strip_bottom,
                    ARRANGER_LANE_SEAM_W,
                    painter.pixels_per_point(),
                ),
                ARRANGER_LANE_SEAM_W,
            )
        };
        painter.hline(
            rect.min.x..=(rect.min.x + sw),
            strip_seam_y,
            Stroke::new(strip_seam_w, grid_seam_color()),
        );
    }

    /// Paints the loop-region band along the bottom of the timeline strip.
    pub(super) fn draw_region(&self, painter: &Painter, rect: Rect) {
        // The band marks the loop / cycle span along the timeline strip. It
        // earns its place in the arranger (the loop region is edited there),
        // but in `Clip` the region just
        // spans the clip being edited — the band stretches the whole ruler and
        // says nothing — so skip it there. Modal states (`Project` etc.) layer
        // over the arranger and keep it.
        if self.active_pane() == Pane::Clip {
            return;
        }

        let area_top = rect.min.y + self.track_area_top();
        let (region_start, region_end) = self.region_bounds_for_render();
        let ry = area_top + Self::TIMELINE_H; // strip bottom — sits on the groove seam
        let band_top = ry - REGION_BAND_H;
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        // Map each bound through the exact round-to-pixel the ruler's bar ticks
        // (`draw_timeline`) and the playhead use, so a bound sitting on a bar
        // lands *on* that bar tick. The earlier right edge was `start_x +
        // round(width)`: two independent roundings that disagree with
        // `round(absolute position)` by 1px whenever both fractional parts sit
        // near .5 — on those bars the edge line drifted a pixel off the bar tick
        // bleeding through the translucent band, doubling it into a blurred
        // "gutter". `region_bound_x` mirrors `tick_to_screen_x` exactly.
        let start_x = self.tick_to_screen_x(region_start);
        let end_x = self.tick_to_screen_x(region_end).max(start_x + 1.0);

        // Scrolled fully past the region (either side): nothing to paint. The
        // band is otherwise clamped to the content area so it never bleeds into
        // the track-header gutter.
        let Some((rx, right)) = region_band_span(start_x, end_x, content_x, content_right) else {
            return;
        };
        let rw = right - rx;

        let region = theme::region();
        // When looping is disabled the region still marks the playback bounds
        // but no longer wraps playback, so everything dims to signal that.
        let loop_dim = if self.is_loop_enabled() { 1.0 } else { 0.4 };

        // Body — shallow translucent band, flush down to the strip-bottom
        // groove. Fill colour/alpha unchanged; it's the edges that were
        // reworked.
        painter.rect_filled(
            Rect::from_min_size(pos2(rx, band_top), vec2(rw, REGION_BAND_H)),
            CornerRadius::ZERO,
            region.gamma_multiply(0.26 * loop_dim),
        );

        // Edges — a single crisp 1px vertical line at each region bound, one
        // alpha (the old band had a near-invisible 0.18 top line, a 0.45 bottom
        // line and 0.75 handle bars, three weights fighting each other). No top
        // or bottom line: the fill marks the span, the strip-bottom groove
        // closes it underneath, and the bounds are what matter. Each edge draws
        // only where that bound is actually on-screen.
        let edge = Stroke::new(1.0_f32, region.gamma_multiply(0.6 * loop_dim));
        if self.content_x_on_screen(start_x) {
            painter.vline(start_x, band_top..=ry, edge);
        }
        if self.content_x_on_screen(end_x) {
            painter.vline(end_x, band_top..=ry, edge);
        }
    }

    /// Draws the time-position markers in the timeline strip.
    ///
    /// With no range selected this marks the cursor with a single right-pointing
    /// "play" triangle (straight edge on the cursor tick, opening rightward) —
    /// derived from the cursor rather than stored, so it follows every cursor
    /// move (arrow keys, paging, clicks) without any syncing. The cursor is
    /// where playback starts, so this collapsed marker is drawn in the clip
    /// pane as well, matching the arranger. A range (arranger-only — the clip
    /// pane ignores `time_selection`, which stays alive under a docked panel
    /// while the arranger has the focus) reads as two arrows
    /// pointing inward from its bounds, with each arrow's flat vertical edge
    /// sitting exactly on its snapped boundary tick. Both states
    /// are bottom-anchored flush on the strip-bottom groove seam, capping the
    /// top of the in-lane playhead line that descends from the same point:
    /// their vertical edges span the full `REGION_BAND_H` — exactly the bar
    /// tick's and loop-region band's height — so every marker in the strip
    /// reads at the same scale regardless of which state is showing.
    pub(super) fn draw_time_selection(&self, painter: &Painter, rect: Rect) {
        /// Height of the marker triangles — the region band's height, shared
        /// with the bar ticks and loop-region band so every timeline marker
        /// reads at the same scale.
        const MARKER_H: f32 = REGION_BAND_H;
        /// Horizontal reach of a marker triangle from its boundary tick.
        const MARKER_W: f32 = 7.0;

        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();
        // Bottom-anchored flush on the strip-bottom groove seam, where the
        // in-lane playhead line descends from and the region band's bottom
        // edge sits — every marker below shares this baseline.
        let base = rect.min.y + self.track_area_top() + Self::TIMELINE_H;
        let top = base - MARKER_H;
        let mid_y = base - MARKER_H * 0.5;
        let accent = theme::accent();
        // One marker: its flat vertical edge on `edge_x`, its tip at `tip_x`.
        let marker = |edge_x: f32, tip_x: f32| {
            painter.add(Shape::convex_polygon(
                vec![pos2(edge_x, top), pos2(edge_x, base), pos2(tip_x, mid_y)],
                accent,
                Stroke::NONE,
            ));
        };

        // Collapsed: a right-pointing "play" triangle marking the cursor. Its
        // straight vertical edge sits on the cursor tick and the glyph opens
        // rightward, the direction playback runs from here. A zero-tick-width
        // selection (a purely vertical marquee drag's point-in-time track
        // selection) has no range to draw a rail/arrows for, so it takes this
        // same collapsed branch — see `TimeSelectionRect::has_tick_range`.
        let Some(TimeSelectionRect {
            start: selection_start,
            end: selection_end,
            ..
        }) = self
            .gesture
            .time_selection
            .filter(|sel| self.active_pane() == Pane::Arranger && sel.has_tick_range())
        else {
            let cursor_x = self.tick_to_screen_x(self.render_cursor_tick());
            if self.content_x_on_screen(cursor_x) {
                marker(cursor_x, cursor_x + MARKER_W);
            }
            return;
        };

        let start_x = self.tick_to_screen_x(selection_start);
        let end_x = self.tick_to_screen_x(selection_end);
        if end_x < content_x || start_x > content_right {
            return;
        }

        // Narrow ranges shrink the arrows so they meet rather than cross
        let reach = MARKER_W.min((end_x - start_x) * 0.5);

        // Start arrow, pointing right into the range
        if self.content_x_on_screen(start_x) {
            marker(start_x, start_x + reach);
        }

        // End arrow, pointing left into the range
        if self.content_x_on_screen(end_x) {
            marker(end_x, end_x - reach);
        }
    }
}

/// The half-open step range `[first, first + count)` the timeline grid loop must
/// walk so every gridline landing in `[content_x, content_x + content_w]` is
/// drawn. Step `i` sits at screen `x = pixels_per_step * i + content_x -
/// scroll_x`, so `content_x` shifts the whole grid right by `content_x /
/// pixels_per_step` steps: the count has to span `content_w + content_x`, not
/// just `content_w`, or the last ~`content_x` px go undrawn (a dead band once
/// the track header widened `content_x` from a 10px gutter to `TRACK_HEADER_W`).
fn timeline_step_span(
    scroll_x: f32,
    content_x: f32,
    content_w: f32,
    pixels_per_step: f32,
) -> (i32, i32) {
    let first = ((scroll_x - content_x) / pixels_per_step).floor() as i32;
    let count = ((content_w + content_x) / pixels_per_step).ceil() as i32 + 2;
    (first, count)
}

/// Screen-x of a region bound at `tick`, rounded to the pixel grid with the
/// same formula as the ruler's bar ticks (`draw_timeline`), the playhead and
/// `Display::tick_to_screen_x`. Keeping every time-anchored line on one rounding
/// is what lets a region bound sit exactly on its bar tick instead of ~1px
/// beside it — see `draw_region`.
fn region_bound_x(tick: i32, ppt: f32, content_origin_x: f32, scroll_x: f32) -> f32 {
    (tick as f32 * ppt + content_origin_x - scroll_x).round()
}

/// Screen x of grid line `step`, `ticks_per_step` apart — deliberately
/// [`region_bound_x`] of its absolute tick rather than `pixels_per_step *
/// step`. The two agree at the default arranger scale but round to different
/// pixels at arbitrary zooms (e.g. 3.3 px/beat), which would put a region
/// bound 1px off its own bar tick; one formula keeps them welded at any scale.
fn grid_line_x(step: i32, ticks_per_step: i32, ppt: f32, content_x: f32, scroll_x: f32) -> f32 {
    region_bound_x(step * ticks_per_step, ppt, content_x, scroll_x)
}

/// Screen-x `[x0, x1]` the region band should paint, clamped to the content
/// area `[content_x, content_right]`, or `None` when the region is scrolled
/// entirely out of view. Keeps a scrolled-away region from bleeding its band
/// and edge lines into the track-header gutter (the handles are gated
/// separately on their own edge being on-screen).
fn region_band_span(
    start_x: f32,
    end_x: f32,
    content_x: f32,
    content_right: f32,
) -> Option<(f32, f32)> {
    if end_x < content_x || start_x > content_right {
        return None;
    }
    let x0 = start_x.max(content_x);
    let x1 = end_x.min(content_right).max(x0 + 1.0);
    Some((x0, x1))
}

#[cfg(test)]
mod tests {
    use super::{grid_line_x, region_band_span, region_bound_x, timeline_step_span};
    use crate::core::time::{self, Meter};

    const CONTENT_X: f32 = 118.0;

    /// Pixels-per-tick at arranger zoom (~32 bars across the content width).
    fn arranger_ppt() -> f32 {
        (1274.0 / 128.0) / Meter::FOUR_FOUR.bar_ticks() as f32
    }

    /// The rounded screen-x of the ruler's bar tick for a 0-based `bar`, via
    /// the same `grid_line_x` call `draw_timeline`'s grid loop makes.
    fn bar_tick_x(bar: i32, ppt: f32, scroll_x: f32) -> f32 {
        let step = bar * 4;
        grid_line_x(step, time::beats_to_ticks(1.0), ppt, CONTENT_X, scroll_x)
    }

    /// The grid loop's *old* formula (`pixels_per_step * step`), kept to
    /// prove the zoom-sweep regression test below isn't vacuous.
    fn old_bar_tick_x(bar: i32, ppt: f32, scroll_x: f32) -> f32 {
        let pixels_per_step = ppt * time::beats_to_ticks(1.0) as f32;
        let step = bar * 4;
        (pixels_per_step * step as f32 + CONTENT_X - scroll_x).round()
    }

    #[test]
    fn region_bound_sits_exactly_on_its_bar_tick_at_every_scroll() {
        let ppt = arranger_ppt();
        let ticks_per_bar = Meter::FOUR_FOUR.bar_ticks();
        // Bars 5, 10, 13, 18 (0-based 4, 9, 12, 17) were the reported blurry
        // ones; sweep sub-pixel scroll offsets so the fractional parts land all
        // over [0, 1).
        for bar in [1, 2, 4, 5, 9, 12, 17, 24, 31] {
            for k in 0..20 {
                let scroll_x = k as f32 * 0.37;
                let bound = region_bound_x(bar * ticks_per_bar, ppt, CONTENT_X, scroll_x);
                let tick = bar_tick_x(bar, ppt, scroll_x);
                assert_eq!(
                    bound, tick,
                    "bar {bar}, scroll {scroll_x}: region bound {bound} vs bar tick {tick}"
                );
            }
        }
    }

    #[test]
    fn region_bound_sits_exactly_on_its_bar_tick_at_every_zoom() {
        // The arranger zoom only changes the `ppt` fed to the one formula, so
        // the bound and the ruler tick must agree at any scale, not just the
        // default — sweep the zoom range (px per beat) with sub-pixel scrolls.
        let ticks_per_bar = Meter::FOUR_FOUR.bar_ticks();
        let mut old_formula_drifted = false;
        for px_per_beat in [0.5, 0.8, 3.3, 9.953_125, 12.44, 47.1, 160.0] {
            let ppt = time::px_per_beat_to_ppt(px_per_beat);
            for bar in [1, 2, 5, 13, 31, 97] {
                for k in 0..20 {
                    let scroll_x = k as f32 * 0.37;
                    let bound = region_bound_x(bar * ticks_per_bar, ppt, CONTENT_X, scroll_x);
                    assert_eq!(
                        bound,
                        bar_tick_x(bar, ppt, scroll_x),
                        "px/beat {px_per_beat}, bar {bar}, scroll {scroll_x}"
                    );
                    old_formula_drifted |= bound != old_bar_tick_x(bar, ppt, scroll_x);
                }
            }
        }
        // 3.3 px/beat, bar 31, scroll 3.7 is one known case.
        assert!(
            old_formula_drifted,
            "old grid formula never drifted — test is vacuous"
        );
    }

    #[test]
    fn split_rounding_drifts_off_the_bar_tick_where_the_fix_does_not() {
        // The old right-edge formula was `start_x + round(width)`: two roundings
        // that disagree with `round(absolute)` by 1px on some bar/scroll combos.
        // Prove it actually drifts (or this regression test is vacuous) and that
        // `region_bound_x` never does.
        let ppt = arranger_ppt();
        let ticks_per_bar = Meter::FOUR_FOUR.bar_ticks();
        let mut saw_divergence = false;
        for bar in 1..40 {
            for k in 0..50 {
                let scroll_x = k as f32 * 0.31;
                let start_x = region_bound_x(0, ppt, CONTENT_X, scroll_x);
                let old_end = start_x + ((bar * ticks_per_bar) as f32 * ppt).round();
                let tick = bar_tick_x(bar, ppt, scroll_x);
                if old_end != tick {
                    saw_divergence = true;
                }
                assert_eq!(
                    region_bound_x(bar * ticks_per_bar, ppt, CONTENT_X, scroll_x),
                    tick
                );
            }
        }
        assert!(
            saw_divergence,
            "old split rounding never drifted — test is vacuous"
        );
    }

    #[test]
    fn region_band_clamps_to_the_content_area() {
        // Start scrolled off the left edge: band starts at content_x, no start handle.
        assert_eq!(
            region_band_span(50.0, 400.0, 118.0, 1000.0),
            Some((118.0, 400.0))
        );
        // End scrolled off the right edge: band ends at content_right.
        assert_eq!(
            region_band_span(200.0, 1200.0, 118.0, 1000.0),
            Some((200.0, 1000.0))
        );
        // Fully visible: unchanged.
        assert_eq!(
            region_band_span(200.0, 400.0, 118.0, 1000.0),
            Some((200.0, 400.0))
        );
    }

    #[test]
    fn region_band_hidden_when_scrolled_past() {
        assert_eq!(region_band_span(-500.0, -10.0, 118.0, 1000.0), None);
        assert_eq!(region_band_span(1100.0, 1400.0, 118.0, 1000.0), None);
    }

    /// For every step in the returned range, does its line fall left of the
    /// content area, and does the range reach past the right edge?
    fn covers_right_edge(scroll_x: f32, content_x: f32, content_w: f32, pps: f32) -> bool {
        let (first, count) = timeline_step_span(scroll_x, content_x, content_w, pps);
        let x = |i: i32| pps * i as f32 + content_x - scroll_x;
        let content_right = content_x + content_w;
        // The last step walked must sit at or past the right edge...
        x(first + count - 1) >= content_right
            // ...and the first step walked must sit at or left of the left edge,
            // so no visible line before it is skipped.
            && x(first) <= content_x
    }

    #[test]
    fn range_reaches_the_right_edge_for_a_wide_left_gutter() {
        // TRACK_HEADER_W-sized gutter, arranger zoom (~32 bars across ~1300px).
        assert!(covers_right_edge(0.0, 118.0, 1274.0, 1274.0 / 128.0));
    }

    #[test]
    fn range_reaches_the_right_edge_for_the_old_thin_gutter() {
        assert!(covers_right_edge(0.0, 10.0, 1382.0, 1382.0 / 128.0));
    }

    #[test]
    fn range_reaches_the_right_edge_when_scrolled() {
        let pps = 1274.0 / 128.0;
        assert!(covers_right_edge(37.0 * pps, 118.0, 1274.0, pps));
        assert!(covers_right_edge(1000.5, 118.0, 1274.0, pps));
    }
}
