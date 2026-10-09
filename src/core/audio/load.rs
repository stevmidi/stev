//! Audio-callback load metering — the number a DAW's "CPU" meter actually
//! shows.
//!
//! Process CPU percent is the wrong measure for audio: it is a throughput
//! figure spread over every core, while the audio callback is a *deadline*
//! problem on one thread. A block of `frames` samples at `sample_rate` must be
//! produced within `frames / sample_rate` seconds — 5.33 ms at 256 frames /
//! 48 kHz — no matter how many cores are idle. [`AudioLoad`] records the share
//! of that budget each callback consumed: `0.5` is half the budget spent, `1.0`
//! is exactly on the edge, and anything at or above `1.0` is a dropout.
//!
//! Written once per callback from the audio thread (two relaxed stores, no
//! allocation, no lock), read once per frame by the header readout
//! (`Display::draw_header`).
//!
//! Alongside the render-side numbers it also latches the *backend's* verdict:
//! `xruns` counts the device's own overload notifications (`cpal`'s
//! `ErrorKind::Xrun`, CoreAudio's `kAudioDeviceProcessorOverload`). The two
//! counters cover different ground and are meant to be read together — see
//! [`AudioLoad::xruns`].
//!
//! The whole-block figure says *that* a block ran over; the per-track slots
//! ([`AudioLoad::record_track`]) say *who*. The instrument mixer times each
//! voice's render and records it here, so an overrun can be attributed to the
//! plugin that ate the budget — the overrun journal (`journal.rs`) reads these
//! for its post-mortem line.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::core::config::MAX_TRACKS;

/// Weight given to the newest block in the rolling average. At ~187 callbacks
/// per second (256 frames / 48 kHz) this settles within about a quarter of a
/// second — smooth enough to read without flicker, quick enough to show a
/// plugin being loaded.
const AVERAGE_ALPHA: f32 = 0.02;

/// Per-block decay applied to the peak hold before the new block is folded in.
/// `0.996^187 ≈ 0.47`, so a spike falls to half in roughly a second: long
/// enough to catch by eye, short enough that it doesn't stick. The decay is
/// per *block*, so its wall-clock rate follows the buffer size — acceptable for
/// a meter, and the alternative (timing the decay) would cost a clock read.
const PEAK_DECAY: f32 = 0.996;

/// Rolling average, decaying peak, and latched overrun count for the audio
/// callback's deadline utilisation. The two utilisation figures are `f32` bit
/// patterns in a single lock-free atomic each, following the
/// [`TrackMixAtomics`](crate::core::shared_atomics::TrackMixAtomics) precedent.
pub(crate) struct AudioLoad {
    /// Rolling-average deadline utilisation, as an `f32` bit pattern.
    average_bits: AtomicU32,
    /// Decaying peak deadline utilisation, as an `f32` bit pattern.
    peak_bits: AtomicU32,
    /// Blocks that reached or passed their deadline since the last reset.
    ///
    /// Latched rather than decayed, because an overrun is a *rare transient*:
    /// one block out of ~187 a second, long gone from the peak hold by the time
    /// anyone looks at the meter. A count that sits there until cleared is the
    /// only way to notice a single glitch in a ten-minute take.
    overruns: AtomicU32,
    /// Overload notifications from the audio backend since the last reset —
    /// the device's own "the IO cycle missed its deadline", as opposed to
    /// `overruns`, which is this code timing its own render. Latched for the
    /// same reason.
    xruns: AtomicU32,
    /// The most recent block's render time per track, in microseconds,
    /// indexed by track.
    tracks_us: [AtomicU32; MAX_TRACKS],
}

/// A render time, in whole microseconds, saturating at `u32::MAX` (~71
/// minutes — any real block is milliseconds). The unit both the per-track
/// slots and the overrun journal keep.
pub(crate) fn micros(d: Duration) -> u32 {
    d.as_micros().min(u128::from(u32::MAX)) as u32
}

impl AudioLoad {
    /// A zeroed meter.
    pub(crate) fn new() -> Self {
        Self {
            average_bits: AtomicU32::new(0),
            peak_bits: AtomicU32::new(0),
            overruns: AtomicU32::new(0),
            xruns: AtomicU32::new(0),
            tracks_us: [const { AtomicU32::new(0) }; MAX_TRACKS],
        }
    }

    /// Share of the block budget that `elapsed` represents, or `None` for a
    /// degenerate block (no frames, no rate, non-finite result).
    fn utilisation(elapsed: Duration, frames: usize, sample_rate: f64) -> Option<f32> {
        if frames == 0 || sample_rate <= 0.0 {
            return None;
        }
        let budget = frames as f64 / sample_rate;
        let load = (elapsed.as_secs_f64() / budget) as f32;
        load.is_finite().then_some(load)
    }

    /// (audio thread) Folds in one callback: `elapsed` spent rendering `frames`
    /// samples at `sample_rate`. Returns whether this block was an overrun, so
    /// the caller can journal it without re-deriving the threshold.
    ///
    /// The audio callback is the only writer, so the load/store pairs here are
    /// a sound read-modify-write — no other thread ever stores to these.
    pub(crate) fn record(&self, elapsed: Duration, frames: usize, sample_rate: f64) -> bool {
        let Some(load) = Self::utilisation(elapsed, frames, sample_rate) else {
            return false;
        };

        let average = f32::from_bits(self.average_bits.load(Ordering::Relaxed));
        let average = average + (load - average) * AVERAGE_ALPHA;
        self.average_bits
            .store(average.to_bits(), Ordering::Relaxed);

        let peak = f32::from_bits(self.peak_bits.load(Ordering::Relaxed)) * PEAK_DECAY;
        self.peak_bits
            .store(peak.max(load).to_bits(), Ordering::Relaxed);

        // At 1.0 the callback used its whole period, so the next one is already
        // late and the device has nothing to play — CoreAudio's IOProc has no
        // queue to absorb it. That is a dropout, not a near miss.
        let overran = load >= 1.0;
        if overran {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        overran
    }

    /// (audio thread) Attributes `elapsed` of this block's render to `track`
    /// — a voice that didn't render records `Duration::ZERO`, so every slot is
    /// fresh each block. Out-of-range tracks are ignored.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn record_track(&self, track: usize, elapsed: Duration) {
        if let Some(slot) = self.tracks_us.get(track) {
            slot.store(micros(elapsed), Ordering::Relaxed);
        }
    }

    /// The most recent block's render time for `track`, in microseconds —
    /// `0` for a silent or empty track, or one out of range. Read by the
    /// overrun journal on the audio thread right after the block's sources
    /// have all rendered.
    pub(crate) fn track_last_us(&self, track: usize) -> u32 {
        self.tracks_us
            .get(track)
            .map_or(0, |slot| slot.load(Ordering::Relaxed))
    }

    /// (UI thread) Smoothed deadline utilisation, `0.0` = idle, `1.0` = the
    /// whole budget. Can exceed `1.0` — that is an overrun, not a clamp bug.
    pub(crate) fn average(&self) -> f32 {
        f32::from_bits(self.average_bits.load(Ordering::Relaxed))
    }

    /// (UI thread) Decaying peak utilisation — the number that catches a single
    /// expensive block the average would smooth away.
    pub(crate) fn peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed))
    }

    /// (backend notification thread) Folds in one overload report from the
    /// audio backend. A plain increment: it may be called from the real-time
    /// thread (CoreAudio delivers `kAudioDeviceProcessorOverload` there), so no
    /// logging, no allocation.
    pub(crate) fn record_xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// (UI thread) Blocks that missed their deadline since the last reset —
    /// i.e. audible glitches. Stays put until cleared, unlike [`peak`](Self::peak).
    pub(crate) fn overruns(&self) -> u32 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// (UI thread) Backend overload notifications since the last reset. Read
    /// next to [`overruns`](Self::overruns) — the pair tells three stories:
    ///
    /// - **both** rose: this code's render ran over and the device heard it;
    /// - **`overruns` only**: the render ran over *its* budget but the
    ///   device's safety offset absorbed it — no audible glitch (the budget
    ///   here is deliberately stricter than the hardware's real deadline);
    /// - **`xruns` only**: the glitch came from *outside* the render — the IO
    ///   thread was pre-empted, a format change, another app on the device —
    ///   the case the render-side timer alone can never see.
    pub(crate) fn xruns(&self) -> u32 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// (UI thread) Clears both latched counts, so they read "this take" rather
    /// than "this session". Racing an increment can lose at most one count,
    /// which does not matter for a diagnostic.
    pub(crate) fn reset_counts(&self) {
        self.overruns.store(0, Ordering::Relaxed);
        self.xruns.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One block of 256 frames at 48 kHz — a 5.333 ms budget.
    const FRAMES: usize = 256;
    const RATE: f64 = 48_000.0;

    fn budget() -> Duration {
        Duration::from_secs_f64(FRAMES as f64 / RATE)
    }

    #[test]
    fn a_fresh_meter_reads_zero() {
        let load = AudioLoad::new();
        assert_eq!(load.average(), 0.0);
        assert_eq!(load.peak(), 0.0);
    }

    #[test]
    fn a_callback_using_half_its_budget_averages_to_half() {
        let load = AudioLoad::new();
        for _ in 0..1000 {
            load.record(budget() / 2, FRAMES, RATE);
        }
        assert!(
            (load.average() - 0.5).abs() < 0.01,
            "average was {}",
            load.average()
        );
    }

    #[test]
    fn an_overrun_reads_above_one() {
        let load = AudioLoad::new();
        load.record(budget() * 2, FRAMES, RATE);
        assert!(load.peak() > 1.0, "peak was {}", load.peak());
    }

    #[test]
    fn the_peak_catches_a_spike_the_average_smooths_away() {
        let load = AudioLoad::new();
        load.record(budget(), FRAMES, RATE);
        for _ in 0..50 {
            load.record(Duration::ZERO, FRAMES, RATE);
        }
        // The average has all but forgotten the spike; the peak has not.
        assert!(load.average() < 0.1, "average was {}", load.average());
        assert!(load.peak() > 0.5, "peak was {}", load.peak());
    }

    #[test]
    fn the_peak_decays_back_towards_zero() {
        let load = AudioLoad::new();
        load.record(budget(), FRAMES, RATE);
        let spike = load.peak();
        for _ in 0..1000 {
            load.record(Duration::ZERO, FRAMES, RATE);
        }
        assert!(load.peak() < spike * 0.05, "peak was {}", load.peak());
    }

    #[test]
    fn a_block_that_misses_its_deadline_is_counted() {
        let load = AudioLoad::new();
        load.record(budget() * 2, FRAMES, RATE);
        assert_eq!(load.overruns(), 1);
    }

    /// Exactly on the deadline is already too late — the next callback is due.
    /// Uses 480 frames rather than [`FRAMES`], because 480 at 48 kHz is exactly
    /// 10 ms and so round-trips through `Duration`'s nanosecond resolution;
    /// 256 frames is 5333333.33 ns and lands a hair under 1.0.
    #[test]
    fn exactly_the_whole_budget_counts_as_an_overrun() {
        let load = AudioLoad::new();
        load.record(Duration::from_millis(10), 480, RATE);
        assert_eq!(load.overruns(), 1);
    }

    #[test]
    fn blocks_inside_the_deadline_are_not_counted() {
        let load = AudioLoad::new();
        for _ in 0..500 {
            load.record(budget() / 2, FRAMES, RATE);
        }
        assert_eq!(load.overruns(), 0);
    }

    /// The point of latching: a single glitch is still visible long after the
    /// peak hold has decayed away.
    #[test]
    fn the_count_survives_the_peak_decaying_away() {
        let load = AudioLoad::new();
        load.record(budget() * 2, FRAMES, RATE);
        for _ in 0..2000 {
            load.record(Duration::ZERO, FRAMES, RATE);
        }
        assert!(load.peak() < 0.01, "peak was {}", load.peak());
        assert_eq!(load.overruns(), 1);
    }

    #[test]
    fn resetting_clears_the_count_and_counting_resumes() {
        let load = AudioLoad::new();
        load.record(budget() * 2, FRAMES, RATE);
        load.record(budget() * 2, FRAMES, RATE);
        assert_eq!(load.overruns(), 2);

        load.reset_counts();
        assert_eq!(load.overruns(), 0);

        load.record(budget() * 2, FRAMES, RATE);
        assert_eq!(load.overruns(), 1);
    }

    #[test]
    fn a_fresh_meter_has_no_xruns() {
        assert_eq!(AudioLoad::new().xruns(), 0);
    }

    #[test]
    fn backend_xruns_are_counted_independently_of_overruns() {
        let load = AudioLoad::new();
        load.record_xrun();
        load.record_xrun();
        assert_eq!(load.xruns(), 2);
        // The backend saying "overload" doesn't imply *this* render ran over.
        assert_eq!(load.overruns(), 0);
    }

    /// The "outside the render" story: blocks well inside the deadline, yet
    /// the device reports an overload — the counter must show it.
    #[test]
    fn an_xrun_with_no_overrun_is_still_latched() {
        let load = AudioLoad::new();
        for _ in 0..500 {
            load.record(budget() / 4, FRAMES, RATE);
        }
        load.record_xrun();
        assert_eq!(load.overruns(), 0);
        assert_eq!(load.xruns(), 1);
    }

    #[test]
    fn resetting_clears_xruns_too() {
        let load = AudioLoad::new();
        load.record(budget() * 2, FRAMES, RATE);
        load.record_xrun();
        load.reset_counts();
        assert_eq!(load.overruns(), 0);
        assert_eq!(load.xruns(), 0);
    }

    #[test]
    fn record_reports_the_overrun_verdict() {
        let load = AudioLoad::new();
        assert!(!load.record(budget() / 2, FRAMES, RATE));
        assert!(load.record(budget() * 2, FRAMES, RATE));
        assert!(
            !load.record(budget(), 0, RATE),
            "degenerate block is not an overrun"
        );
    }

    #[test]
    fn a_track_render_is_attributed_to_its_slot_only() {
        let load = AudioLoad::new();
        load.record_track(2, Duration::from_micros(2_500));
        assert_eq!(load.track_last_us(2), 2_500);
        assert_eq!(load.track_last_us(0), 0);
        assert_eq!(load.track_last_us(1), 0);
    }

    #[test]
    fn a_silent_track_overwrites_its_last_block_figure() {
        let load = AudioLoad::new();
        load.record_track(0, budget());
        load.record_track(0, Duration::ZERO);
        assert_eq!(load.track_last_us(0), 0);
    }

    #[test]
    fn an_out_of_range_track_is_ignored() {
        let load = AudioLoad::new();
        load.record_track(MAX_TRACKS + 5, budget());
        assert_eq!(load.track_last_us(MAX_TRACKS + 5), 0);
    }

    #[test]
    fn micros_saturates_instead_of_wrapping() {
        assert_eq!(micros(Duration::from_micros(1_234)), 1_234);
        assert_eq!(micros(Duration::from_secs(u64::MAX)), u32::MAX);
    }

    #[test]
    fn a_zero_frame_callback_is_ignored() {
        let load = AudioLoad::new();
        load.record(budget(), 0, RATE);
        assert_eq!(load.average(), 0.0);
        assert_eq!(load.peak(), 0.0);
        assert_eq!(load.overruns(), 0);
        assert_eq!(load.xruns(), 0);
    }
}
