//! Painting the piano roll (`Clip`): the note bars, the grid, the shading
//! outside the clip's window, and the velocity panel at the bottom. See
//! `030-ui-design.md`.

use std::collections::HashMap;

use uuid::Uuid;

use crate::models::clip::NoteBounds;

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Stroke, StrokeKind, pos2, vec2};

use super::piano_keys::NoteAreaGeom;
use super::*;

impl Display {
    /// How strongly a note is drawn when it doesn't play — muted, or kept
    /// outside the clip's window (`220-capture-without-pending-view.md`).
    const SILENT_NOTE_SCALE: f32 = 0.35;

    /// Alpha of the background-tone shade laid over the note grid outside the
    /// clip's window, so kept material reads as not playing.
    const OUTSIDE_WINDOW_SHADE_ALPHA: f32 = 0.55;

    /// Breathing room above/below each event bar within its row, per side.
    /// Small and fixed (not proportional) so events read as nearly filling
    /// the row — enough gap to still see the row boundary/grid line, not
    /// enough to look like a thin band floating in the middle of the row.
    const EVENT_V_PADDING: f32 = 1.5 * piano_keys::PIANO_ROLL_SCALE;

    /// Gap between the timeline strip and the top of the note area: a small
    /// breathing gap keeps the piano roll from starting flush against it.
    const NOTE_AREA_TOP_GAP: f32 = 6.0;

    /// Height of the velocity panel drawn at the bottom of the piano roll —
    /// always shown, whether or not an event is selected (display only: there
    /// is no velocity-editing key yet, see `010-keybindings.md`).
    const VELOCITY_PANEL_H: f32 = 80.0;

    /// Note-row viewport geometry shared by rendering (`draw_clip_view`) and
    /// mouse hit-testing (`Display::event_id_at`) — kept in one place so the
    /// two can never drift apart. Placed in the clip pane through
    /// `track_area_top`/`track_area_h`; like them, relative to the canvas
    /// origin. Clip pane only.
    pub(super) fn note_area_geom(&self) -> NoteAreaGeom {
        let clip_area_top = self.track_area_top() + Self::TIMELINE_H;
        let clip_area_h = self.track_area_h() - Self::TIMELINE_H;

        let note_area_top = clip_area_top + Self::NOTE_AREA_TOP_GAP;
        let note_area_h = clip_area_h - Self::NOTE_AREA_TOP_GAP - Self::VELOCITY_PANEL_H;

        // Rows fitted to the home note range saved when the clip opened (or
        // the user's key-column zoom), centred on the user's vertical scroll
        // (or home) — never on the clip's current notes, so an edit can't
        // move the grid. See
        // `NoteAreaGeom::framed`.
        NoteAreaGeom::framed(
            note_area_top,
            note_area_h,
            self.clip_home_notes(),
            self.render.clip_centre_row,
            self.render.clip_row_h,
        )
    }

    /// Scrolls the piano roll vertically by a wheel / trackpad delta
    /// (`NoteAreaGeom::scrolled_centre_row`). User-owned: nothing else moves
    /// the view off home except the edits' own gestures. Clip pane only.
    pub(in crate::view::display) fn scroll_note_area_by(&mut self, delta_y: f32) {
        if !self.is_pane_visible(Pane::Clip) || delta_y == 0.0 {
            return;
        }
        self.render.clip_centre_row = Some(self.note_area_geom().scrolled_centre_row(delta_y));
    }

    /// A capture commit put a take of `pitch_range` into clip `clip_id`
    /// (`UiEvent::CaptureInserted`): if it is the open clip and any of the
    /// take's rows is out of view, re-frames the piano roll vertically as on
    /// opening the clip (`latch_clip_home_notes` — new home range, rows
    /// refitted, centred). A take already in view moves nothing. Decided from
    /// what is on screen, never from whether the user has scrolled (`220`: no
    /// stored mode-like state). The time axis is left alone.
    pub(in crate::view::display) fn reframe_after_capture(
        &mut self,
        clip_id: Uuid,
        pitch_range: (u8, u8),
    ) {
        if !self.is_open_clip(clip_id) {
            return;
        }
        self.in_pane(Pane::Clip, |d| {
            if !d.note_area_geom().shows_rows(pitch_range) {
                d.latch_clip_home_notes();
            }
        });
    }

    /// Draws alternating note-lane backgrounds and the B/C + E/F lane
    /// separators (zone 3, under everything else) so the grid reads as a
    /// piano roll rather than a blank canvas with lines on it. Called
    /// before `draw_timeline` so the bar/beat/16th grid lines composite on
    /// top of *both* the lane fills and these horizontal separators at
    /// their intersections, instead of the separators overlapping the
    /// vertical grid lines.
    ///
    /// Per the "Grid Hierarchy" convention, this uses RGB arithmetic on
    /// `theme::bg()` rather than alpha blending: white-key rows get a subtle
    /// brightness bump, black-key rows are left at the base `bg()` already
    /// painted by the canvas clear (matching how physical black keys read
    /// darker than white keys).
    pub(super) fn draw_note_lane_backgrounds(&self, painter: &Painter, geom: &NoteAreaGeom) {
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        // Left-gutter panel — the legend + piano-key columns *and the strip
        // corner above them*, filled as one continuous
        // `bg_panel().gamma_multiply(0.8)` panel from the top of the timeline
        // strip down through the clip area, exactly as the arranger's track
        // column owns its strip corner. `draw_timeline` stops its strip fill
        // at `content_x`, and this runs first, so the panel owns everything
        // left of the note grid; `draw_piano_keys`'s `grid_seam_color()` edge
        // at `content_x` closes it. No `TRACK_COLUMN_GRID_GAP_X`-style canvas
        // gap here — the piano keys must sit flush against the note grid so a
        // note lines up with its key.
        painter.rect_filled(
            Rect::from_min_size(
                pos2(0.0, self.track_area_top()),
                vec2(content_x, self.track_area_h()),
            ),
            CornerRadius::ZERO,
            theme::bg_panel().gamma_multiply(0.8),
        );

        let note_area_rect = Rect::from_min_size(
            pos2(content_x, geom.top),
            vec2(content_right - content_x, geom.h),
        );
        let p = painter.with_clip_rect(note_area_rect);

        let white_lane_color = shifted_rgb(theme::bg(), 7);

        for nn in geom.note_lo..=geom.note_hi {
            if piano_keys::is_black_key(piano_keys::pitch_class(nn)) {
                continue;
            }
            let y_top = geom.row_top(nn);
            let h = geom.row_h;
            p.rect_filled(
                Rect::from_min_size(pos2(content_x, y_top), vec2(content_right - content_x, h)),
                CornerRadius::ZERO,
                white_lane_color,
            );
        }

        // Velocity lane — the DAW-standard treatment: the same background tone
        // as the white-key note lanes, so it reads as part of the piano-roll
        // editing surface rather than a separate readout. Filled on the raw
        // `painter` (outside the note-area clip) here, *before* `draw_timeline`,
        // so the vertical bar / beat / 16th grid lines — which already run down
        // to `track_area_bottom()` — composite through it.
        painter.rect_filled(
            Rect::from_min_size(
                pos2(content_x, geom.top + geom.h),
                vec2(content_right - content_x, Self::VELOCITY_PANEL_H),
            ),
            CornerRadius::ZERO,
            white_lane_color,
        );

        // Both horizontal separators are the shared `grid_seam_color()`
        // groove tone (darker than the base bg, full alpha), not gray lines —
        // a translucent gray reads as a competing grid tier no matter how
        // dimmed, the recessed-channel tone reads as a groove. Same helper the
        // arranger's lane seams and the piano-key column seams use, so the
        // treatment is identical across every view. Hierarchy is by width:
        // the **B/C octave boundary** is 2px (it's the primary pitch
        // reference, the analogue of the arranger's track seams); **E/F** is
        // 1px (a minor adjacent-white-key seam).
        let seam_color = grid_seam_color();

        // `.round()` the seam y so the stroke lands on a pixel boundary and
        // renders at a consistent thickness — the legend and key columns
        // continue these exact lines and snap the same way.
        // The top of B's row is exactly the bottom of the next octave's C;
        // E and F are the other pair of adjacent white keys with no black key
        // between them — without a line they'd read as one solid
        // double-height lane (`row_seam_w`).
        for nn in geom.note_lo..=geom.note_hi {
            if let Some(seam_w) = piano_keys::row_seam_w(nn) {
                p.hline(
                    content_x..=content_right,
                    geom.row_top(nn).round(),
                    Stroke::new(seam_w, seam_color),
                );
            }
        }
    }

    /// Paints the whole piano roll into `rect`: grid, note bars, cursor,
    /// velocity panel.
    pub(super) fn draw_clip_view(&self, painter: &Painter, rect: Rect, geom: &NoteAreaGeom) {
        let ppt = self.pixels_per_tick();

        self.draw_piano_keys(painter, rect, geom);
        self.draw_octave_legends(painter, rect, geom);

        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        // Backstop, independent of the row-height/range math above: even if
        // some future edge case gets the geometry wrong, events clip to the
        // note area instead of bleeding into the timeline or off-screen —
        // matching the `with_clip_rect` piano_keys already uses for keys,
        // legends, and lane backgrounds.
        let note_area_rect = Rect::from_min_size(
            pos2(content_x, geom.top),
            vec2(content_right - content_x, geom.h),
        );
        let event_painter = painter.with_clip_rect(note_area_rect);

        // The lead clip's window: shade the grid outside it.
        let window @ (region_start, region_end) = self.region_bounds_for_render();
        let origin_x = content_x - self.render.clip_scroll_x;
        let window_start_x = region_start as f32 * ppt + origin_x;
        let window_end_x = region_end as f32 * ppt + origin_x;
        let shade = theme::bg().gamma_multiply(Self::OUTSIDE_WINDOW_SHADE_ALPHA);
        for (left, right) in [(content_x, window_start_x), (window_end_x, content_right)] {
            if right > left {
                event_painter.rect_filled(
                    Rect::from_min_max(pos2(left, geom.top), pos2(right, geom.top + geom.h)),
                    CornerRadius::ZERO,
                    shade,
                );
            }
        }

        // A live note drag: each dragged note is drawn only where it will
        // land, not at its old place too — after the others, so it stays on
        // top.
        let landing: HashMap<Uuid, NoteBounds> = self
            .gesture
            .note_drag
            .as_ref()
            .filter(|drag| drag.dragging)
            .map(|drag| {
                self.note_drag_preview(&drag.origin, drag.drag)
                    .into_iter()
                    .map(|(id, _, after)| (id, after))
                    .collect()
            })
            .unwrap_or_default();
        let mut dragged: Vec<(NoteBounds, bool)> = Vec::with_capacity(landing.len());
        let note_color = self.lead_clip_color();

        for shape in &self.render.event_shapes {
            if let Some(&bounds) = landing.get(&shape.event_id()) {
                dragged.push((bounds, shape.is_muted()));
                continue;
            }
            let Some(note_rect) = self.note_bar_rect(
                geom,
                shape.start_tick(),
                shape.end_tick(),
                shape.note_number(),
            ) else {
                continue;
            };

            // A note that doesn't play — muted, or kept outside the window —
            // reads dimmer, same intent as a muted clip's `note_alpha` in
            // the arranger — the selected tint stays visible (it's still the
            // lead selection) but toned down. The whole note body is the
            // selection indicator here (no separate border), so dimming it
            // rather than dropping it avoids the note reading as silently
            // deselected.
            let mute_scale = if shape.is_muted() || is_outside_window(shape.start_tick(), window) {
                Self::SILENT_NOTE_SCALE
            } else {
                1.0
            };

            if shape.is_selected() {
                event_painter.rect_filled(
                    note_rect,
                    CornerRadius::ZERO,
                    theme::accent().gamma_multiply(mute_scale),
                );
            } else {
                // Solid base so grid lines never bleed through.
                event_painter.rect_filled(note_rect, CornerRadius::ZERO, theme::bg_panel());
                // Velocity-driven tint layer: pianissimo ~30%, fortissimo ~95%.
                let vel_alpha = (0.30 + (shape.velocity() as f32 / 127.0) * 0.65) * mute_scale;
                event_painter.rect_filled(
                    note_rect,
                    CornerRadius::ZERO,
                    note_color.gamma_multiply(vel_alpha),
                );
            }
        }

        for ((start, end, note), muted) in dragged {
            let Some(note_rect) = self.note_bar_rect(geom, start, end, note) else {
                continue;
            };
            let scale = if muted || is_outside_window(start, window) {
                Self::SILENT_NOTE_SCALE
            } else {
                1.0
            };
            event_painter.rect_filled(
                note_rect,
                CornerRadius::ZERO,
                theme::accent().gamma_multiply(scale),
            );
        }

        self.draw_event_marquee(painter, geom);
        self.draw_velocity_panel(painter, geom.top + geom.h, Self::VELOCITY_PANEL_H);
    }

    /// Screen rect of a note bar from `start` to `end` (event ticks) on row
    /// `note`, cut to the visible grid; `None` when it is off the visible
    /// rows or scrolled out of view. Shared by the notes and the drag
    /// preview so the two draw alike.
    fn note_bar_rect(&self, geom: &NoteAreaGeom, start: i32, end: i32, note: u8) -> Option<Rect> {
        if note < geom.note_lo || note > geom.note_hi {
            return None;
        }
        let (content_x, content_right) = (self.content_origin_x(), self.content_right_x());
        let original_width = (end - start).max(1) as f32 * self.pixels_per_tick();
        let original_x = self.tick_to_x(start);
        if original_x + original_width < content_x || original_x > content_right {
            return None;
        }

        let x = original_x.max(content_x);
        let end_x = (original_x + original_width).min(content_right);
        let width = (end_x - x).max(2.0);
        // Same fixed height for every event, centered in its row — rows
        // themselves vary (see `note_area_geom`'s `row_h`, which shrinks
        // below `KEY_ROW_H` when the visible range doesn't fit), but the
        // padding around each event scales down with it rather than
        // being clamped away, so events still read as sitting inside a
        // row rather than filling it edge-to-edge.
        let h = (geom.row_h - 2.0 * Self::EVENT_V_PADDING).max(1.0);
        let note_y = geom.row_top(note) + (geom.row_h - h) * 0.5;
        Some(Rect::from_min_size(pos2(x, note_y), vec2(width, h)))
    }

    /// Draws the in-progress event marquee-select rectangle over the piano
    /// roll — a literal filled+bordered box, unlike the timeline-strip
    /// arrow-glyph style `draw_time_selection` uses for the arranger's 1D
    /// time selection (that style doesn't apply to a genuinely 2D box).
    fn draw_event_marquee(&self, painter: &Painter, geom: &NoteAreaGeom) {
        let Some((tick_min, tick_max, row_min, row_max)) = self.gesture.event_marquee_rect else {
            return;
        };

        let x_min = self.tick_to_screen_x(tick_min).max(self.content_origin_x());
        let x_max = self.tick_to_screen_x(tick_max).min(self.content_right_x());
        if x_max <= x_min {
            return;
        }

        // The rows are content space (see `event_marquee_rect`), so the box
        // stays on its notes as the view scrolls; clamped to the note-area
        // viewport so it doesn't paint over the timeline or velocity panel.
        let y_top = geom.row_to_screen_y(row_min).max(geom.top);
        let y_bottom = geom.row_to_screen_y(row_max).min(geom.top + geom.h);
        if y_bottom <= y_top {
            return;
        }

        let marquee_rect = Rect::from_min_max(pos2(x_min, y_top), pos2(x_max, y_bottom));
        painter.rect_filled(
            marquee_rect,
            CornerRadius::ZERO,
            theme::accent().gamma_multiply(0.18),
        );
        painter.rect_stroke(
            marquee_rect,
            CornerRadius::ZERO,
            Stroke::new(1.0_f32, theme::accent().gamma_multiply(0.85)),
            StrokeKind::Outside,
        );
    }

    /// Velocity readout for every `NoteOn` event — one bar each, height
    /// proportional to velocity. Display only for now: there is no
    /// velocity-editing key yet (see `010-keybindings.md`), so nothing here
    /// responds to input.
    fn draw_velocity_panel(&self, painter: &Painter, panel_top: f32, panel_h: f32) {
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        // Background and the vertical bar/beat/16th grid lines running through
        // this lane are painted upstream (`draw_note_lane_backgrounds` fills it
        // with the white-key lane tone before `draw_timeline` extends the grid
        // down into it) — the DAW-standard velocity lane, time-aligned with the
        // piano roll above it and sharing its surface. Only the note-grid ↔
        // velocity-lane divider and the bars themselves are drawn here.

        // Divider — `grid_seam_color()` groove at 2px, the same lane-boundary
        // treatment as the note grid's B/C octave seam and the arranger's
        // track seams; with the lane now sharing the note-grid background it
        // is the only thing separating the two.
        painter.hline(
            content_x..=content_right,
            panel_top,
            Stroke::new(2.0_f32, grid_seam_color()),
        );

        // Draw bars — one per NoteOn event
        const BAR_W: f32 = 4.0;

        let window = self.region_bounds_for_render();
        let note_color = self.lead_clip_color();
        for shape in &self.render.event_shapes {
            let bar_center_x = self.tick_to_screen_x(shape.start_tick());
            if bar_center_x + BAR_W * 0.5 < content_x || bar_center_x - BAR_W * 0.5 > content_right
            {
                continue;
            }

            let velocity = shape.velocity() as f32;
            let bar_h = ((velocity / 127.0) * panel_h).max(2.0).round();
            let bar_x = (bar_center_x - BAR_W * 0.5).max(content_x).round();
            let bar_y = (panel_top + panel_h - bar_h).round();

            let bar_color = if shape.is_selected() {
                theme::accent()
            } else {
                note_color
            };
            let bar_color = if is_outside_window(shape.start_tick(), window) {
                bar_color.gamma_multiply(Self::SILENT_NOTE_SCALE)
            } else {
                bar_color
            };

            painter.rect_filled(
                Rect::from_min_size(pos2(bar_x, bar_y), vec2(BAR_W, bar_h)),
                CornerRadius::ZERO,
                bar_color,
            );
        }

        // "VEL" label flush right inside the panel
        painter.text(
            pos2(content_right - 4.0, panel_top + 4.0),
            Align2::RIGHT_TOP,
            "VEL",
            FontId::proportional(10.0),
            theme::fg_dim(),
        );
    }
}

/// Whether a note starting at `start_tick` lies outside `window` (half-open)
/// and so never plays.
fn is_outside_window(start_tick: i32, (start, end): (i32, i32)) -> bool {
    !(start..end).contains(&start_tick)
}

#[cfg(test)]
mod tests {
    use super::is_outside_window;

    #[test]
    fn notes_outside_the_half_open_window_are_silent() {
        let window = (960, 1920);
        assert!(is_outside_window(100, window), "before the window");
        assert!(!is_outside_window(960, window), "at its start");
        assert!(!is_outside_window(1919, window));
        assert!(is_outside_window(1920, window), "at its end: half-open");
    }
}
