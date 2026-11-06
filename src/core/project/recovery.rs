//! The crash recovery copy on disk: one slot in the config directory, holding
//! the open project and where it came from, and the [`RecoveryJob`]s the
//! `"recovery"` thread runs on it. The copy is never a project file and never
//! written over one. See `410-crash-recovery.md`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver;
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::core::paths::{RECOVERY_FILE_NAME, RECOVERY_LOCK_FILE_NAME, config_dir};

use super::dto::{ProjectData, ProjectSnapshot};
use super::storage::replace_file;

/// Where a project lives in the library: what a save was given, and what a
/// reopened copy saves back to (`410-crash-recovery.md` decision 10).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectOrigin {
    /// Project folder, or the projects root when `None`.
    pub(crate) folder: Option<String>,
    /// Project file name, without extension.
    pub(crate) name: String,
}

/// The copy, on disk.
#[derive(Serialize, Deserialize)]
struct RecoveryCopy {
    /// Where the project came from; `None` for one never saved.
    origin: Option<ProjectOrigin>,
    /// The project.
    project: ProjectData,
}

/// A change to the copy, for the `"recovery"` thread. Writes and deletes go
/// down one channel, so they run in the order they were sent, and a write in
/// flight can't land after a delete and bring the copy back
/// (`410-crash-recovery.md` decision 5).
pub(crate) enum RecoveryJob {
    /// Replace the copy with this project.
    Write {
        /// The project; encoded by the job, off the `"sequencer"` thread.
        snapshot: ProjectSnapshot,
        /// Where it came from.
        origin: Option<ProjectOrigin>,
    },
    /// Delete the copy, if there is one.
    Delete,
}

/// The copy's path.
pub(crate) fn recovery_path() -> PathBuf {
    config_dir().join(RECOVERY_FILE_NAME)
}

/// Writes the copy to `path`, replacing the one there only once it is whole.
fn write_copy(
    path: &Path,
    snapshot: ProjectSnapshot,
    origin: Option<ProjectOrigin>,
) -> io::Result<()> {
    let copy = RecoveryCopy {
        origin,
        project: snapshot.encoded(),
    };
    let json = serde_json::to_string(&copy).map_err(io::Error::other)?;
    replace_file(path, &json)
}

/// Deletes the copy at `path`; there being none is no failure.
fn delete_copy(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Runs each job on the copy at `path`, in order, until every sender is gone.
/// A write that fails warns and sets `write_failed`, which the next check
/// reads to write again. The `"recovery"` thread's whole loop, apart from the
/// thread so tests needn't wait on one.
pub(crate) fn run_recovery_jobs(
    jobs: Receiver<RecoveryJob>,
    path: &Path,
    write_failed: &AtomicBool,
) {
    for job in jobs {
        match job {
            RecoveryJob::Write { snapshot, origin } => {
                if let Err(e) = write_copy(path, snapshot, origin) {
                    warn!("Could not write the recovery copy: {e}");
                    write_failed.store(true, Ordering::Relaxed);
                }
            }
            RecoveryJob::Delete => {
                if let Err(e) = delete_copy(path) {
                    warn!("Could not delete the recovery copy: {e}");
                }
            }
        }
    }
}

/// Takes the lock on the file at `path`, made if absent; `None` if another
/// process holds it or it can't be taken.
fn lock_at(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path);
    let file = match file {
        Ok(file) => file,
        Err(e) => {
            warn!("No crash recovery: could not open its lock: {e}");
            return None;
        }
    };
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => {
            info!("No crash recovery: another instance of Stev holds it");
            None
        }
        Err(TryLockError::Error(e)) => {
            warn!("No crash recovery: could not take its lock: {e}");
            None
        }
    }
}

/// The lock that lets this instance keep the recovery copy, or `None` if
/// another instance has it: this one then neither offers nor writes. The
/// system lets go when the process dies, however it dies, so a copy with a
/// free lock means a crash. For the app's startup path alone: a `stev --mcp`
/// taking it could turn recovery off (`410-crash-recovery.md` decision 16).
pub(crate) fn take_recovery_lock() -> Option<File> {
    lock_at(&config_dir().join(RECOVERY_LOCK_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;
    use std::process;
    use std::sync::atomic::AtomicUsize;

    use crossbeam_channel::unbounded;

    use crate::core::sequencer::test_support::{stub_instrument, test_sequencer};
    use crate::models::track::TrackOutput;

    use super::*;

    /// A fresh folder for one test, so tests don't share a copy.
    fn test_dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = temp_dir().join(format!("stev-recovery-{}-{n}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A write of a project at `tempo_us`, from `origin`.
    fn write(tempo_us: i32, origin: Option<ProjectOrigin>) -> RecoveryJob {
        let sequencer = test_sequencer();
        sequencer.set_tempo(tempo_us);
        RecoveryJob::Write {
            snapshot: ProjectSnapshot::of(&sequencer),
            origin,
        }
    }

    /// Runs `jobs` in order on the copy at `path`; whether a write failed.
    fn run(jobs: Vec<RecoveryJob>, path: &Path) -> bool {
        let (job_tx, job_rx) = unbounded();
        for job in jobs {
            job_tx.send(job).unwrap();
        }
        drop(job_tx);
        let write_failed = AtomicBool::new(false);
        run_recovery_jobs(job_rx, path, &write_failed);
        write_failed.load(Ordering::Relaxed)
    }

    /// The copy at `path`, as a launch will read it.
    fn read_copy(path: &Path) -> RecoveryCopy {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The copy reads back as the project it was, with where it came from.
    #[test]
    fn a_copy_holds_the_project_and_its_origin() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        let origin = ProjectOrigin {
            folder: Some("Songs".to_owned()),
            name: "Song".to_owned(),
        };
        assert!(!run(vec![write(400_000, Some(origin.clone()))], &path));
        let copy = read_copy(&path);
        assert_eq!(copy.origin, Some(origin));
        assert_eq!(copy.project.tempo_us, 400_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A project never saved has no origin to record.
    #[test]
    fn a_copy_of_a_project_never_saved_has_no_origin() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        run(vec![write(400_000, None)], &path);
        assert_eq!(read_copy(&path).origin, None);
        fs::remove_dir_all(dir).unwrap();
    }

    /// The blobs reach the copy encoded as a save encodes them.
    #[test]
    fn a_copy_encodes_the_plugin_state_as_a_save_does() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].set_output(stub_instrument(vec![1, 2, 3]));
        let job = RecoveryJob::Write {
            snapshot: ProjectSnapshot::of(&sequencer),
            origin: None,
        };
        run(vec![job], &path);
        let restored = read_copy(&path).project;
        restored.apply_to_sequencer(&mut sequencer);
        let TrackOutput::Instrument(instrument) = sequencer.tracks()[1].output() else {
            panic!("track 1 lost its plugin");
        };
        assert_eq!(instrument.state, vec![1, 2, 3]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Jobs run in the order they were sent, so a delete queued behind a
    /// write leaves no copy, and a write behind a delete leaves one.
    #[test]
    fn writes_and_deletes_run_in_order() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        run(vec![write(400_000, None), RecoveryJob::Delete], &path);
        assert!(!path.exists());
        run(
            vec![
                write(400_000, None),
                RecoveryJob::Delete,
                write(300_000, None),
            ],
            &path,
        );
        assert_eq!(read_copy(&path).project.tempo_us, 300_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A later write replaces the copy.
    #[test]
    fn the_last_write_is_the_copy() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        run(vec![write(400_000, None), write(300_000, None)], &path);
        assert_eq!(read_copy(&path).project.tempo_us, 300_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Deleting when there is no copy is no failure.
    #[test]
    fn deleting_no_copy_does_nothing() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_FILE_NAME);
        assert!(!run(vec![RecoveryJob::Delete], &path));
        fs::remove_dir_all(dir).unwrap();
    }

    /// A write that fails sets the flag the next check reads.
    #[test]
    fn a_failed_write_sets_the_flag() {
        let dir = test_dir();
        let path = dir.join("missing").join(RECOVERY_FILE_NAME);
        assert!(run(vec![write(400_000, None)], &path));
        fs::remove_dir_all(dir).unwrap();
    }

    /// Only one holder at a time, and the lock goes with its holder.
    #[test]
    fn the_lock_has_one_holder() {
        let dir = test_dir();
        let path = dir.join(RECOVERY_LOCK_FILE_NAME);
        let held = lock_at(&path);
        assert!(held.is_some());
        assert!(lock_at(&path).is_none());
        drop(held);
        assert!(lock_at(&path).is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
