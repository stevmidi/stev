//! The piano-roll keyboard gutter: pitch-class geometry, the octave legend, and
//! the flat equal-cell key grid. Pure geometry helpers here are unit-tested.
//! Octave numbering is Cubase-style (MIDI 60 = C3). See `030-ui-design.md`.

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Stroke, pos2, vec2};

use crate::models::clip::min_max;

use super::*;

/// Overall vertical scale of the piano roll. Multiplies the default (flat)
/// row height `KEY_ROW_H`, plus `clip_view::Display::EVENT_V_PADDING`, so
/// bumping this one number makes the whole keyboard — and the note events
/// drawn on it — uniformly taller/thicker without touching the per-pitch-class
/// shape ratios.
pub(super) const PIANO_ROLL_SCALE: f32 = 2.0;

/// Default height of every semitone row, white or black alike, used
/// whenever the piano roll has enough vertical space for it. Keys are drawn
/// as a flat grid of equal-size cells side by side (Ableton-style) rather
/// than interlocking, differently sized white/black shapes — that
/// "realistic" geometry made black-key rows visibly thinner than white-key
/// rows, which fought both readability and the goal of `PIANO_ROLL_SCALE`
/// (uniformly thicker rows/events for easier selection). When a clip's home
/// note range doesn't fit at this height, `fit_row_height` shrinks every row
/// uniformly so the whole range is on screen when the clip opens — see
/// `NoteAreaGeom::framed`.
pub(super) const KEY_ROW_H: f32 = 14.0 * PIANO_ROLL_SCALE;

/// Pitch class `0..=11` of a MIDI note.
pub(super) fn pitch_class(note: u8) -> u8 {
    note % 12
}

/// Octave number using the Cubase-style convention where MIDI note 0 is
/// C-2 and MIDI note 127 is G8 — matching the -2..8 legend span the piano
/// roll spec calls for.
pub(super) fn octave_of(note: u8) -> i32 {
    note as i32 / 12 - 2
}

/// Whether pitch class `pc` is a black key.
pub(super) fn is_black_key(pc: u8) -> bool {
    matches!(pc, 1 | 3 | 6 | 8 | 10)
}

/// Row height that fits the inclusive range `lo..=hi` into `note_area_h`,
/// capped at `KEY_ROW_H` — the flat height used whenever there's enough
/// room. Only shrinks below `KEY_ROW_H` when the range itself doesn't fit,
/// so `(hi - lo + 1) as f32 * fit_row_height(lo, hi, note_area_h) <=
/// note_area_h` (barring the floor below kicking in on a pathologically
/// short viewport).
pub(super) fn fit_row_height(lo: u8, hi: u8, note_area_h: f32) -> f32 {
    let rows = (hi - lo + 1) as f32;
    (note_area_h / rows).clamp(1.0, KEY_ROW_H)
}

/// Tallest row the key-column zoom drag reaches.
const MAX_ZOOM_ROW_H: f32 = 2.0 * KEY_ROW_H;

/// Horizontal drag distance, in points, that doubles (rightward) or halves
/// (leftward) the row height in the key-column zoom drag.
const KEY_ZOOM_DOUBLING_PX: f32 = 150.0;

/// `row_h` held to the key-column zoom's range over a note area `h` tall:
/// from the whole keyboard fitting up to [`MAX_ZOOM_ROW_H`].
fn clamp_zoom_row_height(row_h: f32, h: f32) -> f32 {
    row_h.clamp(fit_row_height(0, 127, h), MAX_ZOOM_ROW_H)
}

/// Row height after `dx` more points of key-column zoom drag (rightward =
/// taller rows) from `row_h`: exponential, so a given drag distance scales
/// by the same factor at any zoom, and clamped (`clamp_zoom_row_height`).
/// Applied step by step from the current height, so a drag turning around
/// at a limit acts at once.
pub(super) fn key_zoom_row_height(row_h: f32, dx: f32, h: f32) -> f32 {
    clamp_zoom_row_height(row_h * (dx / KEY_ZOOM_DOUBLING_PX).exp2(), h)
}

/// Virtual-space y-offset to the top of `note`'s row at the given row height,
/// 0 at the top of octave 8 (the B8 row above G8/127, the highest
/// representable note — so G8's top sits 4 rows down). Monotonically
/// increasing as `note` decreases — mirrors how tick offsets grow with
/// `scroll_x` on the horizontal axis. `row_h` is a runtime value (usually
/// `KEY_ROW_H`, but not always — see `fit_row_height`) so the whole keyboard
/// can shrink uniformly when the visible range doesn't fit.
pub(super) fn cum_height_from_top(note: u8, row_h: f32) -> f32 {
    let octaves_above = (8 - octave_of(note)) as f32;
    // Rows from the top of `note`'s octave (top of its B key) down to the
    // top of `note`'s own row: C is 11 rows down, B is 0.
    let rows_from_octave_top = (11 - pitch_class(note)) as f32;
    (octaves_above * 12.0 + rows_from_octave_top) * row_h
}

/// The note range a clip opens on: its notes padded by half an octave each
/// side (clamped to 0..127), or C2..B5 for an empty clip. Saved as the
/// vertical home framing when the clip opens (`Display::latch_clip_home_notes`),
/// so an edit never re-fits the view.
pub(super) fn home_note_range(notes: impl IntoIterator<Item = u8>) -> (u8, u8) {
    match min_max(notes) {
        Some((lo, hi)) => (lo.saturating_sub(6), hi.saturating_add(6).min(127)),
        None => (36, 83), // C2..B5
    }
}

/// Centre of the rows `lo..=hi`, in row units (virtual y at a row height of
/// 1, see `cum_height_from_top`) — the vertical home position.
pub(super) fn centre_row_of((lo, hi): (u8, u8)) -> f32 {
    (cum_height_from_top(hi, 1.0) + cum_height_from_top(lo, 1.0) + 1.0) * 0.5
}

/// The note whose row holds `row` (row units, see `NoteAreaGeom::row_at`),
/// over the whole keyboard rather than just the visible rows, clamped to
/// 0..127 — the event marquee's anchor note, which may have scrolled out of
/// view.
pub(super) fn note_at_row(row: f32) -> u8 {
    let top_row = cum_height_from_top(127, 1.0);
    let rows_down = (row - top_row).floor().clamp(0.0, 127.0);
    127 - rows_down as u8
}

/// Clamp range of the note area's `scroll_y` at `row_h` over a viewport
/// `h` tall: from G8's (127) row top at the top edge to note 0's row bottom
/// at the bottom edge, so the view never scrolls past either end of the
/// keyboard. Collapses to the top when the whole keyboard fits.
fn scroll_y_bounds(row_h: f32, h: f32) -> (f32, f32) {
    let min_scroll_y = cum_height_from_top(127, row_h);
    let max_scroll_y = (cum_height_from_top(0, row_h) + row_h - h).max(min_scroll_y);
    (min_scroll_y, max_scroll_y)
}

/// The view's centre in row units for a viewport `h` tall scrolled to
/// `scroll_y` at `row_h`, the scroll first clamped to the keyboard's ends
/// (`scroll_y_bounds`).
fn clamped_centre_row(scroll_y: f32, row_h: f32, h: f32) -> f32 {
    let (min_scroll_y, max_scroll_y) = scroll_y_bounds(row_h, h);
    (scroll_y.clamp(min_scroll_y, max_scroll_y) + h * 0.5) / row_h
}

/// The note-area viewport geometry, shared by every draw pass and hit-test
/// of the piano roll (`Display::note_area_geom`). Built by
/// [`NoteAreaGeom::framed`].
pub(super) struct NoteAreaGeom {
    /// Top y of the note area.
    pub(super) top: f32,
    /// Height of the note area.
    pub(super) h: f32,
    /// Lowest note with any of its row in view.
    pub(super) note_lo: u8,
    /// Highest note with any of its row in view.
    pub(super) note_hi: u8,
    /// Virtual-space y (see `cum_height_from_top`) of the viewport's top edge.
    pub(super) scroll_y: f32,
    /// Per-row height: the user's key-column zoom, or fitted to the home
    /// range (shrunk from `KEY_ROW_H` when it doesn't fit).
    pub(super) row_h: f32,
}

impl NoteAreaGeom {
    /// The note-area viewport `top`/`h`: rows fitted to the clip's home note
    /// range `home` (`fit_row_height` — flat `KEY_ROW_H` with room to spare,
    /// shrunk just enough for the whole range otherwise), centred on
    /// `centre_row` (row units: the user's vertical scroll, see
    /// `RenderState::clip_centre_row`) or, at home, on `home` itself — then
    /// clamped to the keyboard's ends. Everything comes from the saved home
    /// range and the pane height, never the clip's current notes, so an edit
    /// never moves the grid; a resize refits the rows and keeps the pitch at
    /// the centre (`220-capture-without-pending-view.md`). A user zoom
    /// `row_h` (`RenderState::clip_row_h`, the key-column drag) replaces the
    /// fit, held to the zoom's range at this height.
    pub(super) fn framed(
        top: f32,
        h: f32,
        home: (u8, u8),
        centre_row: Option<f32>,
        row_h: Option<f32>,
    ) -> Self {
        let row_h = row_h.map_or_else(
            || fit_row_height(home.0, home.1, h),
            |row_h| clamp_zoom_row_height(row_h, h),
        );
        let centre_row = centre_row.unwrap_or_else(|| centre_row_of(home));
        let (min_scroll_y, max_scroll_y) = scroll_y_bounds(row_h, h);
        let scroll_y = (centre_row * row_h - h * 0.5).clamp(min_scroll_y, max_scroll_y);
        let (note_lo, note_hi) = min_max((0..=127u8).filter(|&nn| {
            let row_top = cum_height_from_top(nn, row_h);
            row_top < scroll_y + h && row_top + row_h > scroll_y
        }))
        .unwrap_or(home);
        NoteAreaGeom {
            top,
            h,
            note_lo,
            note_hi,
            scroll_y,
            row_h,
        }
    }

    /// The view's centre in row units after a wheel / trackpad scroll of
    /// `delta_y` points (positive = content moves down = toward higher
    /// pitches, already OS natural-scroll aware), clamped to the keyboard's
    /// ends. Row units, not pixels, so the pitch at the centre holds when a
    /// resize changes the row height.
    pub(super) fn scrolled_centre_row(&self, delta_y: f32) -> f32 {
        clamped_centre_row(self.scroll_y - delta_y, self.row_h, self.h)
    }

    /// The view's centre in row units with rows `row_h` tall and content row
    /// `anchor_row` (see [`row_at`](Self::row_at)) at screen y `pointer_y`,
    /// clamped to the keyboard's ends — the key-column drag, which keeps the
    /// row it grabbed under the pointer while it zooms and scrolls.
    pub(super) fn anchored_centre_row(&self, anchor_row: f32, row_h: f32, pointer_y: f32) -> f32 {
        clamped_centre_row(anchor_row * row_h - (pointer_y - self.top), row_h, self.h)
    }

    /// Screen y of the top edge of `note`'s row.
    pub(super) fn row_top(&self, note: u8) -> f32 {
        self.top + (cum_height_from_top(note, self.row_h) - self.scroll_y)
    }

    /// Screen y → virtual-space y (see `cum_height_from_top`).
    pub(super) fn virtual_y(&self, y: f32) -> f32 {
        (y - self.top) + self.scroll_y
    }

    /// Screen y → content-space row units (virtual y at a row height of 1),
    /// with `y` held inside the note area: a position that stays on its
    /// notes when the view scrolls (the event marquee's anchor and box).
    pub(super) fn row_at(&self, y: f32) -> f32 {
        let y = y.clamp(self.top, (self.top + self.h - 0.5).max(self.top));
        self.virtual_y(y) / self.row_h
    }

    /// Row units (see [`row_at`](Self::row_at)) → screen y.
    pub(super) fn row_to_screen_y(&self, row: f32) -> f32 {
        self.top + row * self.row_h - self.scroll_y
    }

    /// Whether every row of `lo..=hi` is wholly inside the viewport — a
    /// capture's take landed in view, so the view stays put
    /// (`Display::reframe_after_capture`).
    pub(super) fn shows_rows(&self, (lo, hi): (u8, u8)) -> bool {
        // Half a pixel of slack for float error at an exact fit.
        let top = cum_height_from_top(hi, self.row_h);
        let bottom = cum_height_from_top(lo, self.row_h) + self.row_h;
        top >= self.scroll_y - 0.5 && bottom <= self.scroll_y + self.h + 0.5
    }

    /// How far screen `y` lies past the note area's edge, in points:
    /// positive above the top, negative below the bottom, `0` inside — the
    /// direction and speed of the edge auto-scroll (positive scrolls toward
    /// higher pitches, as a positive wheel delta does).
    pub(super) fn edge_overshoot(&self, y: f32) -> f32 {
        edge_overshoot(y, self.top, self.h)
    }

    /// The visible note whose row holds screen y `y`, `None` above or below
    /// the visible range — the row lookup shared by the click hit-test and
    /// the event marquee.
    pub(super) fn note_row_at(&self, y: f32) -> Option<u8> {
        let virtual_y = self.virtual_y(y);
        (self.note_lo..=self.note_hi).find(|&nn| {
            let top = cum_height_from_top(nn, self.row_h);
            virtual_y >= top && virtual_y < top + self.row_h
        })
    }
}

/// Width of the horizontal seam at the top of `note`'s row: 2px at the B/C
/// octave boundary (the top of B), 1px between E and F — the only two
/// adjacent white keys with no black key between them — `None` elsewhere.
/// Shared by the key column and the note grid, whose seams continue each
/// other.
pub(super) fn row_seam_w(note: u8) -> Option<f32> {
    match pitch_class(note) {
        11 => Some(2.0),
        4 => Some(1.0),
        _ => None,
    }
}

impl Display {
    /// The clip view's vertical home framing: the note range saved when the
    /// clip was opened (`RenderState::clip_home_notes`), or the clip's
    /// current one before that.
    pub(super) fn clip_home_notes(&self) -> (u8, u8) {
        self.render
            .clip_home_notes
            .unwrap_or_else(|| self.current_home_notes())
    }

    /// The home note range of the open clip's notes as they are now.
    fn current_home_notes(&self) -> (u8, u8) {
        home_note_range(self.render.event_shapes.iter().map(|s| s.note_number()))
    }

    /// Saves the open clip's note range as the vertical home framing and
    /// centres the view on it at the fitted row height (`frame_clip_home`).
    /// The event shapes must already be rebuilt.
    pub(in crate::view::display) fn latch_clip_home_notes(&mut self) {
        self.render.clip_home_notes = Some(self.current_home_notes());
        self.render.clip_centre_row = None;
        self.render.clip_row_h = None;
    }

    /// The octave-legend column (zone 1, left of the keys) beside the note
    /// area, for a canvas whose left edge is at `canvas_left` — drawn by
    /// `draw_octave_legends`, hit by `is_on_octave_legend`.
    fn octave_legend_rect(&self, canvas_left: f32, geom: &NoteAreaGeom) -> Rect {
        let legend_left = canvas_left + self.content_origin_x() - 2.0 * Self::PIANO_MARGIN_W;
        Rect::from_min_size(
            pos2(legend_left, geom.top),
            vec2(Self::PIANO_MARGIN_W, geom.h),
        )
    }

    /// Whether `(x, y)` is on the octave-legend column — where the zoom drag
    /// starts. Not the keys themselves. Clip pane only.
    pub(in crate::view::display) fn is_on_octave_legend(&self, x: f32, y: f32) -> bool {
        let legend_left = self.content_origin_x() - 2.0 * Self::PIANO_MARGIN_W;
        // The x test first: it is free, the note-area geometry isn't.
        (legend_left..legend_left + Self::PIANO_MARGIN_W).contains(&x)
            && self
                .octave_legend_rect(0.0, &self.note_area_geom())
                .contains(pos2(x, y))
    }

    /// Anchors a key-column zoom drag at the press (on the octave legend):
    /// the row under it, which `extend_key_zoom_drag` keeps under the
    /// (hidden) pointer. The drag owns the pointer until release
    /// (`MouseMoved` extends nothing else), so the cursor line and note hover
    /// are cleared here rather than left frozen. Clip pane only.
    pub(in crate::view::display) fn begin_key_zoom_drag(&mut self, y: f32) {
        self.gesture.key_zoom_hover = false;
        self.gesture.hover_cursor = None;
        self.gesture.note_hover = None;
        self.gesture.key_zoom_drag = Some(KeyZoomDrag {
            axes: AxisFilter::new(),
            anchor_row: self.note_area_geom().row_at(y),
        });
    }

    /// Extends a key-column drag (Ableton / Bitwig) by a raw pointer motion
    /// `(dx, dy)` (`InputEvent::PointerMotion`, which keeps arriving past
    /// the window edge), drift dropped (`AxisFilter`), as a step from the
    /// view as it is now: `dx` scales the rows (`key_zoom_row_height`,
    /// rightward = taller) and the grabbed row moves `dy` from where it sits,
    /// so a vertical move scrolls (`NoteAreaGeom::anchored_centre_row`).
    /// Stepping from the clamped view, never from the press, means turning
    /// around at a zoom limit or a keyboard end acts at once. The rows become
    /// the user's own (`RenderState::clip_row_h`) only once the drag actually
    /// zooms — a pure scroll keeps the fit. Clip pane only.
    pub(in crate::view::display) fn extend_key_zoom_drag(&mut self, dx: f32, dy: f32) {
        let Some(drag) = self.gesture.key_zoom_drag.as_mut() else {
            return;
        };
        let (dx, dy) = drag.axes.filter(dx, dy);
        let anchor_row = drag.anchor_row;
        let geom = self.note_area_geom();
        let row_h = key_zoom_row_height(geom.row_h, dx, geom.h);
        let pointer_y = geom.row_to_screen_y(anchor_row) + dy;
        let centre_row = geom.anchored_centre_row(anchor_row, row_h, pointer_y);
        if row_h != geom.row_h {
            self.render.clip_row_h = Some(row_h);
        }
        self.render.clip_centre_row = Some(centre_row);
    }

    /// Updates whether the pointer is over the octave legend with no button
    /// held, for its cursor icon.
    pub(in crate::view::display) fn update_key_zoom_hover(&mut self, x: f32, y: f32) {
        self.gesture.key_zoom_hover = self.active_pane() == Pane::Clip
            && self.gesture.press_pane.is_none()
            && self.is_on_octave_legend(x, y);
    }

    /// Draws the piano-key column (zone 2): white keys with a border at the
    /// top of each, then black keys on top of the borders where one exists.
    pub(super) fn draw_piano_keys(&self, painter: &Painter, rect: Rect, geom: &NoteAreaGeom) {
        let piano_right = rect.min.x + self.content_origin_x();
        let piano_left = piano_right - Self::PIANO_MARGIN_W;

        let note_area_rect = Rect::from_min_size(
            pos2(piano_left, geom.top),
            vec2(Self::PIANO_MARGIN_W, geom.h),
        );
        let p = painter.with_clip_rect(note_area_rect);

        // Flat grid of equal-size key cells, side by side rather than
        // interlocking, differently sized white/black shapes: every row is
        // the same width and height (`KEY_ROW_H`) and differs from its
        // neighbours only in fill color.
        for nn in geom.note_lo..=geom.note_hi {
            let pc = pitch_class(nn);
            let y_top = geom.row_top(nn);
            let color = if is_black_key(pc) {
                theme::piano_key_black()
            } else {
                theme::piano_key_white()
            };
            p.rect_filled(
                Rect::from_min_size(
                    pos2(piano_left, y_top),
                    vec2(Self::PIANO_MARGIN_W, geom.row_h),
                ),
                CornerRadius::ZERO,
                color,
            );
        }

        // E/F and B/C are the only adjacent pairs that are both white with
        // no black key between them — without a seam here they'd read as
        // one tall key instead of two, the same problem
        // `draw_note_lane_backgrounds` fixes for the note-grid lanes. Every
        // other row boundary is already legible from the white/black color
        // change alone. Both use the shared `grid_seam_color()` groove tone
        // and continue the matching seam in the note grid unbroken across the
        // column boundary; hierarchy is by width, as in the arranger — B/C
        // (octave boundary, primary pitch reference) is 2px, E/F is 1px.
        for nn in geom.note_lo..=geom.note_hi {
            let Some(seam_w) = row_seam_w(nn) else {
                continue;
            };
            // `.round()` so the stroke lands on a pixel boundary — the note
            // grid's B/C and E/F seams and the legend's octave seam continue
            // this exact line and must snap the same way or the thickness
            // steps at the column boundary.
            let y_top = geom.row_top(nn).round();
            p.hline(
                piano_left..=piano_right,
                y_top,
                Stroke::new(seam_w, grid_seam_color()),
            );
        }

        // Right-edge divider separating the keys from the note grid — the
        // `grid_seam_color()` groove tone, matching the arranger's
        // track-column right edge (a plain `separator()` rule read brighter
        // and harsher than the recessed seams around it, worst on the darkest
        // palettes).
        painter.vline(
            piano_right,
            geom.top..=(geom.top + geom.h),
            Stroke::new(1.0_f32, grid_seam_color()),
        );
    }

    /// Draws the octave-legend column (zone 1): a groove seam and a
    /// right-aligned "C{octave}" label at the start of each visible octave.
    pub(super) fn draw_octave_legends(&self, painter: &Painter, rect: Rect, geom: &NoteAreaGeom) {
        let legend_rect = self.octave_legend_rect(rect.min.x, geom);
        let (legend_left, legend_right) = (legend_rect.min.x, legend_rect.max.x);
        let p = painter.with_clip_rect(legend_rect);

        // The panel ground under this column is painted by
        // `draw_note_lane_backgrounds` as part of one continuous left-gutter
        // panel (`bg_panel().gamma_multiply(0.8)`, the arranger track-column
        // fill) — nothing to fill here, just the octave seams and labels.
        let font = FontId::proportional(theme::FONT_SIZE_TL);

        for nn in geom.note_lo..=geom.note_hi {
            if pitch_class(nn) != 0 {
                continue;
            }
            // The octave boundary is the *bottom* of C's row (== the top of
            // the previous, lower octave's B row) — not C's own top, which
            // is just the C/C# boundary within this octave. `.round()` so the
            // 2px stroke lands on a pixel boundary and matches the note grid's
            // B/C seam thickness exactly (the three zones' segments form one
            // line, so they must snap the same way).
            let y = (geom.row_top(nn) + geom.row_h).round();
            // Only draw the seam when the octave below is actually in view, so
            // the legend never draws a lone stub the note grid's B/C seam
            // doesn't — that stub, clipped at the viewport's bottom edge, drew
            // half-width. `nn > note_lo` ⇔ the B one semitone down (`nn - 1`)
            // is within `[note_lo, note_hi]`, which is exactly the note grid's
            // condition. The label still always draws.
            if nn > geom.note_lo {
                p.hline(
                    legend_left..=legend_right,
                    y,
                    Stroke::new(2.0_f32, grid_seam_color()),
                );
            }
            p.text(
                pos2(legend_right - 4.0, y),
                Align2::RIGHT_BOTTOM,
                format!("C{}", octave_of(nn)),
                font.clone(),
                theme::fg_dim(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_note_range_pads_half_an_octave_and_defaults_when_empty() {
        assert_eq!(home_note_range([60, 64, 67]), (54, 73));
        assert_eq!(home_note_range([2, 125]), (0, 127));
        assert_eq!(home_note_range([]), (36, 83));
    }

    #[test]
    fn framed_fills_the_height_around_a_small_home_range() {
        // A one-octave home range in a viewport ~3 octaves tall: flat rows,
        // centred, more rows on both sides to fill the height.
        let h = 36.0 * KEY_ROW_H;
        let geom = NoteAreaGeom::framed(0.0, h, (60, 71), None, None);
        assert_eq!(geom.row_h, KEY_ROW_H);
        assert!(geom.note_lo < 60 && geom.note_hi > 71);
        assert!(((geom.note_lo + geom.note_hi) as i32 - (60 + 71)).abs() <= 1);
    }

    #[test]
    fn framed_shrinks_rows_to_show_the_whole_home_range() {
        let geom = NoteAreaGeom::framed(0.0, 100.0, (40, 79), None, None);
        assert!(geom.row_h < KEY_ROW_H);
        assert_eq!((geom.note_lo, geom.note_hi), (40, 79));
    }

    #[test]
    fn framed_holds_the_centre_pitch_across_a_resize() {
        // Scrolled away from home, then the pane shrinks enough to refit the
        // rows: the pitch at the centre stays put.
        let home = (40, 79);
        let tall = NoteAreaGeom::framed(0.0, 40.0 * KEY_ROW_H, home, None, None);
        let centre = tall.scrolled_centre_row(-5.0 * KEY_ROW_H);
        let tall = NoteAreaGeom::framed(0.0, 40.0 * KEY_ROW_H, home, Some(centre), None);
        let short = NoteAreaGeom::framed(0.0, 200.0, home, Some(centre), None);
        assert!(short.row_h < tall.row_h);
        let centre_note = |g: &NoteAreaGeom| g.note_row_at(g.top + g.h * 0.5);
        assert_eq!(centre_note(&tall), centre_note(&short));
    }

    #[test]
    fn scrolling_reaches_both_ends_of_the_keyboard_and_stops() {
        let home = (60, 71);
        let h = 20.0 * KEY_ROW_H;
        let geom = NoteAreaGeom::framed(0.0, h, home, None, None);
        // Content down (positive delta) = toward higher pitches.
        let up = NoteAreaGeom::framed(0.0, h, home, Some(geom.scrolled_centre_row(1e6)), None);
        assert_eq!(up.note_hi, 127);
        assert_eq!(up.note_row_at(0.0), Some(127));
        let down = NoteAreaGeom::framed(0.0, h, home, Some(geom.scrolled_centre_row(-1e6)), None);
        assert_eq!(down.note_lo, 0);
        assert_eq!(down.note_row_at(h - 0.5), Some(0));
        // Clamped: scrolling further past the end changes nothing.
        assert_eq!(up.scrolled_centre_row(50.0), up.scrolled_centre_row(0.0));
    }

    #[test]
    fn note_at_row_matches_the_visible_hit_test_and_clamps() {
        let geom = NoteAreaGeom::framed(0.0, 20.0 * KEY_ROW_H, (60, 71), None, None);
        for y in [0.0, 13.0, 100.0, 300.0] {
            assert_eq!(Some(note_at_row(geom.row_at(y))), geom.note_row_at(y));
        }
        assert_eq!(note_at_row(-50.0), 127);
        assert_eq!(note_at_row(1e6), 0);
    }

    #[test]
    fn row_units_round_trip_and_hold_y_inside_the_area() {
        let geom = NoteAreaGeom::framed(40.0, 300.0, (60, 71), None, None);
        assert!((geom.row_to_screen_y(geom.row_at(123.0)) - 123.0).abs() < 1e-3);
        assert_eq!(geom.row_at(0.0), geom.row_at(40.0));
        assert!(geom.row_to_screen_y(geom.row_at(1000.0)) < 340.0);
    }

    #[test]
    fn shows_rows_only_when_every_row_is_wholly_in_view() {
        // Exactly 12 rows on screen, centred on C3..B3.
        let geom = NoteAreaGeom::framed(0.0, 12.0 * KEY_ROW_H, (60, 71), None, None);
        assert!(geom.shows_rows((60, 71)));
        assert!(geom.shows_rows((64, 64)));
        assert!(!geom.shows_rows((59, 71)));
        assert!(!geom.shows_rows((60, 72)));
        assert!(!geom.shows_rows((20, 20)));
    }

    #[test]
    fn edge_overshoot_is_signed_by_direction() {
        let geom = NoteAreaGeom::framed(40.0, 300.0, (60, 71), None, None);
        assert_eq!(geom.edge_overshoot(30.0), 10.0);
        assert_eq!(geom.edge_overshoot(200.0), 0.0);
        assert_eq!(geom.edge_overshoot(350.0), -10.0);
    }

    #[test]
    fn key_zoom_doubles_rightward_halves_leftward_and_clamps() {
        let h = 600.0;
        assert_eq!(key_zoom_row_height(20.0, 0.0, h), 20.0);
        assert!((key_zoom_row_height(20.0, KEY_ZOOM_DOUBLING_PX, h) - 40.0).abs() < 1e-3);
        assert!((key_zoom_row_height(20.0, -KEY_ZOOM_DOUBLING_PX, h) - 10.0).abs() < 1e-3);
        assert_eq!(key_zoom_row_height(20.0, 1e4, h), MAX_ZOOM_ROW_H);
        // Zoomed all the way out, the whole keyboard fits.
        assert_eq!(key_zoom_row_height(20.0, -1e4, h), h / 128.0);
        assert_eq!(key_zoom_row_height(20.0, -1e4, 50.0), 1.0);
    }

    #[test]
    fn key_zoom_steps_back_from_a_limit_at_once() {
        // Stepped from the current height, a drag far past the zoom-out
        // limit carries no overshoot: the next rightward step zooms in.
        let h = 600.0;
        let floor = key_zoom_row_height(20.0, -1e4, h);
        assert!(key_zoom_row_height(floor, 10.0, h) > floor);
    }

    #[test]
    fn anchored_centre_row_steps_back_from_a_keyboard_end_at_once() {
        let home = (60, 71);
        let geom = NoteAreaGeom::framed(40.0, 20.0 * KEY_ROW_H, home, None, None);
        let anchor = geom.row_at(200.0);
        // Dragged far down, past the keyboard's top end: clamped there.
        let centre = geom.anchored_centre_row(anchor, geom.row_h, 1e6);
        let top = NoteAreaGeom::framed(40.0, geom.h, home, Some(centre), None);
        // A step back up from where the row actually is scrolls at once.
        let y = top.row_to_screen_y(anchor) - 5.0;
        assert!(top.anchored_centre_row(anchor, top.row_h, y) > centre);
    }

    #[test]
    fn framed_uses_the_user_zoom_held_to_its_range() {
        let h = 20.0 * KEY_ROW_H;
        let home = (40, 79);
        assert_eq!(
            NoteAreaGeom::framed(0.0, h, home, None, Some(9.0)).row_h,
            9.0
        );
        // A pane shrunk under a deep zoom-out holds the zoom's floor.
        let short = NoteAreaGeom::framed(0.0, 256.0, home, None, Some(1.0));
        assert_eq!(short.row_h, 2.0);
        assert_eq!(
            NoteAreaGeom::framed(0.0, h, home, None, Some(1e3)).row_h,
            MAX_ZOOM_ROW_H
        );
    }

    #[test]
    fn anchored_centre_row_keeps_the_grabbed_row_under_the_pointer() {
        let home = (60, 71);
        let geom = NoteAreaGeom::framed(40.0, 30.0 * KEY_ROW_H, home, None, None);
        let press_y = 200.0;
        let anchor = geom.row_at(press_y);
        // Zoomed in and dragged down: the grabbed row follows the pointer.
        let row_h = 1.5 * KEY_ROW_H;
        let pointer_y = 260.0;
        let centre = geom.anchored_centre_row(anchor, row_h, pointer_y);
        let after = NoteAreaGeom::framed(40.0, geom.h, home, Some(centre), Some(row_h));
        assert!((after.row_to_screen_y(anchor) - pointer_y).abs() < 1e-2);
        // Unchanged height and pointer: the view doesn't move.
        let still = geom.anchored_centre_row(anchor, geom.row_h, press_y);
        assert!((still * geom.row_h - (geom.scroll_y + geom.h * 0.5)).abs() < 1e-2);
    }

    #[test]
    fn anchored_centre_row_stops_at_the_keyboard_ends() {
        let geom = NoteAreaGeom::framed(0.0, 20.0 * KEY_ROW_H, (60, 71), None, None);
        let anchor = geom.row_at(100.0);
        let up = geom.anchored_centre_row(anchor, geom.row_h, 1e6);
        let top = NoteAreaGeom::framed(0.0, geom.h, (60, 71), Some(up), None);
        assert_eq!(top.note_row_at(0.0), Some(127));
        let down = geom.anchored_centre_row(anchor, geom.row_h, -1e6);
        let bottom = NoteAreaGeom::framed(0.0, geom.h, (60, 71), Some(down), None);
        assert_eq!(bottom.note_row_at(geom.h - 0.5), Some(0));
    }

    #[test]
    fn framed_clamps_a_home_range_at_the_keyboard_top() {
        // Home centred near G8 would put empty space above 127: clamped
        // so 127 sits at the top edge instead.
        let geom = NoteAreaGeom::framed(0.0, 30.0 * KEY_ROW_H, (115, 127), None, None);
        assert_eq!(geom.note_hi, 127);
        assert_eq!(geom.note_row_at(0.0), Some(127));
    }

    #[test]
    fn octave_naming_matches_cubase_convention() {
        assert_eq!(octave_of(0), -2); // C-2
        assert_eq!(pitch_class(0), 0);
        assert_eq!(octave_of(127), 8); // G8
        assert_eq!(pitch_class(127), 7);
        assert_eq!(octave_of(60), 3); // C3, MIDI middle C under this convention
        assert_eq!(pitch_class(60), 0);
    }

    #[test]
    fn fit_row_height_stays_flat_when_there_is_room() {
        assert_eq!(fit_row_height(55, 65, 1000.0), KEY_ROW_H);
    }

    #[test]
    fn fit_row_height_shrinks_just_enough_to_avoid_overflow() {
        // 40 rows squeezed into 100px: far below KEY_ROW_H, but the rows
        // must still exactly cover the available height, not overflow it.
        let row_h = fit_row_height(40, 79, 100.0);
        assert!(row_h < KEY_ROW_H);
        assert!(
            40.0 * row_h <= 100.0 + 1e-3,
            "40 rows at {row_h}px each should fit within 100px"
        );
    }

    #[test]
    fn fit_row_height_has_a_floor_when_the_range_cannot_fit_at_all() {
        // A pathologically short viewport (127 rows, 100px) can't fit even
        // at 1px/row — `fit_row_height` floors rather than going to zero or
        // negative; `draw_clip_view`'s clip-rect backstop handles the
        // resulting overflow visually in this extreme case.
        assert_eq!(fit_row_height(0, 126, 100.0), 1.0);
    }

    #[test]
    fn octave_height_is_twelve_times_row_height() {
        // No hardcoded pixel value, and checked at an arbitrary row height
        // (not just `KEY_ROW_H`) so this holds for both the flat and
        // shrunk-to-fit cases.
        let row_h = 3.7_f32;
        for note in 0u8..115 {
            let diff = cum_height_from_top(note, row_h) - cum_height_from_top(note + 12, row_h);
            let tolerance = diff.abs().max(1.0) * 1e-5;
            assert!(
                (diff - 12.0 * row_h).abs() < tolerance,
                "octave span for note {note} should be exactly 12 rows"
            );
        }
    }

    #[test]
    fn cum_height_is_continuous_across_octave_and_semitone_boundaries() {
        // The next-higher note (note+1) sits directly above `note`, so its
        // bottom edge must exactly meet `note`'s top edge — no gap or
        // overlap between distinct rows. Tolerance is relative, not a fixed
        // 1e-4: at large `PIANO_ROLL_SCALE` values the accumulated virtual-y
        // offsets (up to ~2000px unscaled) grow past where a fixed absolute
        // epsilon still fits within f32 precision.
        for note in 0u8..127 {
            let top_of_note = cum_height_from_top(note, KEY_ROW_H);
            let bottom_of_next = cum_height_from_top(note + 1, KEY_ROW_H) + KEY_ROW_H;
            let tolerance = top_of_note.abs().max(1.0) * 1e-5;
            assert!(
                (top_of_note - bottom_of_next).abs() < tolerance,
                "gap/overlap between note {note} and {}: {top_of_note} vs {bottom_of_next}",
                note + 1
            );
        }
    }

    #[test]
    fn only_b_and_e_rows_carry_a_seam() {
        assert_eq!(row_seam_w(59), Some(2.0)); // B2: octave boundary
        assert_eq!(row_seam_w(64), Some(1.0)); // E3
        assert_eq!(row_seam_w(60), None);
        assert_eq!(row_seam_w(66), None);
    }

    #[test]
    fn note_row_at_finds_the_row_holding_the_y() {
        let row_h = 10.0;
        let geom = NoteAreaGeom {
            top: 50.0,
            h: 200.0,
            note_lo: 60,
            note_hi: 71,
            scroll_y: cum_height_from_top(71, row_h),
            row_h,
        };
        assert_eq!(geom.note_row_at(50.0), Some(71));
        assert_eq!(geom.note_row_at(59.9), Some(71));
        assert_eq!(geom.note_row_at(60.0), Some(70));
        assert_eq!(geom.note_row_at(49.0), None);
        assert_eq!(geom.note_row_at(50.0 + 12.0 * row_h), None);
    }

    #[test]
    fn cum_height_decreases_as_pitch_rises() {
        assert!(cum_height_from_top(127, KEY_ROW_H) < cum_height_from_top(60, KEY_ROW_H));
        assert!(cum_height_from_top(60, KEY_ROW_H) < cum_height_from_top(0, KEY_ROW_H));
    }
}
