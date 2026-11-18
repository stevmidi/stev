//! Painting the arranger's performance-lane row (`110-performance-lane.md`):
//! the gutter and its selection accent, and in the content area the keyboard
//! that drives the lane, lined up with where its keys jump
//! (`510-performance-lane-keyboard.md`). Key `k` is note
//! `PERFORMANCE_LANE_BASE_NOTE + k` (`note_for_key`) and spans one beat
//! from `key_tick(k)`: a thin strip along the lane's bottom edge carries each
//! key, white or black as on the piano roll, the plain lane above it the
//! names (the C's always, every key once a key is wide enough), and a key the
//! live keyboard holds lights while the lane is armed. Static: it changes
//! with zoom, scroll and the meter, never with playing (the playhead is the
//! trigger's feedback).

use std::ops::Range;

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Rect, Stroke, pos2, vec2};

use super::arranger::lane_grid_fill;
use super::piano_keys::{is_black_key, octave_of, pitch_class, row_seam_w};
use super::*;
use crate::models::performance_lane::{KEY_COUNT, key_tick, note_for_key};
use crate::models::pitch::pitch_class_name;

/// Height of the key strip along the lane's bottom edge. Narrow, as the
/// piano roll's key column is: the piano roll's near-white and near-black
/// read well in a narrow band and stark filling the whole row (tried first).
const KEY_STRIP_H: f32 = 5.0;

/// Strength of the key strip while the lane is unarmed, so the row doesn't
/// compete with the tracks under it; armed, it is drawn at full strength.
/// Tune by eye in every palette.
const UNARMED_KEY_STRENGTH: f32 = 0.5;

/// Strength of the accent wash over a held key's span above the strip, whose
/// own piece of the strip lights in the full accent. Tune by eye.
const HELD_KEY_WASH: f32 = 0.25;

/// Gap between a key's left edge and its label, and the least room left on
/// its right before every key is labelled (`labels_every_key`).
const KEY_LABEL_PAD_X: f32 = 3.0;

/// The name the every-key label threshold is measured from: a sharp, as
/// wide as the lane's names get (`labels_every_key`).
const WIDEST_KEY_NAME: &str = "C#4";

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
        let ppt = self.pixels_per_tick();
        let key_ticks = key_tick(meter, 1);
        let keys = visible_keys(self.render_scroll_x(), self.content_w(), ppt, key_ticks);
        let font = FontId::proportional(theme::FONT_SIZE_TL);
        let label_w = p
            .layout_no_wrap(WIDEST_KEY_NAME.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x;
        let label_every_key = labels_every_key(ppt * key_ticks as f32, label_w);
        let (white, black) = if armed {
            (theme::piano_key_white(), theme::piano_key_black())
        } else {
            (
                theme::piano_key_white().gamma_multiply(UNARMED_KEY_STRENGTH),
                theme::piano_key_black().gamma_multiply(UNARMED_KEY_STRENGTH),
            )
        };
        let label_color = theme::fg_dim();

        for key_index in keys {
            let Some(note) = note_for_key(key_index) else {
                continue;
            };
            let left = self.tick_to_screen_x(key_tick(meter, key_index));
            let right = self.tick_to_screen_x(key_tick(meter, key_index + 1));
            let above = Rect::from_x_y_ranges(left..=right, lane_top..=strip_top);
            let key = Rect::from_x_y_ranges(left..=right, strip_top..=strip_bottom);
            let lit = armed && self.live_notes.is_held(note);
            let key_fill = if lit {
                p.rect_filled(
                    above,
                    CornerRadius::ZERO,
                    accent.gamma_multiply(HELD_KEY_WASH),
                );
                accent
            } else if is_black_key(pitch_class(note)) {
                black
            } else {
                white
            };
            p.rect_filled(key, CornerRadius::ZERO, key_fill);

            if let Some(seam_w) = key_seam_w(key_index) {
                p.vline(
                    left,
                    strip_top..=strip_bottom,
                    Stroke::new(seam_w, grid_seam_color()),
                );
            }

            if label_every_key || pitch_class(note) == 0 {
                // Clipped to its own key, so a key scrolled half off never
                // leaves its name on the next one.
                p.with_clip_rect(above.intersect(content)).text(
                    pos2(left + KEY_LABEL_PAD_X, above.center().y),
                    Align2::LEFT_CENTER,
                    key_name(note),
                    font.clone(),
                    label_color,
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
}

/// The keys (0-based, end exclusive) at least partly inside a content area
/// `content_w` wide scrolled `scroll_x` at `pixels_per_tick`, `key_ticks` to
/// a key: from the key at the left edge to the one at the right, held to the
/// keys there are (`KEY_COUNT`), so the lane draws at most that many at any
/// zoom.
fn visible_keys(scroll_x: f32, content_w: f32, pixels_per_tick: f32, key_ticks: i32) -> Range<i32> {
    let key_w = pixels_per_tick * key_ticks as f32;
    // A degenerate zoom (zero or not finite) shows no keys, rather than a
    // range from a NaN or infinite division.
    if !key_w.is_finite() || key_w <= 0.0 {
        return 0..0;
    }
    let first = (scroll_x / key_w).floor() as i32;
    let end = ((scroll_x + content_w) / key_w).ceil() as i32;
    first.max(0)..end.clamp(0, KEY_COUNT)
}

/// The seam at the left edge of key `key_index`, as the piano roll draws it
/// between the same two keys (`row_seam_w`): 2px where B meets C, 1px where
/// E meets F, the only neighbours with no black key between them; `None` at
/// every other boundary, which the colour change already shows, and at the
/// first key, with none before it.
fn key_seam_w(key_index: i32) -> Option<f32> {
    let note = note_for_key(key_index)?;
    let below = note.checked_sub(1).filter(|_| key_index > 0)?;
    row_seam_w(below)
}

/// Whether a key `key_w` wide has room for every key's name: the widest name
/// the lane shows (`label_w`, measured from `WIDEST_KEY_NAME`) and its
/// padding on both sides. Below it only the C's are named.
fn labels_every_key(key_w: f32, label_w: f32) -> bool {
    key_w >= label_w + 2.0 * KEY_LABEL_PAD_X
}

/// A key's name, sharps only as the MCP units write them, with the piano
/// roll's Cubase-style octave (MIDI 60 = C3, `octave_of`).
fn key_name(note: u8) -> String {
    format!("{}{}", pitch_class_name(i32::from(note)), octave_of(note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::PERFORMANCE_LANE_BASE_NOTE;
    use crate::core::time::Meter;

    #[test]
    fn the_keys_end_at_note_127() {
        assert_eq!(note_for_key(KEY_COUNT - 1), Some(127));
        assert_eq!(note_for_key(KEY_COUNT), None);
    }

    #[test]
    fn visible_keys_follow_scroll_and_zoom() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, 1);
        // 100 px to a key, a beat: 400 to a bar.
        let ppt = 100.0 / key_ticks as f32;
        assert_eq!(visible_keys(0.0, 400.0, ppt, key_ticks), 0..4);
        // Half a key in: key 0 is half on screen, key 4 half in at the right.
        assert_eq!(visible_keys(50.0, 400.0, ppt, key_ticks), 0..5);
        assert_eq!(visible_keys(1000.0, 250.0, ppt, key_ticks), 10..13);
    }

    #[test]
    fn visible_keys_stop_where_the_keys_do() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, 1);
        // Zoomed far out: a pixel a key, 1000 keys' room on screen.
        let ppt = 1.0 / key_ticks as f32;
        assert_eq!(visible_keys(0.0, 1000.0, ppt, key_ticks), 0..KEY_COUNT);
        // Scrolled past the last key: nothing.
        let ppt = 100.0 / key_ticks as f32;
        assert!(visible_keys(8000.0, 400.0, ppt, key_ticks).is_empty());
    }

    #[test]
    fn a_degenerate_zoom_shows_no_keys() {
        let key_ticks = key_tick(Meter::FOUR_FOUR, 1);
        assert!(visible_keys(0.0, 400.0, 0.0, key_ticks).is_empty());
        assert!(visible_keys(0.0, 400.0, f32::NAN, key_ticks).is_empty());
        assert!(visible_keys(0.0, 400.0, f32::INFINITY, key_ticks).is_empty());
    }

    #[test]
    fn a_key_is_one_beat_of_the_project_meter() {
        let waltz = Meter::new(3, 4).expect("3/4 is a valid meter");
        let seven_eight = Meter::new(7, 8).expect("7/8 is a valid meter");
        for meter in [waltz, seven_eight] {
            let key_ticks = key_tick(meter, 1);
            assert_eq!(key_ticks, meter.beat_ticks());
            // 100 px to a beat of this meter: four keys in 400 px.
            let ppt = 100.0 / meter.beat_ticks() as f32;
            assert_eq!(visible_keys(0.0, 400.0, ppt, key_ticks), 0..4);
        }
    }

    #[test]
    fn seams_sit_only_where_two_white_keys_meet() {
        // From C3: key 4 is E, key 5 F, key 11 B, key 12 C4.
        assert_eq!(key_seam_w(5), Some(1.0), "E/F");
        assert_eq!(key_seam_w(12), Some(2.0), "B/C");
        assert_eq!(key_seam_w(0), None, "no key before the first");
        for key_index in [1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 13] {
            assert_eq!(key_seam_w(key_index), None, "key index {key_index}");
        }
        assert_eq!(key_seam_w(KEY_COUNT), None, "past the last key");
    }

    #[test]
    fn every_key_is_named_once_a_key_fits_the_widest_name() {
        assert!(!labels_every_key(20.0, 20.0));
        assert!(!labels_every_key(25.9, 20.0));
        assert!(labels_every_key(26.0, 20.0));
    }

    #[test]
    fn keys_are_named_cubase_style_with_sharps() {
        assert_eq!(key_name(PERFORMANCE_LANE_BASE_NOTE), "C3");
        assert_eq!(key_name(PERFORMANCE_LANE_BASE_NOTE + 12), "C4");
        assert_eq!(key_name(61), "C#3");
        assert_eq!(key_name(127), "G8");
    }
}
