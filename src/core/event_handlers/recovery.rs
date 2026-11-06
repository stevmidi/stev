//! [`RecoveryKeeper`] — the `"sequencer"` thread's side of the crash recovery
//! copy: when one should exist, and what it should hold. The writing itself
//! is the `"recovery"` thread's. See `410-crash-recovery.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::core::project::{self, ProjectSnapshot, RecoveryJob};
use crate::core::sequencer::Sequencer;

use super::SavedProject;

/// How often the project is compared with the copy: at most this much of the
/// work is lost to a crash (`410-crash-recovery.md` decision 3).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Keeps a recovery copy exactly while the quit prompt would ask. The check
/// compares fingerprints on a timer, so no path that changes the project has
/// to remember it, and hands any write to the `"recovery"` thread: what is
/// left on the `"sequencer"` thread is the hash, and the snapshot when the
/// project changed.
pub(crate) struct RecoveryKeeper {
    /// Down to the `"recovery"` thread; `None` in an instance that can't keep
    /// the copy, and once [`finish`](Self::finish) has run.
    jobs: Option<Sender<RecoveryJob>>,
    /// The `"recovery"` thread, for [`finish`](Self::finish) to wait on.
    worker: Option<JoinHandle<()>>,
    /// Set by the `"recovery"` thread when a write fails.
    write_failed: Arc<AtomicBool>,
    /// When the next check is due.
    next_check: Instant,
    /// The fingerprint of the project last handed over to be the copy;
    /// `None` while there should be no copy.
    copied: Option<u64>,
}

impl RecoveryKeeper {
    /// A keeper sending to `jobs`, with `worker` to wait on at the end. With
    /// no `jobs` another instance has the copy, and this keeper does nothing
    /// (`410-crash-recovery.md` decision 16).
    pub(crate) fn new(
        jobs: Option<Sender<RecoveryJob>>,
        worker: Option<JoinHandle<()>>,
        write_failed: Arc<AtomicBool>,
    ) -> Self {
        RecoveryKeeper {
            jobs,
            worker,
            write_failed,
            next_check: Instant::now(),
            copied: None,
        }
    }

    /// Once per pass of the tick arm, `at` being the tick's instant: runs the
    /// check when one is due.
    pub(crate) fn tick(&mut self, at: Instant, sequencer: &Sequencer, saved: &SavedProject) {
        if at >= self.next_check {
            self.next_check = at + CHECK_INTERVAL;
            self.check(sequencer, saved);
        }
    }

    /// Brings the copy in line with the project: gone if the project has no
    /// changes, rewritten if it changed since the last copy or the last write
    /// failed. The fingerprint is recorded at the handover, so an unchanged
    /// project isn't written twice while a write is on its way.
    fn check(&mut self, sequencer: &Sequencer, saved: &SavedProject) {
        if self.jobs.is_none() {
            return;
        }
        let retry = self.write_failed.swap(false, Ordering::Relaxed);
        let fingerprint = project::fingerprint(sequencer);
        if saved.is_fingerprint(fingerprint) {
            if self.copied.is_some() {
                self.delete();
            }
        } else if retry || self.copied != Some(fingerprint) {
            self.copied = Some(fingerprint);
            self.send(RecoveryJob::Write {
                snapshot: ProjectSnapshot::of(sequencer),
                origin: saved.origin().cloned(),
            });
        }
    }

    /// Deletes the copy, after any write already queued: the project was
    /// saved, so it is no longer anything to recover.
    pub(crate) fn delete(&mut self) {
        self.copied = None;
        self.send(RecoveryJob::Delete);
    }

    /// A clean quit: deletes the copy and waits for the `"recovery"` thread
    /// to finish, or a copy left behind would claim a crash on the next
    /// launch (`410-crash-recovery.md` decision 13).
    pub(crate) fn finish(&mut self) {
        self.delete();
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }

    /// Sends `job` to the `"recovery"` thread, if this instance has one.
    fn send(&self, job: RecoveryJob) {
        if let Some(jobs) = &self.jobs {
            jobs.send(job).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use crossbeam_channel::unbounded;
    use undo::Record;

    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::paths::scratch_dir;
    use crate::core::project::{ProjectAction, ProjectOrigin};
    use crate::core::sequencer::{SequencerCommand, SequencerEdit};
    use crate::core::threads::start_recovery_thread;

    use super::*;

    /// What a job did to the copy, for comparing.
    #[derive(Debug, PartialEq)]
    enum Job {
        /// A write of a project at this tempo, from this origin.
        Write(i32, Option<ProjectOrigin>),
        /// A delete.
        Delete,
    }

    /// The jobs sent since the last call.
    fn jobs(h: &Harness) -> Vec<Job> {
        h.recovery_jobs
            .try_iter()
            .map(|job| match job {
                RecoveryJob::Write { snapshot, origin } => {
                    Job::Write(snapshot.encoded().tempo_us, origin)
                }
                RecoveryJob::Delete => Job::Delete,
            })
            .collect()
    }

    /// Runs one check.
    fn check(h: &mut Harness) {
        h.recovery.check(&h.sequencer, &h.saved);
    }

    /// Asks to quit, past the prompt when `discard_changes`.
    fn quit(h: &mut Harness, discard_changes: bool) {
        let mut record: Record<SequencerEdit> = Record::new();
        let cmd = SequencerCommand::ProjectAction {
            action: ProjectAction::Quit,
            discard_changes,
        };
        run_command(h, &mut record, cmd);
    }

    /// A project with no changes has no copy.
    #[test]
    fn no_changes_no_copy() {
        let mut h = harness();
        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// A change makes a copy, and an unchanged project isn't written again.
    #[test]
    fn a_change_is_written_once() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// Each further change writes the copy again.
    #[test]
    fn a_further_change_rewrites_the_copy() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        h.sequencer.set_tempo(300_000);
        check(&mut h);
        assert_eq!(
            jobs(&h),
            [Job::Write(400_000, None), Job::Write(300_000, None)]
        );
    }

    /// Undoing back to the saved state deletes the copy, once.
    #[test]
    fn going_back_to_the_saved_state_deletes_the_copy() {
        let mut h = harness();
        let tempo = h.sequencer.tempo_us();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        h.sequencer.set_tempo(tempo);
        check(&mut h);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(400_000, None), Job::Delete]);
    }

    /// The copy says where the project came from.
    #[test]
    fn a_copy_names_the_saved_project() {
        let mut h = harness();
        let origin = ProjectOrigin::new(Some("Songs"), "Song");
        h.saved = SavedProject::of(&h.sequencer, Some(origin.clone()));
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(400_000, Some(origin))]);
    }

    /// A failed write is written again though nothing changed, once.
    #[test]
    fn a_failed_write_is_retried_at_the_next_check() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        h.recovery_write_failed.store(true, Ordering::Relaxed);
        check(&mut h);
        check(&mut h);
        assert_eq!(
            jobs(&h),
            [Job::Write(400_000, None), Job::Write(400_000, None)]
        );
    }

    /// A project that went back to its saved state has nothing to retry.
    #[test]
    fn a_failed_write_of_a_project_with_no_changes_is_dropped() {
        let mut h = harness();
        h.recovery_write_failed.store(true, Ordering::Relaxed);
        check(&mut h);
        assert_eq!(jobs(&h), []);
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
    }

    /// The check runs on the first tick and then every interval, and not
    /// between.
    #[test]
    fn the_timer_checks_every_interval() {
        let mut h = harness();
        let start = Instant::now() + CHECK_INTERVAL;
        let tick = |h: &mut Harness, at| h.recovery.tick(at, &h.sequencer, &h.saved);
        tick(&mut h, start);
        h.sequencer.set_tempo(400_000);
        tick(&mut h, start + CHECK_INTERVAL - Duration::from_millis(1));
        assert_eq!(jobs(&h), []);
        tick(&mut h, start + CHECK_INTERVAL);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
    }

    /// A save makes the copy pointless: it goes at once, not at the next
    /// check, and a later change writes a new one.
    #[test]
    fn a_save_deletes_the_copy() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        h.handlers
            .project_saved(&h.sequencer, &mut h.saved, &mut h.recovery, None, "Song");
        assert_eq!(jobs(&h), [Job::Write(400_000, None), Job::Delete]);

        check(&mut h);
        assert_eq!(jobs(&h), []);

        h.sequencer.set_tempo(300_000);
        check(&mut h);
        let origin = ProjectOrigin::new(None, "Song");
        assert_eq!(jobs(&h), [Job::Write(300_000, Some(origin))]);
    }

    /// Any clean quit deletes the copy, last: with changes discarded, and
    /// with none, in case a copy was left from before. Nothing is sent after.
    #[test]
    fn a_clean_quit_deletes_the_copy_and_ends_the_keeper() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        quit(&mut h, true);
        assert_eq!(jobs(&h), [Job::Write(400_000, None), Job::Delete]);

        h.sequencer.set_tempo(300_000);
        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// A quit with no changes still clears the slot.
    #[test]
    fn a_quit_with_no_changes_deletes_too() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::ProjectAction {
                action: ProjectAction::Quit,
                discard_changes: false,
            },
        );
        assert_eq!(jobs(&h), [Job::Delete]);
    }

    /// A quit the prompt holds touches nothing: the copy stays until the
    /// user answers.
    #[test]
    fn a_quit_waiting_on_the_prompt_keeps_the_copy() {
        let mut h = harness();
        add_clip(&mut h.sequencer, 0);
        quit(&mut h, false);
        assert_eq!(jobs(&h), []);
    }

    /// The real thread: a write reaches the disk, and by the time `finish`
    /// returns the delete behind it has too, and the thread has ended.
    #[test]
    fn finish_waits_for_the_thread_to_delete_the_copy() {
        let h = harness();
        let dir = scratch_dir("recovery-finish");
        let path = dir.join("copy.json");
        let lock = File::create(dir.join("lock")).unwrap();
        let (jobs_tx, jobs_rx) = unbounded();
        let write_failed = Arc::new(AtomicBool::new(false));
        let worker = start_recovery_thread(lock, path.clone(), jobs_rx, write_failed.clone());
        let mut keeper = RecoveryKeeper::new(Some(jobs_tx), Some(worker), write_failed);

        h.sequencer.set_tempo(400_000);
        keeper.check(&h.sequencer, &h.saved);
        keeper.finish();

        assert!(keeper.worker.is_none());
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Without the lock this instance has nowhere to send a job, so nothing
    /// is recorded as copied and quitting has nothing to wait for
    /// (`410-crash-recovery.md` decision 16).
    #[test]
    fn an_instance_without_the_lock_stays_out() {
        let h = harness();
        let mut keeper = RecoveryKeeper::new(None, None, Arc::new(AtomicBool::new(false)));
        h.sequencer.set_tempo(400_000);
        keeper.check(&h.sequencer, &h.saved);
        assert_eq!(keeper.copied, None);
        keeper.finish();
    }
}
