//! All of `Display`'s painting, split by surface: `arranger.rs` (the clip
//! timeline + track headers), `clip_view.rs` (the piano roll), `piano_keys.rs`
//! (the keyboard gutter), `timeline.rs` (the ruler / region strip),
//! `overlays.rs` (cursor line, playhead, live-rec), `modals/` (the settings
//! modal, the help overlay and the project dialogs), `browser.rs` (the browser
//! side panel). This file owns `render` — the per-frame entry — and the
//! shared layout maths. See `030-ui-design.md`.

mod arranger;
mod browser;
mod clip_view;
mod modals;
mod output_menu;
mod overlays;
mod piano_keys;
mod timeline;

#[cfg(target_os = "macos")]
use std::io::Write;
use std::time::{Duration, Instant};

use egui::{
    Align2, CentralPanel, Color32, CornerRadius, CursorGrab, CursorIcon, FontId, Frame, Painter,
    Rect, Stroke, ViewportCommand, emath::TSTransform, pos2, vec2,
};

use super::state::scrolled_offset;
use super::*;
use crate::core::config::MAX_TRACKS;
#[cfg(target_os = "macos")]
use crate::core::plugin_host::exit_without_destructors;

/// `theme::accent()` when `highlighted` (the focused section, the row under
/// the cursor), else `idle`. Shared by the modals and the browser panel.
fn accent_if(highlighted: bool, idle: Color32) -> Color32 {
    if highlighted { theme::accent() } else { idle }
}

/// How often the UI wakes while the transport is stopped, so the header's
/// DSP chip keeps reading the audio load rather than freezing between input
/// events. See `030-ui-design.md` § Rendering Performance.
const DSP_IDLE_REFRESH: Duration = Duration::from_millis(250);

/// Thickness of the accent along the top of the docked pane with the keyboard.
const FOCUS_EDGE_W: f32 = 2.0;

/// The keyboard-focus edge: along the top of the focused docked pane
/// (`draw_pane_split`), or of the track-header column or the browser panel
/// while it has the keyboard (`draw_arranger_backgrounds`, `draw_browser`);
/// and the rename field's outline.
pub(super) fn focus_edge_stroke() -> Stroke {
    Stroke::new(FOCUS_EDGE_W, theme::accent().gamma_multiply(0.7))
}

/// Vertical geometry of the arranger's reserved performance-lane row and
/// the track lanes below it, computed once and shared by drawing
/// (`draw_arranger_backgrounds` / `draw_arranger_view`) and hit-testing
/// (`track_idx_at`/`performance_lane_hit_at`) so they can never drift apart.
///
/// The lanes sit in a viewport (`lanes_top`, `viewport_h` tall) that
/// scrolls vertically once they no longer fit (`ARRANGER_MIN_LANE_H`);
/// `tracks_top()` has the scroll applied, so it can lie above `lanes_top`.
pub(super) struct ArrangerLayout {
    /// Top y of the performance-lane row.
    pub(super) performance_lane_top: f32,
    /// Height of the performance-lane row.
    pub(super) performance_lane_h: f32,
    /// Top y of the lane viewport, right under the performance lane.
    pub(super) lanes_top: f32,
    /// Height of the lane viewport, down to the bottom of the arranger.
    pub(super) viewport_h: f32,
    /// How far the lanes are scrolled — `RenderState::arranger_scroll_y`,
    /// clamped to `0.0..=max_scroll_y`.
    pub(super) scroll_y: f32,
    /// Furthest the lanes can scroll (`arranger_max_scroll_y`).
    pub(super) max_scroll_y: f32,
    /// Per-lane height.
    pub(super) lane_h: f32,
    /// How many tracks there are.
    pub(super) track_count: usize,
    /// Height of the `+` row under the last lane (`ADD_TRACK_ROW_H`), `0.0`
    /// at `MAX_TRACKS`, where there is nothing to add.
    pub(super) add_row_h: f32,
}

impl ArrangerLayout {
    /// Top y of the first track lane, scrolled.
    pub(super) fn tracks_top(&self) -> f32 {
        self.lanes_top - self.scroll_y
    }

    /// Top y of `track_idx`'s lane.
    pub(super) fn lane_top(&self, track_idx: usize) -> f32 {
        self.tracks_top() + track_idx as f32 * self.lane_h
    }

    /// Top y of the `+` row, right under the last lane.
    pub(super) fn add_row_top(&self) -> f32 {
        self.lane_top(self.track_count)
    }

    /// How far screen `y` is past the lane viewport — positive above it,
    /// negative below, `0.0` inside: the sign is the direction to scroll
    /// (`scroll_arranger_lanes_by`'s positive = toward track 1), the same
    /// convention as the piano roll's `NoteAreaGeom::edge_overshoot`.
    pub(super) fn edge_overshoot(&self, y: f32) -> f32 {
        edge_overshoot(y, self.lanes_top, self.viewport_h)
    }

    /// `painter` clipped to the lane viewport (offset to `rect`) — the one
    /// painter for everything drawn in the lanes, so a scrolled lane never
    /// paints over the performance lane or the timeline strip.
    pub(super) fn lanes_painter(&self, painter: &Painter, rect: Rect) -> Painter {
        let top = rect.min.y + self.lanes_top;
        let lanes = Rect::from_x_y_ranges(rect.x_range(), top..=(top + self.viewport_h));
        painter.with_clip_rect(lanes.intersect(painter.clip_rect()))
    }
}

/// Shortest an arranger lane gets: the track header's stack — name row,
/// S/M row and bars (67px, `track_header_rects`) — with room around it. Below this the lanes stop
/// shrinking and the arranger scrolls vertically instead.
pub(super) const ARRANGER_MIN_LANE_H: f32 = 72.0;

/// How far screen `y` lies past the band `[top, top + h]`, in points:
/// positive above it, negative below, `0` inside — the direction and speed
/// of an edge auto-scroll. Shared by the piano roll's note area and the
/// arranger's lane viewport.
pub(super) fn edge_overshoot(y: f32, top: f32, h: f32) -> f32 {
    if y < top {
        top - y
    } else if y > top + h {
        top + h - y
    } else {
        0.0
    }
}

/// Height of the slim `+` row under the last lane — a click on it adds a
/// track at the end.
pub(super) const ADD_TRACK_ROW_H: f32 = 22.0;

/// The `+` row's height with `track_count` tracks: gone at `MAX_TRACKS`.
fn arranger_add_row_h(track_count: usize) -> f32 {
    if track_count < MAX_TRACKS {
        ADD_TRACK_ROW_H
    } else {
        0.0
    }
}

/// Per-lane height for `track_count` lanes in a viewport `viewport_h` tall
/// over an `add_row_h` `+` row: an even share of what the row leaves, never
/// below `ARRANGER_MIN_LANE_H`.
fn arranger_lane_h(viewport_h: f32, track_count: usize, add_row_h: f32) -> f32 {
    ((viewport_h - add_row_h) / track_count.max(1) as f32).max(ARRANGER_MIN_LANE_H)
}

/// Furthest the lanes scroll: the `+` row's bottom (the last lane's, without
/// one) on the viewport's bottom edge; `0.0` when they all fit.
fn arranger_max_scroll_y(viewport_h: f32, lane_h: f32, track_count: usize, add_row_h: f32) -> f32 {
    (lane_h * track_count as f32 + add_row_h - viewport_h).max(0.0)
}

/// The scroll that shows the content span `[top, top + height)` whole (a
/// lane, or the last lane with the `+` row under it), moving `scroll_y` only
/// as far as needed — nothing when it is already in view; its top wins when
/// the span is taller than the viewport.
fn arranger_scroll_y_to_show(scroll_y: f32, viewport_h: f32, top: f32, height: f32) -> f32 {
    let bottom = top + height;
    if top < scroll_y {
        top
    } else if bottom > scroll_y + viewport_h {
        (bottom - viewport_h).min(top)
    } else {
        scroll_y
    }
}

/// Screen rects of one track header's controls: the name row on top, then
/// the **S**/**M** button row, then the volume and pan bars. See
/// `Display::track_header_rects`.
pub(super) struct TrackHeaderRects {
    /// The name row: the track's name, or its number while unnamed. A
    /// double-click on it opens the rename field, drawn over it.
    pub(super) name: Rect,
    /// The **S** button.
    pub(super) solo: Rect,
    /// The **M** button.
    pub(super) mute: Rect,
    /// The output chip right of **M**, to the column's right edge: `Ch 3`
    /// or the plugin's name; a click opens the output menu.
    pub(super) output: Rect,
    /// The volume bar.
    pub(super) volume: Rect,
    /// The pan bar.
    pub(super) pan: Rect,
}

/// Height of one track-header mix bar.
const TRACK_MIX_BAR_H: f32 = 13.0;
/// Vertical gap between the two track-header mix bars.
const TRACK_MIX_BAR_GAP: f32 = 3.0;

/// Height of a clip's full-opacity accent header strip at the top of its lane.
/// Doubles as the clip edge drag-resize *grab band*: only a press landing
/// within this strip starts an edge resize (`clip_resize_band`), so a press
/// lower down — e.g. right between two touching clips — still places the
/// cursor / starts a time selection instead of being swallowed by the edge
/// hit-test. Drawing (`draw_clip_body`) and the grab band must use this one
/// constant so the visible affordance and the draggable area can never drift.
/// See `030-ui-design.md`.
pub(super) const CLIP_HEADER_H: f32 = 20.0;

/// Lane fill left showing between a lane seam's edge and a clip body, top and
/// bottom — the breathing room that makes a clip read as a card floating in
/// its lane. Eye-tuned against the 1.5pt seam: the old fixed 3px inset left
/// 2.25px of lane on each side of a now-thinner seam (clips 6px apart across
/// a 1.5 groove), which read as too much margin.
const CLIP_SEAM_CLEARANCE: f32 = 1.25;

/// Vertical inset, top and bottom, between a clip body and its lane bounds in
/// the arranger, so clips read as cards floating in the lane with the groove
/// seams visible above and below rather than filling it edge to edge. Derived
/// from the seam: half of it intrudes into each lane (it is centred on the
/// boundary), plus `CLIP_SEAM_CLEARANCE` — so retuning `ARRANGER_LANE_SEAM_W`
/// carries the clip margin along. 2.0 at the current values: a whole point,
/// so both clip edges snap the same way in every lane.
/// Drawing-only (via `clip_body_band`, in `draw_arranger_view`): hit-testing —
/// clip selection and the `clip_resize_band` grab band — stays lane-relative,
/// and this inset is small enough that the drawn header still sits well inside
/// that band.
pub(super) const CLIP_V_INSET: f32 = ARRANGER_LANE_SEAM_W / 2.0 + CLIP_SEAM_CLEARANCE;

/// The pixel-snapped `[top, bottom)` screen-y span of a clip body within its
/// lane in the arranger: the lane bounds inset by `CLIP_V_INSET` top and
/// bottom, then rounded to whole pixels. `draw_clip_body` goes through this so
/// clips in adjacent lanes land on the exact same pixel rows and read as flush
/// — an unrounded inset left the two edges antialiased across different pixel
/// columns depending on `lane_h`. Pure geometry, unit-tested.
pub(super) fn clip_body_band(lane_y: f32, lane_h: f32) -> (f32, f32) {
    let top = (lane_y + CLIP_V_INSET).round();
    let bottom = (lane_y + lane_h - CLIP_V_INSET).round().max(top + 1.0);
    (top, bottom)
}

/// Width of the arranger's horizontal lane boundaries — the track-to-track
/// seams, the performance lane's bottom edge and the timeline strip's bottom
/// edge (all three kept equal so no row gets a thinner border than its
/// neighbour). Was 2px; read as too coarse once the grid went subtle. 1px
/// was tried next (seams outranking the 1px bar lines by strength alone);
/// 1.5 splits the difference — a touch more weight than the bar lines
/// without the old coarseness. Eye-tuned. Drawn at `seam_y`, which lands any
/// width crisp. The clip views keep 2px lane-boundary seams, matching their
/// 2px octave dividers.
pub(super) const ARRANGER_LANE_SEAM_W: f32 = 1.5;

/// A horizontal seam's y, in points, snapped so a `width`-point line lands on
/// whole physical pixels at `pixels_per_point` instead of anti-aliasing into
/// soft edges (lane edges sit at fractional y — `lane_h` is the track area
/// over the track count). A line an *odd* number of physical pixels wide must
/// be centred on a pixel centre, an even one on a pixel edge: 1.5pt is 3px on
/// a 2× (Retina) display, 1pt is 2px there but 1px at 1×.
pub(super) fn seam_y(y: f32, width: f32, pixels_per_point: f32) -> f32 {
    let physical_width = (width * pixels_per_point).round() as i32;
    let physical_y = y * pixels_per_point;
    let snapped = if physical_width % 2 == 1 {
        physical_y.floor() + 0.5
    } else {
        physical_y.round()
    };
    snapped / pixels_per_point
}

/// The arranger grid's groove tone — shared by the horizontal lane seams
/// (`draw_arranger_backgrounds`) and every in-lane vertical grid line
/// (`draw_timeline`): bar lines at full strength, the finer tiers as the same
/// colour made translucent (`GridTiers::line_strength`). A few RGB steps
/// *darker* than the canvas (a fixed offset, so it behaves the same across
/// every palette): the whole grid then reads as
/// channels cut into the surface rather than lines laid on top — the same
/// trick as the piano-roll E/F seam (`030-ui-design.md`). The lane fills sit
/// a few steps *above* the canvas (`lane_grid_fill`), giving the grooves real
/// contrast to sit in. In the arranger the lane seams outrank the bar lines by
/// strength (seams full, bars `ARRANGER_BAR_LINE_STRENGTH`) and a little
/// width (`ARRANGER_LANE_SEAM_W` vs 1px bars).
pub(super) fn grid_seam_color() -> Color32 {
    shifted_rgb(theme::bg(), -8)
}

/// `color` with every RGB channel moved `delta` steps (saturating), opaque —
/// the brightness-not-alpha nudge the grid and lane tones are built from
/// (`030-ui-design.md` § Grid Hierarchy).
pub(super) fn shifted_rgb(color: Color32, delta: i16) -> Color32 {
    let shift = |channel: u8| (channel as i16 + delta).clamp(0, 255) as u8;
    Color32::from_rgb(shift(color.r()), shift(color.g()), shift(color.b()))
}

/// The `[top, bottom)` screen-y span of `lane_top`'s clip edge drag-resize
/// grab band — the top `CLIP_HEADER_H` px of the lane, clamped so a very short
/// lane still leaves clip body below it. Pure geometry, unit-tested.
fn clip_resize_band(lane_top: f32, lane_h: f32) -> (f32, f32) {
    let band_h = CLIP_HEADER_H.min(lane_h * 0.5);
    (lane_top, lane_top + band_h)
}

/// Half-open tick-range intersection: `Some((max(a_start,b_start),
/// min(a_end,b_end)))` when non-empty, `None` otherwise — the same overlap
/// test `Track::find_clip_ids_in` inlines, factored out here for the
/// arranger's tick-range-aware clip tint (`draw_clip_body`) and its
/// selection-gap spans (`gap_spans`). Pure geometry, unit-tested.
pub(super) fn tick_overlap(
    a_start: i32,
    a_end: i32,
    b_start: i32,
    b_end: i32,
) -> Option<(i32, i32)> {
    let start = a_start.max(b_start);
    let end = a_end.min(b_end);
    (end > start).then_some((start, end))
}

/// The uncovered sub-ranges of `[start, end)` after removing every range in
/// `covered` (a track's clip tick-ranges — any order, assumed non-overlapping,
/// true for clips on one track). Used to tint the stretches of an active time
/// selection that don't fall under any clip. Pure geometry, unit-tested.
pub(super) fn gap_spans(covered: &[(i32, i32)], start: i32, end: i32) -> Vec<(i32, i32)> {
    let mut clamped: Vec<(i32, i32)> = covered
        .iter()
        .filter_map(|&(c_start, c_end)| tick_overlap(c_start, c_end, start, end))
        .collect();
    clamped.sort_by_key(|&(s, _)| s);

    let mut gaps = Vec::new();
    let mut cursor = start;
    for (c_start, c_end) in clamped {
        if c_start > cursor {
            gaps.push((cursor, c_start));
        }
        cursor = cursor.max(c_end);
    }
    if cursor < end {
        gaps.push((cursor, end));
    }
    gaps
}

/// S/M button width.
const TRACK_BTN_W: f32 = 30.0;
/// S/M button height.
const TRACK_BTN_H: f32 = 14.0;
/// Gap between the S and M buttons.
const TRACK_BTN_GAP: f32 = 3.0;
/// Gap from the S/M button row down to the volume bar.
const TRACK_BTN_ROW_GAP: f32 = 4.0;
/// Height of the track header's name row.
pub(super) const TRACK_NAME_ROW_H: f32 = 16.0;
/// Gap from the name row down to the S/M button row.
const TRACK_NAME_ROW_GAP: f32 = 4.0;
/// Least room left around a header stack (or a lone name row), top and
/// bottom together.
const TRACK_HEADER_V_MARGIN: f32 = 4.0;
/// Narrowest header column the controls are laid out in.
const TRACK_HEADER_MIN_W: f32 = 8.0;

/// The top of a band `h` tall centred in `[lane_top, lane_top + lane_h)` —
/// `None` when the lane can't hold it with `TRACK_HEADER_V_MARGIN` to spare,
/// or `[left, right]` is narrower than `TRACK_HEADER_MIN_W`. The fit test
/// shared by the full header stack and a lone name row.
fn centred_band_top(lane_top: f32, lane_h: f32, left: f32, right: f32, h: f32) -> Option<f32> {
    (lane_h >= h + TRACK_HEADER_V_MARGIN && right - left >= TRACK_HEADER_MIN_W)
        .then_some(lane_top + (lane_h - h) * 0.5)
}

/// The full-width name row with its top at `top`.
fn name_row_at(top: f32, left: f32, right: f32) -> Rect {
    Rect::from_min_max(pos2(left, top), pos2(right, top + TRACK_NAME_ROW_H))
}

/// Lays out one track header within a lane: the full-width name row on top,
/// then the S/M button row (left-aligned, fixed width, the output chip
/// filling the rest of the row), then the two full-width bars (volume then
/// pan), the whole stack vertically centred in `[lane_top, lane_top +
/// lane_h)`. `None` when the lane is too short to hold the stack with a
/// little margin, or the column too narrow — [`track_name_row`] then still
/// places the name on its own. The button row and the bars are always part
/// of the layout — even for a track with no instrument (no bars drawn) — so
/// the rows sit at the same y in every lane. Pure geometry, unit-tested —
/// mirrors the `piano_keys` free-function convention.
fn track_header_rects(
    lane_top: f32,
    lane_h: f32,
    left: f32,
    right: f32,
) -> Option<TrackHeaderRects> {
    let stack_h = TRACK_NAME_ROW_H
        + TRACK_NAME_ROW_GAP
        + TRACK_BTN_H
        + TRACK_BTN_ROW_GAP
        + 2.0 * TRACK_MIX_BAR_H
        + TRACK_MIX_BAR_GAP;
    let name = name_row_at(
        centred_band_top(lane_top, lane_h, left, right, stack_h)?,
        left,
        right,
    );
    let top = name.max.y + TRACK_NAME_ROW_GAP;

    let solo = Rect::from_min_max(pos2(left, top), pos2(left + TRACK_BTN_W, top + TRACK_BTN_H));
    let mute_left = left + TRACK_BTN_W + TRACK_BTN_GAP;
    let mute = Rect::from_min_max(
        pos2(mute_left, top),
        pos2(mute_left + TRACK_BTN_W, top + TRACK_BTN_H),
    );
    let output = Rect::from_min_max(
        pos2(mute.max.x + TRACK_BTN_GAP, top),
        pos2(right.max(mute.max.x + TRACK_BTN_GAP), top + TRACK_BTN_H),
    );

    let volume_top = top + TRACK_BTN_H + TRACK_BTN_ROW_GAP;
    let volume = Rect::from_min_max(
        pos2(left, volume_top),
        pos2(right, volume_top + TRACK_MIX_BAR_H),
    );
    let pan_top = volume_top + TRACK_MIX_BAR_H + TRACK_MIX_BAR_GAP;
    let pan = Rect::from_min_max(pos2(left, pan_top), pos2(right, pan_top + TRACK_MIX_BAR_H));

    Some(TrackHeaderRects {
        name,
        solo,
        mute,
        output,
        volume,
        pan,
    })
}

/// Where a track header's name row sits: in the full stack
/// ([`track_header_rects`]) when the lane holds it; else alone, centred in a
/// lane too short for the rest — the name is the last of the header to go,
/// being the lane's identity. `None` only when not even the name fits.
fn track_name_row(lane_top: f32, lane_h: f32, left: f32, right: f32) -> Option<Rect> {
    if let Some(rects) = track_header_rects(lane_top, lane_h, left, right) {
        return Some(rects.name);
    }
    centred_band_top(lane_top, lane_h, left, right, TRACK_NAME_ROW_H)
        .map(|top| name_row_at(top, left, right))
}

impl Display {
    /// The arranger's vertical geometry ([`ArrangerLayout`]) for this frame —
    /// shared by drawing and hit-testing so they can't drift.
    pub(super) fn arranger_layout(&self) -> ArrangerLayout {
        let area_top = self.track_area_top();
        let area_h = self.track_area_h();
        let performance_lane_top = area_top + Self::TIMELINE_H;
        let performance_lane_h = Self::PERFORMANCE_LANE_H;
        let lanes_top = performance_lane_top + performance_lane_h;
        let viewport_h = (area_h - Self::TIMELINE_H - performance_lane_h).max(0.0);
        let track_count = self.track_count();
        let add_row_h = arranger_add_row_h(track_count);
        let lane_h = arranger_lane_h(viewport_h, track_count, add_row_h);
        let max_scroll_y = arranger_max_scroll_y(viewport_h, lane_h, track_count, add_row_h);
        ArrangerLayout {
            performance_lane_top,
            performance_lane_h,
            lanes_top,
            viewport_h,
            scroll_y: self.render.arranger_scroll_y.clamp(0.0, max_scroll_y),
            max_scroll_y,
            lane_h,
            track_count,
            add_row_h,
        }
    }

    /// Scrolls the arranger's lanes vertically by a wheel / trackpad delta
    /// (positive = toward track 1, already OS natural-scroll aware), within
    /// `arranger_max_scroll_y`. User-owned: only this and
    /// [`reveal_selected_track`](Self::reveal_selected_track) move it.
    pub(in crate::view::display) fn scroll_arranger_lanes_by(&mut self, delta_y: f32) {
        if !self.is_pane_visible(Pane::Arranger) || delta_y == 0.0 {
            return;
        }
        // A menu hanging off a track header would be left behind.
        self.gesture.output_menu = None;
        let layout = self.arranger_layout();
        self.render.arranger_scroll_y =
            scrolled_offset(layout.scroll_y, delta_y, layout.max_scroll_y);
    }

    /// Scrolls the arranger's lanes just enough to show the selected track
    /// whole — after the selection moved (`UiEvent::TrackSelected`), so
    /// Up/Down never walks the selection out of view. A selection already in
    /// view moves nothing, and so does one made while the arranger is hidden
    /// (no viewport to reveal it in).
    pub(in crate::view::display) fn reveal_selected_track(&mut self) {
        self.reveal_track(self.selected_track_idx);
    }

    /// Scrolls the arranger's lanes just enough to show `track_idx` whole —
    /// the selected track ([`reveal_selected_track`](Self::reveal_selected_track)),
    /// or the marquee edge `⇧↑`/`⇧↓` just moved. The last track brings the
    /// `+` row under it along, so adding tracks one after another from the
    /// row keeps it under the pointer. A no-op while the arranger is hidden.
    pub(in crate::view::display) fn reveal_track(&mut self, track_idx: usize) {
        if !self.is_pane_visible(Pane::Arranger) {
            return;
        }
        let layout = self.arranger_layout();
        if layout.viewport_h <= 0.0 {
            return;
        }
        let below = if track_idx + 1 == layout.track_count {
            layout.add_row_h
        } else {
            0.0
        };
        self.render.arranger_scroll_y = arranger_scroll_y_to_show(
            layout.scroll_y,
            layout.viewport_h,
            track_idx as f32 * layout.lane_h,
            layout.lane_h + below,
        );
    }

    /// Draws the active pane (see `in_pane`): its lane backgrounds, timeline
    /// strip and content, then the overlays that ride on top of it — the
    /// cursor line, the playhead and (arranger only) the selected-track
    /// cursor. `rect` is the canvas; each helper places itself inside the
    /// pane through the pane-aware geometry (`track_area_top`, …).
    fn draw_pane(&self, painter: &Painter, rect: Rect) {
        let clip_pane = self.active_pane() == Pane::Clip;
        if clip_pane && self.lead_clip_time.is_none() {
            self.draw_empty_clip_pane(painter, rect);
            return;
        }
        // Lane backgrounds go down before the timeline grid so the
        // vertical bar lines composite on top of the (opaque) lane
        // fills rather than being hidden under them.
        // Clip pane only: one note geometry for the whole draw pass.
        let note_geom = clip_pane.then(|| self.note_area_geom());
        match &note_geom {
            Some(geom) => self.draw_note_lane_backgrounds(painter, geom),
            None => self.draw_arranger_backgrounds(painter, rect),
        }
        self.draw_timeline(painter, rect);
        self.draw_region(painter, rect);
        self.draw_time_selection(painter, rect);

        if let Some(geom) = &note_geom {
            self.draw_clip_view(painter, rect, geom);
            self.draw_cursor_line(painter, rect);
            self.draw_playhead(painter, rect);
            return;
        }

        // What is drawn in the track lanes is clipped to their viewport, so
        // a lane scrolled up never paints over the performance lane or the
        // timeline strip.
        let lanes_painter = self.arranger_layout().lanes_painter(painter, rect);
        self.draw_time_selection_gaps(&lanes_painter, rect);
        self.draw_arranger_view(&lanes_painter, rect);
        self.draw_cursor_line(painter, rect);
        self.draw_playhead(painter, rect);
        self.draw_selected_track_cursor(&lanes_painter, rect);
    }

    /// The docked clip pane with no clip under the cursor: a quiet note in
    /// place of an empty piano roll.
    fn draw_empty_clip_pane(&self, painter: &Painter, rect: Rect) {
        let pane = self.pane_rect().translate(rect.min.to_vec2());
        painter.rect_filled(
            pane,
            CornerRadius::ZERO,
            theme::bg_panel().gamma_multiply(0.5),
        );
        painter.text(
            pane.center(),
            Align2::CENTER_CENTER,
            "No clip at the cursor",
            FontId::proportional(theme::FONT_SIZE_LABEL),
            theme::fg_dim(),
        );
    }

    /// With the clip view docked: the split line in the gap between the two
    /// panes, and a thin accent along the top of the pane with the keyboard
    /// (unless the track headers or the browser panel have it).
    /// Nothing with one pane showing — it has the whole lane area and the
    /// keyboard both.
    fn draw_pane_split(&self, painter: &Painter, panes: PaneRects, rect: Rect) {
        let (Some(arranger), Some(clip)) = (panes.arranger, panes.clip) else {
            return;
        };
        let offset = rect.min.to_vec2();
        let split_y = ((arranger.max.y + clip.min.y) * 0.5).round() + offset.y;
        painter.hline(
            arranger.min.x + offset.x..=arranger.max.x + offset.x,
            split_y,
            Stroke::new(1.0, theme::separator()),
        );
        // The header column and the browser panel draw their own edge while
        // they have the keyboard (`draw_arranger_backgrounds`,
        // `draw_browser`): one edge, on what the keys act on.
        if self.track_headers_have_keyboard() || self.key_focus == KeyFocus::Browser {
            return;
        }
        let focused = if self.focused_pane() == Pane::Clip {
            clip
        } else {
            arranger
        };
        painter.hline(
            focused.min.x + offset.x..=focused.max.x + offset.x,
            focused.min.y + offset.y,
            focus_edge_stroke(),
        );
    }

    /// Hit-tests a click in `Clip` against the currently rendered
    /// note events. Uses the same geometry as `draw_clip_view`
    /// (`note_area_geom`) so the clickable area always matches what's drawn
    /// — including that nothing is drawn left of the grid, over the key
    /// column, where scrolled notes pass underneath.
    pub(super) fn event_id_at(&self, x: f32, y: f32) -> Option<Uuid> {
        let note = self.note_area_geom().note_row_at(y)?;
        let notes = self.render.event_shapes.iter().map(|shape| {
            (
                shape.event_id(),
                shape.note_number(),
                shape.start_tick(),
                shape.end_tick(),
            )
        });
        note_at_grid_point(
            notes,
            self.content_x_on_screen(x),
            note,
            self.screen_x_to_tick(x),
        )
    }

    /// [`event_id_at`](Self::event_id_at) plus which part of the note is
    /// under `x` — its left or right edge zone, or its body
    /// ([`note_part_at`], against the note's drawn x span).
    pub(super) fn note_hit_at(&self, x: f32, y: f32) -> Option<(Uuid, NotePart)> {
        let id = self.event_id_at(x, y)?;
        let shape = self.event_shape(id)?;
        let left = self.tick_to_x(shape.start_tick());
        let width = (shape.end_tick() - shape.start_tick()).max(1) as f32 * self.pixels_per_tick();
        Some((id, note_part_at(x, left, left + width)))
    }

    /// Maps a screen y to the arranger track lane it falls in, using the
    /// shared `arranger_layout()` geometry. Returns `None` above the lane
    /// viewport (e.g. over the timeline header or the performance lane row,
    /// or a lane scrolled up under them); clamps to the last lane at the
    /// bottom edge rather than returning `None` there, since float division
    /// can round the last lane's y range short by a hair.
    pub(super) fn track_idx_at(&self, y: f32) -> Option<usize> {
        let layout = self.arranger_layout();
        if y < layout.lanes_top {
            return None;
        }

        let idx = ((y - layout.tracks_top()) / layout.lane_h) as usize;
        Some(idx.min(layout.track_count - 1))
    }

    /// True when `y` falls within the reserved performance lane row, above
    /// track 1 — mirrors `track_idx_at`'s use of the shared geometry.
    pub(super) fn performance_lane_hit_at(&self, y: f32) -> bool {
        let layout = self.arranger_layout();
        y >= layout.performance_lane_top && y < layout.lanes_top
    }

    /// Screen-space distance from an edge within which a clip's left/right
    /// boundary counts as "hovered" for drag-resize purposes.
    const CLIP_EDGE_HIT_PX: f32 = 5.0;

    /// Longest frame time the edge auto-scroll credits, in seconds, so the
    /// first frame after an idle stretch can't jump the view.
    const MAX_AUTO_SCROLL_DT: f32 = 0.05;

    /// Hit-tests a screen point against every clip's left/right edge on the
    /// track lane at `y`, using the same `tick_to_screen_x` geometry the
    /// arranger draws with. Arranger only.
    ///
    /// Of the clips with an edge within `CLIP_EDGE_HIT_PX`, the one with the
    /// earliest `start_tick` wins (the first in `clip_shapes` on a tie), and
    /// on it the Start edge before the End — which resolves the one
    /// ambiguous case, two clips touching exactly, in favour of the earlier
    /// clip's right edge over the later clip's left edge.
    ///
    /// Only presses within the lane's top `CLIP_HEADER_H`-px grab band
    /// (`clip_resize_band`) count — lower down, an x near a shared clip
    /// boundary must stay free for cursor placement / time selection.
    pub(super) fn clip_edge_at(&self, x: f32, y: f32) -> Option<ClipResizeDrag> {
        let track_idx = self.grab_band_track_at(y)?;
        let near = |tick: i32| (x - self.tick_to_screen_x(tick)).abs() <= Self::CLIP_EDGE_HIT_PX;
        self.render
            .clip_shapes
            .iter()
            .filter(|shape| shape.track_idx() == track_idx)
            .filter_map(|shape| {
                let edge = if near(shape.start_tick()) {
                    ClipResizeEdge::Start
                } else if near(shape.end_tick()) {
                    ClipResizeEdge::End
                } else {
                    return None;
                };
                Some((shape, edge))
            })
            .min_by_key(|(shape, _)| shape.start_tick())
            .map(|(shape, edge)| ClipResizeDrag {
                track_idx,
                clip_id: shape.clip_id(),
                edge,
            })
    }

    /// The arranger track whose clip grab band (`clip_resize_band`) holds
    /// `y`, shared by `clip_edge_at` and `clip_band_at`. `None` outside the
    /// arranger, off the lanes, or lower down the lane.
    fn grab_band_track_at(&self, y: f32) -> Option<usize> {
        if self.active_pane() != Pane::Arranger {
            return None;
        }
        let track_idx = self.track_idx_at(y)?;
        let layout = self.arranger_layout();
        let (band_top, band_bottom) = clip_resize_band(layout.lane_top(track_idx), layout.lane_h);
        (band_top..band_bottom).contains(&y).then_some(track_idx)
    }

    /// Hit-tests a screen point against every clip's header band on the
    /// track lane at `y` — the interior of the band, off the edges — for the
    /// clip band press / move drag. Arranger only. Same `clip_resize_band`
    /// gate as `clip_edge_at`, which callers check *first* so an edge grab
    /// keeps priority over the band behind it. The clip currently being
    /// recorded can't be grabbed.
    pub(super) fn clip_band_at(&self, x: f32, y: f32) -> Option<(usize, Uuid)> {
        let track_idx = self.grab_band_track_at(y)?;
        self.render
            .clip_shapes
            .iter()
            .filter(|shape| shape.track_idx() == track_idx)
            .find(|shape| {
                x >= self.tick_to_screen_x(shape.start_tick())
                    && x < self.tick_to_screen_x(shape.end_tick())
            })
            .map(|shape| shape.clip_id())
            .filter(|id| Some(*id) != self.recording_clip_id)
            .map(|id| (track_idx, id))
    }

    /// Screen rects of `track_idx`'s header controls (S/M/output row, then the volume
    /// and pan bars), or `None` when the lane is too short / narrow to hold
    /// them. Shared by `draw_track_header_buttons` / `draw_track_mix_bars` and
    /// `track_button_at` / `track_mix_bar_at` so drawing and hit-testing can
    /// never drift — the same discipline `arranger_layout()` enforces. Arranger
    /// geometry; the caller decides which controls a given track actually shows
    /// (buttons always, bars only when `track_has_instrument`).
    pub(super) fn track_header_rects(&self, track_idx: usize) -> Option<TrackHeaderRects> {
        let (lane_top, lane_h, left, right) = self.track_header_band(track_idx)?;
        track_header_rects(lane_top, lane_h, left, right)
    }

    /// Screen rect of `track_idx`'s header name row — in the stack, or alone
    /// on a lane too short for the rest ([`track_name_row`]). Shared by its
    /// drawing, the double-click that renames and the rename field.
    pub(super) fn track_name_rect(&self, track_idx: usize) -> Option<Rect> {
        let (lane_top, lane_h, left, right) = self.track_header_band(track_idx)?;
        track_name_row(lane_top, lane_h, left, right)
    }

    /// `track_idx`'s lane top and height and the header controls' left and
    /// right edges — what the header layouts are laid out in. `None` past the
    /// last track.
    fn track_header_band(&self, track_idx: usize) -> Option<(f32, f32, f32, f32)> {
        let layout = self.arranger_layout();
        if track_idx >= layout.track_count {
            return None;
        }
        // Equal padding on both sides so the controls sit centred in the
        // column: `left` insets from the column's left edge, `right` insets
        // by the same amount from its groove-line right edge
        // (`content_origin_x() - TRACK_COLUMN_GRID_GAP_X`).
        let left = self.render.canvas_rect.min.x + Self::TRACK_HEADER_INNER_PAD_X;
        let right = self.content_origin_x()
            - Self::TRACK_COLUMN_GRID_GAP_X
            - Self::TRACK_HEADER_INNER_PAD_X;
        Some((layout.lane_top(track_idx), layout.lane_h, left, right))
    }

    /// The track whose header name row holds `(x, y)` — what a double-click
    /// renames. Arranger only, and only inside the lane viewport.
    pub(super) fn track_name_at(&self, x: f32, y: f32) -> Option<usize> {
        let track_idx = self.arranger_track_at(y)?;
        self.track_name_rect(track_idx)?
            .contains(pos2(x, y))
            .then_some(track_idx)
    }

    /// The `+` row's header-column part — what a click on adds a track, and
    /// where its `+` is drawn — clipped to the lane viewport. `None` at
    /// `MAX_TRACKS`, outside the arranger, or scrolled out of view.
    pub(super) fn add_track_row_rect(&self) -> Option<Rect> {
        if self.active_pane() != Pane::Arranger {
            return None;
        }
        let layout = self.arranger_layout();
        if layout.add_row_h <= 0.0 {
            return None;
        }
        let top = layout.add_row_top().max(layout.lanes_top);
        let bottom =
            (layout.add_row_top() + layout.add_row_h).min(layout.lanes_top + layout.viewport_h);
        let left = self.render.canvas_rect.min.x;
        let right = self.content_origin_x() - Self::TRACK_COLUMN_GRID_GAP_X;
        (bottom > top).then(|| Rect::from_x_y_ranges(left..=right, top..=bottom))
    }

    /// Whether `(x, y)` is on the `+` row's header-column part.
    pub(super) fn add_track_row_at(&self, x: f32, y: f32) -> bool {
        self.add_track_row_rect()
            .is_some_and(|row| row.contains(pos2(x, y)))
    }

    /// Whether the pointer is on the `+` row — its hover wash and the
    /// pointing hand.
    pub(super) fn is_pointer_on_add_track_row(&mut self) -> bool {
        self.in_pane(Pane::Arranger, |display| {
            display
                .canvas_pointer()
                .is_some_and(|p| display.add_track_row_at(p.x, p.y))
        })
    }

    /// Hit-tests a screen point against the volume / pan bars of the
    /// instrument track whose lane holds it. Arranger only. Returns the track
    /// and which bar was hit.
    pub(super) fn track_mix_bar_at(&self, x: f32, y: f32) -> Option<(usize, TrackMixParam)> {
        let (track_idx, rects) = self.track_header_at(y)?;
        if !self.track_has_instrument(track_idx) {
            return None;
        }
        let p = pos2(x, y);
        if rects.volume.contains(p) {
            Some((track_idx, TrackMixParam::Volume))
        } else if rects.pan.contains(p) {
            Some((track_idx, TrackMixParam::Pan))
        } else {
            None
        }
    }

    /// Hit-tests a screen point against the S/M/output button row of the
    /// track whose lane holds it. Unlike the mix bars, these show on **all**
    /// tracks (mute/solo gate MIDI, which works for external-MIDI tracks
    /// too). Arranger only.
    pub(super) fn track_button_at(&self, x: f32, y: f32) -> Option<(usize, TrackButton)> {
        let (track_idx, rects) = self.track_header_at(y)?;
        let p = pos2(x, y);
        if rects.solo.contains(p) {
            Some((track_idx, TrackButton::Solo))
        } else if rects.mute.contains(p) {
            Some((track_idx, TrackButton::Mute))
        } else if rects.output.contains(p) {
            Some((track_idx, TrackButton::Output))
        } else {
            None
        }
    }

    /// The arranger track whose lane holds screen `y`, with its header's
    /// control rects — shared by the header hit-tests. `None` outside the
    /// arranger, off the lane viewport (a lane scrolled up under the
    /// performance lane has no controls there), or on a lane too short for
    /// them.
    fn track_header_at(&self, y: f32) -> Option<(usize, TrackHeaderRects)> {
        let track_idx = self.arranger_track_at(y)?;
        Some((track_idx, self.track_header_rects(track_idx)?))
    }

    /// The arranger track whose lane holds screen `y` — the header
    /// hit-tests' shared rule: arranger only, inside the lane viewport.
    fn arranger_track_at(&self, y: f32) -> Option<usize> {
        if self.active_pane() != Pane::Arranger {
            return None;
        }
        self.track_idx_at(y)
    }

    /// The note row under screen `y`, `None` outside the note area (above
    /// it, or below it in the velocity panel). Unlike
    /// [`note_at_screen_y`](Self::note_at_screen_y) it never clamps — the
    /// piano roll's double-click draws only on a real row.
    pub(super) fn note_row_at_screen_y(&self, y: f32) -> Option<u8> {
        let geom = self.note_area_geom();
        if y < geom.top || y >= geom.top + geom.h {
            return None;
        }
        geom.note_row_at(y)
    }

    /// Screen y → the piano roll's content-space row units, held inside the
    /// note area (`NoteAreaGeom::row_at`).
    pub(super) fn note_area_row_at(&self, y: f32) -> f32 {
        self.note_area_geom().row_at(y)
    }

    /// The note at the piano roll's content-space row `row`, over the whole
    /// keyboard (`piano_keys::note_at_row`) — in view or not.
    pub(super) fn note_at_row(&self, row: f32) -> u8 {
        piano_keys::note_at_row(row)
    }

    /// How the pointer is held during the octave-legend zoom drag: locked in
    /// place where `CursorGrab::Locked` exists (macOS, Wayland), confined to
    /// the window elsewhere (Windows, X11 — `Locked` is unsupported there and
    /// winit has no fallback). The drag runs on raw motion either way.
    const ZOOM_DRAG_GRAB: CursorGrab = if cfg!(target_os = "macos") {
        CursorGrab::Locked
    } else {
        CursorGrab::Confined
    };

    /// Whether a drag owns the pointer: the octave-legend zoom drag or the
    /// BPM chip drag. The pointer is held and hidden (`sync_pointer_grab`),
    /// the drag runs on `PointerMotion`, and no hover follows it — the one
    /// test every site of that rule asks.
    pub(super) fn pointer_held_drag(&self) -> bool {
        self.gesture.key_zoom_drag.is_some() || self.tempo_chip.dragging()
    }

    /// Holds the (hidden) pointer while a [pointer-holding
    /// drag](Self::pointer_held_drag) runs (Ableton / Bitwig) and lets it go,
    /// where it was pressed, on release — on the edges only, so the viewport
    /// command goes out once.
    fn sync_pointer_grab(&mut self, ctx: &egui::Context) {
        let dragging = self.pointer_held_drag();
        if dragging == self.gesture.pointer_grabbed {
            return;
        }
        self.gesture.pointer_grabbed = dragging;
        let grab = if dragging {
            Self::ZOOM_DRAG_GRAB
        } else {
            CursorGrab::None
        };
        ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
    }

    /// How far screen `y` lies past the note area's edge — positive above,
    /// negative below, `0` inside (`NoteAreaGeom::edge_overshoot`).
    pub(super) fn note_area_edge_overshoot(&self, y: f32) -> f32 {
        self.note_area_geom().edge_overshoot(y)
    }

    /// Maps a screen y to the note row it falls in, clamped to the visible
    /// rows (`row_at` holds `y` inside the note area). Used by the note move
    /// drag, which — unlike `event_id_at`'s click hit-test — must always
    /// resolve to *some* note even when the pointer wanders above the
    /// keyboard or down into the velocity panel.
    pub(super) fn note_at_screen_y(&self, y: f32) -> u8 {
        self.note_at_row(self.note_area_row_at(y))
    }
}

impl eframe::App for Display {
    /// Runs before every [`App::ui`](eframe::App::ui) pass — and, unlike
    /// `ui`, also while the main window is hidden (minimised) whenever a
    /// repaint was requested. Plugin editors are separate OS windows that
    /// stay open and visible when ours is not, so their pump lives here
    /// rather than in `ui`: it services their main-thread callbacks and
    /// timers, completes any pending per-track teardown, and schedules the
    /// follow-up wake at the shortest plugin timer cadence — otherwise the UI
    /// idles until a plugin wakes it via `request_callback`. Everything that
    /// reads egui input stays in `ui`: a hidden window's `Context::input` is
    /// a stale copy of the last shown frame, so polling it from here would
    /// replay old key events. A project load's plugin restore steps here too,
    /// one plugin per frame (`restore_next_instrument`), after the pump has
    /// drained the catalog scan.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        {
            self.pump_instrument_editors(_ctx);
            self.restore_next_instrument(_ctx);
        }
    }

    /// While files from the file manager hover (or land), feeds egui the
    /// OS's pointer: the windowing layer reports none during a drag, so the
    /// cursor line and the import ghost would otherwise freeze where the
    /// pointer last was (`input/midi_drag.rs`).
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.inject_drag_pointer(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The root `Ui` spans the whole window content: the canvas, less the
        // browser panel's width while it shows. Set again before drawing, in
        // case this frame's input showed or hid the panel.
        let window = ui.max_rect();
        self.sync_canvas_rect(window);
        let ctx = ui.ctx().clone();
        let ctx = &ctx;

        while let Ok(event) = self.input_event_rx.try_recv() {
            if !self.try_consume_as_modal(&event) {
                self.event_handlers.handle_input_event(&event);
            }
        }

        self.reset_clip_frame_snapshots();
        self.handle_input_events(ctx);
        // A held pitch drag past the note area's edge keeps scrolling with
        // the pointer still, so it asks for the next frame itself.
        let dt = ctx.input(|i| i.stable_dt);
        let pointer = self.canvas_pointer();
        let dt = dt.min(Self::MAX_AUTO_SCROLL_DT);
        if self.in_pane(Pane::Clip, |d| d.auto_scroll_note_area(dt, pointer))
            || self.in_pane(Pane::Arranger, |d| {
                d.auto_scroll_arranger_lanes(dt, pointer)
            })
        {
            ctx.request_repaint();
        }
        #[cfg(target_os = "macos")]
        let was_staging = self.instrument_restore().is_some();
        self.handle_ui_events();
        // A project being opened just queued its plugins: one more frame, for
        // `logic` to load the first after this one has painted the panel.
        // `restore_next_instrument` asks for every frame after that.
        #[cfg(target_os = "macos")]
        if !was_staging && self.instrument_restore().is_some() {
            ctx.request_repaint();
        }
        self.sync_window_close(ctx);
        self.sync_project_dialog_window(ctx);
        self.in_pane(Pane::Clip, Self::sync_clip_frame);
        self.in_pane(Pane::Arranger, Self::sync_arranger_scroll);
        self.sync_time_selection_to_cursor();

        if self.gesture.clip_resize_drag.is_some() || self.gesture.clip_resize_hover.is_some() {
            ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        // A note's edge: the resize arrows on hover and through its drag;
        // the closed hand once a body press becomes a move.
        match &self.gesture.note_drag {
            Some(drag) if drag.is_resize() => {
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            Some(drag) if drag.dragging => ctx.set_cursor_icon(CursorIcon::Grabbing),
            Some(_) => {}
            None => {
                if matches!(
                    self.gesture.note_hover,
                    Some(NotePart::Start | NotePart::End)
                ) {
                    ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            }
        }
        // Band hover/drag comes after the edge branch so the edge icon wins
        // where both could apply (they can't — `update_clip_move_hover`
        // defers to the edge hit-test — but the order documents the intent).
        // Closed hand from the press itself, not from the drag threshold: a
        // click on the band that never moves would otherwise flash back to
        // the arrow between press and release. Only for a *mouse* drag: a
        // keyboard nudge (`via_keyboard`) has nothing in hand, so the
        // pointer keeps its ordinary arrow / hover-hand meaning throughout.
        if self
            .gesture
            .clip_move_drag
            .is_some_and(|drag| !drag.via_keyboard)
        {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        } else if self.gesture.clip_move_hover.is_some() {
            ctx.set_cursor_icon(CursorIcon::Grab);
        }
        // A `.mid` or a plugin dragged from the browser is in hand, from the
        // moment it moves off its row's press point. (Over a drag from the
        // file manager the OS draws its own pointer.)
        if self.gesture.midi_drag.is_some()
            || self.gesture.plugin_drag.is_some()
            || self
                .browser
                .press
                .as_ref()
                .is_some_and(|press| press.dragging)
        {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        }
        if self.gesture.track_mix_drag.is_some()
            || self.gesture.track_mix_hover.is_some()
            || self.tempo_chip.hover
        {
            ctx.set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if self.gesture.track_button_hover.is_some()
            || self.is_pointer_on_output_menu_item()
            || self.is_pointer_on_add_track_row()
            || self.is_pointer_on_help_chip()
        {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        // The octave legend: the magnifying glass (CSS `zoom-in`) on hover;
        // its drag, like the BPM chip's, hides the pointer and holds it
        // (`sync_pointer_grab`). Hidden through the icon, not
        // `ViewportCommand::CursorVisible`: egui-winit re-shows the pointer
        // whenever the icon changes.
        if self.pointer_held_drag() {
            ctx.set_cursor_icon(CursorIcon::None);
        } else if self.gesture.key_zoom_hover {
            ctx.set_cursor_icon(CursorIcon::ZoomIn);
        }
        // A modal overlay swallows the pointer, so hovers under it are
        // stale: it decides the pointer alone.
        match self.overlay {
            Some(Overlay::Settings) if self.is_pointer_on_settings_item() => {
                ctx.set_cursor_icon(CursorIcon::PointingHand);
            }
            Some(_) => ctx.set_cursor_icon(CursorIcon::Default),
            None => {}
        }
        self.sync_pointer_grab(ctx);

        self.sync_canvas_rect(window);
        // The canvas is laid out from x = 0 and drawn shifted right by the
        // browser panel's width; the pointer is shifted back to match
        // (`InputPoller::poll`). The panel draws on a layer of its own.
        ctx.set_transform_layer(
            ui.layer_id(),
            TSTransform::from_translation(vec2(self.browser_width(), 0.0)),
        );
        if self.browser.visible {
            self.draw_browser(&ctx.layer_painter(Self::browser_layer()), window);
            self.show_browser_rename(ui);
        }

        CentralPanel::default()
            .frame(Frame::NONE.fill(theme::bg()))
            .show(ui, |ui| {
                // A handle of its own: the rename field below needs `ui`.
                let painter = &ui.painter().clone();

                let rect = self.render.canvas_rect;
                self.draw_header(painter, rect);
                let panes = self.pane_rects();
                for pane in [Pane::Arranger, Pane::Clip] {
                    if let Some(pane_rect) = panes.get(pane) {
                        // Each pane paints only inside its own rect, so the
                        // docked panes can't bleed into each other.
                        let clipped =
                            painter.with_clip_rect(pane_rect.translate(rect.min.to_vec2()));
                        self.in_pane(pane, |display| display.draw_pane(&clipped, rect));
                    }
                }
                self.draw_pane_split(painter, panes, rect);
                self.draw_status_bar(painter, rect);
                self.show_track_rename(ui);
                self.show_tempo_field(ui, Self::header_value_font());
                self.show_meter_field(ui, Self::header_value_font());
                self.draw_output_menu(painter);

                match self.overlay {
                    Some(Overlay::Settings) => self.draw_settings_view(painter, rect),
                    Some(Overlay::Help) => self.draw_help_view(painter, rect),
                    #[cfg(target_os = "macos")]
                    Some(Overlay::RestoringInstruments) => {
                        self.draw_instrument_restore_view(painter, rect);
                    }
                    None => {}
                }
                self.show_project_dialog(ui, rect);
            });

        // Latch transport running state for the next frame, to detect the
        // start edge (stopped -> running).
        let running = self.running.load(Ordering::Relaxed);
        // A fresh take starts a fresh overrun count. "Did this pass glitch" is
        // the actionable question; "has this session ever glitched" is not, and
        // a count that only ever grows stops meaning anything by mid-session.
        if running
            && !self.render.was_running_last_frame
            && let Some(load) = &self.audio_load
        {
            load.reset_counts();
        }
        self.render.was_running_last_frame = running;

        // ~30 fps while running. Deliberately a fixed timer, not
        // `request_repaint()` at the display refresh rate — that has been
        // tried and it makes the playhead vibrate. See "Rendering
        // Performance" in `030-ui-design.md`.
        //
        // A background-thread trigger (e.g. the performance lane, driven by
        // a physical MIDI key rather than a window input event) can flip
        // `running` true with nothing here to notice — that's handled by an
        // explicit `EventHandlers::request_repaint()` call at the trigger
        // site instead of polling for it here.
        if running {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if self.audio_load.is_some() {
            // Stopped, the UI only wakes for input, and the header's DSP
            // chip would freeze at whatever it read last — while live-played
            // plugins are still rendering. A slow wake keeps it current;
            // faster adds nothing readable (the average settles in ~¼ s).
            ctx.request_repaint_after(DSP_IDLE_REFRESH);
        }
        // The footer message: one wake when its hold ends, then the fade's
        // frames; dropped once it is gone.
        if let Some(status) = &self.render.status {
            match status.next_repaint(Instant::now()) {
                Some(after) => ctx.request_repaint_after(after),
                None => self.render.status = None,
            }
        }
        // Last, after everything this frame that can call into a plugin (the
        // editor pump in `logic`, an editor opened from a key press here):
        // eframe paints next and would not notice a plugin's GL context
        // left current (`MainGlContext`).
        #[cfg(target_os = "macos")]
        if let Some(gl) = &self.main_gl_context {
            gl.restore();
        }
    }

    /// On macOS this is the last thing the process does: it ends with
    /// [`exit_without_destructors`] rather than returning to AppKit's
    /// `terminate:` (which would call `exit()`) or to `main`. When reached
    /// via `terminate:` (Dock Quit, logout) the whole call is nested inside
    /// AppKit's own `-[NSApplication terminate:]` notification post (it runs
    /// off winit's `app_will_terminate`, an observer callback for that same
    /// post). Stopping the audio thread here is required — otherwise the
    /// audio callback can call into a plugin the process is about to
    /// unload.
    ///
    /// Full teardown is split in two:
    /// - `teardown_gui()` (GUI extension `destroy` + close our host `NSWindow`)
    ///   is safe and already runs constantly during ordinary per-track
    ///   teardown — leaving it out left a stale, un-closed `NSWindow`
    ///   registered in AppKit's window list at exit, which crashed later
    ///   (`-[NSApplication _indexOfWindow:]` reading a corrupted weak
    ///   reference while routing the next event — no Stev frames in that
    ///   stack, but it lined up with an editor window left open at quit).
    /// - The plugin *instance* itself (the editor's `!Send` half) is leaked,
    ///   not dropped: a JUCE-based plugin's full `destroy()` tears down
    ///   JUCE's `Desktop` singleton, which calls
    ///   `-[NSDistributedNotificationCenter removeObserver:...]` —
    ///   reentering CoreFoundation's notification-registrar machinery while
    ///   it is already mid-iteration on this same thread, which segfaults
    ///   (observed with Surge XT). `clack` already tolerates a leaked,
    ///   never-destroyed `PluginInstance` (see `130-plugin-host.md`).
    ///
    /// Ending without `exit()` matters because `exit()` runs every loaded
    /// module's static destructors, which tear down the leaked instances'
    /// runtimes still in use (Kontakt 8 and FM8 segfaulted on every quit).
    #[cfg(target_os = "macos")]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(handle) = &self.instruments.audio {
            handle.shutdown.request_and_wait();
        }
        for slot in &mut self.instruments.track_instruments {
            if let Some(mut loaded) = slot.take() {
                loaded.editor.teardown_gui();
                std::mem::forget(loaded.editor);
            }
        }
        for (_, mut editor) in self.instruments.pending_instance_drop.drain(..) {
            editor.teardown_gui();
            std::mem::forget(editor);
        }
        if let Some(staging) = self.instruments.staging.take() {
            staging.leak();
        }
        // `_exit` flushes nothing, so anything still buffered goes now.
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        exit_without_destructors(0)
    }
}

impl Display {
    /// Clears the `clip_frame_*` snapshots at the top of a draw call — they
    /// are repopulated by the sync passes, and a stale value must not carry
    /// across frames.
    pub(super) fn reset_clip_frame_snapshots(&mut self) {
        self.render.clip_frame_region_bounds = None;
        self.render.clip_frame_running = None;
        self.render.clip_frame_playback_tick = None;
        self.render.clip_frame_cursor_tick = None;
        self.render.clip_frame_scroll_x = None;
    }
}

/// The note (`(id, note, start, end)` in `notes`) covering grid point
/// `(note, tick)`, or `None` when the pointer is not over the grid
/// (`on_grid` false: the key column, the right padding). A zero-length note
/// still covers its start tick.
fn note_at_grid_point(
    mut notes: impl Iterator<Item = (Uuid, u8, i32, i32)>,
    on_grid: bool,
    note: u8,
    tick: i32,
) -> Option<Uuid> {
    if !on_grid {
        return None;
    }
    notes
        .find(|&(_, n, start, end)| n == note && tick >= start && tick < end.max(start + 1))
        .map(|(id, ..)| id)
}

/// Widest a note's edge zone gets, in screen pixels — see [`note_part_at`].
const NOTE_EDGE_HIT_PX: f32 = 5.0;

/// Which part of a note drawn from `left` to `right` (screen x) the pointer
/// at `x` is over: an edge within [`NOTE_EDGE_HIT_PX`] inside either end, the
/// body between. A zone is at most a third of the note's width, so a tiny
/// note keeps a body to move it by. Edges win over the body; the zones never
/// reach outside the note, so the empty grid beside it stays the grid.
fn note_part_at(x: f32, left: f32, right: f32) -> NotePart {
    let zone = NOTE_EDGE_HIT_PX.min((right - left) / 3.0);
    if x < left + zone {
        NotePart::Start
    } else if x >= right - zone {
        NotePart::End
    } else {
        NotePart::Body
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        ADD_TRACK_ROW_H, ARRANGER_LANE_SEAM_W, ARRANGER_MIN_LANE_H, ArrangerLayout, CLIP_HEADER_H,
        CLIP_V_INSET, TRACK_BTN_H, TRACK_BTN_W, TRACK_NAME_ROW_H, arranger_add_row_h,
        arranger_lane_h, arranger_max_scroll_y, arranger_scroll_y_to_show, clip_body_band,
        clip_resize_band, gap_spans, seam_y, tick_overlap, track_header_rects, track_name_row,
    };
    use super::{NotePart, note_at_grid_point, note_part_at};
    use crate::core::config::MAX_TRACKS;

    /// The physical-pixel span `[top, bottom)` a `width`-point line centred
    /// at `y` covers at `ppp`.
    fn physical_span(y: f32, width: f32, ppp: f32) -> (f32, f32) {
        let half = width * ppp / 2.0;
        (y * ppp - half, y * ppp + half)
    }

    #[test]
    fn seam_lands_on_whole_physical_pixels_at_any_width_and_scale() {
        for ppp in [1.0_f32, 1.5, 2.0, 3.0] {
            for width in [1.0, 1.5, 2.0] {
                // Only widths that are a whole number of physical pixels can
                // be crisp at all (1.5pt at 1× is 1.5px).
                if (width * ppp).fract() != 0.0 {
                    continue;
                }
                for k in 0..40 {
                    let y = 83.0 + k as f32 * 0.137;
                    let (top, bottom) = physical_span(seam_y(y, width, ppp), width, ppp);
                    assert_eq!(top.fract(), 0.0, "ppp {ppp}, width {width}, y {y}");
                    assert_eq!(bottom.fract(), 0.0, "ppp {ppp}, width {width}, y {y}");
                    // And it never moves the seam by more than a pixel.
                    assert!((seam_y(y, width, ppp) - y).abs() * ppp <= 1.0);
                }
            }
        }
    }

    #[test]
    fn retina_seam_widths_pick_centre_or_edge() {
        // 1.5pt at 2× = 3px (odd): centred on a pixel centre.
        assert_eq!(seam_y(10.3, 1.5, 2.0), 10.25);
        // 1pt at 2× = 2px (even): centred on a pixel edge.
        assert_eq!(seam_y(10.3, 1.0, 2.0), 10.5);
        // 1pt at 1× = 1px (odd): centred on a pixel centre.
        assert_eq!(seam_y(10.3, 1.0, 1.0), 10.5);
    }

    #[test]
    fn clip_body_band_insets_and_pixel_snaps() {
        // Whole-pixel lane: inset by CLIP_V_INSET top and bottom, snapped.
        let (top, bottom) = clip_body_band(100.0, 60.0);
        assert_eq!(top, (100.0 + CLIP_V_INSET).round());
        assert_eq!(bottom, (160.0 - CLIP_V_INSET).round());
    }

    #[test]
    fn clip_body_clears_the_lane_seams() {
        // The seam is centred on the lane boundary, so half of it sits inside
        // the lane; the clip body must start below it (and end above the next
        // one) with lane fill showing in between, at any fractional lane edge.
        let half_seam = ARRANGER_LANE_SEAM_W / 2.0;
        for k in 0..40 {
            let lane_y = 100.0 + k as f32 * 0.173;
            let lane_h = 59.6;
            let (top, bottom) = clip_body_band(lane_y, lane_h);
            assert!(
                top - (lane_y + half_seam) >= 0.5,
                "lane_y {lane_y}: top {top}"
            );
            assert!(
                (lane_y + lane_h - half_seam) - bottom >= 0.5,
                "lane_y {lane_y}: bottom {bottom}"
            );
        }
    }

    #[test]
    fn clip_body_band_bounds_are_always_whole_pixels() {
        // Fractional lane top/height (lane_h = viewport_h / track count rarely
        // divides evenly) must still land on integer pixel rows so clips in
        // adjacent lanes read as flush.
        let (top, bottom) = clip_body_band(100.37, 59.6);
        assert_eq!(top, top.round());
        assert_eq!(bottom, bottom.round());
        assert!(bottom > top);
    }

    #[test]
    fn lanes_share_the_viewport_until_the_minimum_height() {
        assert_eq!(arranger_lane_h(800.0, 4, 0.0), 200.0);
        assert_eq!(arranger_lane_h(800.0, 16, 0.0), ARRANGER_MIN_LANE_H);
        // The `+` row's height comes off the share.
        assert_eq!(arranger_lane_h(822.0, 4, ADD_TRACK_ROW_H), 200.0);
    }

    #[test]
    fn lanes_scroll_only_as_far_as_the_last_lane() {
        assert_eq!(arranger_max_scroll_y(800.0, 200.0, 4, 0.0), 0.0);
        let lane_h = arranger_lane_h(500.0, 16, 0.0);
        assert_eq!(
            arranger_max_scroll_y(500.0, lane_h, 16, 0.0),
            lane_h * 16.0 - 500.0
        );
        // With a `+` row, as far as its bottom.
        let lane_h = arranger_lane_h(500.0, 12, ADD_TRACK_ROW_H);
        assert_eq!(
            arranger_max_scroll_y(500.0, lane_h, 12, ADD_TRACK_ROW_H),
            lane_h * 12.0 + ADD_TRACK_ROW_H - 500.0
        );
    }

    #[test]
    fn lane_viewport_overshoot_points_the_way_to_scroll() {
        let layout = ArrangerLayout {
            performance_lane_top: 20.0,
            performance_lane_h: 30.0,
            lanes_top: 50.0,
            viewport_h: 400.0,
            scroll_y: 0.0,
            max_scroll_y: 600.0,
            lane_h: 72.0,
            track_count: 14,
            add_row_h: ADD_TRACK_ROW_H,
        };
        assert_eq!(layout.edge_overshoot(200.0), 0.0);
        assert_eq!(layout.edge_overshoot(40.0), 10.0);
        assert_eq!(layout.edge_overshoot(470.0), -20.0);
    }

    #[test]
    fn the_add_track_row_goes_at_the_track_cap() {
        assert_eq!(arranger_add_row_h(1), ADD_TRACK_ROW_H);
        assert_eq!(arranger_add_row_h(MAX_TRACKS - 1), ADD_TRACK_ROW_H);
        assert_eq!(arranger_add_row_h(MAX_TRACKS), 0.0);
    }

    #[test]
    fn revealing_a_lane_scrolls_only_as_far_as_needed() {
        let (viewport_h, lane_h) = (300.0, 100.0);
        // Lane 1 (100..200) is in view at scroll 0: nothing moves.
        assert_eq!(
            arranger_scroll_y_to_show(0.0, viewport_h, 100.0, lane_h),
            0.0
        );
        // Lane 4 (400..500) is below: its bottom lands on the viewport's bottom.
        assert_eq!(
            arranger_scroll_y_to_show(0.0, viewport_h, 400.0, lane_h),
            200.0
        );
        // With the `+` row under it, the row's bottom does.
        assert_eq!(
            arranger_scroll_y_to_show(0.0, viewport_h, 400.0, lane_h + ADD_TRACK_ROW_H),
            200.0 + ADD_TRACK_ROW_H
        );
        // Lane 0 is above a scrolled view: its top lands on the viewport's top.
        assert_eq!(
            arranger_scroll_y_to_show(250.0, viewport_h, 0.0, lane_h),
            0.0
        );
        // A lane taller than the viewport shows its top.
        assert_eq!(arranger_scroll_y_to_show(0.0, 50.0, 200.0, lane_h), 200.0);
    }

    #[test]
    fn clip_body_band_never_collapses_on_a_tiny_lane() {
        let (top, bottom) = clip_body_band(0.0, 4.0);
        assert!(bottom >= top + 1.0);
    }

    #[test]
    fn clip_resize_band_is_the_lane_top_strip() {
        let (top, bottom) = clip_resize_band(100.0, 150.0);
        assert_eq!(top, 100.0);
        assert_eq!(bottom, 100.0 + CLIP_HEADER_H);
    }

    #[test]
    fn clip_resize_band_never_exceeds_half_a_short_lane() {
        let (top, bottom) = clip_resize_band(0.0, 10.0);
        assert_eq!(top, 0.0);
        assert_eq!(bottom, 5.0);
    }

    #[test]
    fn controls_fit_and_dont_overlap_in_a_normal_lane() {
        let r = track_header_rects(100.0, 120.0, 12.0, 110.0).expect("should fit");
        // All inside the lane.
        assert!(r.name.min.y >= 100.0);
        assert!(r.pan.max.y <= 220.0);
        // Row order top→bottom: name, solo/mute, then volume, then pan, no
        // overlap.
        assert!(r.name.max.y <= r.solo.min.y);
        assert!(r.solo.max.y <= r.volume.min.y);
        assert!(r.volume.max.y <= r.pan.min.y);
        // S left of M, with the gap between them; the output chip right of
        // M, on the same row, out to the column's right edge.
        assert!(r.solo.max.x < r.mute.min.x);
        assert!(r.mute.max.x < r.output.min.x);
        assert_eq!(r.output.min.y, r.mute.min.y);
        assert_eq!(r.output.height(), TRACK_BTN_H);
        assert_eq!(r.output.max.x, 110.0);
        assert_eq!(r.solo.width(), TRACK_BTN_W);
        assert_eq!(r.mute.width(), TRACK_BTN_W);
        // Buttons left-aligned with the bars.
        assert_eq!(r.solo.min.x, r.volume.min.x);
        // The name row and the bars span the full column.
        assert_eq!((r.name.min.x, r.name.max.x), (12.0, 110.0));
        assert_eq!(r.name.height(), TRACK_NAME_ROW_H);
        assert_eq!(r.volume.min.x, 12.0);
        assert_eq!(r.pan.max.x, 110.0);
        // Vertically centred: equal margin above and below the stack.
        let above = r.name.min.y - 100.0;
        let below = 220.0 - r.pan.max.y;
        assert!((above - below).abs() < 0.01);
    }

    #[test]
    fn none_when_lane_too_short_or_column_too_narrow() {
        assert!(track_header_rects(0.0, 20.0, 12.0, 110.0).is_none());
        assert!(track_header_rects(0.0, 120.0, 12.0, 15.0).is_none());
    }

    #[test]
    fn the_whole_stack_fits_the_shortest_lane() {
        assert!(track_header_rects(0.0, ARRANGER_MIN_LANE_H, 12.0, 110.0).is_some());
    }

    #[test]
    fn the_name_row_is_the_last_of_the_header_to_go() {
        // In the stack when it fits.
        let stack = track_header_rects(0.0, 120.0, 12.0, 110.0).unwrap();
        assert_eq!(track_name_row(0.0, 120.0, 12.0, 110.0), Some(stack.name));
        // Alone and centred on a lane too short for the rest.
        assert!(track_header_rects(50.0, 30.0, 12.0, 110.0).is_none());
        let alone = track_name_row(50.0, 30.0, 12.0, 110.0).unwrap();
        assert_eq!(alone.height(), TRACK_NAME_ROW_H);
        assert!((alone.center().y - 65.0).abs() < 0.01);
        // Gone only when not even it fits.
        assert!(track_name_row(0.0, 12.0, 12.0, 110.0).is_none());
        assert!(track_name_row(0.0, 120.0, 12.0, 15.0).is_none());
    }

    #[test]
    fn a_point_in_each_control_is_inside_only_that_control() {
        let r = track_header_rects(0.0, 120.0, 12.0, 110.0).unwrap();
        for (name, rect) in [
            ("name", r.name),
            ("solo", r.solo),
            ("mute", r.mute),
            ("output", r.output),
            ("volume", r.volume),
            ("pan", r.pan),
        ] {
            let c = rect.center();
            let others = [r.name, r.solo, r.mute, r.output, r.volume, r.pan];
            let hits = others.iter().filter(|o| o.contains(c)).count();
            assert_eq!(hits, 1, "{name} centre hits {hits} rects");
        }
    }

    #[test]
    fn tick_overlap_partial_and_full_and_touching() {
        assert_eq!(tick_overlap(0, 100, 50, 150), Some((50, 100)));
        assert_eq!(tick_overlap(0, 100, 20, 80), Some((20, 80)));
        assert_eq!(tick_overlap(0, 100, -50, 200), Some((0, 100)));
        // Half-open: touching at an edge is not an overlap.
        assert_eq!(tick_overlap(0, 100, 100, 200), None);
        assert_eq!(tick_overlap(0, 100, -100, 0), None);
        // Disjoint.
        assert_eq!(tick_overlap(0, 100, 200, 300), None);
    }

    #[test]
    fn gap_spans_no_clips_is_the_whole_selection() {
        assert_eq!(gap_spans(&[], 0, 100), vec![(0, 100)]);
    }

    #[test]
    fn gap_spans_one_clip_fully_covering_the_selection_leaves_no_gap() {
        assert_eq!(gap_spans(&[(0, 200)], 50, 150), Vec::<(i32, i32)>::new());
    }

    #[test]
    fn gap_spans_clips_on_both_edges_leave_the_middle_gap() {
        // Clips [0,40) and [160,200) inside a [0,200) selection leave a
        // [40,160) gap.
        assert_eq!(gap_spans(&[(0, 40), (160, 200)], 0, 200), vec![(40, 160)]);
    }

    #[test]
    fn gap_spans_clip_fully_outside_the_selection_is_ignored() {
        assert_eq!(gap_spans(&[(500, 600)], 0, 100), vec![(0, 100)]);
    }

    #[test]
    fn gap_spans_unsorted_input_is_handled() {
        assert_eq!(gap_spans(&[(160, 200), (0, 40)], 0, 200), vec![(40, 160)]);
    }

    #[test]
    fn note_at_grid_point_hits_the_note_under_the_pointer() {
        let id = Uuid::new_v4();
        let notes = || [(id, 60, 0, 480), (Uuid::new_v4(), 64, 0, 480)].into_iter();
        assert_eq!(note_at_grid_point(notes(), true, 60, 240), Some(id));
        assert_eq!(note_at_grid_point(notes(), true, 60, 480), None);
        assert_eq!(note_at_grid_point(notes(), true, 62, 240), None);
    }

    /// Regression: a click on the key column hit the note scrolled
    /// underneath it.
    #[test]
    fn note_at_grid_point_ignores_a_pointer_off_the_grid() {
        let notes = [(Uuid::new_v4(), 60, 0, 480)].into_iter();
        assert_eq!(note_at_grid_point(notes, false, 60, 240), None);
    }

    #[test]
    fn note_part_at_tells_the_edges_from_the_body() {
        // A 100px note: 5px edge zones.
        assert_eq!(note_part_at(100.0, 100.0, 200.0), NotePart::Start);
        assert_eq!(note_part_at(104.9, 100.0, 200.0), NotePart::Start);
        assert_eq!(note_part_at(105.0, 100.0, 200.0), NotePart::Body);
        assert_eq!(note_part_at(194.9, 100.0, 200.0), NotePart::Body);
        assert_eq!(note_part_at(195.0, 100.0, 200.0), NotePart::End);
        assert_eq!(note_part_at(199.9, 100.0, 200.0), NotePart::End);
    }

    #[test]
    fn a_tiny_note_keeps_its_middle_third_as_body() {
        // A 9px note: 3px zones, so the middle 3px still move it.
        assert_eq!(note_part_at(102.9, 100.0, 109.0), NotePart::Start);
        assert_eq!(note_part_at(104.5, 100.0, 109.0), NotePart::Body);
        assert_eq!(note_part_at(106.0, 100.0, 109.0), NotePart::End);
    }
}
