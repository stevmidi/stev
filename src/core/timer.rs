//! The periodic firing source behind [`Clock`](crate::core::clock::Clock).
//!
//! Fires a callback roughly every millisecond with the [`Instant`] captured at
//! the top of that firing. `Clock` credits musical time from the *real* elapsed
//! span between firings, so period jitter only affects tick granularity, not
//! tempo. One loop for every platform: each firing's target is an absolute
//! [`Instant`] one period after the last, slept to and then spun for the final
//! [`SPIN_TAIL`], so wake-up delays never add up into a drifting period. See
//! `260-porting.md` § Timing for its measurements, and
//! `archive/140-device-frame-clock.md` for the deferred alternative.

use std::hint::spin_loop;
use std::thread;
use std::time::{Duration, Instant};

use crate::core::thread_priority::{TimingThread, raise_timing_priority};

/// The final stretch before each firing that is spun rather than slept: a
/// `thread::sleep` can overshoot by a few hundred µs. Spun once per period,
/// so it is paid at the clock's whole rate (~10% of a core at 1 ms).
const SPIN_TAIL: Duration = Duration::from_micros(100);

/// The periodic timer behind `Clock`.
pub(crate) struct Timer {
    /// Nominal period between firings.
    period: Duration,
}

impl Timer {
    /// A timer with a nominal period of `period_ms` (at least 1 ms).
    pub(crate) fn new(period_ms: u64) -> Self {
        Self {
            period: Duration::from_millis(period_ms.max(1)),
        }
    }

    /// Starts the `"clock"` thread. `callback` is invoked once per period with
    /// the [`Instant`] captured at the top of that firing — `Clock` uses the
    /// real elapsed time between firings to credit musical time, so the
    /// period's exact accuracy only affects tick granularity, not tempo.
    pub(crate) fn start<F>(&mut self, mut callback: F)
    where
        F: FnMut(Instant) + Send + 'static,
    {
        let period = self.period;

        thread::Builder::new()
            .name("clock".to_string())
            .spawn(move || {
                raise_timing_priority(TimingThread::Clock);
                let mut target = Instant::now();
                loop {
                    callback(Instant::now());
                    target = next_target(target, period, Instant::now());
                    wait_until(target);
                }
            })
            .expect("Failed to spawn clock thread");
    }
}

/// The next firing's target: one `period` after `previous`, on a fixed grid so
/// late wake-ups don't drift the period. A target already past at `now` (the
/// thread stalled for a whole period or more) re-anchors to `now` and fires
/// at once, rather than bursting through every missed period.
fn next_target(previous: Instant, period: Duration, now: Instant) -> Instant {
    let target = previous + period;
    if target > now { target } else { now }
}

/// How to wait at `now` for `target`: `Some` sleep while more than
/// [`SPIN_TAIL`] remains, `None` to spin.
fn sleep_before(target: Instant, now: Instant) -> Option<Duration> {
    let remaining = target.saturating_duration_since(now);
    (remaining > SPIN_TAIL).then(|| remaining - SPIN_TAIL)
}

/// Blocks until `target`: sleeps most of the gap, spins the tail.
fn wait_until(target: Instant) {
    loop {
        let now = Instant::now();
        if now >= target {
            return;
        }
        match sleep_before(target, now) {
            Some(sleep) => thread::sleep(sleep),
            None => spin_loop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_millis(1);

    #[test]
    fn the_next_target_is_one_period_after_the_last_not_after_the_wake_up() {
        let previous = Instant::now();
        // Woke 300 µs late: the grid holds, so the next gap is 700 µs.
        let now = previous + Duration::from_micros(300);
        assert_eq!(next_target(previous, PERIOD, now), previous + PERIOD);
    }

    #[test]
    fn a_missed_period_fires_at_once_and_re_anchors() {
        let previous = Instant::now();
        let now = previous + Duration::from_micros(3_500);
        assert_eq!(next_target(previous, PERIOD, now), now);
    }

    #[test]
    fn a_target_exactly_due_fires_at_once() {
        let previous = Instant::now();
        let now = previous + PERIOD;
        assert_eq!(next_target(previous, PERIOD, now), now);
    }

    #[test]
    fn far_from_the_target_it_sleeps_to_the_spin_tail() {
        let now = Instant::now();
        assert_eq!(sleep_before(now + PERIOD, now), Some(PERIOD - SPIN_TAIL));
    }

    #[test]
    fn within_the_spin_tail_or_past_the_target_it_spins() {
        let now = Instant::now();
        assert_eq!(sleep_before(now + SPIN_TAIL, now), None);
        assert_eq!(sleep_before(now, now + PERIOD), None);
    }

    #[test]
    fn a_zero_period_is_raised_to_one_millisecond() {
        assert_eq!(Timer::new(0).period, PERIOD);
    }
}
