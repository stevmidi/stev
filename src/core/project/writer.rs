//! The [`StorageJob`]s the `"storage"` thread runs, the project's one writer:
//! saves, templates, and the crash recovery copy's writes and deletes, in the
//! order the `"sequencer"` thread sent them. Everything slow about a save —
//! encoding the plugin state, the JSON, the disk — happens here, off the
//! thread that plays. See `420-large-projects.md` decisions 3 and 4 and
//! `410-crash-recovery.md` decision 5.

use std::io;
use std::iter;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver;
use log::warn;

use super::dto::{ProjectData, ProjectSnapshot};
use super::recovery::{delete_copy, write_copy};
use super::storage::{ProjectOrigin, save_project, save_template};

/// One job for the `"storage"` thread. Every job goes down one channel, so
/// they run in the order they were sent: a copy write in flight can't land
/// after a delete and bring the copy back, and a save's delete of the copy
/// comes after every write sent before it.
pub(crate) enum StorageJob {
    /// Replace the recovery copy with this project.
    WriteCopy {
        /// The project; encoded by the job.
        snapshot: ProjectSnapshot,
        /// Where it came from.
        origin: Option<ProjectOrigin>,
    },
    /// Delete the recovery copy, if there is one.
    DeleteCopy,
    /// Write the project to `target`, then report how it went. Never
    /// dropped (`420-large-projects.md` decision 4).
    Save {
        /// The project as it was at the handover; encoded by the job.
        snapshot: ProjectSnapshot,
        /// Where it goes.
        target: SaveTarget,
        /// Delete the recovery copy once the file is safe: a project saved
        /// has nothing to recover. False while a crash's copy waits on the
        /// launch dialog, and in an instance that doesn't keep the copy.
        delete_copy: bool,
    },
}

impl StorageJob {
    /// Whether this job is one of the copy's, which a later one of the
    /// copy's makes pointless.
    fn is_copy(&self) -> bool {
        matches!(self, StorageJob::WriteCopy { .. } | StorageJob::DeleteCopy)
    }
}

/// How a save went, sent back to the `"sequencer"` thread
/// (`SequencerCommand::ProjectWritten`): why it failed, if it did.
pub(crate) type SaveOutcome = Result<(), String>;

/// Where a save goes.
pub(crate) enum SaveTarget {
    /// The project file at this origin; it becomes the open project's.
    Project(ProjectOrigin),
    /// The template of this name, stripped (`ProjectData::into_template`).
    /// The open project stays as it was.
    Template(String),
}

/// Runs the jobs, in order, until every sender is gone. Of the copy's jobs
/// waiting, only the last runs: with one slot it decides what is left, so a
/// stalled disk comes back to one write, not a backlog, and a quit doesn't
/// wait on writes it is about to delete. The ones it makes pointless are
/// dropped as the batch is gathered, their snapshots with them. Every save
/// runs, in its place, and its outcome goes to `report`. A copy write that
/// fails warns and sets `write_failed`, which the next check reads to write
/// again. `write` puts a save on disk ([`write_save`] in the app). The
/// `"storage"` thread's whole loop, apart from the thread so tests needn't
/// wait on one.
pub(crate) fn run_storage_jobs(
    jobs: Receiver<StorageJob>,
    copy_path: &Path,
    write_failed: &AtomicBool,
    write: impl Fn(&SaveTarget, &ProjectData) -> io::Result<()>,
    mut report: impl FnMut(SaveOutcome),
) {
    while let Ok(next) = jobs.recv() {
        let mut batch: Vec<StorageJob> = Vec::new();
        for job in iter::once(next).chain(jobs.try_iter()) {
            if job.is_copy() {
                batch.retain(|queued| !queued.is_copy());
            }
            batch.push(job);
        }
        for job in batch {
            match job {
                StorageJob::Save {
                    snapshot,
                    target,
                    delete_copy,
                } => report(save(snapshot, &target, delete_copy, copy_path, &write)),
                StorageJob::WriteCopy { snapshot, origin } => {
                    if let Err(e) = write_copy(copy_path, snapshot, origin) {
                        warn!("Could not write the recovery copy: {e}");
                        write_failed.store(true, Ordering::Relaxed);
                    }
                }
                StorageJob::DeleteCopy => remove_copy(copy_path),
            }
        }
    }
}

/// Deletes the copy at `copy_path`, warning if that fails.
fn remove_copy(copy_path: &Path) {
    if let Err(e) = delete_copy(copy_path) {
        warn!("Could not delete the recovery copy: {e}");
    }
}

/// Encodes `snapshot`, writes it to `target` and, once it is safe and when
/// `then_delete_copy`, deletes the recovery copy at `copy_path`.
fn save(
    snapshot: ProjectSnapshot,
    target: &SaveTarget,
    then_delete_copy: bool,
    copy_path: &Path,
    write: &impl Fn(&SaveTarget, &ProjectData) -> io::Result<()>,
) -> SaveOutcome {
    let mut data = snapshot.encoded();
    if let SaveTarget::Template(_) = target {
        data = data.into_template();
    }
    let written = write(target, &data).map_err(|e| e.to_string());
    drop(data);
    if written.is_ok() && then_delete_copy {
        remove_copy(copy_path);
    }
    written
}

/// Writes `data` where `target` says, in the library.
pub(crate) fn write_save(target: &SaveTarget, data: &ProjectData) -> io::Result<()> {
    match target {
        SaveTarget::Project(origin) => save_project(data, origin.folder.as_deref(), &origin.name),
        SaveTarget::Template(name) => save_template(data, name),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossbeam_channel::unbounded;

    use crate::core::paths::{RECOVERY_FILE_NAME, scratch_dir};
    use crate::core::project::{RecoveryCopy, load_project_file, read_recovery_copy};
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};

    use super::super::storage::{stev_path, write_project_in};
    use super::*;

    /// A snapshot of a project at `tempo_us`.
    fn snapshot(tempo_us: i32) -> ProjectSnapshot {
        let sequencer = test_sequencer();
        sequencer.set_tempo(tempo_us);
        ProjectSnapshot::of(&sequencer)
    }

    /// A copy write of a project at `tempo_us`, never saved.
    fn write(tempo_us: i32) -> StorageJob {
        StorageJob::WriteCopy {
            snapshot: snapshot(tempo_us),
            origin: None,
        }
    }

    /// A save of a project at `tempo_us` as `name`, deleting the copy.
    fn save_as(tempo_us: i32, name: &str) -> StorageJob {
        StorageJob::Save {
            snapshot: snapshot(tempo_us),
            target: SaveTarget::Project(ProjectOrigin::new(None, name)),
            delete_copy: true,
        }
    }

    /// Runs `jobs` in order, as one batch, on the copy in `dir`, saving into
    /// `dir` too (never the library), a name starting `fail` failing: whether
    /// a copy write failed, and the saves' outcomes.
    fn run(jobs: Vec<StorageJob>, dir: &Path) -> (bool, Vec<SaveOutcome>) {
        let (job_tx, job_rx) = unbounded();
        for job in jobs {
            job_tx.send(job).unwrap();
        }
        drop(job_tx);
        let write = |target: &SaveTarget, data: &ProjectData| {
            let name = match target {
                SaveTarget::Project(origin) => &origin.name,
                SaveTarget::Template(name) => name,
            };
            if name.starts_with("fail") {
                return Err(io::Error::other("disk full"));
            }
            write_project_in(dir, data, name)
        };
        let write_failed = AtomicBool::new(false);
        let mut outcomes = Vec::new();
        let copy_path = dir.join(RECOVERY_FILE_NAME);
        run_storage_jobs(job_rx, &copy_path, &write_failed, write, |outcome| {
            outcomes.push(outcome);
        });
        (write_failed.load(Ordering::Relaxed), outcomes)
    }

    /// The tempo of the copy in `dir`.
    fn copy_tempo(dir: &Path) -> i32 {
        let path = dir.join(RECOVERY_FILE_NAME);
        let copy: RecoveryCopy = read_recovery_copy(&path).unwrap().unwrap();
        copy.into_staged().data.tempo_us
    }

    /// Whether `dir` holds a copy.
    fn has_copy(dir: &Path) -> bool {
        dir.join(RECOVERY_FILE_NAME).exists()
    }

    /// The project saved as `name` in `dir`.
    fn saved(dir: &Path, name: &str) -> ProjectData {
        load_project_file(&stev_path(dir, name)).unwrap()
    }

    /// The last of the copy's jobs decides, so a delete queued behind a
    /// write leaves no copy, and a write behind a delete leaves one.
    #[test]
    fn writes_and_deletes_run_in_order() {
        let dir = scratch_dir("writer-writes_and_deletes_run_in_order");
        run(vec![write(400_000), StorageJob::DeleteCopy], &dir);
        assert!(!has_copy(&dir));
        run(
            vec![write(400_000), StorageJob::DeleteCopy, write(300_000)],
            &dir,
        );
        assert_eq!(copy_tempo(&dir), 300_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Deleting when there is no copy is no failure.
    #[test]
    fn deleting_no_copy_does_nothing() {
        let dir = scratch_dir("writer-deleting_no_copy_does_nothing");
        assert!(!run(vec![StorageJob::DeleteCopy], &dir).0);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A copy write that fails sets the flag the next check reads.
    #[test]
    fn a_failed_write_sets_the_flag() {
        let dir = scratch_dir("writer-a_failed_write_sets_the_flag");
        assert!(run(vec![write(400_000)], &dir.join("missing")).0);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Every save runs and reports, in order, however many wait: saves are
    /// never dropped as the copy's writes are.
    #[test]
    fn every_save_runs_in_order() {
        let dir = scratch_dir("writer-every_save_runs_in_order");
        let jobs = vec![save_as(400_000, "One"), save_as(300_000, "fail")];
        let (_, outcomes) = run(jobs, &dir);
        assert_eq!(outcomes, [Ok(()), Err("disk full".to_owned())]);
        assert_eq!(saved(&dir, "One").tempo_us, 400_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A save behind a copy write deletes the copy once the file is written;
    /// a copy write behind the save is the copy that stays.
    #[test]
    fn a_save_deletes_the_copy_written_before_it() {
        let dir = scratch_dir("writer-a_save_deletes_the_copy_written_before_it");
        run(vec![write(400_000), save_as(400_000, "Song")], &dir);
        assert!(!has_copy(&dir));
        assert_eq!(saved(&dir, "Song").tempo_us, 400_000);

        run(vec![save_as(400_000, "Song"), write(300_000)], &dir);
        assert_eq!(copy_tempo(&dir), 300_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A save told to keep the copy keeps it: a crash's copy waiting on the
    /// launch dialog.
    #[test]
    fn a_save_can_keep_the_copy() {
        let dir = scratch_dir("writer-a_save_can_keep_the_copy");
        let keep = StorageJob::Save {
            snapshot: snapshot(400_000),
            target: SaveTarget::Project(ProjectOrigin::new(None, "Song")),
            delete_copy: false,
        };
        run(vec![write(400_000), keep], &dir);
        assert!(has_copy(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    /// A failed save leaves the copy, written before it, as the only record
    /// of the changes.
    #[test]
    fn a_failed_save_keeps_the_copy() {
        let dir = scratch_dir("writer-a_failed_save_keeps_the_copy");
        run(vec![write(400_000), save_as(400_000, "fail")], &dir);
        assert_eq!(copy_tempo(&dir), 400_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A template is saved stripped, and never touches the copy.
    #[test]
    fn a_template_is_saved_stripped() {
        let dir = scratch_dir("writer-a_template_is_saved_stripped");
        let mut sequencer = test_sequencer();
        sequencer.set_tempo(400_000);
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920));
        let template = StorageJob::Save {
            snapshot: ProjectSnapshot::of(&sequencer),
            target: SaveTarget::Template("Band".to_owned()),
            delete_copy: false,
        };
        run(vec![write(300_000), template], &dir);
        let saved = saved(&dir, "Band");
        assert_eq!(saved.tempo_us, 400_000);
        assert!(saved.tracks[0].clips.is_empty());
        assert!(has_copy(&dir));
        fs::remove_dir_all(dir).unwrap();
    }
}
