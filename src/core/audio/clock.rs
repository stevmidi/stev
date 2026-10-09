//! Wall-clock → output-sample mapping shared by every audio source in the
//! engine.

use std::time::{Duration, Instant};

/// Maps a wall-clock [`Instant`] onto the engine's monotonic `steady` sample
/// counter. Anchored on the first callback, then nudged by a small fraction of
/// the observed error each callback — so it absorbs callback-invocation jitter
/// (which would otherwise modulate every event's placement) and tracks slow
/// drift between the audio device clock and the system clock without
/// accumulating error. Hard re-anchors on a large error (first callback, device
/// restart, a stall).
pub(crate) struct AudioClock {
    /// Output sample rate, Hz.
    sample_rate: f64,
    /// The `Instant` that maps to frame 0. `None` until the first observation.
    anchor: Option<Instant>,
}

impl AudioClock {
    /// Fraction of the observed error folded into the anchor each callback — a
    /// one-pole IIR on the anchor. At a typical 256-frame / 48 kHz callback
    /// (~5.3 ms) the smoothed value settles to ~1/e of a step in `1/ALPHA` ≈ 20
    /// callbacks (~0.1 s): slow enough that per-callback scheduling jitter (a
    /// few hundred µs, mean zero) barely moves the mapping, fast enough to
    /// follow the ppm-level device-vs-system clock skew that actually matters.
    /// Raising it tracks drift faster but lets jitter through; lowering it does
    /// the reverse.
    const ALPHA: f64 = 0.05;
    /// Error past which the anchor is snapped straight to the observation
    /// instead of nudged. Comfortably above any plausible callback jitter or
    /// short scheduler stall, so a genuine discontinuity — first callback,
    /// device restart, a debugger pause — re-anchors in one step rather than
    /// crawling `ALPHA` at a time.
    const RESYNC_SECS: f64 = 0.050;

    /// An unanchored clock at `sample_rate` — the first
    /// [`observe`](Self::observe) sets the anchor.
    pub(crate) fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            anchor: None,
        }
    }

    /// Call once per callback: `now` observed at callback entry, `steady` the
    /// frame index of the callback's first sample.
    pub(crate) fn observe(&mut self, now: Instant, steady: u64) {
        let steady_secs = steady as f64 / self.sample_rate;
        let Some(anchor) = self.anchor else {
            self.anchor = Some(instant_shift(now, -steady_secs));
            return;
        };
        let predicted = instant_shift(anchor, steady_secs);
        let err = signed_secs(predicted, now);
        if err.abs() > Self::RESYNC_SECS {
            self.anchor = Some(instant_shift(now, -steady_secs));
        } else {
            self.anchor = Some(instant_shift(anchor, err * Self::ALPHA));
        }
    }

    /// Frame index (in the `steady` timeline) that `at` corresponds to. May be
    /// negative if `at` is behind the anchor — callers clamp.
    pub(crate) fn frame_for(&self, at: Instant) -> i64 {
        let Some(anchor) = self.anchor else {
            return 0;
        };
        (signed_secs(anchor, at) * self.sample_rate).round() as i64
    }
}

/// `to - from` in seconds, signed (std `Instant` subtraction saturates at 0).
fn signed_secs(from: Instant, to: Instant) -> f64 {
    if to >= from {
        (to - from).as_secs_f64()
    } else {
        -(from - to).as_secs_f64()
    }
}

/// `base` shifted by `secs` (may be negative). `Duration::from_secs_f64` panics
/// on a negative argument, so the sign is handled here.
fn instant_shift(base: Instant, secs: f64) -> Instant {
    if secs >= 0.0 {
        base + Duration::from_secs_f64(secs)
    } else {
        base - Duration::from_secs_f64(-secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_clock_converges_on_a_steady_offset_under_jitter() {
        let sr = 48_000.0;
        let mut clock = AudioClock::new(sr);
        let origin = Instant::now();
        // The device advances 256 frames per callback; the callback `Instant`
        // is the true time plus a bounded jitter that must not leak into the
        // mapping once converged.
        let per_cb = Duration::from_secs_f64(256.0 / sr);
        let jitter = [0.0006, -0.0004, 0.0009, -0.0007, 0.0002, -0.0005];
        let mut steady = 0u64;
        for i in 0..400 {
            let j = jitter[i % jitter.len()];
            let now = instant_shift(origin + per_cb * i as u32, j);
            clock.observe(now, steady);
            steady += 256;
        }
        // A note "intended for now" at the next callback boundary maps to ~that
        // callback's first frame, within a jitter's worth of samples.
        let next = origin + per_cb * 400;
        let frame = clock.frame_for(next);
        assert!(
            (frame - steady as i64).abs() < 64,
            "frame {frame} vs steady {steady}"
        );
    }

    #[test]
    fn audio_clock_hard_reanchors_after_a_large_jump() {
        let sr = 48_000.0;
        let mut clock = AudioClock::new(sr);
        let origin = Instant::now();
        clock.observe(origin, 0);
        // Device kept running (steady advanced) but wall clock jumped forward a
        // second — must snap, not crawl.
        clock.observe(origin + Duration::from_secs(1), 480);
        let frame = clock.frame_for(origin + Duration::from_secs(1));
        assert!((frame - 480).abs() < 64, "frame {frame}");
    }

    #[test]
    fn audio_clock_absorbs_a_single_late_callback_instead_of_chasing_it() {
        let sr = 48_000.0;
        let mut clock = AudioClock::new(sr);
        let origin = Instant::now();
        let per_cb = Duration::from_secs_f64(256.0 / sr);

        let mut steady = 0u64;
        for i in 0..200 {
            clock.observe(origin + per_cb * i as u32, steady);
            steady += 256;
        }

        let target = origin + per_cb * 400;
        let before = clock.frame_for(target);
        // One callback lands 5 ms late — well under `RESYNC_SECS`, so it must be
        // smoothed, not snapped.
        clock.observe(instant_shift(origin + per_cb * 200, 0.005), steady);
        let shift = (clock.frame_for(target) - before).abs();
        // ~`ALPHA` (0.05) of 5 ms ≈ 0.25 ms ≈ 12 samples — an order of magnitude
        // under the 5 ms (240-sample) outlier itself.
        assert!(
            shift > 0 && shift < (0.001 * sr) as i64,
            "shift {shift} samples"
        );
    }

    #[test]
    fn audio_clock_follows_slow_device_clock_drift() {
        let sr = 48_000.0;
        let mut clock = AudioClock::new(sr);
        let origin = Instant::now();
        // Device sample clock runs 200 ppm fast: it reports 256 new frames every
        // callback, but only `(256/sr) * (1 - 200e-6)` of real time elapsed.
        let real_per_cb = Duration::from_secs_f64((256.0 / sr) * (1.0 - 200e-6));

        let mut steady = 0u64;
        for i in 0..1000 {
            clock.observe(origin + real_per_cb * i as u32, steady);
            steady += 256;
        }

        // A note "for now" still maps to ~the current frame: the anchor crept
        // along with the drift instead of accumulating 1000 callbacks of error
        // (~50 frames un-tracked).
        let now = origin + real_per_cb * 1000;
        let frame = clock.frame_for(now);
        assert!(
            (frame - steady as i64).abs() < 64,
            "frame {frame} vs steady {steady}"
        );
    }

    #[test]
    fn signed_secs_is_negative_when_target_precedes_origin() {
        let t = Instant::now();
        assert!(signed_secs(t + Duration::from_millis(5), t) < 0.0);
        assert!(signed_secs(t, t + Duration::from_millis(5)) > 0.0);
    }
}
