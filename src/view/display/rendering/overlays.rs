//! Painting the always-on overlays: the header info panel (tempo, meter, position,
//! DSP/OVR chips, the help overlay's `?` chip), the footer's passing message, the dim in-lane cursor line,
//! and the playhead. See
//! `030-ui-design.md`.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, FontId, Galley, Painter, Rect, Stroke, pos2, vec2};

use crate::core::time::{Meter, format_bpm, sixteenth_straight_ticks};
use crate::view::display::header_chip::HeaderChipRects;

use super::*;

/// Deadline utilisation at which the header's DSP readout switches from the
/// neutral `fg` tone to `accent`. Below this there is comfortable headroom;
/// above it a single late block is close enough to matter, and the user should
/// see it before it becomes an audible dropout at 100%.
const DSP_LOAD_WARN: f32 = 0.70;

impl Display {
    /// Height of a header chip — the readouts and the `?` chip.
    const HEADER_CHIP_H: f32 = 28.0;
    /// Gap between the `?` chip and the header's right edge.
    const HELP_CHIP_INSET: f32 = 12.0;

    /// The header readouts' value font — the BPM field's too, so the text
    /// doesn't jump when it opens.
    pub(super) fn header_value_font() -> FontId {
        FontId::proportional((theme::FONT_SIZE_HEADER * 0.75).clamp(16.0, 24.0))
    }

    /// Paints the header info panel (tempo, meter, position, DSP / OVR
    /// chips), and notes where the BPM and meter chips went
    /// (`tempo_field.rs`, `meter_field.rs`).
    pub(super) fn draw_header(&mut self, painter: &Painter, rect: Rect) {
        let h = theme::HEADER_H;
        let sw = rect.width();

        // Panel background
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(sw, h)),
            CornerRadius::ZERO,
            theme::bg_panel(),
        );

        // Bottom edge — `grid_seam_color()` groove tone, matching the
        // timeline-strip and lane seams so the horizontal dividers all read as
        // one recessed system rather than this one line being brighter.
        painter.hline(
            rect.min.x..=(rect.min.x + sw),
            rect.min.y + h,
            Stroke::new(1.0_f32, grid_seam_color()),
        );

        // --- Data ---

        let (bar, beat, sixteenth) = song_position(self.playback_tick(), self.meter());

        // --- Readout chips, centred as a group ---
        // BPM then song position, each a small dim `LABEL` next to its value,
        // in the same borderless `bg@0.40` chip language as the footer hints.
        // The group is centred in the header so the row stays balanced as more
        // readouts (loop, …) get appended here later. BPM is
        // a control too (`tempo_field.rs`) and stays first, the meter (also a
        // control, `meter_field.rs`) second: the paint loop below records
        // where their chips went.
        let mut readouts: Vec<(&str, String, Color32)> = vec![
            ("BPM", format_bpm(self.tempo_us()), theme::fg()),
            ("METER", self.meter().to_string(), theme::fg()),
            (
                "POS",
                format!("{bar:03}:{beat:02}:{sixteenth:02}"),
                theme::accent(),
            ),
        ];

        // DSP load — the share of each audio block's real-time deadline the
        // render actually consumed, not process CPU percent: the audio callback
        // is a one-thread deadline (5.33 ms at 256 frames / 48 kHz), so this is
        // the number that predicts a dropout. Shown `average/peak`, the usual
        // audio-meter pair: the average is steady enough to read and is what a
        // change should be compared against, while the peak is what actually
        // goes audible, since one late block is already a glitch. Both are
        // shown because the colour keys off the *peak* — displaying only the
        // average made the chip look like it was changing colour at an
        // arbitrary value. Absent when no output device was found and the app is
        // running silent.
        // `fg`, or `accent` when the readout means something is wrong.
        let alert_color = |alert: bool| if alert { theme::accent() } else { theme::fg() };
        if let Some(load) = &self.audio_load {
            let (average, peak) = (load.average(), load.peak());
            let color = alert_color(peak >= DSP_LOAD_WARN);
            readouts.push((
                "DSP",
                format!("{:.0}/{:.0}%", average * 100.0, peak * 100.0),
                color,
            ));

            // Blocks that actually missed the deadline — audible glitches, not
            // near misses. Latched rather than decayed: an overrun is one block
            // out of ~187 a second, so nobody is looking at the meter when it
            // happens. Reset on each transport start (`Display::ui`), so it
            // reads "this take".
            //
            // Always shown, including at `0`. Hiding it until it fires was tried
            // and is worse: a diagnostic nobody has ever seen work is
            // indistinguishable from a broken one, and the first thing it
            // provokes is a hunt for the missing chip. `theme::fg()` at zero
            // keeps it as quiet as the BPM readout; `theme::accent()` is
            // reserved for a count that actually means something.
            let overruns = load.overruns();
            readouts.push(("OVR", overruns.to_string(), alert_color(overruns > 0)));

            // The backend's own overload count (CoreAudio's
            // `kAudioDeviceProcessorOverload`, via cpal `ErrorKind::Xrun`),
            // same latching and colour rules as `OVR`. Read the pair together:
            // `OVR` without `XRUN` was absorbed by the device's safety offset
            // and never went audible; `XRUN` without `OVR` is a glitch from
            // *outside* the render (IO thread pre-empted, device contention) —
            // the case the render-side timer can't see. See `AudioLoad::xruns`.
            let xruns = load.xruns();
            readouts.push(("XRUN", xruns.to_string(), alert_color(xruns > 0)));
        }

        let value_font = Self::header_value_font();
        let label_font = FontId::proportional(theme::FONT_SIZE_LABEL);

        let chip_h = Self::HEADER_CHIP_H;
        let chip_pad_x = 10.0;
        let label_value_gap = 7.0;
        let chip_gap = 8.0;

        // Each chip's label and value laid out once, in their paint colours:
        // the widths centre the row, the galleys are what gets painted.
        let chips: Vec<_> = readouts
            .into_iter()
            .map(|(label, value, value_color)| {
                let label =
                    painter.layout_no_wrap(label.to_owned(), label_font.clone(), theme::fg_dim());
                let value = painter.layout_no_wrap(value, value_font.clone(), value_color);
                let width =
                    chip_pad_x + label.size().x + label_value_gap + value.size().x + chip_pad_x;
                (label, value, value_color, width)
            })
            .collect();
        let total_w: f32 =
            chips.iter().map(|chip| chip.3).sum::<f32>() + chip_gap * (chips.len() as f32 - 1.0);

        let mut cx = rect.min.x + sw * 0.5 - total_w * 0.5;
        let chip_y = rect.min.y + (h - chip_h) * 0.5;
        let mid_y = chip_y + chip_h * 0.5;
        let paint_left_centred = |x: f32, galley: Arc<Galley>, color: Color32| {
            let min = Align2::LEFT_CENTER
                .anchor_size(pos2(x, mid_y), galley.size())
                .min;
            painter.galley(min, galley, color);
        };

        for (idx, (label, value, value_color, cw)) in chips.into_iter().enumerate() {
            let label_w = label.size().x;
            let chip = Rect::from_min_size(pos2(cx, chip_y), vec2(cw, chip_h));
            let control = match idx {
                0 => Some(&mut self.tempo_chip.rects),
                1 => Some(&mut self.meter_chip.rects),
                _ => None,
            };
            if let Some(rects) = control {
                let value_x = cx + chip_pad_x + label_w + label_value_gap * 0.5;
                *rects = Some(HeaderChipRects {
                    chip,
                    value: Rect::from_min_max(pos2(value_x, chip.min.y), chip.max),
                });
            }
            painter.rect_filled(chip, CornerRadius::ZERO, Self::header_chip_fill());
            paint_left_centred(cx + chip_pad_x, label, theme::fg_dim());
            paint_left_centred(
                cx + chip_pad_x + label_w + label_value_gap,
                value,
                value_color,
            );

            cx += cw + chip_gap;
        }

        // The `?` chip at the right edge, apart from the readouts: a control
        // that opens the help overlay, brighter under the pointer.
        let help_chip = Self::help_chip_rect(rect);
        painter.rect_filled(help_chip, CornerRadius::ZERO, Self::header_chip_fill());
        painter.text(
            help_chip.center(),
            Align2::CENTER_CENTER,
            "?",
            value_font,
            accent_if(self.is_pointer_on_help_chip(), theme::fg_dim()),
        );
    }

    /// A header chip's fill — the readouts' and the `?` chip's.
    fn header_chip_fill() -> Color32 {
        theme::bg().gamma_multiply(0.40)
    }

    /// Where the header's `?` chip sits in the canvas `rect`: a square the
    /// readout chips' height, at the header's right edge. Pure geometry.
    fn help_chip_rect(rect: Rect) -> Rect {
        let size = Self::HEADER_CHIP_H;
        Rect::from_min_size(
            pos2(
                rect.max.x - Self::HELP_CHIP_INSET - size,
                rect.min.y + (theme::HEADER_H - size) * 0.5,
            ),
            vec2(size, size),
        )
    }

    /// Whether canvas point `(x, y)` is on the header's `?` chip.
    pub(in crate::view::display) fn is_on_help_chip(&self, x: f32, y: f32) -> bool {
        Self::help_chip_rect(self.render.canvas_rect).contains(pos2(x, y))
    }

    /// Whether the pointer is on the header's `?` chip with no modal overlay
    /// up to swallow it — its hover highlight and pointing hand.
    pub(in crate::view::display) fn is_pointer_on_help_chip(&self) -> bool {
        self.overlay.is_none()
            && self
                .canvas_pointer()
                .is_some_and(|p| self.is_on_help_chip(p.x, p.y))
    }

    /// The hover cursor tick, if it was measured in the active pane.
    fn pane_hover_cursor_tick(&self) -> Option<i32> {
        self.gesture
            .hover_cursor
            .filter(|&(_, pane)| pane == self.active_pane())
            .map(|(tick, _)| tick)
    }

    /// Paints the dim vertical cursor line at the pointer's snapped tick,
    /// only while the pointer is over this pane (`hover_cursor`). It
    /// reflects the pointer and nothing else — the committed cursor is the
    /// timeline's play triangle — so with the pointer elsewhere there is no
    /// line.
    pub(super) fn draw_cursor_line(&self, painter: &Painter, rect: Rect) {
        let Some(tick) = self.pane_hover_cursor_tick() else {
            return;
        };
        self.draw_lane_vline(
            painter,
            rect,
            tick,
            Stroke::new(CURSOR_LINE_W, cursor_line_color()),
        );
    }

    /// Paints the playhead line while the transport is running.
    pub(super) fn draw_playhead(&self, painter: &Painter, rect: Rect) {
        if !self.render_running() {
            return;
        }
        let Some(playback) = self.render_playback_tick() else {
            return;
        };
        self.draw_lane_vline(
            painter,
            rect,
            playback,
            Stroke::new(1.0_f32, theme::playhead()),
        );
    }

    /// Strokes a full-height line down the lanes at `tick` (at the frame's
    /// render scroll), skipped when its screen-x has scrolled into the left
    /// gutter or off the right edge.
    fn draw_lane_vline(&self, painter: &Painter, rect: Rect, tick: i32, stroke: Stroke) {
        let x = (tick as f32 * self.pixels_per_tick() + self.content_origin_x()
            - self.render_scroll_x())
        .round();
        if !self.content_x_on_screen(x) {
            return;
        }
        let y_top = rect.min.y + self.track_area_top() + Self::TIMELINE_H;
        let y_bottom = rect.min.y + self.track_area_bottom();
        painter.vline(x, y_top..=y_bottom, stroke);
    }

    /// Paints a bold, neutral-toned accent across the **selected track's row
    /// only** — or, while an active marquee spans more than one track, across
    /// every row `time_selection.track_start..=track_end` covers, so the
    /// accent visually "expands" with the drag — marking the **playback start
    /// position**: the committed cursor tick (`render_cursor_tick()`), the
    /// same tick the timeline's "play" triangle marks (`draw_time_selection`,
    /// `timeline.rs`) — not the live, moving playback position `draw_playhead`
    /// above tracks. It therefore does not move during playback; it only
    /// moves when the cursor itself does (arrow keys, paging, clicks), exactly
    /// like that triangle. Coloured with `theme::fg_dim()` (dimmed while the
    /// clip pane has the keyboard, see below) — a neutral tone
    /// rather than the track colour, so the cursor reads as one unified
    /// marker regardless of which track(s) it spans (see `030-ui-design.md`).
    /// This is what lets a purely vertical marquee drag (same tick, several
    /// tracks) read as "these tracks, at this instant" — the multi-track
    /// operand `⌘/Ctrl+E` (Split) then acts on.
    ///
    /// Hidden once the marquee gains real tick width
    /// (`time_selection.has_tick_range()`): the marquee's own tint already
    /// marks the selected span at that point, and the cursor accent sitting
    /// at one edge of it reads as a stray leftover rather than useful
    /// information. A purely vertical drag (tracks only, no tick width) keeps
    /// it visible — that's the "these tracks, at this instant" case above,
    /// which has no tint of its own to carry the meaning. Esc
    /// (`clear_time_selection`), a fresh click (`begin_time_selection`) and
    /// re-anchoring the selected track/cursor outside of a drag
    /// (`sync_time_selection_to_cursor`) all drop `time_selection`, which
    /// restores it.
    ///
    /// Arranger-only: skipped in the clip pane,
    /// where `arranger_layout()`'s
    /// per-track rows aren't meaningful. Always drawn regardless of transport
    /// state, matching the timeline triangle it mirrors — no running check.
    ///
    /// Drawn in the unfocused tone (`unfocused_tone`) while the
    /// clip pane has the keyboard: `Space` still plays from here, so it stays
    /// visible, but dimmed so it can't be mistaken for the clip pane's own
    /// cursor, which the arrow keys move.
    pub(super) fn draw_selected_track_cursor(&self, painter: &Painter, rect: Rect) {
        if self.active_pane() == Pane::Clip {
            return;
        }

        if self
            .gesture
            .time_selection
            .is_some_and(|sel| sel.has_tick_range())
        {
            return;
        }

        let layout = self.arranger_layout();
        let (track_lo, track_hi) = self
            .gesture
            .time_selection
            .map(|sel| (sel.track_start, sel.track_end))
            .unwrap_or((self.selected_track_idx, self.selected_track_idx));

        let tracks_top = rect.min.y + layout.tracks_top();
        let (row_top, _) = track_row_y_range(tracks_top, layout.lane_h, track_lo);
        let (_, row_bottom) = track_row_y_range(tracks_top, layout.lane_h, track_hi);

        let x = self.tick_to_screen_x(self.render_cursor_tick());
        if !self.content_x_on_screen(x) {
            return;
        }

        let color = if self.focused_pane() == Pane::Arranger {
            theme::fg_dim()
        } else {
            unfocused_tone(theme::fg_dim(), theme::bg())
        };
        painter.vline(
            x,
            (row_top + SELECTED_TRACK_CURSOR_MARGIN_Y)
                ..=(row_bottom - SELECTED_TRACK_CURSOR_MARGIN_Y),
            Stroke::new(SELECTED_TRACK_CURSOR_W, color),
        );
    }

    /// Reserved footer band. It carries no content of its own any more — the
    /// old per-view key-hint chip system was removed — but the panel, its inset
    /// fill and its top seam stay so the layout keeps a framed bottom edge and
    /// the space is held for a future status readout.
    pub(super) fn draw_status_bar(&self, painter: &Painter, rect: Rect) {
        let h = theme::STATUS_H;
        let sw = rect.width();
        let y_top = rect.max.y - h - Self::VIEW_BOTTOM_MARGIN_Y;

        // Bottom inset fill
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.min.x, y_top + h),
                vec2(sw, Self::VIEW_BOTTOM_MARGIN_Y),
            ),
            CornerRadius::ZERO,
            theme::bg_panel(),
        );

        // Status bar background
        painter.rect_filled(
            Rect::from_min_size(pos2(rect.min.x, y_top), vec2(sw, h)),
            CornerRadius::ZERO,
            theme::bg_panel(),
        );

        // Top edge — `grid_seam_color()` groove tone, matching the header and
        // timeline dividers so every horizontal edge in the view reads as one
        // recessed system rather than this line being brighter.
        painter.hline(
            rect.min.x..=(rect.min.x + sw),
            y_top,
            Stroke::new(1.0_f32, grid_seam_color()),
        );

        // The passing message (`UiEvent::Status`): left-aligned, dim, fading
        // out once its time is up.
        if let Some(status) = &self.render.status
            && let Some(opacity) = status.opacity(Instant::now())
        {
            painter.text(
                pos2(rect.min.x + Self::STATUS_TEXT_X, y_top + h * 0.5),
                Align2::LEFT_CENTER,
                &status.text,
                FontId::proportional(theme::FONT_SIZE_LABEL),
                theme::fg_dim().gamma_multiply(opacity),
            );
        }
    }

    /// The footer message's left inset from the window edge.
    const STATUS_TEXT_X: f32 = 12.0;
}

/// Colour and stroke width of the in-lane cursor line(s) drawn by
/// `draw_cursor_line` (`draw_lane_vline`): a dimmed, neutral `fg` tone
/// (~0.45 brightness — luminance, not alpha, so it can't bloom over bright
/// clip headers) drawn 0.75px wide. It stays this quiet because the committed
/// / playback position is carried by the timeline's own play triangle, which
/// is where the emphasis goes.
fn cursor_line_color() -> Color32 {
    let [r, g, b, _] = theme::fg().to_array();
    Color32::from_rgb(
        (r as f32 * 0.45) as u8,
        (g as f32 * 0.45) as u8,
        (b as f32 * 0.45) as u8,
    )
}

/// Width of the cursor line, in points.
const CURSOR_LINE_W: f32 = 0.75;

/// Screen `[y_top, y_bottom)` of one track's lane, given the arranger
/// layout's `tracks_top` (already offset to screen space) and `lane_h` —
/// mirrors the `lane_y = clip_area_top + track_idx as f32 * lane_h` maths
/// `draw_arranger_backgrounds` computes inline.
fn track_row_y_range(tracks_top: f32, lane_h: f32, track_idx: usize) -> (f32, f32) {
    let y_top = tracks_top + track_idx as f32 * lane_h;
    (y_top, y_top + lane_h)
}

/// Stroke width of the selected-track row cursor accent — bolder than the
/// full-grid playhead's 1px since it only spans a single row.
const SELECTED_TRACK_CURSOR_W: f32 = 2.0;

/// How far the unfocused row cursor accent is blended from `fg_dim` toward
/// the canvas: far enough to read as inactive at a glance, not so far that it
/// vanishes against the lane.
const UNFOCUSED_CURSOR_BLEND: f32 = 0.6;

/// `fg` blended toward the canvas `bg` by `UNFOCUSED_CURSOR_BLEND` — the
/// inactive-selection look of the selected-track row cursor accent while the
/// clip pane has the keyboard. Brightness, not alpha, so it can't bloom over
/// bright clip headers.
fn unfocused_tone(fg: Color32, bg: Color32) -> Color32 {
    fg.lerp_to_gamma(bg, UNFOCUSED_CURSOR_BLEND)
}

/// Vertical margin, top and bottom, between the row cursor accent and its
/// lane's separator seams. Without it the line's own stroke width let it
/// visually bleed past `track_row_y_range`'s exact lane bounds into the
/// neighbouring row; this keeps it clearly inside the lane, matching
/// `CLIP_V_INSET`'s inset-from-lane-bounds precedent.
const SELECTED_TRACK_CURSOR_MARGIN_Y: f32 = 2.0;

/// The header's `POS` readout for `tick`, one-based: bar, counted beat
/// within it (an eighth in x/8), and the 16th within that beat.
fn song_position(tick: i32, meter: Meter) -> (i32, i32, i32) {
    let in_bar = tick % meter.bar_ticks();
    let bar = meter.ticks_to_bars(tick) + 1;
    let beat = in_bar / meter.beat_ticks() + 1;
    let sixteenth = (in_bar % meter.beat_ticks()) / sixteenth_straight_ticks() + 1;
    (bar, beat, sixteenth)
}

#[cfg(test)]
mod tests {
    use egui::Color32;

    use super::{song_position, track_row_y_range, unfocused_tone};
    use crate::core::time::{Meter, PPQN};

    #[test]
    fn song_position_counts_bars_beats_and_sixteenths() {
        assert_eq!(song_position(0, Meter::FOUR_FOUR), (1, 1, 1));
        let tick = 4 * PPQN + 2 * PPQN + 3 * PPQN / 4;
        assert_eq!(song_position(tick, Meter::FOUR_FOUR), (2, 3, 4));
    }

    #[test]
    fn song_position_counts_the_meters_beats() {
        // 3/4: bar 2 starts on quarter 3.
        let three_four = Meter::new(3, 4).unwrap();
        assert_eq!(song_position(3 * PPQN, three_four), (2, 1, 1));
        // 6/8: beats are eighths, two 16ths each; quarter 2 is beat 3.
        let six_eight = Meter::new(6, 8).unwrap();
        assert_eq!(song_position(PPQN, six_eight), (1, 3, 1));
        assert_eq!(song_position(PPQN + PPQN / 4, six_eight), (1, 3, 2));
        assert_eq!(song_position(3 * PPQN + PPQN / 2, six_eight), (2, 2, 1));
    }

    #[test]
    fn track_row_y_range_is_the_lane_slice_at_its_track_index() {
        assert_eq!(track_row_y_range(100.0, 40.0, 0), (100.0, 140.0));
        assert_eq!(track_row_y_range(100.0, 40.0, 3), (220.0, 260.0));
    }

    #[test]
    fn track_row_y_range_rows_are_contiguous_and_non_overlapping() {
        let (lane_h, tracks_top) = (37.5, 88.0);
        for track_idx in 0..7 {
            let (_, bottom) = track_row_y_range(tracks_top, lane_h, track_idx);
            let (next_top, _) = track_row_y_range(tracks_top, lane_h, track_idx + 1);
            assert_eq!(bottom, next_top);
        }
    }

    #[test]
    fn unfocused_tone_lies_between_bg_and_fg() {
        let (fg, bg) = (
            Color32::from_rgb(200, 180, 160),
            Color32::from_rgb(20, 20, 20),
        );
        let dimmed = unfocused_tone(fg, bg);
        for ((d, f), b) in dimmed.to_array()[..3]
            .iter()
            .zip(&fg.to_array()[..3])
            .zip(&bg.to_array()[..3])
        {
            assert!(d < f && d > b, "{dimmed:?} not between {bg:?} and {fg:?}");
        }
    }
}
