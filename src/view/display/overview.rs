//! The arranger overview's pure geometry (`590-arranger-overview.md`): the
//! strip's height per pane, the span it maps across its width, the mapping
//! between ticks and strip x, the box around the view, the track rows, and
//! the drag's maths (where the view goes for a pointer, how far a vertical
//! move zooms). `rendering/overview.rs` draws it and `input/gestures.rs`
//! drives the drag; both read it through these, so drawing and hit-testing
//! can't drift.

use super::pointer_state::AxisFilter;
use crate::core::view_state::Pane;

/// Height of the arranger's overview strip, above its timeline strip. 18
/// at first; 24 since 2026-11-27, from the ruler's and the marker strip's
/// slack, so a project of nine tracks gets over 2px a row.
pub(super) const OVERVIEW_H: f32 = 24.0;

/// Pixels of vertical drag on the overview that double the zoom (down) or
/// halve it (up). Tuned by hand.
pub(super) const OVERVIEW_ZOOM_PX_PER_DOUBLING: f32 = 60.0;

/// The view box is never drawn narrower than this, so it can still be seen
/// and grabbed zoomed far in.
pub(super) const OVERVIEW_MIN_BOX_PX: f32 = 6.0;

/// How much of the pane's top the overview takes: `OVERVIEW_H` in the
/// arranger, nothing in the clip view (it has its home framing instead).
pub(super) fn overview_h(pane: Pane) -> f32 {
    match pane {
        Pane::Arranger => OVERVIEW_H,
        Pane::Clip => 0.0,
    }
}

/// The view's left tick for a drag that wants it at `start`, held inside the
/// overview (`0 ..= span_end`), so the box stops at both ends of the strip
/// rather than running past them. A view wider than the span (zoomed out
/// past the floor before the content shrank) sits at bar 1.
pub(super) fn overview_clamped_start(start: f32, view_ticks: f32, span_end: f32) -> f32 {
    start.min(span_end - view_ticks).max(0.0)
}

/// The overview's mapping between ticks (fractional, since the view's edges
/// are) and strip x: `0 ..= span_end` across `left ..= left + width`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OverviewMap {
    /// Strip x of tick 0.
    left: f32,
    /// The strip's width, never under a pixel.
    width: f32,
    /// The tick at the right edge, never under one.
    span_end: f32,
}

impl OverviewMap {
    /// The map of `0 ..= span_end` across the strip from `left` to `right`.
    pub(super) fn new(left: f32, right: f32, span_end: f32) -> Self {
        Self {
            left,
            width: (right - left).max(1.0),
            span_end: span_end.max(1.0),
        }
    }

    /// Strip x of `tick`.
    pub(super) fn x_of(&self, tick: f32) -> f32 {
        self.left + tick / self.span_end * self.width
    }

    /// The tick under strip x `x`, unclamped: a pointer past either end maps
    /// past the span, and the scroll clamp decides what that means.
    pub(super) fn tick_at(&self, x: f32) -> f32 {
        (x - self.left) / self.width * self.span_end
    }

    /// How many ticks `dx` points along the strip cover: a sideways drag
    /// moves the view by this.
    pub(super) fn ticks_for_px(&self, dx: f32) -> f32 {
        dx / self.width * self.span_end
    }

    /// The view box's x span for the view `view_start ..= view_end` (ticks),
    /// widened about its centre to `OVERVIEW_MIN_BOX_PX` when narrower, and
    /// held inside the strip: a wheel can scroll the view past the strip's
    /// end (its left edge may reach the last content tick), and the box then
    /// stays at the end. Drawing and the press's hit test both use it.
    pub(super) fn view_box(&self, view_start: f32, view_end: f32) -> (f32, f32) {
        let (mut left, mut right) = (self.x_of(view_start), self.x_of(view_end));
        if right - left < OVERVIEW_MIN_BOX_PX {
            let centre = (left + right) * 0.5;
            let half = OVERVIEW_MIN_BOX_PX * 0.5;
            (left, right) = (centre - half, centre + half);
        }
        let strip_right = self.left + self.width;
        let left = left.max(self.left).min(strip_right - OVERVIEW_MIN_BOX_PX);
        (left, right.min(strip_right).max(left + OVERVIEW_MIN_BOX_PX))
    }
}

/// The y span of `track_idx`'s row when the strip from `top`, `h` tall, is
/// split evenly between `track_count` tracks: never under a pixel, so with
/// many tracks rows touch or share one rather than vanish.
pub(super) fn overview_row(track_idx: usize, track_count: usize, top: f32, h: f32) -> (f32, f32) {
    let row_h = h / track_count.max(1) as f32;
    let row_top = top + track_idx as f32 * row_h;
    (row_top, row_top + row_h.max(1.0))
}

/// The zoom factor for a vertical drag of `dy` points: down (positive) zooms
/// in, up zooms out, `OVERVIEW_ZOOM_PX_PER_DOUBLING` a doubling.
pub(super) fn overview_zoom_factor(dy: f32) -> f32 {
    (dy / OVERVIEW_ZOOM_PX_PER_DOUBLING).exp2()
}

/// Where across the view (`0.0` its left edge, `1.0` its right) a press at
/// `pointer_tick` grabs it: the pointer's own place inside the box, or the
/// middle for a press outside it, which centres the view on the pointer.
pub(super) fn overview_grab_frac(
    pointer_tick: f32,
    (view_start, view_end): (f32, f32),
    in_box: bool,
) -> f32 {
    if !in_box {
        return 0.5;
    }
    let view_ticks = view_end - view_start;
    if view_ticks <= 0.0 {
        return 0.5;
    }
    ((pointer_tick - view_start) / view_ticks).clamp(0.0, 1.0)
}

/// The view's left tick that keeps `grab_frac` of a `view_ticks`-wide view
/// under `pointer_tick`, before the scroll clamp.
pub(super) fn overview_view_start(pointer_tick: f32, grab_frac: f32, view_ticks: f32) -> f32 {
    pointer_tick - grab_frac * view_ticks
}

/// An overview drag in progress: `Some` from a press on the strip to the
/// release. Moves the arranger's view only (`590-arranger-overview.md`).
/// Holds the (hidden) pointer and runs on raw motion, like the piano roll's
/// legend drag, so it keeps going past the window edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OverviewDrag {
    /// The tick a vertical move zooms about: where the press grabbed the
    /// view, moved only by the pans the view actually made, so every zoom
    /// step pivots on the same point, also after one held at bar 1 or the
    /// scroll clamp moved the view under it.
    pub(super) anchor_tick: f32,
    /// Passes only the axis the hand leads with (`AxisFilter::one_axis`), so
    /// a pan never zooms and a zoom never pans.
    pub(super) axes: AxisFilter,
    /// The press's y, where the pointer comes back on release.
    pub(super) press_y: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_arranger_has_an_overview() {
        assert_eq!(overview_h(Pane::Arranger), OVERVIEW_H);
        assert_eq!(overview_h(Pane::Clip), 0.0);
    }

    #[test]
    fn a_drag_holds_the_view_inside_the_strip() {
        assert_eq!(overview_clamped_start(-50.0, 100.0, 1000.0), 0.0);
        assert_eq!(overview_clamped_start(400.0, 100.0, 1000.0), 400.0);
        assert_eq!(overview_clamped_start(950.0, 100.0, 1000.0), 900.0);
        assert_eq!(overview_clamped_start(300.0, 2000.0, 1000.0), 0.0);
    }

    #[test]
    fn box_reaches_both_ends_of_the_strip() {
        let map = OverviewMap::new(0.0, 1000.0, 10_000.0);
        let start = overview_clamped_start(1e9, 2000.0, 10_000.0);
        assert_eq!(map.view_box(start, start + 2000.0), (800.0, 1000.0));
        let start = overview_clamped_start(-1e9, 2000.0, 10_000.0);
        assert_eq!(map.view_box(start, start + 2000.0), (0.0, 200.0));
    }

    #[test]
    fn the_full_zoom_out_fills_the_strip() {
        let map = OverviewMap::new(0.0, 1000.0, 10_000.0);
        assert_eq!(map.view_box(0.0, 10_000.0), (0.0, 1000.0));
    }

    #[test]
    fn empty_span_still_maps() {
        let map = OverviewMap::new(0.0, 1000.0, 0.0);
        assert!(map.x_of(5.0).is_finite());
    }

    #[test]
    fn map_round_trips() {
        let map = OverviewMap::new(100.0, 1100.0, 5000.0);
        assert_eq!(map.x_of(0.0), 100.0);
        assert_eq!(map.x_of(5000.0), 1100.0);
        assert_eq!(map.tick_at(600.0), 2500.0);
        for tick in [0.0, 123.0, 2500.0, 4999.0] {
            assert!((map.tick_at(map.x_of(tick)) - tick).abs() < 1e-3);
        }
    }

    #[test]
    fn map_survives_a_degenerate_strip() {
        let map = OverviewMap::new(100.0, 100.0, 0.0);
        assert!(map.x_of(10.0).is_finite());
        assert!(map.tick_at(150.0).is_finite());
    }

    #[test]
    fn a_strip_pixel_covers_its_share_of_the_span() {
        let map = OverviewMap::new(100.0, 1100.0, 5000.0);
        assert_eq!(map.ticks_for_px(10.0), 50.0);
        assert_eq!(map.ticks_for_px(-10.0), -50.0);
    }

    #[test]
    fn view_box_is_the_views_ticks() {
        let map = OverviewMap::new(0.0, 1000.0, 10_000.0);
        assert_eq!(map.view_box(2000.0, 4000.0), (200.0, 400.0));
    }

    #[test]
    fn view_box_stays_inside_the_strip() {
        let map = OverviewMap::new(0.0, 1000.0, 10_000.0);
        assert_eq!(map.view_box(9500.0, 12_000.0), (950.0, 1000.0));
        assert_eq!(map.view_box(20_000.0, 22_000.0), (994.0, 1000.0));
    }

    #[test]
    fn narrow_view_box_is_widened_about_its_centre() {
        let map = OverviewMap::new(0.0, 1000.0, 10_000.0);
        let (left, right) = map.view_box(5000.0, 5010.0);
        assert!((right - left - OVERVIEW_MIN_BOX_PX).abs() < 1e-4);
        assert!(((left + right) * 0.5 - 500.5).abs() < 1e-4);
    }

    #[test]
    fn rows_split_the_strip_evenly() {
        assert_eq!(overview_row(0, 2, 10.0, 18.0), (10.0, 19.0));
        assert_eq!(overview_row(1, 2, 10.0, 18.0), (19.0, 28.0));
    }

    #[test]
    fn rows_are_never_under_a_pixel() {
        let (top, bottom) = overview_row(40, 64, 0.0, 18.0);
        assert!(bottom - top >= 1.0);
        assert_eq!(overview_row(0, 0, 0.0, 18.0), (0.0, 18.0));
    }

    #[test]
    fn zoom_factor_doubles_down_and_halves_up() {
        assert_eq!(overview_zoom_factor(0.0), 1.0);
        assert!((overview_zoom_factor(OVERVIEW_ZOOM_PX_PER_DOUBLING) - 2.0).abs() < 1e-6);
        assert!((overview_zoom_factor(-OVERVIEW_ZOOM_PX_PER_DOUBLING) - 0.5).abs() < 1e-6);
        let (down, up) = (overview_zoom_factor(17.0), overview_zoom_factor(-17.0));
        assert!((down * up - 1.0).abs() < 1e-6);
    }

    #[test]
    fn grab_inside_the_box_keeps_the_pointers_place() {
        assert_eq!(overview_grab_frac(1500.0, (1000.0, 3000.0), true), 0.25);
        // Inside a box widened past the view, held to the view's edges.
        assert_eq!(overview_grab_frac(900.0, (1000.0, 1010.0), true), 0.0);
        assert_eq!(overview_grab_frac(1200.0, (1000.0, 1010.0), true), 1.0);
    }

    #[test]
    fn grab_outside_the_box_centres_the_view() {
        assert_eq!(overview_grab_frac(9000.0, (1000.0, 3000.0), false), 0.5);
        assert_eq!(overview_grab_frac(1000.0, (1000.0, 1000.0), true), 0.5);
    }

    #[test]
    fn view_start_keeps_the_grab_under_the_pointer() {
        assert_eq!(overview_view_start(5000.0, 0.25, 2000.0), 4500.0);
        assert_eq!(overview_view_start(5000.0, 0.5, 2000.0), 4000.0);
    }
}
