//! `Display`'s horizontal scroll: the arranger's and the clip view's (the
//! piano roll's vertical scroll is `NoteAreaGeom`'s, `rendering/piano_keys.rs`).
//! `sync_arranger_scroll` pages the arranger
//! toward the cursor (unless a manual trackpad scroll suspended it);
//! `sync_clip_scroll` only keeps the clip view valid — it never zooms or
//! scrolls on the user's behalf, and edits leave it still
//! (`220-capture-without-pending-view.md`). See `030-ui-design.md`,
//! `archive/200-clip-view-zoom.md`.

use crate::core::time::px_per_beat_to_ppt;
use crate::models::clip::reach_over;

use super::zoom::clip_zoom_state;
use super::*;

impl Display {
    // --- Scrolling and navigation ---
    /// The clip view's scrollable span in event ticks: the lead clip's reach
    /// (`reach_over` — the same rule the clip cursor is held to, so the view
    /// can always show where the cursor can go) over its note shapes, for
    /// the window `region`. Material a trim or a stopped capture commit left
    /// outside the window can be scrolled to, and the end pushed later with
    /// `]` (`220-capture-without-pending-view.md`). The zoom-out floor stays
    /// the window plus its headroom.
    pub(super) fn clip_reach_of(&self, region: (i32, i32)) -> (i32, i32) {
        reach_over(
            self.meter(),
            region,
            self.render
                .event_shapes
                .iter()
                .map(|shape| (shape.start_tick(), shape.end_tick())),
        )
    }

    /// The clip view's home framing: the span frozen when the clip was opened
    /// (`RenderState::clip_home_span`), or the clip's current reach before
    /// that.
    pub(in crate::view::display) fn clip_home_span(&self) -> (i32, i32) {
        self.render
            .clip_home_span
            .unwrap_or_else(|| self.clip_reach_of(self.region_bounds_for_render()))
    }

    /// Where the clip view can scroll, in event ticks: the home framing and
    /// the clip's current reach together, so an edit that shortens the reach
    /// can never shrink the range under the view and move it.
    pub(super) fn clip_scroll_span(&self, region: (i32, i32)) -> (i32, i32) {
        let (home_start, home_end) = self.clip_home_span();
        let (reach_start, reach_end) = self.clip_reach_of(region);
        (home_start.min(reach_start), home_end.max(reach_end))
    }

    /// The clip view's scroll clamp range at `ppt`, in pixels:
    /// `clip_scroll_span` for the window `region`, via `clip_scroll_range`.
    pub(super) fn clip_scroll_bounds(&self, region: (i32, i32), ppt: f32) -> (f32, f32) {
        let (span_start, span_end) = self.clip_scroll_span(region);
        clip_scroll_range(span_start, span_end, ppt, self.content_w())
    }

    /// Frames a freshly opened clip (`ClipEntered`, `LeadClipChanged`): its
    /// whole reach across the width and its note range down the height
    /// (`latch_clip_home_notes`), frozen as the home framing until the user
    /// zooms, scrolls or opens another clip. The event shapes must already
    /// be rebuilt.
    pub(super) fn frame_clip_home(&mut self) {
        self.latch_clip_home_notes();
        let span = self.clip_reach_of(self.region_bounds_snapshot());
        self.render.clip_home_span = Some(span);
        let ppt = px_per_beat_to_ppt(self.clip_px_per_beat());
        self.set_clip_scroll(span.0 as f32 * ppt, ppt);
    }

    /// Sets the clip view's scroll, worked out at scale `ppt`, and records
    /// that scale as the one it was set at. Every write that comes with a
    /// scale change goes through here: without the record, the next frame's
    /// `sync_clip_scroll` takes the change for a resize and rescales the
    /// scroll a second time — which is how a zoom used to drift the content
    /// left, further with every step in.
    pub(super) fn set_clip_scroll(&mut self, scroll_x: f32, ppt: f32) {
        self.render.clip_scroll_x = scroll_x;
        self.render.clip_last_ppt = Some(ppt);
    }

    /// Captures one clip-pane snapshot per frame (`clip_frame_*`), so the draw
    /// pass is internally coherent, and keeps the clip view's scroll valid.
    pub(in crate::view::display) fn sync_clip_frame(&mut self) {
        if self.is_pane_visible(Pane::Clip) {
            let frame_region_bounds = self.region_bounds_snapshot();
            // `running` before the playhead — see `clip_frame_running`.
            let frame_running = self.running.load(Ordering::Relaxed);
            let frame_playback_tick = self.pane_playback_tick();
            let frame_cursor_tick = self.cursor_tick_atomic().load(Ordering::Relaxed);

            self.render.clip_frame_region_bounds = Some(frame_region_bounds);
            self.render.clip_frame_running = Some(frame_running);
            self.render.clip_frame_playback_tick = frame_playback_tick;
            self.render.clip_frame_cursor_tick = Some(frame_cursor_tick);

            self.sync_clip_scroll();

            self.render.clip_frame_scroll_x = Some(self.render.clip_scroll_x);
        } else {
            self.reset_clip_frame_snapshots();
        }
    }

    /// Furthest-right tick the arranger holds real material at: the latest clip
    /// end, the cursor, the region end, or the time selection's end (a marquee
    /// dragged past the last clip is still something the user is looking at —
    /// without it, `Z` on such a selection was clamped off-centre). Drives the
    /// right-hand scroll clamp and the content-relative zoom-out floor.
    pub(super) fn arranger_last_content_tick(&self) -> i32 {
        let (_, region_end) = self.region_bounds_snapshot();
        let last_clip_end = self
            .render
            .clip_shapes
            .iter()
            .map(|shape| shape.end_tick())
            .max()
            .unwrap_or(0);
        let selection_end = self.gesture.time_selection.map_or(0, |rect| rect.end);
        last_clip_end
            .max(self.cursor_tick.load(Ordering::Relaxed))
            .max(region_end)
            .max(selection_end)
    }

    /// Applies a trackpad/wheel scroll to the pane under the pointer
    /// (`pointer_y`), or the focused one — see `scroll_arranger_by` /
    /// `scroll_clip_by`, and `scroll_note_area_by` / `scroll_arranger_lanes_by`
    /// for the vertical axis.
    pub(in crate::view::display) fn scroll_timeline_by(
        &mut self,
        delta_x: f32,
        delta_y: f32,
        pointer_y: Option<f32>,
    ) {
        let pane = pointer_y
            .and_then(|y| self.pane_rects().pane_at_y(y))
            .unwrap_or_else(|| self.focused_pane());
        match pane {
            Pane::Arranger => self.in_pane(pane, |d| {
                d.scroll_arranger_by(delta_x);
                d.scroll_arranger_lanes_by(delta_y);
            }),
            Pane::Clip => self.in_pane(pane, |d| {
                d.scroll_clip_by(delta_x);
                d.scroll_note_area_by(delta_y);
            }),
        }
    }

    /// Pans the clip view by a trackpad/wheel delta, within
    /// `clip_scroll_span`. Does nothing visible while the whole span is on
    /// screen — the range is a single offset then — and nothing outside the
    /// clip view.
    fn scroll_clip_by(&mut self, delta_x: f32) {
        if !self.is_pane_visible(Pane::Clip) || delta_x == 0.0 {
            return;
        }
        let ppt = px_per_beat_to_ppt(self.clip_px_per_beat());
        let (min_scroll_x, max_scroll_x) =
            self.clip_scroll_bounds(self.region_bounds_snapshot(), ppt);
        self.render.clip_scroll_x =
            (self.render.clip_scroll_x - delta_x).clamp(min_scroll_x, max_scroll_x);
    }

    /// Per-frame clip-view scroll, run before the frame's scroll snapshot. The
    /// app never zooms or scrolls the clip view on the user's behalf
    /// (`220-capture-without-pending-view.md`): no cursor-follow, no re-fit
    /// after an edit. This only keeps the view valid — a zoom back at the
    /// home scale becomes home again (`clip_zoom_state`), a scale that changed
    /// without the user (a window resize at home) keeps the same tick at the
    /// left edge, and the scroll stays within `clip_scroll_span`, which edits
    /// can't shrink.
    fn sync_clip_scroll(&mut self) {
        self.render.clip_px_per_beat = self.render.clip_px_per_beat.and_then(|px_per_beat| {
            clip_zoom_state(px_per_beat, 0.0, self.clip_home_px_per_beat())
        });

        let ppt = px_per_beat_to_ppt(self.clip_px_per_beat());
        self.render.clip_scroll_x =
            rescaled_clip_scroll_x(self.render.clip_scroll_x, self.render.clip_last_ppt, ppt);
        self.render.clip_last_ppt = Some(ppt);

        let (min_scroll_x, max_scroll_x) =
            self.clip_scroll_bounds(self.region_bounds_for_render(), ppt);
        self.render.clip_scroll_x = self.render.clip_scroll_x.clamp(min_scroll_x, max_scroll_x);
    }

    /// Applies a trackpad/wheel horizontal scroll to the arranger. Suspends
    /// cursor-follow (see `arranger_follow_suspended`) so the view holds where
    /// the user put it — even with the cursor off-screen — until the cursor
    /// next moves. `delta_x` is egui's smoothed scroll delta in points;
    /// positive scrolls toward bar 1 (already OS natural-scroll aware).
    pub(in crate::view::display) fn scroll_arranger_by(&mut self, delta_x: f32) {
        if !self.is_pane_visible(Pane::Arranger) || delta_x == 0.0 {
            return;
        }
        self.render.arranger_follow_suspended = true;
        let max_scroll_x =
            arranger_max_scroll_x(self.arranger_last_content_tick(), self.pixels_per_tick());
        self.render.arranger_scroll_x =
            scrolled_offset(self.render.arranger_scroll_x, delta_x, max_scroll_x);
    }

    /// Keeps the arranger view offset in step with the transport cursor: pages
    /// `scroll_x` in steps of two structural grid units (`GridTiers::bar_ticks`
    /// — 2 bars at the default zoom, 2 × 4 bars zoomed far out) whenever the
    /// cursor moves past a viewport edge, and clamps it into the valid range
    /// (`0.0 ..= arranger_max_scroll_x`). A trackpad/wheel scroll
    /// (`scroll_arranger_by`) suspends the paging until the cursor next moves,
    /// so the view can be parked anywhere; the clamp still applies so material
    /// shrinking (clips deleted, region shortened) can't strand it. A zoom
    /// (`zoom_arranger_by`) suspends the paging the same way. Also latches the
    /// default zoom on the first arranger frame (`latch_arranger_zoom`).
    pub(in crate::view::display) fn sync_arranger_scroll(&mut self) {
        if !self.is_pane_visible(Pane::Arranger) {
            self.rearm_arranger_follow();
            return;
        }
        self.latch_arranger_zoom();

        let cursor_tick = self.cursor_tick.load(Ordering::Relaxed);
        if self.render.arranger_follow_last_cursor_tick != Some(cursor_tick) {
            self.render.arranger_follow_last_cursor_tick = Some(cursor_tick);
            self.render.arranger_follow_suspended = false;
        }

        let ppt = self.pixels_per_tick();
        let max_scroll_x = arranger_max_scroll_x(self.arranger_last_content_tick(), ppt);

        self.render.arranger_scroll_x = if self.render.arranger_follow_suspended {
            self.render.arranger_scroll_x.clamp(0.0, max_scroll_x)
        } else {
            followed_scroll_x(
                self.render.arranger_scroll_x,
                cursor_tick as f32,
                ppt,
                self.content_w(),
                (0.0, max_scroll_x),
                2 * self.grid_tiers().bar_ticks,
            )
        };
    }

    /// Collapses the time selection whenever the cursor moves or the selected
    /// track changes.
    ///
    /// A time selection is anchored to the cursor and to `selected_track_idx`,
    /// so any move of either — arrow keys, viewport paging, a click, a seek,
    /// ↑/↓ track navigation — invalidates it, the same way clicking elsewhere
    /// does in Ableton. Watching both here rather than enumerating the keys
    /// that move them in `forward_input_event` keeps every present and future
    /// path covered by one rule, and keeps the collapsed glyph (and the
    /// selected-track cursor accent, hidden while a real marquee is active —
    /// see `draw_selected_track_cursor`) in step with the cursor line and
    /// selected track.
    pub(in crate::view::display) fn sync_time_selection_to_cursor(&mut self) {
        let cursor_tick = self.cursor_tick.load(Ordering::Relaxed);
        let cursor_moved = self
            .gesture
            .last_cursor_tick
            .is_some_and(|last_tick| last_tick != cursor_tick);
        self.gesture.last_cursor_tick = Some(cursor_tick);

        let track_changed = self
            .gesture
            .last_selected_track_idx
            .is_some_and(|last_idx| last_idx != self.selected_track_idx);
        self.gesture.last_selected_track_idx = Some(self.selected_track_idx);

        // A drag sets the cursor/track at its own anchor, so the move it
        // causes must not wipe the range that same drag is building. This
        // also shields against `TrackSelected` arriving a frame or two late
        // (it round-trips through the sequencer thread) for a click that
        // both starts a drag and reselects its track: the drag's anchor is
        // still set when it lands, so it's ignored here instead of wiping
        // the selection mid-drag.
        if (cursor_moved || track_changed) && self.gesture.time_selection_anchor.is_none() {
            self.gesture.time_selection = None;
        }
    }
}

/// Upper bound for arranger `scroll_x`, in pixels: `last_content_tick` (the
/// furthest-right of the last clip end, cursor, and region end) placed at the
/// left content edge, i.e. one viewport of empty space past the arrangement.
pub(super) fn arranger_max_scroll_x(last_content_tick: i32, ppt: f32) -> f32 {
    (last_content_tick.max(0) as f32 * ppt).max(0.0)
}

/// Pure core of a trackpad/wheel scroll gesture on either axis: apply
/// `delta` points to `scroll` and clamp into `0.0..=max_scroll`. A positive
/// `delta` (content moving right / down) scrolls toward bar 1 / track 1.
pub(in crate::view::display) fn scrolled_offset(scroll: f32, delta: f32, max_scroll: f32) -> f32 {
    (scroll - delta).clamp(0.0, max_scroll.max(0.0))
}

/// The clip scroll after a scale change the user didn't make (`last_ppt` →
/// `ppt`, e.g. a window resize at the home framing): the same tick stays at
/// the left edge. Unchanged when the scale hasn't moved or there is no
/// previous scale.
pub(super) fn rescaled_clip_scroll_x(scroll_x: f32, last_ppt: Option<f32>, ppt: f32) -> f32 {
    match last_ppt {
        Some(last_ppt) if last_ppt > 0.0 && last_ppt != ppt => scroll_x * ppt / last_ppt,
        _ => scroll_x,
    }
}

/// Scroll range of a clip view, in pixels: from `reach_start` at the left
/// content edge to `reach_end` at the right one (the window, or the clip's
/// reach — `clip_reach`). Collapses to the start when that span is narrower
/// than the viewport.
pub(super) fn clip_scroll_range(
    region_start: i32,
    region_end: i32,
    ppt: f32,
    viewport_w: f32,
) -> (f32, f32) {
    let min_scroll_x = region_start as f32 * ppt;
    let max_scroll_x = (region_end as f32 * ppt - viewport_w).max(min_scroll_x);
    (min_scroll_x, max_scroll_x)
}

/// Pure core of the arranger's cursor-follow paging: step `scroll_x` by
/// `page_ticks` at a time until `cursor_tick` sits within
/// `[scroll_x, scroll_x + viewport_w]`, then clamp into
/// `min_scroll_x..=max_scroll_x` (the lower bound is bar 1). The caller passes
/// two structural grid units, so an unclamped page always lands on a visible
/// bar line. The clip view doesn't follow (`220`).
fn followed_scroll_x(
    mut scroll_x: f32,
    cursor_tick: f32,
    ppt: f32,
    viewport_w: f32,
    (min_scroll_x, max_scroll_x): (f32, f32),
    page_ticks: i32,
) -> f32 {
    let page_px = page_ticks as f32 * ppt;

    while cursor_tick < scroll_x / ppt {
        scroll_x -= page_px;
    }
    scroll_x = scroll_x.max(min_scroll_x);

    while cursor_tick >= (scroll_x + viewport_w) / ppt {
        scroll_x += page_px;
    }

    scroll_x.clamp(min_scroll_x, max_scroll_x)
}

#[cfg(test)]
mod tests {
    use super::{
        arranger_max_scroll_x, clip_scroll_range, followed_scroll_x, rescaled_clip_scroll_x,
        scrolled_offset,
    };
    use crate::core::time::Meter;
    use crate::view::display::state::zoom::zoomed_scroll_x;

    const TWO_BARS: i32 = Meter::FOUR_FOUR.bars_to_ticks(2);

    #[test]
    fn scroll_left_advances_and_right_rewinds() {
        // Swipe left => negative delta_x => scroll toward later bars.
        assert_eq!(scrolled_offset(100.0, -40.0, 1000.0), 140.0);
        // Swipe right => positive delta_x => scroll back toward bar 1.
        assert_eq!(scrolled_offset(100.0, 40.0, 1000.0), 60.0);
    }

    #[test]
    fn scroll_clamps_both_ends() {
        assert_eq!(scrolled_offset(10.0, 999.0, 1000.0), 0.0);
        assert_eq!(scrolled_offset(990.0, -999.0, 1000.0), 1000.0);
        // A degenerate (negative) max still yields a sane 0.
        assert_eq!(scrolled_offset(50.0, -10.0, -5.0), 0.0);
    }

    #[test]
    fn max_scroll_x_picks_largest_tick_and_never_negative() {
        assert_eq!(arranger_max_scroll_x(400, 2.0), 800.0);
        assert_eq!(arranger_max_scroll_x(-100, 2.0), 0.0);
    }

    #[test]
    fn follow_pages_toward_the_cursor() {
        let ppt = 1.0;
        let viewport_w = 38_400.0; // 10 bars wide, so the 2-bar steps land cleanly
        let max = 10_000_000.0;

        // Cursor near bar 1 but the view is scrolled far right: pages left to 0.
        assert_eq!(
            followed_scroll_x(500_000.0, 10.0, ppt, viewport_w, (0.0, max), TWO_BARS),
            0.0
        );
        // Cursor past the right edge: pages right until it is visible.
        let cursor = 76_800.0; // bar 21
        let scrolled = followed_scroll_x(0.0, cursor, ppt, viewport_w, (0.0, max), TWO_BARS);
        assert!(scrolled <= cursor && scrolled + viewport_w > cursor);
        // Cursor already visible: unchanged.
        assert_eq!(
            followed_scroll_x(50_000.0, 60_000.0, ppt, viewport_w, (0.0, max), TWO_BARS),
            50_000.0
        );
    }

    #[test]
    fn follow_pages_land_on_the_page_grid() {
        // Zoomed out, the page is two 4-bar structural units: every paged
        // offset is a whole multiple of 8 bars, i.e. on a visible bar line.
        let ppt = 0.001;
        let page = Meter::FOUR_FOUR.bars_to_ticks(8);
        let scrolled = followed_scroll_x(0.0, 500_000.0, ppt, 400.0, (0.0, 1e9), page);
        let ticks = (scrolled / ppt).round() as i32;
        assert_eq!(ticks % page, 0, "{ticks}");
        assert!(scrolled <= 500_000.0 * ppt && scrolled + 400.0 > 500_000.0 * ppt);
    }

    #[test]
    fn follow_respects_the_right_clamp() {
        // Cursor far right but max_scroll_x tight: never scrolls past the clamp.
        assert_eq!(
            followed_scroll_x(0.0, 10_000_000.0, 1.0, 38_400.0, (0.0, 30_000.0), TWO_BARS),
            30_000.0
        );
    }

    #[test]
    fn clip_scroll_range_spans_exactly_the_clip() {
        // A 4-bar clip at bar 9, zoomed so 1 bar = 960px (exact in f32) on
        // a 1920px view: two bars on screen.
        let ppt = 0.25;
        let (start, end) = (
            Meter::FOUR_FOUR.bars_to_ticks(8),
            Meter::FOUR_FOUR.bars_to_ticks(12),
        );
        let (min, max) = clip_scroll_range(start, end, ppt, 1920.0);
        assert_eq!(min, 8.0 * 960.0);
        assert_eq!(max, 10.0 * 960.0);
        // Narrower than the viewport: collapses onto the clip start.
        assert_eq!(
            clip_scroll_range(start, end, ppt / 4.0, 1920.0),
            (2.0 * 960.0, 2.0 * 960.0)
        );
    }

    #[test]
    fn a_resize_keeps_the_left_edge_tick() {
        // Scale doubles under the view: the tick at the left edge stays there.
        assert_eq!(rescaled_clip_scroll_x(800.0, Some(0.1), 0.2), 1600.0);
        assert_eq!(rescaled_clip_scroll_x(800.0, Some(0.1), 0.1), 800.0);
        assert_eq!(rescaled_clip_scroll_x(800.0, None, 0.2), 800.0);
    }

    #[test]
    fn a_zoom_survives_the_next_frames_sync() {
        // Regression: a clip-view zoom set the scroll for the new scale but
        // left `clip_last_ppt` at the old one, so the next frame's sync
        // rescaled it again and the content drifted left — further with every
        // step in. The zoom records its scale (`set_clip_scroll`), so the
        // sync sees no change and the anchor stays on its pixel.
        let (old_ppt, new_ppt) = (0.01, 0.0125);
        let (scroll_x, anchor_tick) = (350.0, 61_234.5);
        let anchor_px = anchor_tick * old_ppt - scroll_x;
        let zoomed = zoomed_scroll_x(scroll_x, anchor_tick, old_ppt, new_ppt);

        let synced = rescaled_clip_scroll_x(zoomed, Some(new_ppt), new_ppt);
        assert!((anchor_tick * new_ppt - synced - anchor_px).abs() < 1e-3);

        let double_scaled = rescaled_clip_scroll_x(zoomed, Some(old_ppt), new_ppt);
        assert!(anchor_tick * new_ppt - double_scaled < anchor_px - 100.0);
    }
}
