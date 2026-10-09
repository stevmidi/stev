//! The periodic firing source behind [`Clock`](crate::core::clock::Clock).
//!
//! Fires a callback roughly every millisecond with the [`Instant`] captured at
//! the top of that firing. `Clock` credits musical time from the *real* elapsed
//! span between firings, so period jitter only affects tick granularity, not
//! tempo. macOS uses a `mach_absolute_time` spin-tail (`MachWaitUntilTimer`)
//! for tighter timing; Linux and Windows a plain sleep loop (`SleepTimer`). See
//! `archive/140-device-frame-clock.md` for the deferred alternative.

use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use mach2::mach_time::{mach_absolute_time, mach_timebase_info};

/// The platform's periodic timer, behind one interface.
pub(crate) enum Timer {
    /// macOS: `mach_absolute_time` with a spin-wait tail.
    #[cfg(target_os = "macos")]
    Mach(MachWaitUntilTimer),
    /// Linux / Windows: `thread::sleep` loop.
    #[cfg(not(target_os = "macos"))]
    Sleep(SleepTimer),
}

impl Timer {
    /// The platform's timer with a nominal period of `period_ms`.
    pub(crate) fn new(period_ms: u64) -> Self {
        #[cfg(target_os = "macos")]
        return Timer::Mach(MachWaitUntilTimer::new(period_ms));
        #[cfg(not(target_os = "macos"))]
        return Timer::Sleep(SleepTimer::new(period_ms));
    }

    /// Starts the timer thread. `callback` is invoked once per period with the
    /// [`Instant`] captured at the top of that firing — `Clock` uses the real
    /// elapsed time between firings to credit musical time, so the period's
    /// exact accuracy only affects tick granularity, not tempo.
    pub(crate) fn start<F>(&mut self, callback: F)
    where
        F: FnMut(Instant) + Send + 'static,
    {
        match self {
            #[cfg(target_os = "macos")]
            Timer::Mach(timer) => timer.start(callback),
            #[cfg(not(target_os = "macos"))]
            Timer::Sleep(timer) => timer.start(callback),
        }
    }
}

/// Linux / Windows periodic timer: a thread that calls the callback then sleeps
/// `period_ms`.
#[cfg(not(target_os = "macos"))]
pub(crate) struct SleepTimer {
    /// Nominal period between firings, in milliseconds.
    period_ms: u64,
}

#[cfg(not(target_os = "macos"))]
impl SleepTimer {
    /// A timer with the given nominal period.
    fn new(period_ms: u64) -> Self {
        Self { period_ms }
    }

    /// Spawns the `"clock"` thread and runs the sleep loop, calling `callback`
    /// once per period.
    fn start<F>(&mut self, mut callback: F)
    where
        F: FnMut(Instant) + Send + 'static,
    {
        let period = Duration::from_millis(self.period_ms.max(1));

        thread::Builder::new()
            .name("clock".to_string())
            .spawn(move || {
                loop {
                    callback(Instant::now());
                    thread::sleep(period);
                }
            })
            .expect("Failed to spawn clock thread");
    }
}

/// macOS periodic timer: a thread that waits on `mach_absolute_time` targets,
/// sleeping most of each gap and spinning the last ~100 µs for a tight period.
#[cfg(target_os = "macos")]
pub(crate) struct MachWaitUntilTimer {
    /// Nominal period between firings, in milliseconds.
    period_ms: u64,
}

#[cfg(target_os = "macos")]
impl MachWaitUntilTimer {
    /// A timer with the given nominal period.
    fn new(period_ms: u64) -> Self {
        Self { period_ms }
    }

    /// Converts a nanosecond duration into mach-absolute-time units using the
    /// timebase ratio `numer/denom` (`time_ns = mach * numer / denom`, so
    /// `mach = ns * denom / numer`). Done in one rational step to avoid the
    /// truncation of computing `numer/denom` first — on Apple Silicon that
    /// ratio is 125/3 and truncating to 41 stretched every "1 ms" period to
    /// ~1.016 ms.
    fn nanos_to_mach(period_ns: u64, numer: u32, denom: u32) -> u64 {
        if numer == 0 {
            return period_ns;
        }
        ((period_ns as u128 * denom as u128) / numer as u128) as u64
    }

    /// Spawns the `"clock"` thread and runs the wait-until loop, calling
    /// `callback` once per period.
    fn start<F>(&mut self, mut callback: F)
    where
        F: FnMut(Instant) + Send + 'static,
    {
        let period_ns = self.period_ms * 1_000_000;

        thread::Builder::new()
            .name("clock".to_string())
            .spawn(move || {
                let mut info = mach_timebase_info { numer: 0, denom: 0 };
                unsafe { mach_timebase_info(&mut info) };

                // Nanoseconds per mach unit, for turning a "remaining mach
                // units" gap back into a sleep duration.
                let nanos_per_unit = if info.denom == 0 {
                    1.0
                } else {
                    f64::from(info.numer) / f64::from(info.denom)
                };

                let period_units = Self::nanos_to_mach(period_ns, info.numer, info.denom).max(1);

                let mut next_time = unsafe { mach_absolute_time() };
                loop {
                    callback(Instant::now());
                    next_time = next_time.wrapping_add(period_units);
                    let mut now = unsafe { mach_absolute_time() };
                    while now < next_time {
                        let remaining_units = next_time - now;
                        let remaining_ns = (remaining_units as f64 * nanos_per_unit) as u64;
                        if remaining_ns > 200_000 {
                            thread::sleep(Duration::from_nanos(remaining_ns - 100_000));
                        }
                        now = unsafe { mach_absolute_time() };
                    }
                }
            })
            .expect("Failed to spawn clock thread");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::MachWaitUntilTimer;

    #[test]
    fn nanos_to_mach_apple_silicon_timebase_is_not_truncated() {
        // Apple Silicon reports numer/denom = 125/3. A true 1 ms is
        // 1_000_000 * 3 / 125 = 24_000 mach units; the old `numer/denom`-first
        // math gave 1_000_000 / 41 = 24_390 (~1.016 ms).
        assert_eq!(MachWaitUntilTimer::nanos_to_mach(1_000_000, 125, 3), 24_000);
    }

    #[test]
    fn nanos_to_mach_identity_timebase_is_unchanged() {
        // Intel Macs report 1/1.
        assert_eq!(
            MachWaitUntilTimer::nanos_to_mach(1_000_000, 1, 1),
            1_000_000
        );
    }

    #[test]
    fn nanos_to_mach_zero_numer_falls_back_to_nanoseconds() {
        assert_eq!(
            MachWaitUntilTimer::nanos_to_mach(1_000_000, 0, 0),
            1_000_000
        );
    }
}
