//! Painting the arranger's marker strip (`600-markers.md` decision 2): a
//! band of its own right under the timeline strip, in the ruler's
//! `bg_timeline` and closed off underneath by the same groove seam, with a
//! quiet `MARKERS` label in the track column beside it. Each marker is a
//! small flag at its tick, its left edge on the tick as the cursor
//! triangle's is, showing its number. Nothing is drawn down through the
//! lanes. The flag hit test a click recalls through is here too, beside the
//! geometry it shares with the drawing.

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Stroke, pos2};

use super::*;

/// Width of a flag, which shows its number alone.
const FLAG_W: f32 = 14.0;
/// Gap above and below a flag, inside the strip.
const FLAG_INSET_Y: f32 = 2.0;
/// Width of the flag's pole: the line on the marker's tick.
const POLE_W: f32 = 1.0;
/// Strength of a flag's fill (`fg_dim`), so the flags read without
/// outshouting the ruler. Tune by eye in every palette.
const FLAG_FILL_STRENGTH: f32 = 0.3;

impl Display {
    /// Draws the marker strip, from `draw_arranger_backgrounds` (the strip's
    /// slot in `arranger_layout`): the `MARKERS` label in the track column,
    /// the strip's fill over the content area, the flags, and the groove
    /// seam closing it off from the performance lane. The timeline strip's
    /// own seam is its top edge.
    pub(super) fn draw_marker_strip(&self, painter: &Painter, rect: Rect) {
        let layout = self.arranger_layout();
        let top = rect.min.y + layout.markers_top;
        let bottom = top + layout.markers_h;
        let font = FontId::proportional(theme::FONT_SIZE_TL);

        painter.text(
            pos2(
                rect.min.x + Self::TRACK_HEADER_INNER_PAD_X,
                (top + bottom) * 0.5,
            ),
            Align2::LEFT_CENTER,
            "MARKERS",
            font.clone(),
            theme::fg_dim(),
        );

        let strip = Rect::from_x_y_ranges(self.content_origin_x()..=rect.max.x, top..=bottom);
        painter.rect_filled(strip, CornerRadius::ZERO, theme::bg_timeline());

        // A flag sits on the seam's top edge, not under it.
        let seam_y = seam_y(bottom, ARRANGER_LANE_SEAM_W, painter.pixels_per_point());
        let flag_top = top + FLAG_INSET_Y;
        let flag_bottom = seam_y - ARRANGER_LANE_SEAM_W / 2.0 - FLAG_INSET_Y;
        let content = Rect::from_x_y_ranges(
            self.content_origin_x()..=self.content_right_x(),
            top..=bottom,
        );
        let flags = painter.with_clip_rect(content.intersect(painter.clip_rect()));
        let fill = theme::fg_dim().gamma_multiply(FLAG_FILL_STRENGTH);
        for (number, x) in self.marker_flags() {
            let flag = Rect::from_min_max(pos2(x, flag_top), pos2(x + FLAG_W, flag_bottom));
            flags.rect_filled(flag, CornerRadius::ZERO, fill);
            flags.vline(
                x + POLE_W * 0.5,
                flag_top..=flag_bottom,
                Stroke::new(POLE_W, theme::fg_dim()),
            );
            flags.text(
                pos2(x + POLE_W + (FLAG_W - POLE_W) * 0.5, flag.center().y),
                Align2::CENTER_CENTER,
                number.to_string(),
                font.clone(),
                theme::fg(),
            );
        }

        painter.hline(
            rect.min.x..=rect.max.x,
            seam_y,
            Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
        );
    }

    /// Each marker's number and its flag's left edge on screen (its tick),
    /// in tick order, so a later flag draws over an earlier one it overlaps.
    /// Those scrolled wholly out of the content area are left out.
    fn marker_flags(&self) -> Vec<(u8, f32)> {
        let (left, right) = (self.content_origin_x(), self.content_right_x());
        self.markers
            .as_slice()
            .iter()
            .map(|marker| (marker.number, self.tick_to_screen_x(marker.tick)))
            .filter(|&(_, x)| x + FLAG_W > left && x < right)
            .collect()
    }

    /// The number of the marker whose flag holds `(x, y)`, if any: the one
    /// drawn on top where two overlap (`flag_at`). Arranger only.
    pub(in crate::view::display) fn marker_flag_at(&self, x: f32, y: f32) -> Option<u8> {
        if !self.is_mouse_on_marker_strip(x, y) || !self.content_x_on_screen(x) {
            return None;
        }
        flag_at(&self.marker_flags(), x)
    }
}

/// The number of the flag in `flags` (`(number, left edge)`, in drawing
/// order) whose `FLAG_W` span holds `x`, the last drawn winning where two
/// overlap, as the eye sees it. A flag's right edge is outside it.
fn flag_at(flags: &[(u8, f32)], x: f32) -> Option<u8> {
    flags
        .iter()
        .rev()
        .find(|&&(_, left)| (left..left + FLAG_W).contains(&x))
        .map(|&(number, _)| number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_on_a_flag_finds_its_number() {
        let flags = [(1, 100.0), (4, 300.0)];
        assert_eq!(flag_at(&flags, 100.0), Some(1));
        assert_eq!(flag_at(&flags, 100.0 + FLAG_W - 0.5), Some(1));
        assert_eq!(flag_at(&flags, 305.0), Some(4));
    }

    #[test]
    fn a_press_beside_every_flag_finds_none() {
        let flags = [(1, 100.0), (4, 300.0)];
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
        let flags = [(2, 100.0), (7, 106.0)];
        assert_eq!(flag_at(&flags, 108.0), Some(7));
        assert_eq!(flag_at(&flags, 103.0), Some(2));
    }
}
