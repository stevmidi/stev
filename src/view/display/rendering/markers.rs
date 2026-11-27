//! Painting the arranger's marker strip (`600-markers.md` decision 2): a
//! band of its own right under the timeline strip, in the ruler's
//! `bg_timeline` with no seam between them, closed off underneath by the
//! lanes' groove seam, with a quiet `Marker` label in the track column
//! beside it. Each marker is a small flag at its tick, its left edge on the
//! tick as the cursor triangle's is, showing its number and its name if it
//! has one (`2 Chorus`); the marker selected while the strip has the
//! keyboard is drawn in `accent`, and one held in a drag where it would
//! land. The lanes' grid lines run through the strip under the flags;
//! nothing of the strip is drawn down through the lanes. The flag hit test
//! the strip's presses go through is here too, beside the geometry it
//! shares with the drawing.

use egui::{Align2, CornerRadius, FontId, Galley, Painter, Rect, Stroke, pos2};

use crate::models::markers::{MAX_MARKER, Markers};

use super::*;

/// The narrowest a flag gets: one showing a single digit.
const FLAG_W: f32 = 14.0;
/// Room either side of a flag's label, between the pole and the flag's
/// right edge.
const LABEL_PAD_X: f32 = 3.0;
/// Room above and below a flag's label: between the ink of its number and
/// the flag's top and bottom edges (`flag_span`).
const LABEL_PAD_Y: f32 = 2.0;
/// Width of the flag's pole: the line on the marker's tick.
const POLE_W: f32 = 1.0;
/// How far a flag's fill goes from the strip's `bg_timeline` toward
/// `fg_dim`, so the flags read without outshouting the ruler. Opaque, so a
/// flag on top hides the label under it. Tune by eye in every palette.
const FLAG_FILL_STRENGTH: f32 = 0.3;
/// How far the selected flag's fill goes toward `accent`.
const SELECTED_FILL_STRENGTH: f32 = 0.45;

/// One flag as drawn and hit-tested: its marker's number, its left edge on
/// screen (the marker's tick) and its width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Flag {
    /// The marker's number.
    number: u8,
    /// Left edge, on the marker's tick.
    left: f32,
    /// Width: [`flag_w`] of its label.
    width: f32,
}

/// What a marker's flag shows: its number, and its name after it.
fn flag_label(number: u8, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{number} {name}"),
        None => number.to_string(),
    }
}

/// A flag's width for a label `label_w` wide: the pole and the label with
/// its room, never narrower than [`FLAG_W`].
fn flag_w(label_w: f32) -> f32 {
    (POLE_W + label_w + 2.0 * LABEL_PAD_X).max(FLAG_W)
}

impl Display {
    /// Measures each marker's label (`flag_label`) in the strip's font for
    /// the hit test, which has no fonts at hand (`marker_label_w`). Once a
    /// frame, before the strip is drawn; egui caches the layouts.
    pub(in crate::view::display) fn measure_marker_flags(&mut self, painter: &Painter) {
        let font = FontId::proportional(theme::FONT_SIZE_TL);
        let mut widths = [0.0; MAX_MARKER as usize];
        for marker in self.markers.as_slice() {
            let label = flag_label(marker.number, marker.name.as_deref());
            let galley = painter.layout_no_wrap(label, font.clone(), theme::fg());
            widths[usize::from(marker.number - 1)] = galley.size().x;
        }
        self.marker_label_w = widths;
    }

    /// Draws the marker strip's background, from `draw_arranger_backgrounds`
    /// (the strip's slot in `arranger_layout`): the `Marker` label in the
    /// track column, the strip's fill over the content area, and the groove
    /// seam closing it off from the performance lane. No seam above it: it
    /// runs on from the timeline strip in the same `bg_timeline`, and
    /// `draw_timeline` draws the lanes' grid lines through it. The flags go
    /// over those lines (`draw_marker_flags`).
    pub(super) fn draw_marker_strip(&self, painter: &Painter, rect: Rect) {
        let (top, bottom) = self.marker_strip_span(rect);
        let font = FontId::proportional(theme::FONT_SIZE_TL);

        painter.text(
            pos2(
                rect.min.x + Self::TRACK_HEADER_INNER_PAD_X,
                (top + bottom) * 0.5,
            ),
            Align2::LEFT_CENTER,
            "Marker",
            font.clone(),
            theme::fg_dim(),
        );

        let strip = Rect::from_x_y_ranges(self.content_origin_x()..=rect.max.x, top..=bottom);
        painter.rect_filled(strip, CornerRadius::ZERO, theme::bg_timeline());

        painter.hline(
            rect.min.x..=rect.max.x,
            seam_y(bottom, ARRANGER_LANE_SEAM_W, painter.pixels_per_point()),
            Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
        );
    }

    /// The marker strip's top and bottom on screen.
    pub(super) fn marker_strip_span(&self, rect: Rect) -> (f32, f32) {
        let layout = self.arranger_layout();
        let top = rect.min.y + layout.markers_top;
        (top, top + layout.markers_h)
    }

    /// Draws the marker strip's flags, from `draw_pane` after `draw_timeline`
    /// so they sit over the grid lines it draws through the strip.
    pub(super) fn draw_marker_flags(&self, painter: &Painter, rect: Rect) {
        let (top, bottom) = self.marker_strip_span(rect);
        let font = FontId::proportional(theme::FONT_SIZE_TL);
        let ppp = painter.pixels_per_point();
        // Every label starts with its number, so one digit's ink sets every
        // flag's height: the ink and `LABEL_PAD_Y` above and below it, in
        // whole pixels, centred between the strip's top (no seam: it runs
        // on from the ruler) and its own seam's top edge.
        let ink = ink_span(&painter.layout_no_wrap("1".to_owned(), font.clone(), theme::fg()));
        let (_, free_bottom) = seam_inset_band(top, bottom, ARRANGER_LANE_SEAM_W / 2.0, ppp);
        let free = ((top * ppp).round() / ppp, free_bottom);
        let (flag_top, flag_bottom) = flag_span(free, ink.1 - ink.0, ppp);
        let label_y = flag_top + (LABEL_PAD_Y * ppp).round() / ppp - ink.0;
        let content = Rect::from_x_y_ranges(
            self.content_origin_x()..=self.content_right_x(),
            top..=bottom,
        );
        let flags = painter.with_clip_rect(content.intersect(painter.clip_rect()));
        let markers = self.shown_markers();
        let selected = self.selected_marker();
        for flag in self.marker_flags(&markers) {
            let (fill, pole) = if selected == Some(flag.number) {
                (
                    theme::bg_timeline().lerp_to_gamma(theme::accent(), SELECTED_FILL_STRENGTH),
                    theme::accent(),
                )
            } else {
                (
                    theme::bg_timeline().lerp_to_gamma(theme::fg_dim(), FLAG_FILL_STRENGTH),
                    theme::fg_dim(),
                )
            };
            let shape = Rect::from_min_max(
                pos2(flag.left, flag_top),
                pos2(flag.left + flag.width, flag_bottom),
            );
            flags.rect_filled(shape, CornerRadius::ZERO, fill);
            flags.vline(
                flag.left + POLE_W * 0.5,
                flag_top..=flag_bottom,
                Stroke::new(POLE_W, pole),
            );
            let name = markers
                .get(flag.number)
                .and_then(|marker| marker.name.as_deref());
            // One digit centres in the narrowest flag; a name reads from
            // the pole. Placed by the ink of its number, not the line box,
            // which puts the digits half a point low at 1x.
            let label =
                flags.layout_no_wrap(flag_label(flag.number, name), font.clone(), theme::fg());
            let x = if name.is_some() {
                flag.left + POLE_W + LABEL_PAD_X
            } else {
                flag.left + POLE_W + (flag.width - POLE_W - label.size().x) * 0.5
            };
            flags.galley(pos2(x, label_y), label, theme::fg());
        }
    }

    /// Each of `markers`' flags on screen, in drawing order: by tick, so a
    /// later flag covers an earlier one it overlaps, then those raised by a
    /// selection over them in the order they were (`stack`, from
    /// `marker_stack`), then the selected one (the one dragged too) over
    /// them all (`raise`, so a press is on top before the next frame stacks
    /// it). Those scrolled wholly out of the content area are left out.
    fn marker_flags(&self, markers: &Markers) -> Vec<Flag> {
        let (left, right) = (self.content_origin_x(), self.content_right_x());
        let flags = markers
            .as_slice()
            .iter()
            .map(|marker| Flag {
                number: marker.number,
                left: self.tick_to_screen_x(marker.tick),
                width: flag_w(self.marker_label_w[usize::from(marker.number - 1)]),
            })
            .filter(|flag| flag.left + flag.width > left && flag.left < right)
            .collect();
        raise(stack(flags, &self.marker_stack), self.selected_marker())
    }

    /// Once a frame, before the strip is drawn: puts the selected marker on
    /// top of `marker_stack`, however it was selected (a press, a drag,
    /// `←`/`→`, an undo), and drops the markers gone. So a flag raised by a
    /// selection stays on top of the ones it overlaps after the strip gives
    /// the keyboard back, rather than falling back under a later one.
    /// The strip's deleted markers (`markers_deleted`) are dropped here too
    /// once no marker is selected: the strip has given the keyboard back.
    pub(in crate::view::display) fn sync_marker_stack(&mut self) {
        match self.selected_marker() {
            Some(number) => put_on_top(&mut self.marker_stack, number),
            None => self.markers_deleted.clear(),
        }
        let markers = &self.markers;
        self.marker_stack
            .retain(|&number| markers.get(number).is_some());
    }

    /// The number of the marker whose flag holds `(x, y)`, if any: the one
    /// drawn on top where two overlap (`flag_at`). Arranger only.
    pub(in crate::view::display) fn marker_flag_at(&self, x: f32, y: f32) -> Option<u8> {
        if !self.is_mouse_on_marker_strip(x, y) || !self.content_x_on_screen(x) {
            return None;
        }
        flag_at(&self.marker_flags(&self.shown_markers()), x)
    }

    /// Marker `number`'s flag on screen, the strip's full height: where its
    /// rename field goes. `None` when it isn't set or is scrolled away.
    pub(in crate::view::display) fn marker_flag_rect(&self, number: u8) -> Option<Rect> {
        let layout = self.arranger_layout();
        let flag = self
            .marker_flags(&self.markers)
            .into_iter()
            .find(|flag| flag.number == number)?;
        Some(Rect::from_min_max(
            pos2(flag.left, layout.markers_top),
            pos2(
                flag.left + flag.width,
                layout.markers_top + layout.markers_h,
            ),
        ))
    }
}

/// Where the ink of `galley`'s first glyph starts and ends, below the
/// galley's top: a marker label's number. The whole galley when it has no
/// glyph.
fn ink_span(galley: &Galley) -> (f32, f32) {
    galley
        .rows
        .first()
        .and_then(|row| {
            let glyph = row.glyphs.first()?;
            let top = row.pos.y + glyph.pos.y + glyph.uv_rect.offset.y;
            Some((top, top + glyph.uv_rect.size.y))
        })
        .unwrap_or((0.0, galley.size().y))
}

/// A flag's `(top, bottom)` around ink `ink_h` tall: `LABEL_PAD_Y` above and
/// below it, each in whole pixels, so the label has exactly as much room
/// above as below, centred in `free` (the strip between its seams, whole
/// pixels). Where the two can't both be even, the room left over is a pixel
/// more below the flag than above: the label's own padding wins.
fn flag_span(free: (f32, f32), ink_h: f32, ppp: f32) -> (f32, f32) {
    let pad = (LABEL_PAD_Y * ppp).round();
    let height = (ink_h * ppp).round() + 2.0 * pad;
    let (free_top, free_bottom) = ((free.0 * ppp).round(), (free.1 * ppp).round());
    let top = free_top + ((free_bottom - free_top - height) / 2.0).floor();
    (top / ppp, (top + height) / ppp)
}

/// `order` with `number` moved (or added) to its end: on top.
fn put_on_top(order: &mut Vec<u8>, number: u8) {
    if order.last() != Some(&number) {
        order.retain(|&raised| raised != number);
        order.push(number);
    }
}

/// `flags` (in tick order) with those in `order` moved to the end in that
/// order, the last in it drawn last and so on top; the rest keep tick
/// order under them.
fn stack(mut flags: Vec<Flag>, order: &[u8]) -> Vec<Flag> {
    flags.sort_by_key(|flag| order.iter().position(|&number| number == flag.number));
    flags
}

/// `flags` with marker `number`'s moved to the end, drawn last and so on
/// top: the selected flag, the dragged one, wins where flags overlap, as
/// the eye and the hit test both read it. Otherwise unchanged.
fn raise(mut flags: Vec<Flag>, number: Option<u8>) -> Vec<Flag> {
    if let Some(at) = flags.iter().position(|flag| Some(flag.number) == number) {
        let flag = flags.remove(at);
        flags.push(flag);
    }
    flags
}

/// The number of the flag in `flags` (in drawing order) whose span holds
/// `x`, the last drawn winning where two overlap, as the eye sees it. A
/// flag's right edge is outside it.
fn flag_at(flags: &[Flag], x: f32) -> Option<u8> {
    flags
        .iter()
        .rev()
        .find(|flag| (flag.left..flag.left + flag.width).contains(&x))
        .map(|flag| flag.number)
}

#[cfg(test)]
mod tests {
    use egui::Color32;
    use egui::epaint::{Fonts, TextOptions};

    use crate::view::theme::fonts::font_definitions;

    use super::*;

    /// Flags `(number, left edge)`, each [`FLAG_W`] wide.
    fn flags(list: &[(u8, f32)]) -> Vec<Flag> {
        list.iter()
            .map(|&(number, left)| Flag {
                number,
                left,
                width: FLAG_W,
            })
            .collect()
    }

    #[test]
    fn a_press_on_a_flag_finds_its_number() {
        let flags = flags(&[(1, 100.0), (4, 300.0)]);
        assert_eq!(flag_at(&flags, 100.0), Some(1));
        assert_eq!(flag_at(&flags, 100.0 + FLAG_W - 0.5), Some(1));
        assert_eq!(flag_at(&flags, 305.0), Some(4));
    }

    #[test]
    fn a_press_beside_every_flag_finds_none() {
        let flags = flags(&[(1, 100.0), (4, 300.0)]);
        assert_eq!(flag_at(&flags, 99.5), None);
        assert_eq!(flag_at(&flags, 100.0 + FLAG_W), None);
        assert_eq!(flag_at(&flags, 200.0), None);
        assert_eq!(flag_at(&[], 200.0), None);
    }

    /// Two flags close enough to overlap: the later one, drawn on top, takes
    /// the press where they overlap, and the earlier one keeps the part of
    /// it still showing.
    #[test]
    fn of_two_overlapping_flags_the_one_on_top_wins() {
        let flags = flags(&[(2, 100.0), (7, 106.0)]);
        assert_eq!(flag_at(&flags, 108.0), Some(7));
        assert_eq!(flag_at(&flags, 103.0), Some(2));
    }

    /// The selected flag goes on top of one it overlaps, wherever its tick
    /// is, and takes the press there.
    #[test]
    fn the_selected_flag_is_raised_over_the_others() {
        let flags = flags(&[(2, 100.0), (1, 106.0), (5, 300.0)]);
        let raised = raise(flags.clone(), Some(2));
        let order: Vec<u8> = raised.iter().map(|flag| flag.number).collect();
        assert_eq!(order, vec![1, 5, 2]);
        assert_eq!(flag_at(&raised, 108.0), Some(2));
        assert_eq!(raise(flags.clone(), None), flags);
        assert_eq!(raise(flags.clone(), Some(9)), flags);
    }

    /// A flag raised once stays over the flags it overlaps with nothing
    /// selected; the last raised is on top, the rest keep tick order.
    #[test]
    fn raised_flags_stay_on_top_in_the_order_raised() {
        let flags = flags(&[(2, 100.0), (5, 106.0), (7, 112.0), (9, 300.0)]);
        let mut order = Vec::new();
        put_on_top(&mut order, 7);
        put_on_top(&mut order, 2);
        put_on_top(&mut order, 7);
        assert_eq!(order, vec![2, 7]);
        let stacked = stack(flags, &order);
        let numbers: Vec<u8> = stacked.iter().map(|flag| flag.number).collect();
        assert_eq!(numbers, vec![5, 9, 2, 7]);
        assert_eq!(flag_at(&stacked, 108.0), Some(2), "2 over 5");
        assert_eq!(flag_at(&stacked, 113.0), Some(7));
    }

    /// A named flag is as wide as its label, so its whole name takes the
    /// press; a bare number keeps the narrowest flag.
    #[test]
    fn a_named_flag_is_as_wide_as_its_label() {
        assert_eq!(flag_w(7.0), FLAG_W);
        let named = flag_w(60.0);
        assert_eq!(named, POLE_W + 60.0 + 2.0 * LABEL_PAD_X);
        let flags = [Flag {
            number: 2,
            left: 100.0,
            width: named,
        }];
        assert_eq!(flag_at(&flags, 160.0), Some(2));
    }

    /// The ink is where the number's quad is drawn, at 1x and at 2x.
    #[test]
    fn the_ink_is_the_numbers_quad() {
        for ppp in [1.0, 2.0] {
            let mut fonts = Fonts::new(TextOptions::default(), font_definitions());
            let mut fonts = fonts.with_pixels_per_point(ppp);
            let galley = fonts.layout_no_wrap(
                "2 Chorus".to_owned(),
                FontId::proportional(theme::FONT_SIZE_TL),
                Color32::WHITE,
            );
            let row = &galley.rows[0];
            let first = row.glyphs[0].first_vertex as usize;
            let quad = &row.visuals.mesh.vertices[first..first + 4];
            let top = quad.iter().map(|v| v.pos.y).fold(f32::MAX, f32::min);
            let bottom = quad.iter().map(|v| v.pos.y).fold(f32::MIN, f32::max);
            assert_eq!(ink_span(&galley), (top, bottom), "at {ppp}x");
        }
    }

    /// 9px of digits at 1x in 18px between the seams: the flag is the ink
    /// and 2px either side, the leftover pixel below it.
    #[test]
    fn a_flag_pads_its_label_evenly() {
        let (top, bottom) = flag_span((12.0, 30.0), 9.0, 1.0);
        assert_eq!((top, bottom), (14.0, 27.0));
        assert_eq!(bottom - top - 9.0, 2.0 * LABEL_PAD_Y);
        // At 2x every span is whole pixels too.
        let (top, bottom) = flag_span((12.0, 30.5), 9.0, 2.0);
        assert_eq!((top * 2.0).fract(), 0.0);
        assert_eq!(bottom - top, 13.0);
    }

    #[test]
    fn a_label_is_the_number_then_the_name() {
        assert_eq!(flag_label(2, Some("Chorus")), "2 Chorus");
        assert_eq!(flag_label(9, None), "9");
    }
}
