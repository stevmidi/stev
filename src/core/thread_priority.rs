//! Best-effort scheduling priority for the timing threads, `"clock"` and
//! `"midiout"`, so a busy machine doesn't delay a clock firing or a MIDI
//! write (`260-porting.md` § The fixes).
//!
//! - **macOS**: the time-constraint (real-time) policy, as CoreAudio gives its
//!   IO thread. Measured under full load on an Intel Mac: clock gap p99 went
//!   from ~3.5 ms to 1000.1 µs, `"midiout"` lateness p99 from ~1.9 ms to under
//!   10 µs. A QoS class (user-interactive) changed nothing there.
//! - **Linux**: `SCHED_FIFO` at a modest priority, which only succeeds when
//!   the user's `RLIMIT_RTPRIO` allows it (an `audio` group, say). No rtkit.
//! - **Windows**: MMCSS's "Pro Audio" task.
//!
//! Only the macOS path is measured. Each one is a request: a refusal is
//! logged and the thread runs on at normal priority. The audio workers
//! deliberately take no such policy (`170-multicore-scheduling.md`).

#[cfg(target_os = "macos")]
use mach2::kern_return::KERN_SUCCESS;
#[cfg(target_os = "macos")]
use mach2::mach_init::mach_thread_self;
#[cfg(target_os = "macos")]
use mach2::mach_time::{mach_timebase_info, mach_timebase_info_data_t};
#[cfg(target_os = "macos")]
use mach2::thread_policy::{
    THREAD_TIME_CONSTRAINT_POLICY, THREAD_TIME_CONSTRAINT_POLICY_COUNT, thread_policy_set,
    thread_policy_t, thread_time_constraint_policy,
};

#[cfg(target_os = "linux")]
use libc::{SCHED_FIFO, pthread_self, pthread_setschedparam, sched_param};

#[cfg(target_os = "windows")]
use std::iter::once;
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

/// Which timing thread is asking, for its scheduling parameters.
#[derive(Clone, Copy)]
pub(crate) enum TimingThread {
    /// The 1 ms clock timer (`core/timer.rs`).
    Clock,
    /// The MIDI output thread (`core/threads/midi_output.rs`).
    MidiOut,
}

/// A thread's time-constraint parameters, in nanoseconds.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, PartialEq)]
struct TimeConstraint {
    /// How often the thread wants to run; 0 for a thread with no period.
    period: u64,
    /// How much CPU it needs in each run, its spin tail included. A thread
    /// that runs past this without blocking is demoted to normal priority
    /// until it blocks, so a runaway can't starve the machine.
    computation: u64,
    /// How soon after waking that computation must be done.
    constraint: u64,
}

impl TimingThread {
    /// The time-constraint parameters this thread asks macOS for. The clock
    /// fires every millisecond and spins at most 100 µs of it; `"midiout"` has
    /// no period, and spins up to 500 µs before a deadline after 250 µs
    /// sleeps.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn time_constraint(self) -> TimeConstraint {
        match self {
            TimingThread::Clock => TimeConstraint {
                period: 1_000_000,
                computation: 300_000,
                constraint: 1_000_000,
            },
            TimingThread::MidiOut => TimeConstraint {
                period: 0,
                computation: 1_000_000,
                constraint: 2_000_000,
            },
        }
    }
}

/// Raises the calling thread's priority for its timing role, best effort.
pub(crate) fn raise_timing_priority(thread: TimingThread) {
    if let Err(_e) = raise(thread) {
        dprintln!("timing priority refused: {_e}");
    }
}

/// macOS: the time-constraint policy.
#[cfg(target_os = "macos")]
fn raise(thread: TimingThread) -> Result<(), String> {
    let mut info = mach_timebase_info_data_t { numer: 0, denom: 0 };
    unsafe { mach_timebase_info(&mut info) };
    let to_mach = |ns| nanos_to_mach(ns, info.numer, info.denom);
    let wanted = thread.time_constraint();
    let mut policy = thread_time_constraint_policy {
        period: to_mach(wanted.period),
        computation: to_mach(wanted.computation),
        constraint: to_mach(wanted.constraint),
        preemptible: 1,
    };
    let result = unsafe {
        thread_policy_set(
            mach_thread_self(),
            THREAD_TIME_CONSTRAINT_POLICY,
            (&mut policy as *mut thread_time_constraint_policy).cast() as thread_policy_t,
            THREAD_TIME_CONSTRAINT_POLICY_COUNT,
        )
    };
    if result == KERN_SUCCESS {
        Ok(())
    } else {
        Err(format!("thread_policy_set returned {result}"))
    }
}

/// Linux: `SCHED_FIFO`, when `RLIMIT_RTPRIO` allows.
#[cfg(target_os = "linux")]
fn raise(_thread: TimingThread) -> Result<(), String> {
    // Low in the 1–99 range, under audio servers' own threads; rtkit's
    // default ceiling is 20.
    let param = sched_param { sched_priority: 10 };
    let result = unsafe { pthread_setschedparam(pthread_self(), SCHED_FIFO, &param) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!("pthread_setschedparam returned {result}"))
    }
}

/// Windows: MMCSS's "Pro Audio" task.
#[cfg(target_os = "windows")]
fn raise(_thread: TimingThread) -> Result<(), String> {
    let task: Vec<u16> = "Pro Audio".encode_utf16().chain(once(0)).collect();
    let mut task_index = 0u32;
    // The handle would revert the thread on exit; these threads never exit.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_index) };
    if handle.is_null() {
        Err("AvSetMmThreadCharacteristicsW failed".to_string())
    } else {
        Ok(())
    }
}

/// Anywhere else: nothing to ask for.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn raise(_thread: TimingThread) -> Result<(), String> {
    Ok(())
}

/// Converts nanoseconds into mach absolute time units using the timebase
/// ratio `numer/denom` (`ns = mach * numer / denom`), in one rational step:
/// computing `numer/denom` first truncates Apple Silicon's 125/3 to 41.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn nanos_to_mach(ns: u64, numer: u32, denom: u32) -> u32 {
    if numer == 0 {
        return ns as u32;
    }
    ((u128::from(ns) * u128::from(denom)) / u128::from(numer)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanos_to_mach_apple_silicon_timebase_is_not_truncated() {
        // 125/3: a true 1 ms is 1_000_000 * 3 / 125 = 24_000 units, where
        // dividing 125/3 first would give 1_000_000 / 41 = 24_390.
        assert_eq!(nanos_to_mach(1_000_000, 125, 3), 24_000);
    }

    #[test]
    fn nanos_to_mach_identity_timebase_is_unchanged() {
        // Intel Macs report 1/1.
        assert_eq!(nanos_to_mach(1_000_000, 1, 1), 1_000_000);
    }

    #[test]
    fn nanos_to_mach_zero_numer_falls_back_to_nanoseconds() {
        assert_eq!(nanos_to_mach(1_000_000, 0, 0), 1_000_000);
    }

    #[test]
    fn each_thread_asks_for_a_computation_within_its_constraint() {
        for thread in [TimingThread::Clock, TimingThread::MidiOut] {
            let wanted = thread.time_constraint();
            assert!(wanted.computation <= wanted.constraint);
            assert!(wanted.period == 0 || wanted.constraint <= wanted.period);
            // macOS refuses computations under 50 µs or over 50 ms.
            assert!((50_000..=50_000_000).contains(&wanted.computation));
        }
    }
}
