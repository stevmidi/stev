//! Musical-time arithmetic: the tick resolution ([`PPQN`]), bar/beat/tick and
//! tempo conversions, grid snapping, tap-tempo, and the process-wide monotonic
//! origin used to exchange sub-millisecond timing between threads as a plain
//! `u64`.
//!
//! The `080-conventions.md` `tick`/`ticks` rule applies to every helper name
//! here: [`Meter::bars_to_ticks`], `sixteenth_straight_ticks` etc. all return *amounts*;
//! [`Meter::next_bar_boundary_after`] returns a *position*. Pure functions, unit-tested
//! at the bottom. `150-clock-position-sync.md` explains the timing model these
//! feed.

use std::fmt;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Pulses (ticks) per quarter note — the sequencer's internal timing resolution.
pub(crate) const PPQN: i32 = 960;

/// Microseconds in a minute — the constant relating a µs-per-quarter tempo to BPM.
pub(crate) const MICROSECONDS_PER_MINUTE: i32 = 60_000_000;

/// Process-wide monotonic zero. Only *differences* on it are meaningful — it
/// exists so threads can exchange sub-millisecond timing as a plain `u64`
/// without passing `Instant`s around: `Clock` stamps each tick's time on it and
/// the MIDI-input callback reads back to interpolate a fractional tick for a
/// note that arrives between clock firings.
static MONOTONIC_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Forces [`MONOTONIC_ORIGIN`] to initialize now, so the first timestamps on it
/// start near zero rather than after whatever lazy first touch. Idempotent.
pub(crate) fn anchor_monotonic_origin() {
    LazyLock::force(&MONOTONIC_ORIGIN);
}

/// Nanoseconds since [`MONOTONIC_ORIGIN`].
pub(crate) fn monotonic_nanos() -> u64 {
    MONOTONIC_ORIGIN.elapsed().as_nanos() as u64
}

/// The same timeline value for a specific `Instant` (saturates to 0 for an
/// instant recorded before the origin was anchored).
pub(crate) fn monotonic_nanos_at(at: Instant) -> u64 {
    at.saturating_duration_since(*MONOTONIC_ORIGIN).as_nanos() as u64
}

/// Pixels → ticks (an amount) at a given zoom, rounded.
pub const fn pixels_to_ticks(pixels: f32, ticks_per_pixel: f32) -> i32 {
    (pixels * ticks_per_pixel).round() as i32
}

/// Rounds a tick position or offset to the nearest multiple of
/// `grid_resolution`, a half rounding up — alike either side of zero, so a
/// negative offset (a leftward drag) snaps like a positive one. Used to snap
/// mouse-driven cursor positions to a view-appropriate grid (e.g. bars in the
/// arranger, beats in the clip view). A resolution of zero or less leaves
/// `tick` as is.
pub const fn snap_to_grid(tick: i32, grid_resolution: i32) -> i32 {
    if grid_resolution <= 0 {
        return tick;
    }
    (tick + grid_resolution / 2).div_euclid(grid_resolution) * grid_resolution
}

/// Steps a non-negative tick position to the next/previous `grid_ticks`
/// boundary: `direction > 0` jumps forward to the next boundary strictly
/// ahead, `direction <= 0` jumps back to the previous one (a full step back
/// when already sitting exactly on one). Used to give keyboard cursor
/// stepping the same grid resolution as mouse-driven placement
/// (`Display::cursor_grid_ticks`) — the zoom-adaptive snap in the arranger,
/// a sixteenth note in the clip view — instead of a coarser fixed step.
pub const fn step_to_grid(tick: i32, grid_ticks: i32, direction: i32) -> i32 {
    if direction > 0 {
        (tick / grid_ticks + 1) * grid_ticks
    } else if tick % grid_ticks == 0 {
        tick - grid_ticks
    } else {
        (tick / grid_ticks) * grid_ticks
    }
}

/// A project's time signature: `numerator` counted beats of a `denominator`
/// note to the bar. Only the meters Stev supports can be built — numerator
/// 1–16 over 4 or 8 ([`new`](Self::new)) — so every bar length is a whole
/// number of ticks. The tempo stays quarter notes per minute in every meter.
/// One meter per project (`270-time-signature.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meter {
    /// Counted beats per bar, 1–16.
    numerator: u8,
    /// The counted beat's note value: 4 (a quarter) or 8 (an eighth).
    denominator: u8,
}

impl Meter {
    /// Common time — what a project without a meter means.
    pub(crate) const FOUR_FOUR: Meter = Meter {
        numerator: 4,
        denominator: 4,
    };

    /// The largest numerator a meter may have.
    const MAX_NUMERATOR: u8 = 16;

    /// A meter of `numerator` over `denominator`, or `None` outside the
    /// supported range (numerator 1–16, denominator 4 or 8).
    pub(crate) const fn new(numerator: u8, denominator: u8) -> Option<Meter> {
        if numerator >= 1 && numerator <= Self::MAX_NUMERATOR && matches!(denominator, 4 | 8) {
            Some(Meter {
                numerator,
                denominator,
            })
        } else {
            None
        }
    }

    /// Counted beats per bar.
    pub(crate) const fn numerator(self) -> u8 {
        self.numerator
    }

    /// The counted beat's note value (4 or 8).
    pub(crate) const fn denominator(self) -> u8 {
        self.denominator
    }

    /// One bar, in ticks (an amount): `PPQN × 4 × numerator / denominator`.
    /// Exact for every meter [`new`](Self::new) accepts.
    pub(crate) const fn bar_ticks(self) -> i32 {
        PPQN * 4 * self.numerator as i32 / self.denominator as i32
    }

    /// One counted beat — a `denominator` note — in ticks (an amount): 960
    /// for x/4, 480 for x/8. The click sounds on each one.
    pub(crate) const fn beat_ticks(self) -> i32 {
        PPQN * 4 / self.denominator as i32
    }

    /// Bars → ticks (an amount).
    pub(crate) const fn bars_to_ticks(self, bars: i32) -> i32 {
        self.bar_ticks() * bars
    }

    /// Ticks → whole bars (truncating).
    pub(crate) const fn ticks_to_bars(self, ticks: i32) -> i32 {
        ticks / self.bar_ticks()
    }

    /// The first whole-bar tick *strictly* after `tick`: a tick already
    /// sitting on a bar line advances a full bar. `rem_euclid`-based, so
    /// negative input works (`-1` → `0`). Bars count from tick 0.
    pub(crate) const fn next_bar_boundary_after(self, tick: i32) -> i32 {
        let bar = self.bar_ticks();
        tick + (bar - tick.rem_euclid(bar))
    }

    /// Packs the meter into one `u16` for a shared atomic: numerator in the
    /// high byte, denominator in the low one.
    pub(crate) const fn to_bits(self) -> u16 {
        (self.numerator as u16) << 8 | self.denominator as u16
    }

    /// Unpacks [`to_bits`](Self::to_bits). Anything that isn't a supported
    /// meter reads as 4/4 — only `to_bits` ever writes the atomic, so that
    /// is a guard, not a path.
    pub(crate) const fn from_bits(bits: u16) -> Meter {
        match Meter::new((bits >> 8) as u8, bits as u8) {
            Some(meter) => meter,
            None => Meter::FOUR_FOUR,
        }
    }

    /// A typed meter — `7/8`, spaces allowed around the parts — or `None`
    /// when it isn't one [`new`](Self::new) accepts. The header's meter field
    /// reads its text with this.
    pub(crate) fn parse(text: &str) -> Option<Meter> {
        let (numerator, denominator) = text.split_once('/')?;
        Meter::new(
            numerator.trim().parse().ok()?,
            denominator.trim().parse().ok()?,
        )
    }
}

/// `7/8` — how the header shows the meter, and what [`Meter::parse`] reads.
impl fmt::Display for Meter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

/// Fractional beat (quarter-note) position of `ticks`, e.g. for a CLAP
/// transport's `song_pos_beats`. Keeps the sub-beat part, and unlike [`ticks_to_microseconds`] it does not truncate before dividing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn ticks_to_beats_f64(ticks: i32) -> f64 {
    f64::from(ticks) / f64::from(PPQN)
}

/// Beats (fractional) → ticks (an amount), truncating.
pub const fn beats_to_ticks(beats: f32) -> i32 {
    (beats * PPQN as f32) as i32
}

/// A horizontal scale in pixels per beat → pixels per tick — the form every
/// tick ↔ screen-x conversion uses.
pub const fn px_per_beat_to_ppt(px_per_beat: f32) -> f32 {
    px_per_beat / PPQN as f32
}

/// The shortest an arranger clip can be — one beat, in ticks (an amount): the
/// floor for clip edge drags (`Sequencer::resize_selected_clip_region_*`),
/// capture commits and region windows. A model invariant, deliberately *not*
/// the arranger's snap grid, which follows the zoom view-side
/// (`Display::cursor_grid_ticks`, `archive/190-arranger-zoom.md`) and can be finer.
/// Named `arranger_grid_ticks` until the snap stopped using it.
pub const fn min_clip_length_ticks() -> i32 {
    PPQN
}

/// Length of a straight 16th note, in ticks (an amount).
pub const fn sixteenth_straight_ticks() -> i32 {
    PPQN / 4
}

/// Length of a triplet 16th note, in ticks (an amount).
pub const fn sixteenth_triplet_ticks() -> i32 {
    PPQN / 6
}

/// Wall-clock microseconds for `ticks` at `tempo` (µs per quarter). Truncates
/// `tempo / PPQN` first, so pick a tempo divisible by PPQN where exactness
/// matters.
pub const fn ticks_to_microseconds(tempo: i32, ticks: i32) -> i32 {
    tempo / PPQN * ticks
}

/// A BPM → µs-per-quarter tempo (truncating). The app shows a tempo through
/// [`format_bpm`].
pub const fn bpm_to_tempo_us(bpm: i32) -> i32 {
    MICROSECONDS_PER_MINUTE / bpm
}

/// A µs-per-quarter tempo → BPM, for tests to compare against with a
/// tolerance; the app goes through tenths ([`tempo_us_to_bpm_tenths`]).
#[cfg(test)]
pub const fn tempo_us_to_bpm(tempo: i32) -> f32 {
    MICROSECONDS_PER_MINUTE as f32 / tempo as f32
}

/// Microseconds in a minute, times ten — relates a µs-per-quarter tempo to
/// tenths of a BPM, the header's BPM resolution.
const MICROSECONDS_PER_TENTH_BPM_MINUTE: i64 = 600_000_000;

/// The slowest tempo the app sets, in BPM — by hand, a drag or tapping.
pub(crate) const TEMPO_BPM_MIN: i32 = 20;
/// The fastest tempo the app sets, in BPM — see [`TEMPO_BPM_MIN`].
pub(crate) const TEMPO_BPM_MAX: i32 = 300;

/// A tempo in tenths of a BPM → µs per quarter, rounded to the nearest µs.
/// `tenths` must be positive.
pub(crate) fn bpm_tenths_to_tempo_us(tenths: i32) -> i32 {
    let tenths = i64::from(tenths);
    ((MICROSECONDS_PER_TENTH_BPM_MINUTE + tenths / 2) / tenths) as i32
}

/// A µs-per-quarter tempo → the nearest tenth of a BPM (what the header
/// shows). `tempo_us` must be positive.
pub(crate) fn tempo_us_to_bpm_tenths(tempo_us: i32) -> i32 {
    // The units are reciprocal: the same rounded division goes either way.
    bpm_tenths_to_tempo_us(tempo_us)
}

/// Clamps a µs-per-quarter tempo to [`TEMPO_BPM_MIN`]..=[`TEMPO_BPM_MAX`].
pub(crate) const fn clamp_tempo_us(tempo_us: i32) -> i32 {
    let fastest = bpm_to_tempo_us(TEMPO_BPM_MAX);
    let slowest = bpm_to_tempo_us(TEMPO_BPM_MIN);
    if tempo_us < fastest {
        fastest
    } else if tempo_us > slowest {
        slowest
    } else {
        tempo_us
    }
}

/// Clamps a tempo in tenths of a BPM to [`TEMPO_BPM_MIN`]..=[`TEMPO_BPM_MAX`].
pub(crate) const fn clamp_bpm_tenths(tenths: i32) -> i32 {
    if tenths < TEMPO_BPM_MIN * 10 {
        TEMPO_BPM_MIN * 10
    } else if tenths > TEMPO_BPM_MAX * 10 {
        TEMPO_BPM_MAX * 10
    } else {
        tenths
    }
}

/// A µs-per-quarter tempo as the header shows it: to the nearest tenth of a
/// BPM, one decimal (`120.0`). The BPM field opens with the same text.
pub(crate) fn format_bpm(tempo_us: i32) -> String {
    let tenths = tempo_us_to_bpm_tenths(tempo_us);
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// A typed BPM → tenths of a BPM, clamped to the app's tempo range; `None`
/// when it isn't a number. A comma works as the decimal point too.
pub(crate) fn parse_bpm_tenths(text: &str) -> Option<i32> {
    let bpm: f32 = text.trim().replace(',', ".").parse().ok()?;
    if !bpm.is_finite() {
        return None;
    }
    Some(clamp_bpm_tenths((bpm * 10.0).round() as i32))
}

/// The tempo (µs per quarter) that makes `scaled_ticks` of musical content last
/// as long as `reference_ticks` did at `current_tempo_us` — the maths behind
/// clip tempo rescale. `None` if either length is non-positive.
pub(crate) fn scaled_tempo_us(
    current_tempo_us: i32,
    scaled_ticks: i32,
    reference_ticks: i32,
) -> Option<i32> {
    if scaled_ticks <= 0 || reference_ticks <= 0 {
        return None;
    }

    let numerator = i64::from(current_tempo_us) * i64::from(scaled_ticks);
    let denominator = i64::from(reference_ticks);

    Some(((numerator + denominator / 2) / denominator) as i32)
}

/// How far back a tap still counts toward the tempo; a tap this long after
/// the one before starts a new burst ([`TapTempo`]).
const TAP_WINDOW: Duration = Duration::from_secs(2);

/// Given a slice of `Instant` tap timestamps (most recent last), returns a
/// tempo in microseconds-per-beat, or `None` with fewer than 3 recent taps.
/// Taps older than [`TAP_WINDOW`] from the most recent are ignored.
pub fn tap_tempo_us(taps: &[Instant]) -> Option<i32> {
    let now = taps.last()?;

    let recent: Vec<_> = taps
        .iter()
        .filter(|t| now.duration_since(**t) < TAP_WINDOW)
        .collect();

    if recent.len() < 3 {
        return None;
    }

    let intervals: Vec<u128> = recent
        .windows(2)
        .map(|w| w[1].duration_since(*w[0]).as_micros())
        .collect();

    let avg_us = intervals.iter().sum::<u128>() / intervals.len() as u128;

    let avg_us = i32::try_from(avg_us).unwrap_or(i32::MAX);
    Some(clamp_tempo_us(avg_us))
}

/// Tap tempo (`T`): the recent taps, and which burst they belong to. A burst
/// is a run of taps each within [`TAP_WINDOW`] of the one before; its tempo
/// steps are one undo step (`TempoGesture::Taps`).
#[derive(Debug, Default)]
pub(crate) struct TapTempo {
    /// The burst's last few taps, most recent last.
    taps: Vec<Instant>,
    /// The burst's number, one up for each new burst.
    burst: u64,
}

impl TapTempo {
    /// Taps kept for the average.
    const MAX_TAPS: usize = 8;

    /// Registers a tap at `now`. Once the burst has enough taps, returns the
    /// tempo they give and the burst's number.
    pub(crate) fn tap(&mut self, now: Instant) -> Option<(i32, u64)> {
        if self
            .taps
            .last()
            .is_some_and(|&last| now.duration_since(last) >= TAP_WINDOW)
        {
            self.taps.clear();
            self.burst += 1;
        }
        self.taps.push(now);
        if self.taps.len() > Self::MAX_TAPS {
            self.taps.remove(0);
        }
        tap_tempo_us(&self.taps).map(|tempo_us| (tempo_us, self.burst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a meter the test knows is supported.
    fn meter(numerator: u8, denominator: u8) -> Meter {
        Meter::new(numerator, denominator).unwrap()
    }

    #[test]
    fn a_bar_is_numerator_notes_of_the_denominator() {
        assert_eq!(Meter::FOUR_FOUR.bar_ticks(), PPQN * 4);
        assert_eq!(meter(3, 4).bar_ticks(), PPQN * 3);
        assert_eq!(meter(6, 8).bar_ticks(), PPQN * 3);
        assert_eq!(meter(7, 8).bar_ticks(), PPQN * 7 / 2);
        assert_eq!(meter(1, 8).bar_ticks(), PPQN / 2);
        assert_eq!(meter(16, 4).bar_ticks(), PPQN * 16);
    }

    #[test]
    fn only_supported_meters_can_be_built() {
        assert_eq!(Meter::new(0, 4), None);
        assert_eq!(Meter::new(17, 4), None);
        assert_eq!(Meter::new(4, 2), None);
        assert_eq!(Meter::new(4, 3), None);
        assert_eq!(Meter::new(4, 16), None);
        assert_eq!(Meter::new(4, 4), Some(Meter::FOUR_FOUR));
    }

    #[test]
    fn meter_bar_conversions_follow_the_meter() {
        let m = meter(7, 8);
        assert_eq!(m.bars_to_ticks(3), m.bar_ticks() * 3);
        assert_eq!(m.ticks_to_bars(m.bars_to_ticks(5)), 5);
        assert_eq!(m.ticks_to_bars(m.bar_ticks() - 1), 0);
        assert_eq!(m.next_bar_boundary_after(0), m.bar_ticks());
        assert_eq!(
            m.next_bar_boundary_after(m.bar_ticks() + 1),
            m.bar_ticks() * 2
        );
        assert_eq!(m.next_bar_boundary_after(-1), 0);
    }

    #[test]
    fn the_counted_beat_is_one_denominator_note() {
        assert_eq!(Meter::FOUR_FOUR.beat_ticks(), PPQN);
        assert_eq!(meter(3, 4).beat_ticks(), PPQN);
        assert_eq!(meter(6, 8).beat_ticks(), PPQN / 2);
        assert_eq!(meter(7, 8).beat_ticks(), PPQN / 2);
    }

    #[test]
    fn a_meter_round_trips_through_its_bits() {
        for m in [
            Meter::FOUR_FOUR,
            meter(3, 4),
            meter(6, 8),
            meter(16, 8),
            meter(1, 4),
        ] {
            assert_eq!(Meter::from_bits(m.to_bits()), m);
        }
    }

    #[test]
    fn a_meter_reads_back_what_it_shows() {
        for m in [Meter::FOUR_FOUR, meter(3, 4), meter(7, 8), meter(16, 8)] {
            assert_eq!(Meter::parse(&m.to_string()), Some(m));
        }
        assert_eq!(meter(6, 8).to_string(), "6/8");
        assert_eq!(Meter::parse(" 5 / 4 "), Some(meter(5, 4)));
    }

    #[test]
    fn a_typed_meter_outside_the_scope_is_rejected() {
        for text in [
            "", "4", "4/", "/4", "4/4/4", "0/4", "17/4", "4/2", "4/16", "-3/4", "three/4",
        ] {
            assert_eq!(Meter::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn unsupported_bits_read_as_four_four() {
        assert_eq!(Meter::from_bits(0), Meter::FOUR_FOUR);
        assert_eq!(Meter::from_bits(17 << 8 | 4), Meter::FOUR_FOUR);
        assert_eq!(Meter::from_bits(3 << 8 | 2), Meter::FOUR_FOUR);
    }

    #[test]
    fn next_bar_boundary_after_on_boundary_advances_one_bar() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(0), bar);
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(bar), bar * 2);
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(bar * 3), bar * 4);
    }

    #[test]
    fn next_bar_boundary_after_mid_bar_rounds_up() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(1), bar);
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(bar + 600), bar * 2);
    }

    #[test]
    fn next_bar_boundary_after_negative_input() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(-1), 0);
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(-bar), 0);
        assert_eq!(Meter::FOUR_FOUR.next_bar_boundary_after(-bar - 1), -bar);
    }

    #[test]
    fn sixteenth_straight_ticks_is_ppqn_over_4() {
        assert_eq!(sixteenth_straight_ticks(), PPQN / 4);
    }

    #[test]
    fn snap_to_grid_rounds_to_nearest_multiple() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(snap_to_grid(bar - 1, bar), bar);
        assert_eq!(snap_to_grid(bar / 2 - 1, bar), 0);
        assert_eq!(snap_to_grid(0, bar), 0);
    }

    #[test]
    fn snap_to_grid_rounds_negative_values_like_positive_ones() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        assert_eq!(snap_to_grid(-bar - 5, bar), -bar);
        assert_eq!(snap_to_grid(-bar / 2 - 1, bar), -bar);
        assert_eq!(snap_to_grid(-bar / 2, bar), 0, "halves round up");
        assert_eq!(snap_to_grid(bar / 2, bar), bar, "halves round up");
    }

    #[test]
    fn snap_to_grid_zero_resolution_is_noop() {
        assert_eq!(snap_to_grid(123, 0), 123);
    }

    #[test]
    fn step_to_grid_forward_from_boundary_advances_one_step() {
        let beat = beats_to_ticks(1.0);
        assert_eq!(step_to_grid(0, beat, 1), beat);
        assert_eq!(step_to_grid(beat, beat, 1), beat * 2);
    }

    #[test]
    fn step_to_grid_forward_mid_grid_rounds_up_to_next_boundary() {
        let beat = beats_to_ticks(1.0);
        assert_eq!(step_to_grid(beat + 10, beat, 1), beat * 2);
    }

    #[test]
    fn step_to_grid_backward_from_boundary_retreats_one_full_step() {
        let beat = beats_to_ticks(1.0);
        assert_eq!(step_to_grid(beat * 2, beat, -1), beat);
    }

    #[test]
    fn step_to_grid_backward_mid_grid_rounds_down_to_current_boundary() {
        let beat = beats_to_ticks(1.0);
        assert_eq!(step_to_grid(beat + 10, beat, -1), beat);
    }

    #[test]
    fn px_per_beat_to_ppt_divides_by_one_beat() {
        assert_eq!(px_per_beat_to_ppt(PPQN as f32 * 2.0), 2.0);
        assert_eq!(px_per_beat_to_ppt(47.1), 47.1 / beats_to_ticks(1.0) as f32);
    }

    #[test]
    fn sixteenth_triplet_ticks_is_ppqn_over_6() {
        assert_eq!(sixteenth_triplet_ticks(), PPQN / 6);
    }

    #[test]
    fn ticks_to_beats_f64_keeps_sub_beat_and_does_not_truncate() {
        assert_eq!(ticks_to_beats_f64(PPQN), 1.0);
        assert_eq!(ticks_to_beats_f64(PPQN / 2), 0.5);
        assert_eq!(ticks_to_beats_f64(0), 0.0);
        // 3 beats + a quarter of a beat.
        assert!((ticks_to_beats_f64(PPQN * 3 + PPQN / 4) - 3.25).abs() < 1e-9);
    }

    #[test]
    fn ticks_to_microseconds_one_beat() {
        // Use a tempo where integer division is exact: tempo=480_000, PPQN=960
        // 480_000 / 960 = 500; 500 * 960 = 480_000.
        assert_eq!(ticks_to_microseconds(480_000, PPQN), 480_000);
    }

    #[test]
    fn ticks_to_microseconds_zero_ticks_is_zero() {
        assert_eq!(ticks_to_microseconds(500_000, 0), 0);
    }

    #[test]
    fn tempo_us_to_bpm_120_bpm() {
        let bpm = tempo_us_to_bpm(500_000);
        assert!((bpm - 120.0).abs() < 0.01, "expected 120 bpm, got {bpm}");
    }

    #[test]
    fn tempo_us_to_bpm_60_bpm() {
        let bpm = tempo_us_to_bpm(1_000_000);
        assert!((bpm - 60.0).abs() < 0.01, "expected 60 bpm, got {bpm}");
    }

    #[test]
    fn scaled_tempo_us_equal_ticks_unchanged() {
        assert_eq!(scaled_tempo_us(500_000, 960, 960), Some(500_000));
    }

    #[test]
    fn scaled_tempo_us_double_scaled_ticks_doubles_tempo() {
        assert_eq!(scaled_tempo_us(500_000, 1920, 960), Some(1_000_000));
    }

    #[test]
    fn scaled_tempo_us_half_scaled_ticks_halves_tempo() {
        assert_eq!(scaled_tempo_us(1_000_000, 480, 960), Some(500_000));
    }

    #[test]
    fn scaled_tempo_us_zero_scaled_ticks_returns_none() {
        assert_eq!(scaled_tempo_us(500_000, 0, 960), None);
    }

    #[test]
    fn scaled_tempo_us_zero_reference_ticks_returns_none() {
        assert_eq!(scaled_tempo_us(500_000, 960, 0), None);
    }

    #[test]
    fn tap_tempo_us_returns_none_for_empty_slice() {
        assert_eq!(tap_tempo_us(&[]), None);
    }

    #[test]
    fn tap_tempo_us_returns_none_for_one_tap() {
        let now = Instant::now();
        assert_eq!(tap_tempo_us(&[now]), None);
    }

    #[test]
    fn tap_tempo_us_returns_none_for_two_taps() {
        let now = Instant::now();
        let taps = vec![now - Duration::from_micros(500_000), now];
        assert_eq!(tap_tempo_us(&taps), None);
    }

    #[test]
    fn tap_tempo_us_three_taps_at_120_bpm() {
        let now = Instant::now();
        let taps = vec![
            now - Duration::from_micros(1_000_000),
            now - Duration::from_micros(500_000),
            now,
        ];
        assert_eq!(tap_tempo_us(&taps), Some(500_000));
    }

    #[test]
    fn tap_tempo_us_averages_multiple_intervals() {
        let now = Instant::now();
        let taps = vec![
            now - Duration::from_micros(1_500_000),
            now - Duration::from_micros(1_000_000),
            now - Duration::from_micros(500_000),
            now,
        ];
        assert_eq!(tap_tempo_us(&taps), Some(500_000));
    }

    #[test]
    fn tap_tempo_us_ignores_stale_taps() {
        let now = Instant::now();
        let taps = vec![
            now - Duration::from_secs(5), // stale — older than 2s cutoff
            now - Duration::from_micros(1_000_000),
            now - Duration::from_micros(500_000),
            now,
        ];
        // Stale tap is excluded; remaining 3 taps at 500ms intervals → 120 BPM
        assert_eq!(tap_tempo_us(&taps), Some(500_000));
    }

    #[test]
    fn tap_tempo_us_clamps_to_maximum_bpm() {
        let now = Instant::now();
        // ~10ms intervals → ~6000 BPM, clamps to 300 BPM = 200_000µs
        let taps = vec![
            now - Duration::from_micros(20_000),
            now - Duration::from_micros(10_000),
            now,
        ];
        assert_eq!(tap_tempo_us(&taps), Some(200_000));
    }

    #[test]
    fn bpm_tenths_round_trip_through_tempo_us() {
        assert_eq!(bpm_tenths_to_tempo_us(1200), 500_000);
        // 120.1 BPM is 499_583.68 µs: rounded, and back to 120.1.
        assert_eq!(bpm_tenths_to_tempo_us(1201), 499_584);
        for tenths in [200, 901, 1200, 1335, 2999, 3000] {
            assert_eq!(
                tempo_us_to_bpm_tenths(bpm_tenths_to_tempo_us(tenths)),
                tenths
            );
        }
    }

    #[test]
    fn clamp_tempo_us_keeps_the_app_range() {
        assert_eq!(clamp_tempo_us(100_000), bpm_to_tempo_us(TEMPO_BPM_MAX));
        assert_eq!(clamp_tempo_us(9_000_000), bpm_to_tempo_us(TEMPO_BPM_MIN));
        assert_eq!(clamp_tempo_us(500_000), 500_000);
    }

    #[test]
    fn format_bpm_shows_the_nearest_tenth() {
        assert_eq!(format_bpm(500_000), "120.0");
        assert_eq!(format_bpm(bpm_to_tempo_us(90)), "90.0");
        assert_eq!(format_bpm(bpm_tenths_to_tempo_us(1201)), "120.1");
    }

    #[test]
    fn parse_bpm_tenths_reads_decimals_and_clamps() {
        assert_eq!(parse_bpm_tenths("120"), Some(1200));
        assert_eq!(parse_bpm_tenths(" 98.5 "), Some(985));
        assert_eq!(parse_bpm_tenths("98,5"), Some(985));
        assert_eq!(parse_bpm_tenths("98.46"), Some(985));
        assert_eq!(parse_bpm_tenths("5"), Some(TEMPO_BPM_MIN * 10));
        assert_eq!(parse_bpm_tenths("1000"), Some(TEMPO_BPM_MAX * 10));
        assert_eq!(parse_bpm_tenths(""), None);
        assert_eq!(parse_bpm_tenths("fast"), None);
        assert_eq!(parse_bpm_tenths("inf"), None);
    }

    #[test]
    fn tap_tempo_numbers_each_burst() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut taps = TapTempo::default();
        assert_eq!(taps.tap(at(0)), None);
        assert_eq!(taps.tap(at(500)), None);
        assert_eq!(taps.tap(at(1000)), Some((500_000, 0)));
        assert_eq!(taps.tap(at(1500)), Some((500_000, 0)));
        // A pause starts a new burst, which needs its three taps again.
        assert_eq!(taps.tap(at(4000)), None);
        assert_eq!(taps.tap(at(4250)), None);
        assert_eq!(taps.tap(at(4500)), Some((250_000, 1)));
    }

    /// Tapping on past the kept taps stays in one burst.
    #[test]
    fn tap_tempo_long_burst_keeps_its_number() {
        let start = Instant::now();
        let mut taps = TapTempo::default();
        let mut last = None;
        for i in 0..20 {
            last = taps.tap(start + Duration::from_millis(500 * i));
        }
        assert_eq!(last, Some((500_000, 0)));
    }
}
