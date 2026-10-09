//! Quantize and swing detection.
//!
//! [`quantize`](Clip::quantize) pulls the selected `NoteOn`s — or, with
//! nothing selected, every `NoteOn` in the clip's window — toward a 16th grid. The grid — straight, triplet, or swung — is voted on **once per
//! call** by summing every candidate's distance to each, so a phrase never
//! splits across subdivisions; the pull strength then ramps smoothly out of a
//! dead zone, so one tick of input difference never flips a note between
//! "untouched" and "snapped". [`detect_and_store_swing`](Clip::detect_and_store_swing)
//! measures a fresh take's off-beat timing and records it as `swing_pct` for
//! that vote to use later.
//!
//! The grid arithmetic is the private `*_grid_tick` / `*_grid_distance`
//! helpers. See `070-quantization.md`.

use crate::core::{
    config::{QUANTIZE_LERP_FACTOR, QUANTIZE_TOLERANCE_TICKS},
    time,
};
use crate::models::event::EventType;

use super::Clip;

impl Clip {
    /// Quantize toward a 16th grid: the selected `NoteOn`s, or every `NoteOn`
    /// inside the region window `[region.start, region.end)` when nothing is
    /// selected. Material outside the window is never a candidate — it
    /// doesn't play, so it must not sway the vote. Bound to `Q` in the clip
    /// view (`QuantizeEventsEdit`).
    ///
    /// Behaviour:
    /// - The grid (straight 16ths vs triplet 16ths) is chosen **once per call**
    ///   by summing each candidate's distance to both grids and picking the
    ///   smaller total. This prevents split-grid feel where neighbouring notes
    ///   in a phrase get pulled to different subdivisions.
    /// - The grid is anchored to clip-tick 0 (the natural beat boundary), not
    ///   to `region.start`, so notes on absolute beats are recognised even when
    ///   the region starts off-beat (e.g. trimmed pending clips).
    /// - Pull strength ramps smoothly from 0 inside `QUANTIZE_TOLERANCE_TICKS`
    ///   up to `QUANTIZE_LERP_FACTOR` at half a grid step, so tiny input
    ///   differences do not produce large output jumps.
    pub(crate) fn quantize(&mut self) {
        let candidate_indices: Vec<usize> = if self.event_selection.is_empty() {
            (0..self.events.len())
                .filter(|&idx| {
                    let event = &self.events[idx];
                    event.event_type() == Some(EventType::NoteOn) && self.is_in_window(event.tick())
                })
                .collect()
        } else {
            self.note_on_indices(self.event_selection.ids())
        };
        self.quantize_candidates(candidate_indices);
    }

    /// The vote and the pull of [`quantize`](Self::quantize), over the
    /// `NoteOn`s at `candidate_indices` — whichever notes the caller scoped.
    fn quantize_candidates(&mut self, candidate_indices: Vec<usize>) {
        if candidate_indices.is_empty() {
            return;
        }

        let straight = time::sixteenth_straight_ticks();
        let triplet = time::sixteenth_triplet_ticks();

        let total_straight: i64 = candidate_indices
            .iter()
            .map(|&idx| nearest_grid_distance(self.events[idx].tick(), straight) as i64)
            .sum();
        let total_triplet: i64 = candidate_indices
            .iter()
            .map(|&idx| nearest_grid_distance(self.events[idx].tick(), triplet) as i64)
            .sum();
        let total_swung: i64 = if self.swing_pct > 50 {
            candidate_indices
                .iter()
                .map(|&idx| swung_grid_distance(self.events[idx].tick(), self.swing_pct) as i64)
                .sum()
        } else {
            i64::MAX
        };

        let (grid_resolution, use_swung) =
            if total_swung <= total_straight && total_swung <= total_triplet {
                (straight, true)
            } else if total_straight <= total_triplet {
                (straight, false)
            } else {
                (triplet, false)
            };

        let pairing = self.pair_note_events();
        for idx in candidate_indices {
            let tick = self.events[idx].tick();
            let nearest = if use_swung {
                nearest_swung_grid_tick(tick, self.swing_pct)
            } else {
                nearest_grid_tick(tick, grid_resolution)
            };
            let signed_delta = nearest - tick;
            let dist = signed_delta.abs();

            let strength = quantize_strength(dist, grid_resolution);
            if strength <= 0.0 {
                continue;
            }

            let nudge = ((signed_delta as f32) * strength).round() as i32;
            if nudge == 0 {
                continue;
            }
            self.nudge_note_pair(idx, nudge, &pairing);
        }
        self.sort_events_by_tick();
    }

    /// Analyses the NoteOn events in this clip and stores the detected swing percentage
    /// (50–75) in `self.swing_pct`. The two swingable 16th positions within each beat
    /// are at offsets `straight` and `3 × straight` (240 and 720 ticks at PPQN=960).
    /// Notes within half a straight 16th (±120 ticks) of those positions contribute
    /// to the measurement. Fewer than 2 off-beat samples defaults to 50 (straight).
    pub(crate) fn detect_and_store_swing(&mut self) {
        let straight = time::sixteenth_straight_ticks(); // 240
        let beat = straight * 4; // 960

        let mut offsets: Vec<i32> = Vec::new();

        for event in &self.events {
            if event.event_type() != Some(EventType::NoteOn) {
                continue;
            }
            // Only count notes whose nearest straight 16th IS one of the two
            // swingable positions within the beat (`240` or `720`). This excludes
            // notes that are closer to an on-beat (e.g. 32nd-note midpoints),
            // which would otherwise bias the average.
            let nearest = nearest_grid_tick(event.tick(), straight);
            let nearest_phase = nearest.rem_euclid(beat);
            if nearest_phase != straight && nearest_phase != 3 * straight {
                continue;
            }
            offsets.push(event.tick() - nearest);
        }

        if offsets.len() < 2 {
            self.swing_pct = 50;
            return;
        }

        let avg_offset = offsets.iter().sum::<i32>() as f32 / offsets.len() as f32;
        // Formula: swing_pct = 50 * (1 + avg_offset / straight)
        //   avg_offset=0   → 50% (straight)
        //   avg_offset=80  → 66.7% (triplet feel)
        //   avg_offset=120 → 75% (shuffle)
        let swing_pct_f = 50.0 * (1.0 + avg_offset / straight as f32);
        self.swing_pct = swing_pct_f.round().clamp(50.0, 75.0) as u8;
    }
}

/// Round `ticks` to the nearest multiple of `grid_resolution`, anchored at 0.
fn nearest_grid_tick(tick: i32, grid_resolution: i32) -> i32 {
    time::snap_to_grid(tick.abs(), grid_resolution) * tick.signum()
}

/// Absolute tick distance from `tick` to its nearest straight-grid point —
/// used to score a candidate grid when the batch votes (see [`Clip::quantize`]).
fn nearest_grid_distance(tick: i32, grid_resolution: i32) -> i32 {
    (nearest_grid_tick(tick, grid_resolution) - tick).abs()
}

/// Snap `tick` to the nearest position on a swing grid. On-beat positions sit at
/// multiples of `beat` (4 × straight). The two off-beat 16th positions within each
/// beat are at `straight + swing_shift` and `3 × straight + swing_shift`, where
/// `swing_shift = (swing_pct/50 − 1) × straight`. At `swing_pct == 50` the result
/// is identical to the straight 16th grid.
fn nearest_swung_grid_tick(tick: i32, swing_pct: u8) -> i32 {
    if swing_pct <= 50 {
        return nearest_grid_tick(tick, time::sixteenth_straight_ticks());
    }
    let straight = time::sixteenth_straight_ticks();
    let beat = straight * 4;
    let swing_shift = ((swing_pct as f32 / 50.0 - 1.0) * straight as f32).round() as i32;
    let beat_n = tick.div_euclid(beat);
    // Check current beat and the next to handle notes near a beat boundary.
    let mut best = tick;
    let mut best_dist = i32::MAX;
    for b in [beat_n, beat_n + 1] {
        let base = b * beat;
        for pos in [
            base,
            base + straight + swing_shift,
            base + 2 * straight,
            base + 3 * straight + swing_shift,
        ] {
            let dist = (pos - tick).abs();
            if dist < best_dist {
                best_dist = dist;
                best = pos;
            }
        }
    }
    best
}

/// Absolute tick distance from `tick` to its nearest swung-grid point — the
/// swing-aware counterpart of [`nearest_grid_distance`].
fn swung_grid_distance(tick: i32, swing_pct: u8) -> i32 {
    (nearest_swung_grid_tick(tick, swing_pct) - tick).abs()
}

/// Smooth pull strength: 0 inside the tolerance dead zone, ramping linearly to
/// `QUANTIZE_LERP_FACTOR` at half-grid distance. Eliminates the discontinuity
/// where one tick of input difference flips between "untouched" and "fully
/// pulled".
fn quantize_strength(dist: i32, grid_resolution: i32) -> f32 {
    let dist = dist as f32;
    let dead_zone = QUANTIZE_TOLERANCE_TICKS as f32;
    let half_grid = (grid_resolution as f32) * 0.5;
    if dist <= dead_zone || half_grid <= dead_zone {
        return 0.0;
    }
    let normalized = ((dist - dead_zone) / (half_grid - dead_zone)).clamp(0.0, 1.0);
    normalized * QUANTIZE_LERP_FACTOR
}

#[cfg(test)]
mod tests {
    use crate::{
        core::time::{self, Meter},
        models::{
            clip::Clip,
            event::{Event, EventType},
        },
    };

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    fn clip_with_region(region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    // --- quantize ---

    #[test]
    fn quantize_skips_notes_within_tolerance() {
        // PPQN=960, straight 16th = 240. Place note 10 ticks off (< tolerance of 30).
        let straight = time::sixteenth_straight_ticks();
        let mut clip = clip_with_region(0, straight * 8);
        clip.add_event(on(straight + 10, 60));
        clip.add_event(off(straight + 50, 60));
        let original_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();

        clip.quantize();

        let after_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(after_tick, original_tick);
    }

    #[test]
    fn quantize_snaps_far_note_toward_nearest_triplet_grid() {
        // Single note at 100. straight nearest=0 (dist=100), triplet nearest=160
        // (dist=60). Triplet has the smaller total distance so the batch grid is
        // triplet (160 ticks). Smooth strength: dist=60, dead_zone=30,
        // half_grid=80 → normalized=(60-30)/(80-30)=0.6,
        // strength = 0.6 * QUANTIZE_LERP_FACTOR (0.8) = 0.48.
        // nudge = round(60 * 0.48) = 29. New tick = 129.
        let straight = time::sixteenth_straight_ticks();
        let mut clip = clip_with_region(0, straight * 8);
        clip.add_event(on(100, 60));
        clip.add_event(off(200, 60));

        clip.quantize();

        let after_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(after_tick, 129);
    }

    #[test]
    fn quantize_picks_single_grid_for_whole_batch() {
        // Three notes that, individually, would split between grids:
        //   tick 250 → straight 240 (dist 10) vs triplet 320 (dist 70) → straight wins
        //   tick 100 → straight 0   (dist 100) vs triplet 160 (dist 60) → triplet wins
        //   tick 500 → straight 480 (dist 20) vs triplet 480 (dist 20) → tie
        // Batch totals: straight = 10+100+20 = 130; triplet = 70+60+20 = 150.
        // Straight grid is chosen for ALL notes; the lone "triplet-leaning" note
        // gets pulled toward 0 (the straight neighbour) rather than 160.
        let straight = time::sixteenth_straight_ticks();
        let mut clip = clip_with_region(0, straight * 16);
        clip.add_event(on(100, 60));
        clip.add_event(off(150, 60));
        clip.add_event(on(250, 61));
        clip.add_event(off(280, 61));
        clip.add_event(on(500, 62));
        clip.add_event(off(550, 62));

        clip.quantize();

        // Note originally at 100: pulled toward straight 0, never toward triplet 160.
        let note_60_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn) && e.note_number() == Some(60))
            .unwrap()
            .tick();
        assert!(
            note_60_tick < 100,
            "note should move toward straight 0, got {note_60_tick}"
        );
    }

    #[test]
    fn quantize_smooth_strength_has_no_cliff_at_tolerance_boundary() {
        // Just outside the dead zone (dist=31 from straight grid) the old
        // implementation snapped at 80% strength. The new smooth ramp pulls
        // only ~1.6% there, so the note barely moves.
        let straight = time::sixteenth_straight_ticks();
        let mut clip = clip_with_region(0, straight * 8);
        clip.add_event(on(31, 60)); // 31 ticks past straight grid (0)
        clip.add_event(off(60, 60));

        clip.quantize();

        let after_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        // Old behaviour would have pulled it to 31 - round(31*0.8) = 31 - 25 = 6.
        // New ramp: normalized=(31-30)/(120-30)≈0.011, strength≈0.0089,
        // nudge = round(-31 * 0.0089) = 0 → unchanged.
        assert_eq!(after_tick, 31);
    }

    #[test]
    fn quantize_anchors_grid_to_clip_origin_not_region_start() {
        // Region starts at 200 (off-beat). A note exactly on beat 1 (tick 960)
        // should be recognised as on-grid, not pulled because of the region offset.
        let mut clip = clip_with_region(200, 200 + Meter::FOUR_FOUR.bar_ticks());
        clip.add_event(on(960, 60));
        clip.add_event(off(1200, 60));

        clip.quantize();

        let after_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(after_tick, 960);
    }

    /// Every `NoteOn` tick in the clip, in event order.
    fn note_on_ticks(clip: &Clip) -> Vec<i32> {
        clip.events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.tick())
            .collect()
    }

    #[test]
    fn quantize_with_a_selection_moves_only_the_selected_notes() {
        // Both notes sit at the worked-example 100 (→ 129 on the triplet
        // grid, see above) a bar apart; only the second is selected.
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = clip_with_region(0, bar * 2);
        clip.add_event(on(100, 60));
        clip.add_event(off(200, 60));
        clip.add_event(on(bar + 100, 62));
        clip.add_event(off(bar + 200, 62));
        let second_id = clip
            .events()
            .iter()
            .find(|e| e.note_number() == Some(62))
            .unwrap()
            .id();
        clip.select_event(Some(second_id));

        clip.quantize();

        assert_eq!(note_on_ticks(&clip), vec![100, bar + 129]);
    }

    #[test]
    fn quantize_without_a_selection_leaves_material_outside_the_window() {
        // The window is the first bar; the note in the second bar is
        // retained capture material and must stay put.
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = clip_with_region(0, bar);
        clip.add_event(on(100, 60));
        clip.add_event(off(200, 60));
        clip.add_event(on(bar + 100, 62));
        clip.add_event(off(bar + 200, 62));

        clip.quantize();

        assert_eq!(note_on_ticks(&clip), vec![129, bar + 100]);
    }

    // --- detect_and_store_swing ---

    #[test]
    fn detect_swing_returns_50_for_notes_only_on_beat() {
        // Notes exclusively at beat positions — no off-beat samples → expect 50.
        let mut clip = Clip::new();
        clip.add_event(on(0, 60));
        clip.add_event(on(960, 62));
        clip.detect_and_store_swing();
        assert_eq!(clip.swing_pct(), 50);
    }

    #[test]
    fn detect_swing_requires_minimum_two_off_beat_notes() {
        // Only one off-beat note is insufficient for reliable detection → expect 50.
        let mut clip = Clip::new();
        clip.add_event(on(0, 60));
        clip.add_event(on(320, 62)); // one off-beat note
        clip.detect_and_store_swing();
        assert_eq!(clip.swing_pct(), 50);
    }

    #[test]
    fn detect_swing_measures_triplet_feel_off_beats() {
        // Off-beats at tick 320 = straight (240) + 80 ticks late.
        // avg_offset=80; swing_pct = 50*(1+80/240) ≈ 67.
        let mut clip = Clip::new();
        clip.add_event(on(0, 60));
        clip.add_event(on(320, 62)); // off-beat 1 (beat 0)
        clip.add_event(on(960, 60));
        clip.add_event(on(1280, 62)); // off-beat 2 (beat 1)
        clip.detect_and_store_swing();
        assert_eq!(clip.swing_pct(), 67);
    }

    #[test]
    fn detect_swing_measures_heavy_shuffle_feel_off_beats() {
        // Off-beats at tick 350 = straight (240) + 110 ticks late.
        // avg_offset=110; swing_pct = 50*(1+110/240) ≈ 72.9 → 73.
        let mut clip = Clip::new();
        clip.add_event(on(0, 60));
        clip.add_event(on(350, 62));
        clip.add_event(on(960, 60));
        clip.add_event(on(1310, 62));
        clip.detect_and_store_swing();
        assert_eq!(clip.swing_pct(), 73);
    }

    #[test]
    fn quantize_uses_swung_grid_when_swing_pct_set() {
        // swing_pct=67: swing_shift=82, swung off-beat=322.
        // Notes at 312 (10 ticks before swung position).
        //   Swung grid dist = 10 → inside dead zone (30) → no movement.
        //   Straight grid dist = |312-240| = 72 → outside dead zone → would pull to 240.
        // Test verifies the swung grid wins the vote so notes stay at 312.
        let mut clip = clip_with_region(0, Meter::FOUR_FOUR.bars_to_ticks(2));
        clip.set_swing_pct(67);
        clip.add_event(on(312, 62));
        clip.add_event(on(1272, 62)); // beat 1 (960) + 312
        clip.quantize();
        let ticks: Vec<i32> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.tick())
            .collect();
        assert_eq!(ticks, vec![312, 1272]);
    }
}
