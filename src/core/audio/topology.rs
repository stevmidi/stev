//! How many cores are actually useful for real-time DSP.
//!
//! [`available_parallelism`] is the obvious answer and the wrong one. It is
//! wrong in a different way on each architecture, and both errors
//! *over*-provision the audio [`WorkerPool`](super::WorkerPool):
//!
//! - **x86 with SMT** — it counts logical CPUs, so an i7-8700B reports 12 for 6
//!   physical cores. Two SMT siblings share one core's execution resources, and
//!   saturated floating-point DSP gets nowhere near a second core's worth out of
//!   the second thread. Sizing from it puts two real-time threads on one core to
//!   contend for the same units, on a path with a hard deadline.
//! - **Apple Silicon** — it counts efficiency cores, which should not be running
//!   deadline work at all.
//!
//! On macOS `sysctlbyname` answers both. `hw.perflevel0.physicalcpu` is the
//! physical core count of the *highest-performance* cluster — the P-cores on
//! Apple Silicon, and simply all the cores on a homogeneous Intel Mac, which
//! does publish the perf-level keys (verified: an i7-8700B reports 6 for both
//! `hw.perflevel0.physicalcpu` and `hw.physicalcpu`). So it is the right key on
//! either architecture, and `hw.physicalcpu` is the fallback for macOS versions
//! predating perf levels. Off macOS there is no worker pool today (see
//! `engine::worker_thread_count`), so [`available_parallelism`] is a fine last
//! resort.

use std::thread::available_parallelism;

/// The most runner threads worth having for one block of DSP.
///
/// This is architecture-dependent, because the logical CPUs *beyond* the
/// physical cores mean opposite things:
///
/// - **x86 with SMT** — they are hyperthread siblings of the same fast cores.
///   When there is more work than physical cores, the alternative to using a
///   sibling is running two items back to back on one core, and two independent
///   DSP threads sharing a core is never slower than that (usually 10–30%
///   faster). So it is worth going up to the logical count.
/// - **Apple Silicon** — they are *efficiency* cores, several times slower. An
///   item placed on one holds the barrier up for every other runner, which can
///   be worse than queueing it behind another item on a P-core. Never exceed the
///   performance cluster.
///
/// The pool is still never asked for more runners than there is work — see
/// `WorkerPool::for_each`'s `runners` argument — so this is only a ceiling.
pub(crate) fn max_dsp_threads() -> usize {
    if cfg!(target_arch = "aarch64") {
        performance_core_count()
    } else {
        logical_cpu_count()
    }
}

/// Every logical CPU, SMT siblings and efficiency cores included.
fn logical_cpu_count() -> usize {
    available_parallelism().map_or(1, |n| n.get())
}

/// Physical cores suitable for deadline work — the performance cluster on Apple
/// Silicon, physical (not logical) cores on Intel. Always at least 1.
fn performance_core_count() -> usize {
    #[cfg(target_os = "macos")]
    {
        // Highest-performance cluster: P-cores on Apple Silicon, all cores on
        // a homogeneous Intel Mac. Correct on both.
        if let Some(cores) = sysctl::read_int(c"hw.perflevel0.physicalcpu") {
            return cores;
        }
        // macOS versions predating the perf-level keys: physical, not logical.
        if let Some(cores) = sysctl::read_int(c"hw.physicalcpu") {
            return cores;
        }
    }
    logical_cpu_count()
}

/// Minimal `sysctlbyname` FFI for reading the macOS performance-core count.
#[cfg(target_os = "macos")]
mod sysctl {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::ptr::null_mut;

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    /// Reads an integer `sysctl` by name. `None` when the key does not exist
    /// (an older kernel without the caller's key, say) or when it reads back
    /// something implausible.
    pub(super) fn read_int(name: &CStr) -> Option<usize> {
        let mut value: i32 = 0;
        let mut len = size_of::<i32>();
        // SAFETY: `name` is NUL-terminated by construction; `value` and `len`
        // are live locals, and `len` correctly describes the `i32` these keys
        // return. A wrong-sized or missing key is reported through the return
        // code and `len`, both checked below.
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
                (&raw mut value).cast(),
                &raw mut len,
                null_mut(),
                0,
            )
        };
        if rc != 0 || len != size_of::<i32>() || value <= 0 {
            return None;
        }
        Some(value as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_always_at_least_one_core() {
        assert!(performance_core_count() >= 1);
    }

    /// The whole point: physical (or performance) cores are a subset of what
    /// `available_parallelism` counts, never more.
    #[test]
    fn it_never_exceeds_the_logical_cpu_count() {
        let logical = logical_cpu_count();
        let cores = performance_core_count();
        assert!(cores <= logical, "{cores} cores vs {logical} logical");
    }

    /// SMT siblings are worth using when there is more work than cores;
    /// efficiency cores never are. Either way the ceiling is never *below* the
    /// performance cluster, or work would queue behind a free fast core.
    #[test]
    fn the_thread_ceiling_is_at_least_the_performance_cluster() {
        let threads = max_dsp_threads();
        let cores = performance_core_count();
        assert!(threads >= cores, "{threads} threads vs {cores} cores");
        assert!(threads <= logical_cpu_count());
        if cfg!(target_arch = "aarch64") {
            assert_eq!(threads, cores, "never schedule DSP onto efficiency cores");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hw_physicalcpu_is_readable_on_every_mac() {
        assert!(sysctl::read_int(c"hw.physicalcpu").is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_absent_key_reads_none_rather_than_garbage() {
        assert!(sysctl::read_int(c"hw.no.such.key").is_none());
    }
}
