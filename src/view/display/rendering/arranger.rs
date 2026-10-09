//! Painting the Arranger: the bar grid, the clip rectangles with their note
//! thumbnails, the reserved performance-lane row, and the left track-header
//! column (per-track colour swatch, the track's name, S/M buttons and the
//! output chip, volume/pan bars for instrument tracks). See `030-ui-design.md`.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, CornerRadius, FontId, Painter, Rect, Stroke, pos2, vec2};

use crate::core::audio::mix;

use super::output_menu::{draw_hover_wash, draw_truncated};
use super::*;
use crate::view::display::track_rename::{TRACK_NAME_FONT_SIZE, track_number_label};

impl Display {
    /// Paints the arranger lane fills, gutter, per-track header chrome and the
    /// groove seams between tracks. Called from `rendering/mod.rs` **before**
    /// `draw_timeline`, the same discipline as `draw_note_lane_backgrounds` in
    /// clip view: the lane fills are opaque, so the vertical bar lines have to
    /// be painted on top of them, not under. Clip bodies are drawn afterward
    /// by `draw_arranger_view`.
    pub(super) fn draw_arranger_backgrounds(&self, painter: &Painter, rect: Rect) {
        let layout = self.arranger_layout();
        let performance_lane_top = rect.min.y + layout.performance_lane_top;
        let performance_lane_h = layout.performance_lane_h;
        let lane_h = layout.lane_h;

        let labels_right = self.content_origin_x() - Self::TRACK_COLUMN_GRID_GAP_X;
        let content_x = self.content_origin_x();
        let content_w = self.content_w();
        let sw = rect.width();

        // Track-column panel — one continuous fill from the top of the
        // timeline strip, through the reserved performance-lane row, down every
        // track lane. The whole left column then reads as one panel: the
        // timeline's `bg_timeline` fill is confined to the content area
        // (`draw_timeline` starts it at `content_x`), and the corner above the
        // performance lane is column, not a stray patch of ruler colour. The
        // performance-lane / per-track gutter overlays drawn below sit on top
        // of this and land at the same brightness as before.
        let column_top = performance_lane_top - Self::TIMELINE_H;
        // The lanes always fill the viewport (`arranger_lane_h`), so the
        // column runs to its bottom whatever the scroll.
        let column_h = Self::TIMELINE_H + performance_lane_h + layout.viewport_h;
        painter.rect_filled(
            Rect::from_min_size(pos2(rect.min.x, column_top), vec2(labels_right, column_h)),
            CornerRadius::ZERO,
            theme::bg_panel().gamma_multiply(0.8),
        );

        // Track-column right edge — same `grid_seam_color()` groove tone as
        // the horizontal lane seams and the bar lines, so the column reads as
        // recessed into the surface on its grid side rather than fenced off
        // with a lighter `separator()` rule. The 8px canvas gap
        // (`TRACK_COLUMN_GRID_GAP_X`) beyond it does most of the separating;
        // this is just the lip. Runs the full height of the column panel,
        // including the strip corner.
        painter.vline(
            labels_right,
            column_top..=(column_top + column_h),
            Stroke::new(1.0_f32, grid_seam_color()),
        );

        // The header column with the keyboard: the docked panes' focus edge
        // (`draw_pane_split`), across the top of the column only.
        if self.track_headers_have_keyboard() {
            painter.hline(
                rect.min.x..=(rect.min.x + labels_right),
                column_top,
                focus_edge_stroke(),
            );
        }

        self.draw_performance_lane_row(
            painter,
            rect,
            performance_lane_top,
            performance_lane_h,
            labels_right,
        );

        let painter = &layout.lanes_painter(painter, rect);
        for track_idx in 0..layout.track_count {
            let lane_y = rect.min.y + layout.lane_top(track_idx);
            let accent = self.track_color(track_idx);
            let is_selected_track = track_idx == self.selected_track_idx;

            // One common lane shade for every unselected track — the old
            // even/odd alternation read as distracting zebra banding, and the
            // selected track's accent fill + 4px left marker is enough
            // distinction on its own.
            let lane_fill = lane_grid_fill(is_selected_track, accent);
            let gutter_fill = if is_selected_track {
                accent.gamma_multiply(0.18)
            } else {
                theme::bg_panel().gamma_multiply(0.16)
            };

            // Lane content fill
            painter.rect_filled(
                Rect::from_min_size(pos2(content_x, lane_y), vec2(content_w, lane_h)),
                CornerRadius::ZERO,
                lane_fill,
            );

            // Gutter fill
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.min.x, lane_y), vec2(labels_right, lane_h)),
                CornerRadius::ZERO,
                gutter_fill,
            );

            if is_selected_track {
                // Bold left-edge marker for selected track
                painter.rect_filled(
                    Rect::from_min_size(pos2(rect.min.x, lane_y), vec2(4.0, lane_h)),
                    CornerRadius::ZERO,
                    accent,
                );
            }

            // Laid out once per lane; the name row may show alone on a lane
            // too short for the rest.
            match self.track_header_rects(track_idx) {
                Some(rects) => {
                    self.draw_track_name(painter, track_idx, rects.name);
                    self.draw_track_header_buttons(painter, track_idx, &rects);
                    if self.track_has_instrument(track_idx) {
                        self.draw_track_mix_bars(painter, track_idx, accent, &rects);
                    }
                }
                None => {
                    if let Some(row) = self.track_name_rect(track_idx) {
                        self.draw_track_name(painter, track_idx, row);
                    }
                }
            }
        }

        // Lane separator seams — thin, at full groove strength: they outrank
        // the translucent 1px bar lines mostly by strength (see
        // `ARRANGER_LANE_SEAM_W`).
        for track_idx in 1..layout.track_count {
            let sep_y = seam_y(
                rect.min.y + layout.lane_top(track_idx),
                ARRANGER_LANE_SEAM_W,
                painter.pixels_per_point(),
            );
            painter.hline(
                rect.min.x..=(rect.min.x + sw),
                sep_y,
                Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
            );
        }

        self.draw_add_track_row(painter, rect, &layout);
    }

    /// The slim `+` row under the last lane (gone at `MAX_TRACKS`): a seam
    /// closing the last lane, then a dim `+` centred in the header column,
    /// washed and brightened under the pointer. A click adds a track at the
    /// end (`handle_mouse_click`). `painter` is the lanes painter.
    fn draw_add_track_row(&self, painter: &Painter, rect: Rect, layout: &ArrangerLayout) {
        if layout.add_row_h <= 0.0 {
            return;
        }
        let sep_y = seam_y(
            rect.min.y + layout.add_row_top(),
            ARRANGER_LANE_SEAM_W,
            painter.pixels_per_point(),
        );
        painter.hline(
            rect.min.x..=rect.max.x,
            sep_y,
            Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
        );
        let Some(row) = self.add_track_row_rect() else {
            return;
        };
        let hovered = self.canvas_pointer().is_some_and(|p| row.contains(p));
        if hovered {
            draw_hover_wash(painter, row);
        }
        painter.text(
            row.center(),
            Align2::CENTER_CENTER,
            "+",
            FontId::proportional(15.0),
            if hovered {
                theme::fg()
            } else {
                theme::fg_dim()
            },
        );
    }

    /// Opacity of the band-drag ghost — the dragged clip drawn once more at
    /// its would-be drop position, translucent so whatever it hovers over
    /// shows through. The one sanctioned use of alpha on a whole clip body
    /// (`030-ui-design.md` § Clip Anatomy). Tune by eye.
    const CLIP_GHOST_ALPHA: f32 = 0.45;

    /// Renders all visible clip shapes within the content area, then the
    /// band-drag ghost on top of everything. Called from `rendering/mod.rs`
    /// **after** `draw_timeline` so clip bodies sit on top of the grid; the
    /// lane fills / seams / header chrome they sit within are painted earlier
    /// by `draw_arranger_backgrounds`.
    ///
    /// The ghost is the only drag feedback: while a band drag is live the
    /// clip keeps drawing at its real position at full opacity — that *is*
    /// still the data, nothing moves until release — and a translucent copy
    /// follows the pointer at the snapped drop position. Drawn last so it
    /// composites over whatever it hovers. A `.mid` dragged in for import
    /// gets the same ghost, of the clip it will become.
    pub(super) fn draw_arranger_view(&self, painter: &Painter, rect: Rect) {
        let track_count = self.track_count();
        for shape in &self.render.clip_shapes {
            let track_idx = shape.track_idx();
            if track_idx >= track_count {
                continue;
            }

            let end_tick = if self.is_recording_clip(shape) {
                self.playback_tick()
            } else {
                shape.end_tick()
            };

            self.draw_clip_at(
                painter,
                rect,
                shape,
                shape.start_tick(),
                end_tick,
                None,
                track_idx,
                1.0,
            );
        }

        if let Some(drag) = self.gesture.clip_move_drag.filter(|drag| drag.dragging) {
            self.draw_clip_move_ghost(painter, rect, &drag);
        }
        if let Some(track_idx) = self.gesture.plugin_drag.as_ref().and_then(|d| d.target) {
            self.draw_plugin_drop_target(painter, rect, track_idx);
        }
        if let Some((ghost, track_idx, tick)) =
            self.gesture.midi_drag.as_ref().and_then(MidiDrag::ghost_at)
        {
            self.draw_clip_at(
                painter,
                rect,
                ghost,
                tick,
                tick + ghost.end_tick(),
                None,
                track_idx,
                Self::CLIP_GHOST_ALPHA,
            );
        }
    }

    /// The track a dragged plugin would land on: an accent wash over its
    /// whole row, header and lane, under the pointer's grabbing hand.
    fn draw_plugin_drop_target(&self, painter: &Painter, rect: Rect, track_idx: usize) {
        let layout = self.arranger_layout();
        let top = rect.min.y + layout.lane_top(track_idx);
        let row = Rect::from_min_max(pos2(rect.min.x, top), pos2(rect.max.x, top + layout.lane_h));
        painter.rect_filled(
            row,
            CornerRadius::ZERO,
            theme::accent().gamma_multiply(0.12),
        );
    }

    /// The band-drag ghost: every clip in the dragged block, translated by
    /// the drag's tick and lane delta at `CLIP_GHOST_ALPHA`. A whole-clip
    /// drag is that one clip; a marquee drag is every clip on the marqueed
    /// lanes overlapping the marquee's ticks, each painted through
    /// `draw_clip_at`'s `window` so only its marqueed piece shows — with
    /// the note thumbnails still lined up to where they sit inside the
    /// clip, exactly as the split-out piece will look once dropped.
    fn draw_clip_move_ghost(&self, painter: &Painter, rect: Rect, drag: &ClipMoveDrag) {
        let shift = drag.delta_ticks();
        if drag.kind == ClipMoveKind::Marquee {
            self.draw_marquee_ghost_frame(painter, rect, drag);
        }
        for shape in &self.render.clip_shapes {
            let in_block = match drag.kind {
                ClipMoveKind::Clip(clip_id) => shape.matches(drag.track_start, clip_id),
                ClipMoveKind::Marquee => {
                    (drag.track_start..=drag.track_end).contains(&shape.track_idx())
                        && tick_overlap(
                            shape.start_tick(),
                            shape.end_tick(),
                            drag.start_tick,
                            drag.end_tick,
                        )
                        .is_some()
                }
            };
            if !in_block {
                continue;
            }
            let window = (
                shape.start_tick().max(drag.start_tick) + shift,
                shape.end_tick().min(drag.end_tick) + shift,
            );
            self.draw_clip_at(
                painter,
                rect,
                shape,
                shape.start_tick() + shift,
                shape.end_tick() + shift,
                Some(window),
                (shape.track_idx() as i32 + drag.delta_tracks) as usize,
                Self::CLIP_GHOST_ALPHA,
            );
        }
    }

    /// The marquee drag's *rectangle* at its would-be drop position: the
    /// whole block's bounds — full lane-row height across every lane it
    /// spans — as a light fill in the marquee's own neutral tint, painted
    /// under the ghost clips. The ghost clips alone only show the pieces; a
    /// selection with gaps in it, or one reaching past its clips, would
    /// otherwise lose its shape mid-drag. The source marquee stays tinted
    /// where it is, so both "from" and "to" read at once — the convention
    /// Logic / Bitwig / Live all follow for a dragged selection. Same
    /// `theme::fg_dim()` base as the marquee tint so it reads as *the
    /// selection*, moving. Fill only — an outline on top of it was tried
    /// and read as clutter against the ghost clips' own edges.
    fn draw_marquee_ghost_frame(&self, painter: &Painter, rect: Rect, drag: &ClipMoveDrag) {
        let layout = self.arranger_layout();
        let clip_area_top = rect.min.y + layout.tracks_top();
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        let shift = drag.delta_ticks();
        let x = self
            .tick_to_screen_x(drag.start_tick + shift)
            .max(content_x);
        let end_x = self
            .tick_to_screen_x(drag.end_tick + shift)
            .min(content_right);
        if end_x <= x {
            return;
        }
        let top =
            clip_area_top + (drag.track_start as i32 + drag.delta_tracks) as f32 * layout.lane_h;
        let bottom =
            clip_area_top + (drag.track_end as i32 + drag.delta_tracks + 1) as f32 * layout.lane_h;

        let frame = Rect::from_min_max(pos2(x.round(), top), pos2(end_x.round(), bottom));
        painter.rect_filled(
            frame,
            CornerRadius::ZERO,
            theme::fg_dim().gamma_multiply(0.10),
        );
    }

    /// Paints `shape`'s body at an explicit `[start_tick, end_tick)` × lane —
    /// its own position for the normal pass, the drop position for the
    /// band-drag ghost — at `alpha` (1.0 = opaque). `window` narrows what is
    /// actually painted to that absolute tick sub-span (the band-drag ghost
    /// of a marqueed piece) while the thumbnail math keeps using the full
    /// `[start_tick, end_tick)` extent; `None` paints the whole clip.
    /// Resolves the lane band, the pixel-snapped horizontal extent and the
    /// content-area clipping, then hands off to `draw_clip_body`.
    #[allow(clippy::too_many_arguments)]
    fn draw_clip_at(
        &self,
        painter: &Painter,
        rect: Rect,
        shape: &ClipShape,
        start_tick: i32,
        end_tick: i32,
        window: Option<(i32, i32)>,
        track_idx: usize,
        alpha: f32,
    ) {
        let pixels_per_tick = self.pixels_per_tick();
        let layout = self.arranger_layout();
        let clip_area_top = rect.min.y + layout.tracks_top();
        let lane_h = layout.lane_h;

        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        let original_width = (end_tick - start_tick) as f32 * pixels_per_tick;
        if original_width <= 0.0 {
            return;
        }
        let lane_y = clip_area_top + track_idx as f32 * lane_h;
        let (clip_y, clip_bottom) = clip_body_band(lane_y, lane_h);
        let clip_h = clip_bottom - clip_y;
        let original_x = self.tick_to_x(start_tick);

        let (window_start, window_end) = window.unwrap_or((start_tick, end_tick));
        let window_x = original_x + (window_start - start_tick) as f32 * pixels_per_tick;
        let window_end_x = original_x + (window_end - start_tick) as f32 * pixels_per_tick;

        if window_end_x < content_x || window_x > content_right {
            return;
        }

        // Snap the drawn body edges to whole pixels. A fractional edge gets
        // antialiased across two pixel columns, and one clip's tint/header
        // then bleeds a faint sliver onto the column its right-hand
        // neighbour also occupies — visible on roughly every other clip as
        // `start_tick * pixels_per_tick` beats against the pixel grid. Two
        // clips whose ticks touch now round to the same boundary.
        // `original_x` / `original_width` stay unrounded for the
        // note-thumbnail math.
        let x = window_x.max(content_x).round();
        let end_x = window_end_x.min(content_right).round();
        let width = end_x - x;

        if width <= 0.0 {
            return;
        }

        self.draw_clip_body(
            painter,
            shape,
            x,
            clip_y,
            width,
            clip_h,
            original_x,
            original_width,
            alpha,
        );
    }

    /// Tints the stretches of the active marquee that don't fall under any
    /// clip, on the marqueed track lanes only — the "empty space" half of the
    /// marquee tint `draw_clip_body` paints onto clip bodies (same base tint
    /// level, `theme::fg_dim()` — a neutral tone, not the track colour, so the
    /// marquee reads as one unified selection regardless of which tracks it
    /// spans). Full lane-row height — unlike a clip body's
    /// `clip_body_band` inset — so the selection reads as extending straight
    /// through the gap rather than a phantom card. Called from
    /// `rendering/mod.rs` between `draw_time_selection` and
    /// `draw_arranger_view`, so clip bodies paint over it wherever they
    /// exist. No-op with no active marquee.
    pub(super) fn draw_time_selection_gaps(&self, painter: &Painter, rect: Rect) {
        let Some(sel) = self
            .gesture
            .time_selection
            .filter(|sel| sel.has_tick_range())
        else {
            return;
        };

        let layout = self.arranger_layout();
        let clip_area_top = rect.min.y + layout.tracks_top();
        let lane_h = layout.lane_h;
        let content_x = self.content_origin_x();
        let content_right = self.content_right_x();

        for track_idx in sel.track_start..=sel.track_end {
            let covered: Vec<(i32, i32)> = self
                .render
                .clip_shapes
                .iter()
                .filter(|shape| shape.track_idx() == track_idx)
                .map(|shape| (shape.start_tick(), shape.end_tick()))
                .collect();

            let lane_y = clip_area_top + track_idx as f32 * lane_h;
            let tint = theme::fg_dim().gamma_multiply(0.16);

            for (gap_start, gap_end) in gap_spans(&covered, sel.start, sel.end) {
                let x = self.tick_to_screen_x(gap_start).max(content_x);
                let end_x = self.tick_to_screen_x(gap_end).min(content_right);
                if end_x <= x {
                    continue;
                }
                painter.rect_filled(
                    Rect::from_min_size(pos2(x, lane_y), vec2(end_x - x, lane_h)),
                    CornerRadius::ZERO,
                    tint,
                );
            }
        }
    }

    /// Draws `track_idx`'s name on its header's name row `row`, cut with an
    /// ellipsis to the row — or, while unnamed, its number (its position,
    /// so it follows a remove above it), dimmer. Nothing under an open
    /// rename field, which draws over the row itself.
    fn draw_track_name(&self, painter: &Painter, track_idx: usize, row: Rect) {
        if self.renaming_track() == Some(track_idx) {
            return;
        }
        let (label, color) = match &self.tracks[track_idx].name {
            Some(name) => (name.clone(), theme::fg()),
            None => (track_number_label(track_idx), theme::fg_dim()),
        };
        let format = TextFormat {
            font_id: FontId::proportional(TRACK_NAME_FONT_SIZE),
            color,
            ..Default::default()
        };
        draw_truncated(
            painter,
            pos2(row.min.x, row.center().y),
            LayoutJob::single_section(label, format),
            row.width(),
        );
    }

    /// Draws `track_idx`'s S (solo) and M (mute) toggle buttons. Off, both wear
    /// the same trough fill the mix bars use (brightness, not alpha —
    /// `030-ui-design.md`) and a dim label; on, `theme::track_solo()` /
    /// `theme::track_mute()` at full opacity with the label knocked out to the
    /// canvas colour. Shown for every track, unlike the instrument-only bars,
    /// and so is the output chip right of them (`draw_output_chip`).
    fn draw_track_header_buttons(
        &self,
        painter: &Painter,
        track_idx: usize,
        rects: &TrackHeaderRects,
    ) {
        let trough = grid_seam_color();
        let font = FontId::proportional(11.0);

        for (rect, label, on, on_fill) in [
            (
                rects.solo,
                "S",
                self.track_soloed(track_idx),
                theme::track_solo(),
            ),
            (
                rects.mute,
                "M",
                self.track_muted(track_idx),
                theme::track_mute(),
            ),
        ] {
            let (fill, text_color) = if on {
                (on_fill, theme::bg())
            } else {
                (trough, theme::fg_dim())
            };
            painter.rect_filled(rect, CornerRadius::same(2), fill);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                font.clone(),
                text_color,
            );
        }
        self.draw_output_chip(painter, track_idx, rects.output);
    }

    /// Draws `track_idx`'s two header bars: volume (fills left→right along the
    /// fader taper) above pan (fills from the centre outward). `accent` is the
    /// track colour, used at full opacity for the fill — the one place a small
    /// accent signal is allowed (`030-ui-design.md`). Readout is right-aligned
    /// so no text measurement is needed.
    fn draw_track_mix_bars(
        &self,
        painter: &Painter,
        track_idx: usize,
        accent: Color32,
        bars: &TrackHeaderRects,
    ) {
        let volume_db = self.track_volume_db(track_idx);
        let pan = self.track_pan(track_idx);

        // Trough: the groove tone, a few RGB steps darker than the canvas
        // (brightness, not alpha — `030-ui-design.md`).
        let trough = grid_seam_color();
        let font = FontId::proportional(11.0);

        // --- Volume ---
        painter.rect_filled(bars.volume, CornerRadius::ZERO, trough);
        let frac = mix::fader_pos_from_db(volume_db).clamp(0.0, 1.0);
        if frac > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(
                    bars.volume.min,
                    vec2(bars.volume.width() * frac, bars.volume.height()),
                ),
                CornerRadius::ZERO,
                accent,
            );
        }
        let vol_text = if volume_db <= mix::MIN_DB {
            "-inf".to_string()
        } else {
            format!("{volume_db:.1}")
        };
        painter.text(
            pos2(bars.volume.max.x - 3.0, bars.volume.center().y),
            Align2::RIGHT_CENTER,
            vol_text,
            font.clone(),
            theme::fg(),
        );

        // --- Pan ---
        painter.rect_filled(bars.pan, CornerRadius::ZERO, trough);
        let mid_x = bars.pan.center().x;
        if pan.abs() > f32::EPSILON {
            let half = bars.pan.width() * 0.5 * pan.abs().min(1.0);
            let (x0, x1) = if pan < 0.0 {
                (mid_x - half, mid_x)
            } else {
                (mid_x, mid_x + half)
            };
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, bars.pan.min.y), pos2(x1, bars.pan.max.y)),
                CornerRadius::ZERO,
                accent,
            );
        }
        // Centre tick, always visible.
        painter.vline(
            mid_x,
            bars.pan.min.y..=bars.pan.max.y,
            Stroke::new(1.0_f32, theme::separator()),
        );
        let amount = (pan * 50.0).round() as i32;
        let pan_text = if amount == 0 {
            "C".to_string()
        } else if amount < 0 {
            format!("{}L", -amount)
        } else {
            format!("{amount}R")
        };
        painter.text(
            pos2(bars.pan.max.x - 3.0, bars.pan.center().y),
            Align2::RIGHT_CENTER,
            pan_text,
            font,
            theme::fg(),
        );
    }

    /// Paints one clip rectangle: fill, border, mute dimming, the marquee
    /// highlight, and the note thumbnail. `alpha` scales every fill (1.0 for a real
    /// clip; `CLIP_GHOST_ALPHA` for the band-drag ghost, which also skips the
    /// marquee highlight — a ghost is part of no selection).
    #[allow(clippy::too_many_arguments)]
    fn draw_clip_body(
        &self,
        painter: &Painter,
        shape: &ClipShape,
        x: f32,
        clip_y: f32,
        width: f32,
        clip_h: f32,
        original_x: f32,
        original_width: f32,
        alpha: f32,
    ) {
        const MARQUEE_TINT: f32 = 0.32;
        const BODY_TINT: f32 = 0.16;
        let base = self.track_color(shape.track_idx());
        let is_muted = shape.is_muted();
        // The header accent sits toned down so a lane packed with clips
        // reads calmly; a muted clip is dimmer still. The strip is
        // `CLIP_HEADER_H` (20px), so an untamed row of them would shout.
        // Untouched by `time_selection` — the marquee highlight below is the
        // only selection signal a clip ever carries.
        let header_alpha = if is_muted { 0.2 } else { 0.45 };
        let note_alpha = if is_muted { 0.25 } else { 0.65 };

        // Clip body background
        painter.rect_filled(
            Rect::from_min_size(pos2(x, clip_y), vec2(width, clip_h)),
            CornerRadius::ZERO,
            theme::bg_panel().gamma_multiply(alpha),
        );
        // Tint: every clip reads the same flat way, tinted with its own
        // track colour like the header strip below — a clip that isn't part
        // of an active marquee must render exactly as if there were no
        // selection at all, full stop. There is deliberately no per-clip
        // "selected" look: the lead clip the model tracks for `Shift+Tab` /
        // `/` gets no visual, because a highlighted clip promises that
        // Delete/Duplicate/Cut act on it and here they never do — only the
        // marquee is read by those. An active, real-tick-width marquee
        // (`TimeSelectionRect::has_tick_range` — a zero-width one has no time
        // range to visualize) paints one highlight on top, scoped tightly to
        // the sub-rect that actually overlaps the marquee's tick range
        // **and** whose track falls inside the marquee's track range.
        // Nothing outside that sub-rect is touched, on this clip or any
        // other: earlier revisions dimmed the *whole* body of every clip in
        // the project as a "second signal", which read as the entire
        // arranger going flat gray the moment any drag started. The
        // highlight uses a neutral `theme::fg_dim()` base rather than the
        // track colour, so a marquee spanning several tracks still reads as
        // one unified selection rather than a patchwork of per-track tints.
        // See `020-views-and-state.md`.
        painter.rect_filled(
            Rect::from_min_size(pos2(x, clip_y), vec2(width, clip_h)),
            CornerRadius::ZERO,
            base.gamma_multiply(BODY_TINT * alpha),
        );
        if alpha >= 1.0
            && let Some(sel) = self
                .gesture
                .time_selection
                .filter(|sel| sel.has_tick_range())
        {
            let in_track_range = (sel.track_start..=sel.track_end).contains(&shape.track_idx());
            if in_track_range
                && let Some((ov_start, ov_end)) =
                    tick_overlap(shape.start_tick(), shape.end_tick(), sel.start, sel.end)
            {
                let ov_x = self.tick_to_screen_x(ov_start).clamp(x, x + width);
                let ov_end_x = self.tick_to_screen_x(ov_end).clamp(x, x + width);
                if ov_end_x > ov_x {
                    painter.rect_filled(
                        Rect::from_min_size(pos2(ov_x, clip_y), vec2(ov_end_x - ov_x, clip_h)),
                        CornerRadius::ZERO,
                        theme::fg_dim().gamma_multiply(MARQUEE_TINT),
                    );
                }
            }
        }
        // Header strip
        painter.rect_filled(
            Rect::from_min_size(pos2(x, clip_y), vec2(width, CLIP_HEADER_H)),
            CornerRadius::ZERO,
            base.gamma_multiply(header_alpha * alpha),
        );

        let body_top = clip_y + CLIP_HEADER_H + 2.0;
        let body_h = (clip_h - CLIP_HEADER_H - 4.0).max(0.0);
        let clip_right = x + width;

        // Draw static and live note thumbnails whenever there is a body to
        // draw them in. No minimum clip width: at 32 bars per viewport a
        // one-beat clip — the natural product of a marqueed-piece move or
        // the sliver a carve leaves behind — is only ~10-14px wide, and a
        // width gate here made exactly those clips render note-less while
        // their siblings two beats wide kept theirs. `draw_thumb` already
        // clamps every bar to the drawn rect with a 1px minimum, so narrow
        // clips degrade to slivers rather than needing a cut-off.
        if body_h > 0.0 {
            self.draw_note_thumbnails(
                painter,
                shape,
                base,
                note_alpha * alpha,
                body_top,
                body_h,
                original_x,
                original_width,
                x,
                clip_right,
            );
        }

        // Right-edge divider
        painter.vline(
            x + width - 1.0,
            clip_y..=(clip_y + clip_h),
            Stroke::new(1.0_f32, theme::bg().gamma_multiply(alpha)),
        );
    }

    /// Draw static note thumbnails and live recording notes if recording.
    #[allow(clippy::too_many_arguments)]
    fn draw_note_thumbnails(
        &self,
        painter: &Painter,
        shape: &ClipShape,
        base: Color32,
        note_alpha: f32,
        body_top: f32,
        body_h: f32,
        original_x: f32,
        original_width: f32,
        x: f32,
        clip_right: f32,
    ) {
        const PIXELS_PER_SEMITONE: f32 = 0.5;

        // Helper closure: convert normalized (x_start, x_end, pitch) to screen coords and draw.
        let draw_thumb = |draw_color: Color32, x_start: f32, x_end: f32, pitch_frac: f32| {
            let nx_start = original_x + x_start * original_width;
            let nx_end = (original_x + x_end * original_width).max(nx_start + 1.0);
            let draw_x = nx_start.max(x);
            let draw_end = nx_end.min(clip_right);

            if draw_end <= draw_x {
                return;
            }

            let note_height = (PIXELS_PER_SEMITONE * 12.0).max(2.0);
            let ny = body_top + (1.0 - pitch_frac) * (body_h - note_height);
            painter.rect_filled(
                Rect::from_min_size(pos2(draw_x, ny), vec2(draw_end - draw_x, 2.0)),
                CornerRadius::ZERO,
                draw_color,
            );
        };

        // Draw static note thumbnails
        let note_color = base.gamma_multiply(note_alpha);
        for &(x_start, x_end, pitch_frac) in shape.note_thumbnails() {
            draw_thumb(note_color, x_start, x_end, pitch_frac);
        }

        // Draw live recording thumbnails if recording this clip
        if self.is_recording_clip(shape) {
            self.draw_live_recording_thumbnails(&draw_thumb);
        }
    }

    /// Whether `shape` is the clip being recorded right now.
    fn is_recording_clip(&self, shape: &ClipShape) -> bool {
        self.is_recording() && Some(shape.clip_id()) == self.recording_clip_id
    }

    /// Draw live note thumbnails during active recording of this clip.
    fn draw_live_recording_thumbnails(&self, draw_thumb: &dyn Fn(Color32, f32, f32, f32)) {
        const LIVE_NOTE_ALPHA: f32 = 0.75;
        const MIN_THUMBNAIL_WIDTH_FRAC: f32 = 0.005;

        let elapsed_start = self
            .live_rec_state
            .elapsed_start_tick
            .load(Ordering::Relaxed);
        let span = (self.elapsed_tick() - elapsed_start).max(1) as f32;
        let live_color = theme::accent().gamma_multiply(LIVE_NOTE_ALPHA);

        if let Ok(snapshot) = self.live_rec_state.thumbnail_snapshot.lock() {
            for &(start_tick, end_tick, pitch_frac) in snapshot.iter() {
                let effective_end = if end_tick == LiveRecState::HELD_NOTE_SENTINEL {
                    self.elapsed_tick() - elapsed_start
                } else {
                    end_tick
                };
                let x0 = (start_tick as f32 / span).clamp(0.0, 1.0);
                let x1 = ((effective_end as f32) / span)
                    .max(x0 + MIN_THUMBNAIL_WIDTH_FRAC)
                    .clamp(0.0, 1.0);
                draw_thumb(live_color, x0, x1, pitch_frac);
            }
        }
    }

    /// Draws the reserved arranger performance lane row (above track 1):
    /// just a selection accent, so it's clear performance mode is armed.
    /// Not a track — its geometry comes from `Display::arranger_layout()`,
    /// the same source `track_idx_at`/`performance_lane_hit_at` use for
    /// hit-testing. The playhead itself (drawn elsewhere) is the feedback
    /// for a live trigger.
    fn draw_performance_lane_row(
        &self,
        painter: &Painter,
        rect: Rect,
        lane_top: f32,
        lane_h: f32,
        labels_right: f32,
    ) {
        let content_x = self.content_origin_x();
        let content_w = self.content_w();
        let accent = theme::accent();
        let is_selected = self.performance_lane_selected;

        let lane_fill = lane_grid_fill(is_selected, accent);
        let gutter_fill = if is_selected {
            accent.gamma_multiply(0.18)
        } else {
            theme::bg_panel().gamma_multiply(0.16)
        };

        // Content fill
        painter.rect_filled(
            Rect::from_min_size(pos2(content_x, lane_top), vec2(content_w, lane_h)),
            CornerRadius::ZERO,
            lane_fill,
        );
        // Gutter fill
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.min.x, lane_top),
                vec2(labels_right - rect.min.x, lane_h),
            ),
            CornerRadius::ZERO,
            gutter_fill,
        );

        if is_selected {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.min.x, lane_top), vec2(4.0, lane_h)),
                CornerRadius::ZERO,
                accent,
            );
        }

        // Seam below the row, matching the track-lane separators.
        painter.hline(
            rect.min.x..=(rect.min.x + rect.width()),
            seam_y(
                lane_top + lane_h,
                ARRANGER_LANE_SEAM_W,
                painter.pixels_per_point(),
            ),
            Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
        );
    }
}

/// Grid-area background fill for one unselected track lane: `theme::bg()`
/// nudged a few RGB steps *brighter* (luminance, not alpha, so it works the
/// same across every palette) so the darker `grid_seam_color()` grooves
/// stand out against it. The selected lane swaps this for a low accent tint.
/// Used for the track lanes and the performance lane alike.
fn lane_grid_fill(is_selected: bool, accent: Color32) -> Color32 {
    if is_selected {
        accent.gamma_multiply(0.12)
    } else {
        shifted_rgb(theme::bg(), 5)
    }
}
