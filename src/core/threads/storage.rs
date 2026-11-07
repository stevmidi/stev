//! The `"storage"` thread, the project's one writer: saves, templates and
//! the crash recovery copy, in the order the `"sequencer"` thread sent them.
//! Everything slow about them — encoding the plugin state, the JSON, the
//! disk — happens here, off the thread that plays. See
//! `420-large-projects.md` decision 3 and `410-crash-recovery.md` decision 5.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender};

use crate::core::project::{StorageJob, run_storage_jobs, write_save};
use crate::core::sequencer::SequencerCommand;

/// Starts the `"storage"` thread, which runs each job until the sender is
/// dropped and then ends, letting go of `lock` last: the recovery lock, held
/// by the instance that keeps the copy at `copy_path` (`None` in one that
/// doesn't, which still saves). A failed copy write sets `write_failed`; each
/// save's outcome goes back to the `"sequencer"` thread on `results`
/// (`SequencerCommand::ProjectWritten`).
pub(crate) fn start_storage_thread(
    lock: Option<File>,
    copy_path: PathBuf,
    jobs: Receiver<StorageJob>,
    write_failed: Arc<AtomicBool>,
    results: Sender<SequencerCommand>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("storage".to_string())
        .spawn(move || {
            let _lock = lock;
            run_storage_jobs(jobs, &copy_path, &write_failed, write_save, |outcome| {
                results.send(SequencerCommand::ProjectWritten(outcome)).ok();
            });
        })
        .expect("Failed to spawn storage thread")
}
