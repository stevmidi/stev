//! The `"recovery"` thread: everything slow about the crash recovery copy —
//! encoding the plugin state, the JSON, the disk — off the `"sequencer"`
//! thread, where a stalled disk would reach playback. See
//! `410-crash-recovery.md` decision 5.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};

use crossbeam_channel::Receiver;

use crate::core::project::{RecoveryJob, run_recovery_jobs};

/// Starts the `"recovery"` thread, which runs each job on the copy at `path`
/// until the sender is dropped and then ends, letting go of `lock` last. A
/// failed write sets `write_failed`.
pub(crate) fn start_recovery_thread(
    lock: File,
    path: PathBuf,
    jobs: Receiver<RecoveryJob>,
    write_failed: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("recovery".to_string())
        .spawn(move || {
            let _lock = lock;
            run_recovery_jobs(jobs, &path, &write_failed);
        })
        .expect("Failed to spawn recovery thread")
}
