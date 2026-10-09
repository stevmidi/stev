//! Sub-tick timestamping for live MIDI input.
//!
//! Both musical counters ([`clock_tick`](crate::core::clock) and the transport
//! odometer) only advance on the ~1 ms `"clock"` firing, so a note arriving
//! between firings would be recorded up to ~2 ticks early. These functions
//! recover the fractional tick: calibrate midir's unknown timestamp epoch
//! against [`time::monotonic_nanos`], measure how long ago the counters last
//! stepped, and add that many ticks to *both* bases (they step at the same
//! instant, so one correction serves both). See `150-clock-position-sync.md`.
//!
//! [`precise_input_tick`] is the entry point, called once per message from the
//! `midir` callback in `input.rs`.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use crate::core::time::{self, PPQN};

use super::input::InputTicks;

/// Upper bound on the fractional-tick correction [`interpolate_input_tick`] adds
/// — a note can arrive at most one clock period (plus jitter) after `clock_tick`
/// last stepped, so anything larger means the `"clock"` thread stalled and the
/// counter is stale too; cap rather than inject a wild offset.
const MAX_INPUT_INTERP_NANOS: u64 = 2_000_000;

/// Best estimate of the two tick coordinates a live-input message landed on —
/// see [`InputTicks`] for why there are two. Both counters only step on the
/// ~1 ms `"clock"` firing, so a note arriving between firings would be recorded
/// up to ~2 ticks early; the same sub-tick correction refines both, which is
/// valid because they step at the same instant (`Clock::start`).
///
/// `midir_ts_us` is midir's per-connection microsecond stamp (`0` when the
/// backend gave none). `min_offset_ns` is the caller-owned running calibration
/// of midir's unknown epoch against [`time::monotonic_nanos`] — see the comment
/// at its definition. When no usable stamp is available the callback's own
/// monotonic time is used instead (loses only the sub-tick dispatch latency).
pub(super) fn precise_input_tick(
    clock_tick: &AtomicI32,
    clock_tick_instant_nanos: &AtomicU64,
    elapsed_ticks: &AtomicI32,
    tempo: &AtomicI32,
    midir_ts_us: u64,
    min_offset_ns: &mut i64,
) -> InputTicks {
    let now_ns = time::monotonic_nanos();

    let arrival_ns = if midir_ts_us == 0 {
        now_ns
    } else {
        calibrated_arrival_ns(now_ns, midir_ts_us as i64 * 1_000, min_offset_ns)
    };

    let last_tick_ns = clock_tick_instant_nanos.load(Ordering::Relaxed);
    // Two separate loads: a clock firing landing between them leaves one of the
    // bases a single tick behind the other. That is smaller than the correction
    // being applied below and far smaller than the ~1 ms the counters step in,
    // so it is not worth a lock on this callback.
    let elapsed_base = elapsed_ticks.load(Ordering::Relaxed);
    let position_base = clock_tick.load(Ordering::Relaxed);
    let tempo_us = tempo.load(Ordering::Relaxed);

    input_ticks_at(
        position_base,
        elapsed_base,
        tempo_us,
        arrival_ns.saturating_sub(last_tick_ns),
    )
}

/// Applies one sub-tick correction to both counter bases. They step at the same
/// instant (`Clock::start`), so `since_last_tick_ns` is the age of both — but
/// each must be interpolated from its *own* base, since the two sit whole loop
/// lengths apart and mean different things. See [`InputTicks`].
fn input_ticks_at(
    position_base: i32,
    elapsed_base: i32,
    tempo_us: i32,
    since_last_tick_ns: u64,
) -> InputTicks {
    InputTicks {
        position: interpolate_input_tick(position_base, tempo_us, since_last_tick_ns),
        elapsed: interpolate_input_tick(elapsed_base, tempo_us, since_last_tick_ns),
    }
}

/// Maps a message midir stamped `stamp_ns` into its own (unknown, fixed) epoch
/// onto the [`time::monotonic_nanos`] timeline, given `now_ns` observed when our
/// callback ran. `min_offset_ns` accumulates the smallest `now_ns - stamp_ns`
/// ever seen: since callback dispatch latency is always ≥ 0, that minimum is the
/// best estimate of the true epoch offset, and it lets a message processed late
/// (a burst after a scheduler hiccup) still resolve to its real arrival time.
fn calibrated_arrival_ns(now_ns: u64, stamp_ns: i64, min_offset_ns: &mut i64) -> u64 {
    let observed = now_ns as i64 - stamp_ns;
    *min_offset_ns = (*min_offset_ns).min(observed);
    (stamp_ns + *min_offset_ns).max(0) as u64
}

/// Rounds `base_tick` plus `elapsed_ns` worth of ticks to the nearest whole
/// tick. The correction is capped at [`MAX_INPUT_INTERP_NANOS`] so a stalled
/// clock thread (which also freezes `base_tick`) can't inject a wild offset.
fn interpolate_input_tick(base_tick: i32, tempo_us: i32, elapsed_ns: u64) -> i32 {
    if tempo_us <= 0 {
        return base_tick;
    }
    let ns_per_tick = tempo_us as u64 * 1_000 / PPQN as u64;
    if ns_per_tick == 0 {
        return base_tick;
    }
    let elapsed_ns = elapsed_ns.min(MAX_INPUT_INTERP_NANOS);
    let sub_ticks = ((elapsed_ns + ns_per_tick / 2) / ns_per_tick) as i32;
    base_tick.saturating_add(sub_ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 120 BPM: 500_000 µs/quarter ⇒ ns_per_tick = 500_000 * 1000 / 960 = 520_833.
    const TEMPO_120: i32 = 500_000;
    const NS_PER_TICK_120: u64 = 520_833;

    #[test]
    fn interpolate_input_tick_no_elapsed_returns_base() {
        assert_eq!(interpolate_input_tick(1000, TEMPO_120, 0), 1000);
    }

    #[test]
    fn interpolate_input_tick_rounds_to_nearest_tick() {
        // ~0.4 ticks elapsed → rounds down.
        assert_eq!(
            interpolate_input_tick(1000, TEMPO_120, (NS_PER_TICK_120 * 2) / 5),
            1000
        );
        // ~0.6 ticks elapsed → rounds up.
        assert_eq!(
            interpolate_input_tick(1000, TEMPO_120, (NS_PER_TICK_120 * 3) / 5),
            1001
        );
        // Exactly 2 ticks.
        assert_eq!(
            interpolate_input_tick(1000, TEMPO_120, NS_PER_TICK_120 * 2),
            1002
        );
    }

    #[test]
    fn input_ticks_at_applies_the_same_correction_to_both_bases() {
        // The two counters step at the same instant but sit whole loop lengths
        // apart, so each must be interpolated from its *own* base by the *same*
        // sub-tick amount. Deriving one from the other would fold a
        // repositioned position into the odometer, which is the corruption this
        // phase exists to remove.
        const POSITION_BASE: i32 = 40_000;
        const ELAPSED_BASE: i32 = 137;

        let got = input_ticks_at(POSITION_BASE, ELAPSED_BASE, TEMPO_120, NS_PER_TICK_120 * 2);

        assert_eq!(got.position, POSITION_BASE + 2);
        assert_eq!(got.elapsed, ELAPSED_BASE + 2);
        assert_eq!(
            got.position - POSITION_BASE,
            got.elapsed - ELAPSED_BASE,
            "both bases must move by the same sub-tick correction"
        );
    }

    #[test]
    fn input_ticks_at_anchors_each_base_independently() {
        // No elapsed time means no correction; each coordinate must then come
        // back as exactly its own counter, not the other one.
        let got = input_ticks_at(40_000, 137, TEMPO_120, 0);

        assert_eq!(got.position, 40_000);
        assert_eq!(got.elapsed, 137);
    }

    #[test]
    fn interpolate_input_tick_caps_a_wild_elapsed() {
        // A 1-second gap (clock thread stalled) must not add ~1900 ticks.
        let got = interpolate_input_tick(1000, TEMPO_120, 1_000_000_000);
        assert!(
            got - 1000 <= 4,
            "correction {} should be capped",
            got - 1000
        );
    }

    #[test]
    fn interpolate_input_tick_nonpositive_tempo_returns_base() {
        assert_eq!(interpolate_input_tick(1000, 0, NS_PER_TICK_120 * 3), 1000);
    }

    #[test]
    fn calibrated_arrival_recovers_arrival_time_of_a_late_burst() {
        // midir's epoch sits 500 µs below our timeline (stamp = arrival - 500_000).
        let mut off = i64::MAX;

        // First two messages arrive promptly: callback time == true arrival.
        assert_eq!(
            calibrated_arrival_ns(1_000_000, 500_000, &mut off),
            1_000_000
        );
        assert_eq!(
            calibrated_arrival_ns(2_000_000, 1_500_000, &mut off),
            2_000_000
        );

        // Third message truly arrived at 4_000_000 (stamp 3_500_000) but the
        // callback only ran 3 ms later — its arrival is still recovered, not
        // collapsed onto "now".
        assert_eq!(
            calibrated_arrival_ns(7_000_000, 3_500_000, &mut off),
            4_000_000
        );
    }

    #[test]
    fn calibrated_arrival_tightens_toward_the_true_offset() {
        let mut off = i64::MAX;
        // First observation carries 900 µs of dispatch latency — the estimate
        // is loose, arrival == callback time.
        assert_eq!(
            calibrated_arrival_ns(1_900_000, 1_000_000, &mut off),
            1_900_000
        );
        // A low-latency observation (50 µs) tightens the offset.
        assert_eq!(
            calibrated_arrival_ns(2_050_000, 2_000_000, &mut off),
            2_050_000
        );
        // Now a message with 200 µs latency resolves ~150 µs before the
        // callback ran, instead of being pinned to "now".
        assert_eq!(
            calibrated_arrival_ns(3_200_000, 3_000_000, &mut off),
            3_050_000
        );
    }
}
