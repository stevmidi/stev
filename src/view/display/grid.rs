//! The zoom-adaptive timeline grid: [`GridTiers`] and the pure [`grid_tiers`]
//! that picks them from the on-screen scale. One rule drives the grid lines,
//! ruler labels, mouse/keyboard snap and cursor-follow paging of the arranger
//! and the piano roll alike, so what you see is what you snap to. Phase 2 of
//! `archive/190-arranger-zoom.md` (arranger) and of `archive/200-clip-view-zoom.md` (clip
//! views); the rendering side is `030-ui-design.md` § Grid Hierarchy.
//!
//! The ladder follows the project's [`Meter`]: binary subdivisions of the
//! counted beat up to the beat, the half bar when the numerator is even,
//! then doubling whole bars. Every rung divides every coarser one, so each
//! tier divides the next and a tick snapped at one zoom is still on the grid
//! at any coarser zoom. Most rungs are 2× their neighbour, so a tier switch
//! reads as half the lines appearing or vanishing; only the step from the
//! beat to the (half) bar can be wider (3× in 3/4, 7× in 7/8). No hysteresis
//! needed either way.

use crate::core::time::{Meter, PPQN, sixteenth_straight_ticks};

/// Coarsest structural spacing, in bars. Past this the bar tier stops
/// coarsening; the zoom-out floor (`ARRANGER_MIN_PX_PER_BEAT`) keeps it out
/// of reach in practice.
const MAX_BAR_TIER_BARS: i32 = 64;
/// Finest ladder rung, in ticks: a 256th note — the finest binary
/// subdivision of a quarter (and so of an eighth) that stays a whole tick.
/// The same in every meter. The piano roll snaps down to it when zoomed in
/// (setting a clip's end by ear needs ~5–10 ms, a 32nd is ~60 ms at 120 BPM);
/// the arranger's snap floor stops it well above.
const FINEST_RUNG_TICKS: i32 = PPQN / 64;

/// Snap / finest drawn tier: the finest rung at least this far apart, px.
/// 8 keeps the default 32-bar view's beat (≈ 9.95 px) as the snap and stops
/// the grid ever drawing lines tighter than it.
const MIN_SNAP_PX: f32 = 8.0;
/// The piano roll's spacing for the rungs finer than a beat — Ableton's
/// "Adaptive: Narrowest" density: a 2-bar clip fitted to a ~1300 pt clip
/// view snaps to 64ths, 16 steps a beat, and zooming in keeps halving the
/// step down to `FINEST_RUNG_TICKS`. Denser than `MIN_SNAP_PX` because the
/// piano roll is where notes and clip ends are placed precisely.
const PIANO_ROLL_MIN_SUB_BEAT_PX: f32 = 6.0;
/// The arranger's stricter spacing for the rungs *finer than a beat* (8ths,
/// 16ths): they appear only once the arranger is zoomed in well past the
/// default, so beats are the finest step at full zoom-out on any window
/// width. With the shared 8 px threshold a wide window's default 32-bar view
/// (≈ 2100pt+ content, ≥ 16 px/beat) already showed 8ths before any zooming.
/// At 16, 8ths arrive at 32 px/beat and 16ths at 64. Eye-tuned. The piano
/// roll doesn't use it — see [`GridSurface`].
const ARRANGER_MIN_SUB_BEAT_PX: f32 = 16.0;
/// Beat tier: drawn only when beats are at least this far apart, px.
const MIN_BEAT_PX: f32 = 8.0;
/// Bar tier: a bar line at least this far apart, px — below it the
/// structural tier coarsens to every 2, 4, 8 … bars.
const MIN_BAR_PX: f32 = 16.0;
/// Ruler graduations for the rungs finer than a beat: at least this far
/// apart, px. The ruler is a readout, not the snap grid — the piano roll's
/// 6 px snap lines are fine as faint in-lane grooves, but a crisp
/// `grid_major` tick every 6 px turns the strip between the bars into a comb.
/// Eye-tuned.
const MIN_RULER_SUB_BEAT_PX: f32 = 16.0;
/// Bar-number labels: at least this far apart, px — room for a three-digit
/// number at `FONT_SIZE_TL` plus the label's 3 px inset.
const MIN_LABEL_PX: f32 = 28.0;

/// In-lane strength of a bar line that crosses track seams — the arranger's.
/// There the lane seams, drawn in the same groove at full strength, are the
/// primary structure (tracks first, time second, as in Ableton / Bitwig):
/// full-strength bar lines matched the old 2px seams and closed every lane ×
/// bar cell into a tile, reading as a chessboard. 0.65 fixed that but made
/// bars too soft; with the seams thinned to a hairline
/// (`ARRANGER_LANE_SEAM_W`) the lattice is lighter overall and bars can sit
/// higher, still below the seams and above `BEAT_LINE_STRENGTH`. Eye-tuned.
const ARRANGER_BAR_LINE_STRENGTH: f32 = 0.8;
/// In-lane line strength of the beat tier when a finer tier sits under it —
/// the opacity of the shared groove colour (`grid_seam_color()`), which bar
/// lines draw at full strength. Eye-tuned; see `line_strength`.
const BEAT_LINE_STRENGTH: f32 = 0.5;
/// In-lane line strength of the finest visible tier (and of the beat tier
/// when it *is* the finest). Lowest of the three so the densest lines stay
/// the faintest. Eye-tuned; the near-black themes are the ones to check, as
/// their groove has the least room below the lane.
const SNAP_LINE_STRENGTH: f32 = 0.28;

/// The grid visible at one scale, every field in ticks (an amount), coarse →
/// fine: `label_ticks ≥ bar_ticks ≥ beat_ticks ≥ snap_ticks`, each dividing
/// the one before it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct GridTiers {
    /// Bar-number label spacing: a multiple of `bar_ticks`.
    pub(super) label_ticks: i32,
    /// Structural tier: every N bars (N ≥ 1). Groove tone, full-height ruler
    /// tick; also the cursor-follow paging unit.
    pub(super) bar_ticks: i32,
    /// Beat tier — `None` when beats are too tight to draw, or the bar tier
    /// has coarsened past one bar.
    pub(super) beat_ticks: Option<i32>,
    /// Finest visible tier, and the snap grid. Equal to `beat_ticks` (or
    /// `bar_ticks`) when nothing finer fits; a line is drawn once, as its
    /// coarsest tier.
    pub(super) snap_ticks: i32,
    /// Ruler graduation spacing: which lines get a tick hanging off the
    /// timeline strip. A multiple of `snap_ticks`, and equal to it at or
    /// above the beat; below the beat it thins to the finest rung at least
    /// `MIN_RULER_SUB_BEAT_PX` apart and never finer than a 16th — the
    /// in-lane grid keeps every snap line.
    pub(super) ruler_ticks: i32,
}

/// Which timeline a grid is for. The ladder and every threshold are shared
/// except the spacing a sub-beat rung (8th, 16th) needs before it is drawn:
/// the arranger holds it back (`ARRANGER_MIN_SUB_BEAT_PX`) to keep its
/// default view calm, while the piano roll — where notes are placed — gives
/// it the ordinary `MIN_SNAP_PX`, so a clip fitted to the width keeps its
/// 16ths up to `content_w / 128` bars (≈ 10 bars at 1300pt, 16 at 2100pt)
/// instead of losing them at half that.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum GridSurface {
    /// The arranger: tracks of clips.
    Arranger,
    /// The clip view (`Clip`): one clip's notes.
    PianoRoll,
}

impl GridSurface {
    /// Minimum spacing, px, of a rung finer than a beat on this surface.
    const fn min_sub_beat_px(self) -> f32 {
        match self {
            GridSurface::Arranger => ARRANGER_MIN_SUB_BEAT_PX,
            GridSurface::PianoRoll => PIANO_ROLL_MIN_SUB_BEAT_PX,
        }
    }

    /// The finest snap this surface ever uses (Logic's "division" floor): a
    /// 16th in the arranger, the ladder's finest rung in the piano roll.
    pub(super) const fn snap_floor_ticks(self) -> i32 {
        match self {
            GridSurface::Arranger => sixteenth_straight_ticks(),
            GridSurface::PianoRoll => FINEST_RUNG_TICKS,
        }
    }
}

/// Which tier a grid line at some tick belongs to — its *coarsest* one, since
/// each line is drawn once. Rendering styles by role, never by rung: a bar
/// line looks like a bar line at any zoom.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum GridRole {
    /// On the structural tier (`bar_ticks`).
    Bar,
    /// On the beat tier, not the bar tier.
    Beat,
    /// Only on the snap tier.
    Snap,
}

impl GridTiers {
    /// The role of the grid line at `tick` (a multiple of `snap_ticks`).
    pub(super) fn role_at(&self, tick: i32) -> GridRole {
        if tick % self.bar_ticks == 0 {
            GridRole::Bar
        } else if self.beat_ticks.is_some_and(|beat| tick % beat == 0) {
            GridRole::Beat
        } else {
            GridRole::Snap
        }
    }

    /// Whether a finer tier sits under the beat tier. When it doesn't, the
    /// beat lines *are* the finest tier and take its dimmest tone — the
    /// finest visible tier is always the dimmest.
    pub(super) fn has_tier_below_beat(&self) -> bool {
        self.beat_ticks.is_some_and(|beat| self.snap_ticks < beat)
    }

    /// Opacity, `0.0..=1.0`, of the one groove colour an in-lane line of
    /// `role` is drawn in. Every tier inherits the bar groove and only gets
    /// softer, never a different hue or polarity, so the hierarchy reads the
    /// same in every theme and the translucent line blends with whatever it
    /// crosses (the selected-track tint, the piano-roll key rows). The finest
    /// visible tier is always the faintest: beats take the snap strength when
    /// nothing finer sits under them. `crosses_track_seams` (the arranger)
    /// softens the bar line below the lane seams it crosses — see
    /// `ARRANGER_BAR_LINE_STRENGTH`.
    pub(super) fn line_strength(&self, role: GridRole, crosses_track_seams: bool) -> f32 {
        match role {
            GridRole::Bar if crosses_track_seams => ARRANGER_BAR_LINE_STRENGTH,
            GridRole::Bar => 1.0,
            GridRole::Beat if self.has_tier_below_beat() => BEAT_LINE_STRENGTH,
            GridRole::Beat | GridRole::Snap => SNAP_LINE_STRENGTH,
        }
    }

    /// Whether the grid line at `tick` (a multiple of `snap_ticks`) gets a
    /// ruler tick. `ruler_ticks` divides the beat (or is the snap), so bar
    /// and beat lines always do.
    pub(super) fn has_ruler_tick(&self, tick: i32) -> bool {
        tick % self.ruler_ticks == 0
    }

    /// Whether the bar at `tick` gets its number drawn.
    pub(super) fn is_labelled(&self, tick: i32) -> bool {
        tick % self.label_ticks == 0
    }
}

/// The ladder's rungs in `meter`, in ticks, fine → coarse: a 256th, 128th …
/// doubling up to the counted beat (a quarter in x/4, an eighth in x/8), the
/// half bar when the numerator is even (half a 7/8 bar is off the beat
/// grid), then 1, 2, 4 … `MAX_BAR_TIER_BARS` bars. A rung that isn't above
/// the beat (half a 2/4 bar, a whole 1/4 bar) is the beat, listed once.
fn ladder(meter: Meter) -> impl Iterator<Item = i32> {
    let beat = meter.beat_ticks();
    let bar = meter.bar_ticks();
    let to_beat = std::iter::successors(Some(FINEST_RUNG_TICKS), move |&t| {
        (t < beat).then_some(t * 2)
    });
    let half_bar = meter.numerator().is_multiple_of(2).then_some(bar / 2);
    let bars = std::iter::successors(Some(1), |&n| (n < MAX_BAR_TIER_BARS).then_some(n * 2))
        .map(move |n| bar * n);
    to_beat.chain(half_bar.into_iter().chain(bars).filter(move |&t| t > beat))
}

/// Picks the grid tiers for `px_per_tick` on `surface` in `meter`, never
/// subdividing the snap below `snap_floor_ticks` (Logic's "division" floor).
/// See the module docs and `archive/190-arranger-zoom.md` § The grid ladder
/// for the per-tier rules.
pub(super) fn grid_tiers(
    px_per_tick: f32,
    snap_floor_ticks: i32,
    surface: GridSurface,
    meter: Meter,
) -> GridTiers {
    let bar = meter.bar_ticks();
    let beat = meter.beat_ticks();
    let spacing = |ticks: i32| ticks as f32 * px_per_tick;

    // Finest rung at or above `min_ticks` whose lines are ≥ `min_px` apart.
    let finest_rung = |min_ticks: i32, min_px: f32| {
        ladder(meter)
            .find(|&ticks| ticks >= min_ticks && spacing(ticks) >= min_px)
            .unwrap_or(bar * MAX_BAR_TIER_BARS)
    };

    let bar_ticks = finest_rung(bar, MIN_BAR_PX);
    let beat_ticks = (bar_ticks == bar && spacing(beat) >= MIN_BEAT_PX).then_some(beat);
    // Snap: the finest rung clearing its spacing threshold — the surface's
    // own one below the beat. Thresholds are fixed per rung, so the tiers
    // stay monotonic in zoom.
    let snap_ticks = ladder(meter)
        .find(|&ticks| {
            let min_px = if ticks < beat {
                surface.min_sub_beat_px()
            } else {
                MIN_SNAP_PX
            };
            ticks >= snap_floor_ticks && spacing(ticks) >= min_px
        })
        .unwrap_or(bar * MAX_BAR_TIER_BARS)
        .min(bar_ticks);
    let label_ticks = finest_rung(bar_ticks, MIN_LABEL_PX);
    // Ruler: below the beat, the finest rung both wide enough and no finer
    // than a 16th, capped at the beat; at or above it, the snap itself.
    let ruler_ticks = finest_rung(
        snap_ticks.max(sixteenth_straight_ticks()),
        MIN_RULER_SUB_BEAT_PX,
    )
    .min(beat)
    .max(snap_ticks);

    GridTiers {
        label_ticks,
        bar_ticks,
        beat_ticks,
        snap_ticks,
        ruler_ticks,
    }
}

#[cfg(test)]
mod tests {
    use super::{GridRole, GridSurface, GridTiers, MIN_LABEL_PX, grid_tiers, ladder};
    use crate::core::config::{ARRANGER_MIN_PX_PER_BEAT, BARS_IN_VIEWPORT, MAX_PX_PER_BEAT};
    use crate::core::time::{Meter, beats_to_ticks, px_per_beat_to_ppt, sixteenth_straight_ticks};

    const FLOOR: i32 = sixteenth_straight_ticks();
    const FOUR_FOUR: Meter = Meter::FOUR_FOUR;

    /// Bar, beat, 16th, a label on every bar — the clip views' old fixed
    /// grid, and what the piano roll shows for a short clip.
    fn bar_beat_sixteenth() -> GridTiers {
        GridTiers {
            label_ticks: FOUR_FOUR.bars_to_ticks(1),
            bar_ticks: FOUR_FOUR.bars_to_ticks(1),
            beat_ticks: Some(beats_to_ticks(1.0)),
            snap_ticks: FLOOR,
            ruler_ticks: FLOOR,
        }
    }

    /// A dense sweep of the whole zoom range, coarse → fine.
    fn zoom_sweep() -> impl Iterator<Item = f32> {
        let steps = 400;
        let (lo, hi) = (ARRANGER_MIN_PX_PER_BEAT.ln(), MAX_PX_PER_BEAT.ln());
        (0..=steps).map(move |i| (lo + (hi - lo) * i as f32 / steps as f32).exp())
    }

    #[test]
    fn ladder_runs_a_256th_to_64_bars_doubling() {
        let bar = FOUR_FOUR.bars_to_ticks(1);
        let rungs: Vec<i32> = ladder(FOUR_FOUR).collect();
        assert_eq!(rungs.first(), Some(&(bar / 256)));
        assert_eq!(bar % 256, 0, "the finest rung is a whole tick count");
        assert_eq!(rungs.last(), Some(&(bar * 64)));
        assert!(rungs.windows(2).all(|w| w[1] == w[0] * 2), "{rungs:?}");
    }

    #[test]
    fn default_arranger_view_is_todays_bar_and_beat_grid() {
        let ppt = 1274.0 / BARS_IN_VIEWPORT as f32 / FOUR_FOUR.bars_to_ticks(1) as f32;
        let beat = beats_to_ticks(1.0);
        assert_eq!(
            grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR),
            GridTiers {
                label_ticks: FOUR_FOUR.bars_to_ticks(1),
                bar_ticks: FOUR_FOUR.bars_to_ticks(1),
                beat_ticks: Some(beat),
                snap_ticks: beat,
                ruler_ticks: beat,
            }
        );
    }

    #[test]
    fn tiers_are_monotonic_in_zoom() {
        let mut previous: Option<GridTiers> = None;
        for px_per_beat in zoom_sweep() {
            let tiers = grid_tiers(
                px_per_beat_to_ppt(px_per_beat),
                FLOOR,
                GridSurface::Arranger,
                FOUR_FOUR,
            );
            if let Some(prev) = previous {
                // Zooming in never coarsens a tier.
                assert!(tiers.label_ticks <= prev.label_ticks, "{px_per_beat}");
                assert!(tiers.bar_ticks <= prev.bar_ticks, "{px_per_beat}");
                assert!(tiers.snap_ticks <= prev.snap_ticks, "{px_per_beat}");
                assert!(
                    prev.beat_ticks.is_none() || tiers.beat_ticks.is_some(),
                    "beat tier vanished zooming in at {px_per_beat}"
                );
            }
            previous = Some(tiers);
        }
    }

    #[test]
    fn every_tier_divides_the_next_coarser_one() {
        for px_per_beat in zoom_sweep() {
            let t = grid_tiers(
                px_per_beat_to_ppt(px_per_beat),
                FLOOR,
                GridSurface::Arranger,
                FOUR_FOUR,
            );
            assert_eq!(t.label_ticks % t.bar_ticks, 0, "{px_per_beat}: {t:?}");
            assert_eq!(t.bar_ticks % t.snap_ticks, 0, "{px_per_beat}: {t:?}");
            if let Some(beat) = t.beat_ticks {
                assert_eq!(t.bar_ticks % beat, 0, "{px_per_beat}: {t:?}");
                assert_eq!(beat % t.snap_ticks, 0, "{px_per_beat}: {t:?}");
            }
            // And the bar tier is always whole bars.
            assert_eq!(
                t.bar_ticks % FOUR_FOUR.bars_to_ticks(1),
                0,
                "{px_per_beat}: {t:?}"
            );
        }
    }

    #[test]
    fn snap_never_goes_below_the_floor() {
        for px_per_beat in zoom_sweep() {
            let t = grid_tiers(
                px_per_beat_to_ppt(px_per_beat),
                FLOOR,
                GridSurface::Arranger,
                FOUR_FOUR,
            );
            assert!(t.snap_ticks >= FLOOR, "{px_per_beat}: {t:?}");
        }
        // Fully zoomed in the snap is the floor itself, even though a 32nd
        // (480 px at 3840 px/beat) would clear `ARRANGER_MIN_SUB_BEAT_PX`.
        let max = grid_tiers(
            px_per_beat_to_ppt(MAX_PX_PER_BEAT),
            FLOOR,
            GridSurface::Arranger,
            FOUR_FOUR,
        );
        assert_eq!(max.snap_ticks, FLOOR);
    }

    #[test]
    fn fully_zoomed_out_coarsens_bars_and_keeps_labels_apart() {
        let ppt = px_per_beat_to_ppt(ARRANGER_MIN_PX_PER_BEAT);
        let t = grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR);
        assert!(t.bar_ticks >= FOUR_FOUR.bars_to_ticks(2), "{t:?}");
        assert_eq!(t.beat_ticks, None);
        assert!(t.label_ticks as f32 * ppt >= MIN_LABEL_PX, "{t:?}");
    }

    #[test]
    fn half_bar_snap_appears_between_beat_and_bar() {
        // 5 px/beat: beats (5 px) too tight to draw or snap to, half-bars
        // (10 px) clear the snap threshold, bars (20 px) stay one bar.
        let t = grid_tiers(
            px_per_beat_to_ppt(5.0),
            FLOOR,
            GridSurface::Arranger,
            FOUR_FOUR,
        );
        assert_eq!(t.bar_ticks, FOUR_FOUR.bars_to_ticks(1));
        assert_eq!(t.beat_ticks, None);
        assert_eq!(t.snap_ticks, FOUR_FOUR.bars_to_ticks(1) / 2);
    }

    #[test]
    fn default_view_is_never_finer_than_beats_at_any_window_width() {
        // Fully zoomed out on a short project *is* the default 32-bar view,
        // so on a wide window too the finest step must not go below the beat
        // (a narrow one may go coarser — half-bars below 8 px/beat).
        for content_w in [900.0, 1274.0, 1800.0, 2400.0, 3400.0] {
            let ppt = content_w / BARS_IN_VIEWPORT as f32 / FOUR_FOUR.bars_to_ticks(1) as f32;
            let t = grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR);
            assert!(
                t.snap_ticks >= beats_to_ticks(1.0),
                "content_w {content_w}: {t:?}"
            );
            assert!(!t.has_tier_below_beat(), "content_w {content_w}");
        }
        // From the reference width up it is exactly the beat.
        for content_w in [1274.0, 2400.0, 3400.0] {
            let ppt = content_w / BARS_IN_VIEWPORT as f32 / FOUR_FOUR.bars_to_ticks(1) as f32;
            assert_eq!(
                grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR).snap_ticks,
                beats_to_ticks(1.0)
            );
        }
    }

    #[test]
    fn zooming_in_adds_eighths_then_sixteenths_under_the_beat() {
        let beat = beats_to_ticks(1.0);
        // Just short of 8ths (15.9 px apart): still beats.
        let beats_only = grid_tiers(
            px_per_beat_to_ppt(31.8),
            FLOOR,
            GridSurface::Arranger,
            FOUR_FOUR,
        );
        assert_eq!(beats_only.snap_ticks, beat);
        let eighth = grid_tiers(
            px_per_beat_to_ppt(32.0),
            FLOOR,
            GridSurface::Arranger,
            FOUR_FOUR,
        );
        assert_eq!(
            (eighth.beat_ticks, eighth.snap_ticks),
            (Some(beat), beat / 2)
        );
        let sixteenth = grid_tiers(
            px_per_beat_to_ppt(64.0),
            FLOOR,
            GridSurface::Arranger,
            FOUR_FOUR,
        );
        assert_eq!(
            (sixteenth.beat_ticks, sixteenth.snap_ticks),
            (Some(beat), beat / 4)
        );
    }

    #[test]
    fn a_line_is_drawn_as_its_coarsest_tier() {
        let bar = FOUR_FOUR.bars_to_ticks(1);
        let beat = beats_to_ticks(1.0);
        let tiers = bar_beat_sixteenth();
        assert_eq!(tiers.role_at(0), GridRole::Bar);
        assert_eq!(tiers.role_at(3 * bar), GridRole::Bar);
        assert_eq!(tiers.role_at(bar + beat), GridRole::Beat);
        assert_eq!(tiers.role_at(bar + beat / 4), GridRole::Snap);
        assert!(tiers.has_tier_below_beat());
    }

    #[test]
    fn coarsened_bar_tier_demotes_odd_bars_to_snap_and_thins_labels() {
        // Zoomed out: structural tier every 2 bars, snap every bar, labels
        // every 4 — bar 2 (tick = 1 bar) is a snap line, unlabelled.
        let bar = FOUR_FOUR.bars_to_ticks(1);
        let tiers = GridTiers {
            label_ticks: 4 * bar,
            bar_ticks: 2 * bar,
            beat_ticks: None,
            snap_ticks: bar,
            ruler_ticks: bar,
        };
        assert_eq!(tiers.role_at(bar), GridRole::Snap);
        assert_eq!(tiers.role_at(2 * bar), GridRole::Bar);
        assert!(!tiers.is_labelled(2 * bar));
        assert!(tiers.is_labelled(4 * bar));
        assert!(!tiers.has_tier_below_beat());
    }

    #[test]
    fn default_arranger_beats_are_the_finest_tier() {
        let ppt = 1274.0 / BARS_IN_VIEWPORT as f32 / FOUR_FOUR.bars_to_ticks(1) as f32;
        assert!(!grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR).has_tier_below_beat());
    }

    #[test]
    fn line_strength_falls_off_with_fineness() {
        let tiers = bar_beat_sixteenth();
        for crosses_track_seams in [false, true] {
            let bar = tiers.line_strength(GridRole::Bar, crosses_track_seams);
            let beat = tiers.line_strength(GridRole::Beat, crosses_track_seams);
            let snap = tiers.line_strength(GridRole::Snap, crosses_track_seams);
            assert!(bar <= 1.0, "{bar}");
            assert!(
                bar > beat && beat > snap && snap > 0.0,
                "{bar} {beat} {snap}"
            );
        }
    }

    #[test]
    fn arranger_bar_lines_sit_below_the_full_strength_track_seams() {
        let tiers = bar_beat_sixteenth();
        assert_eq!(tiers.line_strength(GridRole::Bar, false), 1.0);
        assert!(tiers.line_strength(GridRole::Bar, true) < 1.0);
        // Only the bar tier changes — beats and snap lines are the same on
        // either surface.
        for role in [GridRole::Beat, GridRole::Snap] {
            assert_eq!(
                tiers.line_strength(role, true),
                tiers.line_strength(role, false)
            );
        }
    }

    #[test]
    fn beats_that_are_the_finest_tier_draw_at_snap_strength() {
        let ppt = 1274.0 / BARS_IN_VIEWPORT as f32 / FOUR_FOUR.bars_to_ticks(1) as f32;
        let tiers = grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR);
        assert_eq!(
            tiers.line_strength(GridRole::Beat, true),
            tiers.line_strength(GridRole::Snap, true)
        );
    }

    #[test]
    fn the_piano_roll_shows_sub_beats_sooner_than_the_arranger() {
        // 40 px/beat: 16ths 10 px apart. The arranger holds them back (and
        // its 8ths, 20 px, are the finest); the piano roll draws them.
        let ppt = px_per_beat_to_ppt(40.0);
        let beat = beats_to_ticks(1.0);
        assert_eq!(
            grid_tiers(ppt, FLOOR, GridSurface::Arranger, FOUR_FOUR).snap_ticks,
            beat / 2
        );
        // Its ruler thins the 10 px 16ths to 8ths (`MIN_RULER_SUB_BEAT_PX`).
        assert_eq!(
            grid_tiers(ppt, FLOOR, GridSurface::PianoRoll, FOUR_FOUR),
            GridTiers {
                ruler_ticks: beat / 2,
                ..bar_beat_sixteenth()
            }
        );
    }

    #[test]
    fn a_fitted_clip_rulers_its_fine_snap_no_finer_than_16ths() {
        // A 2-bar clip fitted to ~1300 pt snaps to 64ths (≈ 10 px), but the
        // ruler graduates only every 16th — and zooming all the way in
        // never takes the ruler below the 16th either.
        let beat = beats_to_ticks(1.0);
        let floor = GridSurface::PianoRoll.snap_floor_ticks();
        let fitted = grid_tiers(
            1274.0 / FOUR_FOUR.bars_to_ticks(2) as f32,
            floor,
            GridSurface::PianoRoll,
            FOUR_FOUR,
        );
        assert_eq!(fitted.snap_ticks, beat / 16);
        assert_eq!(fitted.ruler_ticks, FLOOR);
        let max = grid_tiers(
            px_per_beat_to_ppt(MAX_PX_PER_BEAT),
            floor,
            GridSurface::PianoRoll,
            FOUR_FOUR,
        );
        assert_eq!(max.ruler_ticks, FLOOR);
        // Tight 16ths (beat ≈ 40 px) fall back to 8ths, then to the beat.
        let tight = grid_tiers(
            px_per_beat_to_ppt(24.0),
            floor,
            GridSurface::PianoRoll,
            FOUR_FOUR,
        );
        assert_eq!(tight.ruler_ticks, beat);
    }

    #[test]
    fn ruler_ticks_nest_and_never_thin_beats_or_bars() {
        for surface in [GridSurface::Arranger, GridSurface::PianoRoll] {
            for px_per_beat in zoom_sweep() {
                let ppt = px_per_beat_to_ppt(px_per_beat);
                let t = grid_tiers(ppt, surface.snap_floor_ticks(), surface, FOUR_FOUR);
                assert_eq!(t.ruler_ticks % t.snap_ticks, 0, "{px_per_beat}: {t:?}");
                let beat = beats_to_ticks(1.0);
                if t.snap_ticks < beat {
                    assert_eq!(beat % t.ruler_ticks, 0, "{px_per_beat}: {t:?}");
                    assert!(t.ruler_ticks >= FLOOR, "{px_per_beat}: {t:?}");
                } else {
                    assert_eq!(t.ruler_ticks, t.snap_ticks, "{px_per_beat}: {t:?}");
                }
                assert!(t.has_ruler_tick(0));
                if let Some(beat) = t.beat_ticks {
                    assert!(t.has_ruler_tick(beat), "{px_per_beat}: {t:?}");
                }
            }
        }
    }

    #[test]
    fn snap_lines_off_the_ruler_step_get_no_ruler_tick() {
        let beat = beats_to_ticks(1.0);
        let tiers = GridTiers {
            snap_ticks: beat / 16,
            ruler_ticks: beat / 4,
            ..bar_beat_sixteenth()
        };
        assert!(!tiers.has_ruler_tick(beat / 16));
        assert!(!tiers.has_ruler_tick(beat / 8));
        assert!(tiers.has_ruler_tick(beat / 4));
        assert!(tiers.has_ruler_tick(beat));
    }

    #[test]
    fn a_fitted_two_bar_clip_snaps_to_64ths_like_abletons_narrowest() {
        // The clip view's home framing of a 2-bar clip: 16 steps a beat on
        // an ordinary window, finer on a wide one, never coarser than 32nds.
        let beat = beats_to_ticks(1.0);
        let floor = GridSurface::PianoRoll.snap_floor_ticks();
        for (content_w, snap) in [
            (1274.0_f32, beat / 16),
            (2100.0, beat / 32),
            (700.0, beat / 8),
        ] {
            let ppt = content_w / FOUR_FOUR.bars_to_ticks(2) as f32;
            let t = grid_tiers(ppt, floor, GridSurface::PianoRoll, FOUR_FOUR);
            assert_eq!(t.snap_ticks, snap, "content_w {content_w}: {t:?}");
        }
    }

    #[test]
    fn zoomed_all_the_way_in_the_piano_roll_snaps_to_256ths() {
        let floor = GridSurface::PianoRoll.snap_floor_ticks();
        let t = grid_tiers(
            px_per_beat_to_ppt(MAX_PX_PER_BEAT),
            floor,
            GridSurface::PianoRoll,
            FOUR_FOUR,
        );
        assert_eq!(t.snap_ticks, FOUR_FOUR.bars_to_ticks(1) / 256);
        assert_eq!(
            GridSurface::Arranger.snap_floor_ticks(),
            FLOOR,
            "the arranger keeps its 16th floor"
        );
    }

    #[test]
    fn piano_roll_tiers_are_monotonic_and_nest() {
        let mut previous: Option<GridTiers> = None;
        let floor = GridSurface::PianoRoll.snap_floor_ticks();
        for px_per_beat in zoom_sweep() {
            let t = grid_tiers(
                px_per_beat_to_ppt(px_per_beat),
                floor,
                GridSurface::PianoRoll,
                FOUR_FOUR,
            );
            assert!(t.snap_ticks >= floor, "{px_per_beat}: {t:?}");
            assert_eq!(t.bar_ticks % t.snap_ticks, 0, "{px_per_beat}: {t:?}");
            if let Some(beat) = t.beat_ticks {
                assert_eq!(beat % t.snap_ticks, 0, "{px_per_beat}: {t:?}");
            }
            if let Some(prev) = previous {
                assert!(t.snap_ticks <= prev.snap_ticks, "{px_per_beat}");
                assert!(t.bar_ticks <= prev.bar_ticks, "{px_per_beat}");
                assert!(t.label_ticks <= prev.label_ticks, "{px_per_beat}");
            }
            previous = Some(t);
        }
    }

    /// Builds a meter the test knows is supported.
    fn meter(numerator: u8, denominator: u8) -> Meter {
        Meter::new(numerator, denominator).unwrap()
    }

    #[test]
    fn the_ladder_follows_the_meter() {
        let eighth = beats_to_ticks(1.0) / 2;
        // 3/4: binary up to the quarter, no half bar (odd numerator).
        let three_four: Vec<i32> = ladder(meter(3, 4)).collect();
        assert_eq!(three_four[6..8], [eighth * 2, eighth * 6]);
        // 6/8: binary up to the eighth, the dotted-quarter half bar, the bar.
        let six_eight: Vec<i32> = ladder(meter(6, 8)).collect();
        assert_eq!(six_eight[5..8], [eighth, eighth * 3, eighth * 6]);
        // 7/8: no half bar — half a 7/8 bar is off the eighth grid.
        let seven_eight: Vec<i32> = ladder(meter(7, 8)).collect();
        assert_eq!(seven_eight[5..7], [eighth, eighth * 7]);
        // 2/4: the half bar *is* the beat, listed once.
        let two_four: Vec<i32> = ladder(meter(2, 4)).collect();
        assert_eq!(two_four[6..8], [eighth * 2, eighth * 4]);
        for m in [meter(3, 4), meter(6, 8), meter(7, 8), meter(2, 4)] {
            let rungs: Vec<i32> = ladder(m).collect();
            assert_eq!(rungs.first(), Some(&15), "{m:?}");
            assert_eq!(rungs.last(), Some(&(m.bar_ticks() * 64)), "{m:?}");
        }
    }

    #[test]
    fn every_rung_divides_the_next_in_every_meter() {
        for n in 1..=16 {
            for d in [4, 8] {
                let m = meter(n, d);
                let rungs: Vec<i32> = ladder(m).collect();
                assert!(
                    rungs.windows(2).all(|w| w[1] > w[0] && w[1] % w[0] == 0),
                    "{m:?}: {rungs:?}"
                );
                assert!(rungs.contains(&m.beat_ticks()), "{m:?}");
                assert!(rungs.contains(&m.bar_ticks()), "{m:?}");
            }
        }
    }

    #[test]
    fn tiers_nest_and_bars_are_whole_bars_in_other_meters() {
        for m in [meter(3, 4), meter(6, 8), meter(7, 8), meter(5, 4)] {
            for surface in [GridSurface::Arranger, GridSurface::PianoRoll] {
                for px_per_beat in zoom_sweep() {
                    let ppt = px_per_beat_to_ppt(px_per_beat);
                    let t = grid_tiers(ppt, surface.snap_floor_ticks(), surface, m);
                    let at = format!("{m:?} {surface:?} {px_per_beat}: {t:?}");
                    assert_eq!(t.label_ticks % t.bar_ticks, 0, "{at}");
                    assert_eq!(t.bar_ticks % m.bar_ticks(), 0, "{at}");
                    assert_eq!(t.bar_ticks % t.snap_ticks, 0, "{at}");
                    assert_eq!(t.ruler_ticks % t.snap_ticks, 0, "{at}");
                    if let Some(beat) = t.beat_ticks {
                        assert_eq!(beat, m.beat_ticks(), "{at}");
                        assert_eq!(t.bar_ticks % beat, 0, "{at}");
                        assert_eq!(beat % t.snap_ticks, 0, "{at}");
                    }
                }
            }
        }
    }

    #[test]
    fn seven_eight_never_snaps_to_a_half_bar() {
        let m = meter(7, 8);
        for px_per_beat in zoom_sweep() {
            let ppt = px_per_beat_to_ppt(px_per_beat);
            let t = grid_tiers(ppt, FLOOR, GridSurface::Arranger, m);
            assert_ne!(t.snap_ticks, m.bar_ticks() / 2, "{px_per_beat}: {t:?}");
        }
    }

    #[test]
    fn six_eight_beat_lines_are_eighths() {
        // 40 px a quarter: eighths 20 px apart, beats drawn and snapped to.
        let t = grid_tiers(
            px_per_beat_to_ppt(40.0),
            FLOOR,
            GridSurface::Arranger,
            meter(6, 8),
        );
        let eighth = beats_to_ticks(1.0) / 2;
        assert_eq!(t.bar_ticks, eighth * 6);
        assert_eq!(t.beat_ticks, Some(eighth));
        assert_eq!(t.snap_ticks, eighth);
    }

    #[test]
    fn three_four_snaps_to_beats_then_bars_zooming_out() {
        // 5 px a quarter: beats too tight, and with no half bar in 3/4 the
        // snap goes straight to the bar.
        let m = meter(3, 4);
        let t = grid_tiers(px_per_beat_to_ppt(5.0), FLOOR, GridSurface::Arranger, m);
        assert_eq!(t.beat_ticks, None);
        assert_eq!(t.snap_ticks, m.bar_ticks());
        let t = grid_tiers(px_per_beat_to_ppt(10.0), FLOOR, GridSurface::Arranger, m);
        assert_eq!(t.beat_ticks, Some(m.beat_ticks()));
        assert_eq!(t.snap_ticks, m.beat_ticks());
    }
}
