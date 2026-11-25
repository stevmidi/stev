//! Painting the arranger's performance-lane row (`110-performance-lane.md`):
//! the gutter and its selection accent, and in the content area the keyboard
//! that drives the lane, lined up with where its keys jump
//! (`archive/510-performance-lane-keyboard.md`), running on along the timeline
//! from bar 1 (`560-performance-lane-reach.md`). Key `k` is a white or black
//! key by its pitch class (`key_pitch_class`, key 0 a C) and spans one key
//! length (a bar, half a bar or a beat, `KeyLength`) from `key_tick(k)`,
//! picked from the `Bar` / `½` / `Beat` labels in the gutter
//! (§ Step 1b): a thin strip along the lane's bottom edge carries each
//! key, drawn as a keyboard (an unbroken white strip, each black key a block
//! hanging from its top), unnamed: which note plays a key depends on where
//! the hand is (`570-lane-octave-buttons.md` § 4). A key the
//! live keyboard holds lights while the lane is armed, on the key its press
//! landed on. While armed, the keys the player's hand is on (the
//! sequencer's `Hand`) are washed where its presses land: one unbroken
//! stretch placed by octave around the arranger cursor, moved an octave by
//! the keyboard's previous/next buttons (`570-lane-octave-buttons.md`).
//! Static otherwise: it changes with zoom, scroll, the meter, the cursor
//! and those buttons, never with playing (the playhead is the trigger's
//! feedback).

use std::ops::Range;

use egui::{Align2, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, pos2, vec2};

use super::arranger::lane_grid_fill;
use super::piano_keys::{is_black_key, row_seam_w};
use super::*;
use crate::core::time::Meter;
use crate::models::performance_lane::{KeyLength, key_pitch_class, key_tick};

/// Height of the key strip along the lane's bottom edge. Narrow, as the
/// piano roll's key column is: the piano roll's near-white and near-black
/// read well in a narrow band and stark filling the whole row (tried first).
const KEY_STRIP_H: f32 = 8.0;

/// How far down the strip a black key hangs, as a share of `KEY_STRIP_H`; the
/// white strip runs unbroken beneath it. Drawn flat, white and black
/// alternating, the black keys vanished into the dark lane and the strip
/// read as a dashed line.
const BLACK_KEY_DEPTH: f32 = 0.6;

/// Strength of the key strip while the lane is unarmed, so the row doesn't
/// compete with the tracks under it; armed, it is drawn at full strength.
/// Tune by eye in every palette.
const UNARMED_KEY_STRENGTH: f32 = 0.5;

/// Strength of the accent wash over a held key's span above the strip, whose
/// own piece of the strip lights in the full accent. Tune by eye.
const HELD_KEY_WASH: f32 = 0.25;

/// Strength of the accent wash over the keys the player's hand is on,
/// above the strip: under a held key's, so a held key still
/// stands out inside it. Tune by eye.
const KEYBED_WINDOW_WASH: f32 = 0.1;

/// A gutter label's cell: its text and the room after it, so a cell's width
/// is what a click on it takes (`key_length_labels`).
fn key_length_label(length: KeyLength) -> (&'static str, f32) {
    match length {
        KeyLength::Bar => ("Bar", 32.0),
        KeyLength::HalfBar => ("½", 22.0),
        KeyLength::Beat => ("Beat", 36.0),
    }
}

impl Display {
    /// Draws the performance-lane row (above track 1), from
    /// `draw_arranger_backgrounds`: the gutter (an accent tint and the 4px
    /// left marker while armed), then the content area's lane fill and the
    /// keys (module doc), and the row's bottom seam. `draw_timeline` stops
    /// its in-lane grid lines short of the row: the key edges are the beat
    /// lines here. Not a track: its geometry comes from
    /// `Display::arranger_layout()`, the same source
    /// `track_idx_at`/`performance_lane_hit_at` use for hit-testing.
    pub(super) fn draw_performance_lane_row(&self, painter: &Painter, rect: Rect) {
        let layout = self.arranger_layout();
        let lane_top = rect.min.y + layout.performance_lane_top;
        let lane_h = layout.performance_lane_h;
        let lane_bottom = lane_top + lane_h;
        let armed = self.performance_lane_selected();
        let accent = theme::accent();

        let labels_right = self.content_origin_x() - Self::TRACK_COLUMN_GRID_GAP_X;
        let gutter_fill = if armed {
            accent.gamma_multiply(0.18)
        } else {
            theme::bg_panel().gamma_multiply(0.16)
        };
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.min.x, lane_top),
                vec2(labels_right - rect.min.x, lane_h),
            ),
            CornerRadius::ZERO,
            gutter_fill,
        );
        if armed {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.min.x, lane_top), vec2(4.0, lane_h)),
                CornerRadius::ZERO,
                accent,
            );
        }
        self.draw_key_length_labels(painter);

        let content = Rect::from_x_y_ranges(
            self.content_origin_x()..=self.content_right_x(),
            lane_top..=lane_bottom,
        );
        let p = painter.with_clip_rect(content);
        p.rect_filled(content, CornerRadius::ZERO, lane_grid_fill(armed, accent));

        // The strip sits on the bottom seam's top edge, not under it.
        let bottom_seam_y = seam_y(
            lane_bottom,
            ARRANGER_LANE_SEAM_W,
            painter.pixels_per_point(),
        );
        let strip_bottom = bottom_seam_y - ARRANGER_LANE_SEAM_W / 2.0;
        let strip_top = strip_bottom - KEY_STRIP_H;

        let meter = self.meter();
        let length = self.performance_lane_key_length();
        let ppt = self.pixels_per_tick();
        let key_ticks = key_tick(meter, length, 1);
        let keys = visible_keys(self.render_scroll_x(), self.content_w(), ppt, key_ticks);
        // Opaque, so a black key drawn over the white strip shows its own
        // tone rather than a blend of the two.
        let lane_fill = lane_grid_fill(armed, accent);
        let (white, black) = if armed {
            (theme::piano_key_white(), theme::piano_key_black())
        } else {
            (
                lane_fill.lerp_to_gamma(theme::piano_key_white(), UNARMED_KEY_STRENGTH),
                lane_fill.lerp_to_gamma(theme::piano_key_black(), UNARMED_KEY_STRENGTH),
            )
        };
        let hand = self.performance_lane_hand();

        for key_index in keys {
            let left = self.tick_to_screen_x(key_tick(meter, length, key_index));
            let right = self.tick_to_screen_x(key_tick(meter, length, key_index + 1));
            let above = Rect::from_x_y_ranges(left..=right, lane_top..=strip_top);
            let key = Rect::from_x_y_ranges(left..=right, strip_top..=strip_bottom);
            // The note of the hand whose press lands on this key, while
            // armed: lit held, washed otherwise.
            let note_here = hand.note_on(key_index).filter(|_| armed);
            let lit = note_here.is_some_and(|n| self.live_notes.is_held(n));
            let wash = if lit {
                Some(HELD_KEY_WASH)
            } else if note_here.is_some() {
                Some(KEYBED_WINDOW_WASH)
            } else {
                None
            };
            if let Some(strength) = wash {
                p.rect_filled(above, CornerRadius::ZERO, accent.gamma_multiply(strength));
            }
            if is_black_key(key_pitch_class(key_index)) {
                // White beneath, the black key hanging from the top, and the
                // seam where the two white keys either side meet under it.
                let (hanging, under) = black_key_parts(key);
                p.rect_filled(key, CornerRadius::ZERO, white);
                p.rect_filled(
                    hanging,
                    CornerRadius::ZERO,
                    if lit { accent } else { black },
                );
                p.vline(
                    under.min.x,
                    under.y_range(),
                    Stroke::new(1.0_f32, grid_seam_color()),
                );
            } else {
                p.rect_filled(key, CornerRadius::ZERO, if lit { accent } else { white });
            }

            if let Some(seam_w) = key_seam_w(key_index) {
                p.vline(
                    left,
                    strip_top..=strip_bottom,
                    Stroke::new(seam_w, grid_seam_color()),
                );
            }
        }

        // Seam below the row, matching the track-lane separators.
        painter.hline(
            rect.x_range(),
            bottom_seam_y,
            Stroke::new(ARRANGER_LANE_SEAM_W, grid_seam_color()),
        );
    }

    /// The gutter's key-length labels this frame, each with the cell a click
    /// on it takes: shared by the drawing and the hit test, as
    /// `arranger_layout` is for the row.
    fn key_length_labels(&self) -> Vec<(KeyLength, Rect)> {
        let layout = self.arranger_layout();
        let row = Rect::from_min_size(
            pos2(
                self.render.canvas_rect.min.x + Self::TRACK_HEADER_INNER_PAD_X,
                layout.performance_lane_top,
            ),
            vec2(0.0, layout.performance_lane_h),
        );
        key_length_labels(self.meter(), row)
    }

    /// The key length whose gutter label holds `(x, y)`, if any. Arranger
    /// only: a click on it picks that length and does nothing else
    /// (`archive/510-performance-lane-keyboard.md` § Step 1b decision 3).
    pub(in crate::view::display) fn key_length_label_at(
        &self,
        x: f32,
        y: f32,
    ) -> Option<KeyLength> {
        key_length_label_in(&self.key_length_labels(), pos2(x, y))
    }

    /// Whether the pointer is on a gutter label a click would switch to:
    /// not the length that plays. Its pointing hand.
    pub(in crate::view::display) fn is_pointer_on_key_length_switch(&mut self) -> bool {
        let playing = self.performance_lane_key_length().in_meter(self.meter());
        self.in_pane(Pane::Arranger, |display| {
            display
                .canvas_pointer()
                .and_then(|p| display.key_length_label_at(p.x, p.y))
        })
        .is_some_and(|length| length != playing)
    }

    /// Draws `Bar`, `½` and `Beat` in the gutter, coloured as the piano
    /// roll's lane switch is (`toggle_icon_color`): the length that plays
    /// `accent`, one under the pointer `fg`, the rest `fg_dim`. In an odd
    /// meter `½` isn't drawn, and `Beat` is lit if `½` is the stored choice.
    fn draw_key_length_labels(&self, painter: &Painter) {
        let playing = self.performance_lane_key_length().in_meter(self.meter());
        let labels = self.key_length_labels();
        let hovered = self
            .canvas_pointer()
            .and_then(|p| key_length_label_in(&labels, p));
        let font = FontId::proportional(theme::FONT_SIZE_TL);
        for (length, cell) in labels {
            painter.text(
                pos2(cell.left(), cell.center().y),
                Align2::LEFT_CENTER,
                key_length_label(length).0,
                font.clone(),
                toggle_icon_color(length == playing, hovered == Some(length)),
            );
        }
    }
}

/// The gutter's key-length labels laid out left to right from `row`'s left
/// edge, each cell `row`'s height and its label's width
/// (`key_length_label`): `Bar`, `½` where `meter` offers half a bar
/// (`KeyLength::is_offered`), `Beat`.
fn key_length_labels(meter: Meter, row: Rect) -> Vec<(KeyLength, Rect)> {
    let mut left = row.left();
    KeyLength::ALL
        .into_iter()
        .filter(|length| length.is_offered(meter))
        .map(|length| {
            let w = key_length_label(length).1;
            let cell = Rect::from_x_y_ranges(left..=left + w, row.y_range());
            left += w;
            (length, cell)
        })
        .collect()
}

/// The length whose cell of `labels` holds `pos`. A cell's right edge
/// belongs to the next one.
fn key_length_label_in(labels: &[(KeyLength, Rect)], pos: Pos2) -> Option<KeyLength> {
    labels
        .iter()
        .find(|(_, cell)| {
            (cell.left()..cell.right()).contains(&pos.x)
                && (cell.top()..cell.bottom()).contains(&pos.y)
        })
        .map(|&(length, _)| length)
}

/// The keys (0-based, end exclusive) at least partly inside a content area
/// `content_w` wide scrolled `scroll_x` at `pixels_per_tick`, `key_ticks` to
/// a key: from the key at the left edge to the one at the right, none
/// before bar 1. The keys repeat for as long as the timeline runs
/// (`560-performance-lane-reach.md`).
fn visible_keys(scroll_x: f32, content_w: f32, pixels_per_tick: f32, key_ticks: i32) -> Range<i32> {
    let key_w = pixels_per_tick * key_ticks as f32;
    // A degenerate zoom (zero or not finite) shows no keys, rather than a
    // range from a NaN or infinite division.
    if !key_w.is_finite() || key_w <= 0.0 {
        return 0..0;
    }
    let first = (scroll_x / key_w).floor() as i32;
    let end = ((scroll_x + content_w) / key_w).ceil() as i32;
    first.max(0)..end.max(0)
}

/// The seam at the left edge of key `key_index`, as the piano roll draws it
/// between the same two keys (`row_seam_w`): 2px where B meets C, 1px where
/// E meets F, the only neighbours with no black key between them; `None` at
/// every other boundary, which the colour change already shows, and at the
/// first key, with none before it. Across a cycle's wrap, B7 meets C-2.
fn key_seam_w(key_index: i32) -> Option<f32> {
    if key_index <= 0 {
        return None;
    }
    row_seam_w(key_pitch_class(key_index - 1))
}

/// A black key's two parts in its slice `key` of the strip: the block hanging
/// `BLACK_KEY_DEPTH` of the way down from its top, and the zero-width span
/// below it, at the slice's middle, where the white keys either side meet.
fn black_key_parts(key: Rect) -> (Rect, Rect) {
    let hang_bottom = key.top() + key.height() * BLACK_KEY_DEPTH;
    let hanging = Rect::from_x_y_ranges(key.x_range(), key.top()..=hang_bottom);
    let mid_x = key.center().x.round();
    let under = Rect::from_x_y_ranges(mid_x..=mid_x, hang_bottom..=key.bottom());
    (hanging, under)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_keys_follow_scroll_and_zoom() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, KeyLength::Beat, 1);
        // 100 px to a key, a beat: 400 to a bar.
        let ppt = 100.0 / key_ticks as f32;
        assert_eq!(visible_keys(0.0, 400.0, ppt, key_ticks), 0..4);
        // Half a key in: key 0 is half on screen, key 4 half in at the right.
        assert_eq!(visible_keys(50.0, 400.0, ppt, key_ticks), 0..5);
        assert_eq!(visible_keys(1000.0, 250.0, ppt, key_ticks), 10..13);
    }

    #[test]
    fn visible_keys_run_on_past_the_first_cycle() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, KeyLength::Beat, 1);
        // Zoomed far out: a pixel a key, 1000 keys' room on screen.
        let ppt = 1.0 / key_ticks as f32;
        assert_eq!(visible_keys(0.0, 1000.0, ppt, key_ticks), 0..1000);
        // Scrolled far along: the keys there.
        let ppt = 100.0 / key_ticks as f32;
        assert_eq!(visible_keys(30_000.0, 400.0, ppt, key_ticks), 300..304);
        // Scrolled before bar 1: none there.
        assert_eq!(visible_keys(-400.0, 300.0, ppt, key_ticks), 0..0);
    }

    #[test]
    fn a_degenerate_zoom_shows_no_keys() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, KeyLength::Beat, 1);
        assert!(visible_keys(0.0, 400.0, 0.0, key_ticks).is_empty());
        assert!(visible_keys(0.0, 400.0, f32::NAN, key_ticks).is_empty());
        assert!(visible_keys(0.0, 400.0, f32::INFINITY, key_ticks).is_empty());
    }

    #[test]
    fn a_key_is_one_beat_of_the_project_meter() {
        let waltz = Meter::new(3, 4).expect("3/4 is a valid meter");
        let seven_eight = Meter::new(7, 8).expect("7/8 is a valid meter");
        for meter in [waltz, seven_eight] {
            let key_ticks = key_tick(meter, KeyLength::Beat, 1);
            assert_eq!(key_ticks, meter.beat_ticks());
            // 100 px to a beat of this meter: four keys in 400 px.
            let ppt = 100.0 / meter.beat_ticks() as f32;
            assert_eq!(visible_keys(0.0, 400.0, ppt, key_ticks), 0..4);
        }
    }

    #[test]
    fn a_key_is_one_bar_or_half_a_bar_when_chosen() {
        let meter = Meter::FOUR_FOUR;
        let bar = key_tick(meter, KeyLength::Bar, 1);
        assert_eq!(bar, meter.bar_ticks());
        // 100 px to a bar: four bar keys in 400 px, from bar 3 scrolled 200.
        let ppt = 100.0 / bar as f32;
        assert_eq!(visible_keys(200.0, 400.0, ppt, bar), 2..6);
        let half = key_tick(meter, KeyLength::HalfBar, 1);
        assert_eq!(visible_keys(0.0, 400.0, ppt, half), 0..8);
        // Zoomed far out, a bar a key runs on as far as the screen does.
        let ppt = 1.0 / bar as f32;
        assert_eq!(visible_keys(0.0, 1000.0, ppt, bar), 0..1000);
    }

    /// The gutter row the label tests lay out in: from x 12, 24 high.
    fn gutter_row() -> Rect {
        Rect::from_min_size(pos2(12.0, 100.0), vec2(0.0, 24.0))
    }

    #[test]
    fn the_gutter_shows_bar_half_and_beat_in_an_even_meter() {
        for meter in [Meter::FOUR_FOUR, Meter::new(6, 8).unwrap()] {
            let labels = key_length_labels(meter, gutter_row());
            let lengths: Vec<KeyLength> = labels.iter().map(|&(length, _)| length).collect();
            assert_eq!(lengths, KeyLength::ALL);
            // Side by side from the row's left edge, each the row's height.
            assert_eq!(labels[0].1.left(), 12.0);
            for pair in labels.windows(2) {
                assert_eq!(pair[0].1.right(), pair[1].1.left());
            }
            assert!(
                labels
                    .iter()
                    .all(|(_, cell)| cell.y_range() == gutter_row().y_range())
            );
        }
    }

    #[test]
    fn the_gutter_drops_the_half_bar_in_an_odd_meter() {
        for meter in [Meter::new(3, 4).unwrap(), Meter::new(7, 8).unwrap()] {
            let labels = key_length_labels(meter, gutter_row());
            let lengths: Vec<KeyLength> = labels.iter().map(|&(length, _)| length).collect();
            assert_eq!(lengths, [KeyLength::Bar, KeyLength::Beat]);
            assert_eq!(
                labels[0].1.right(),
                labels[1].1.left(),
                "no gap where ½ was"
            );
        }
    }

    #[test]
    fn a_click_on_a_label_finds_its_length() {
        let labels = key_length_labels(Meter::FOUR_FOUR, gutter_row());
        let at = |x, y| key_length_label_in(&labels, pos2(x, y));
        let (bar, half, beat) = (labels[0].1, labels[1].1, labels[2].1);
        assert_eq!(at(bar.left(), 112.0), Some(KeyLength::Bar));
        assert_eq!(at(half.center().x, 101.0), Some(KeyLength::HalfBar));
        assert_eq!(
            at(bar.right(), 112.0),
            Some(KeyLength::HalfBar),
            "an edge is the next cell's"
        );
        assert_eq!(at(beat.right() - 0.5, 123.5), Some(KeyLength::Beat));
        assert_eq!(
            at(beat.right(), 112.0),
            None,
            "past the last label: arms the lane"
        );
        assert_eq!(at(11.0, 112.0), None, "left of the labels, over the marker");
        assert_eq!(at(half.center().x, 99.0), None, "above the row");
        assert_eq!(at(half.center().x, 124.0), None, "below the row");
    }

    #[test]
    fn seams_sit_only_where_two_white_keys_meet() {
        // From C-2: key 4 is E, key 5 F, key 11 B, key 12 C-1.
        assert_eq!(key_seam_w(5), Some(1.0), "E/F");
        assert_eq!(key_seam_w(12), Some(2.0), "B/C");
        assert_eq!(key_seam_w(0), None, "no key before the first");
        for key_index in [1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 13] {
            assert_eq!(key_seam_w(key_index), None, "key index {key_index}");
        }
        assert_eq!(key_seam_w(120), Some(2.0), "B/C far along");
        assert_eq!(key_seam_w(125), Some(1.0), "E/F far along");
    }

    #[test]
    fn a_black_key_hangs_from_the_top_over_an_unbroken_white_strip() {
        let key = Rect::from_x_y_ranges(100.0..=120.0, 10.0..=20.0);
        let (hanging, under) = black_key_parts(key);
        assert_eq!(hanging.x_range(), key.x_range(), "the slice's full width");
        assert_eq!(hanging.top(), key.top());
        assert_eq!(hanging.bottom(), 10.0 + 10.0 * BLACK_KEY_DEPTH);
        assert!(hanging.bottom() < key.bottom(), "white shows below it");
        assert_eq!(under.top(), hanging.bottom());
        assert_eq!(under.bottom(), key.bottom());
        assert_eq!(under.min.x, 110.0, "the white keys meet at its middle");
        assert_eq!(under.width(), 0.0);
    }
}
