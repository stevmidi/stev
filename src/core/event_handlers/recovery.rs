//! [`RecoveryKeeper`] — the `"sequencer"` thread's side of the crash recovery
//! copy: when one should exist, and what it should hold. The writing itself
//! is the `"recovery"` thread's. See `410-crash-recovery.md`.

use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::core::project::{self, ProjectOrigin, ProjectSnapshot, RecoveryCopy, RecoveryJob};
use crate::core::sequencer::Sequencer;

use super::SavedProject;

/// How often the project is compared with the copy: at most this much of the
/// work is lost to a crash (`410-crash-recovery.md` decision 3).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Where the copy a crash left stands. Anything but
/// [`Free`](Self::Free) keeps every job from the copy
/// (`410-crash-recovery.md` decision 14).
enum Hold {
    /// No offer to answer: the check keeps the copy.
    Free,
    /// This copy waits for the launch dialog's answer.
    Offered(RecoveryCopy),
    /// Restore was answered, and the project isn't in yet.
    Restoring,
}

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
    /// The fingerprint and origin of the project last handed over to be the
    /// copy; `None` while there should be no copy. The origin counts: a
    /// rename has to reach the copy, or Restore looks for a file that has gone.
    copied: Option<(u64, Option<ProjectOrigin>)>,
    /// Whether a plugin's state was captured since the last copy was handed
    /// over. Blobs stay out of the fingerprint, so this is how a new preset
    /// reaches the copy (`410-crash-recovery.md` decision 7).
    captured: bool,
    /// Whether a crash's copy keeps the jobs back.
    hold: Hold,
}

impl RecoveryKeeper {
    /// A keeper sending to `jobs`, with `worker` to wait on at the end. With
    /// no `jobs` another instance has the copy, and this keeper does nothing
    /// (`410-crash-recovery.md` decision 16). `offer` is the copy a crash
    /// left, which holds everything until the launch dialog is answered.
    pub(crate) fn new(
        jobs: Option<Sender<RecoveryJob>>,
        worker: Option<JoinHandle<()>>,
        write_failed: Arc<AtomicBool>,
        offer: Option<RecoveryCopy>,
    ) -> Self {
        RecoveryKeeper {
            jobs,
            worker,
            write_failed,
            next_check: Instant::now(),
            copied: None,
            captured: false,
            hold: offer.map_or(Hold::Free, Hold::Offered),
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
    /// changes, rewritten if it changed since the last copy, a plugin's state
    /// was captured, the last write failed or its origin moved. A capture
    /// alone never makes a copy: a project with no changes has none. The fingerprint is recorded at the
    /// handover, so an unchanged project isn't written twice while a write is
    /// on its way.
    fn check(&mut self, sequencer: &Sequencer, saved: &SavedProject) {
        if self.jobs().is_none() {
            return;
        }
        let retry = self.write_failed.swap(false, Ordering::Relaxed);
        let fingerprint = project::fingerprint(sequencer);
        if saved.is_fingerprint(fingerprint) {
            if self.copied.is_some() {
                self.delete();
            }
        } else if retry || self.captured || !self.is_copied(fingerprint, saved.origin()) {
            self.write(sequencer, saved, fingerprint);
        }
    }

    /// Hands `sequencer`'s project, with `fingerprint`, over to be the copy.
    fn write(&mut self, sequencer: &Sequencer, saved: &SavedProject, fingerprint: u64) {
        let origin = saved.origin().cloned();
        self.copied = Some((fingerprint, origin.clone()));
        self.captured = false;
        self.send(RecoveryJob::Write {
            snapshot: ProjectSnapshot::of(sequencer),
            origin,
        });
    }

    /// A plugin's state, newly captured, now differs from the one the
    /// project held: the next check rewrites the copy, if there is to be one.
    pub(super) fn state_captured(&mut self) {
        self.captured = true;
    }

    /// Whether the last copy handed over is of this project, from `origin`.
    fn is_copied(&self, fingerprint: u64, origin: Option<&ProjectOrigin>) -> bool {
        self.copied
            .as_ref()
            .is_some_and(|(copied, from)| *copied == fingerprint && from.as_ref() == origin)
    }

    /// The copy on offer, taken to answer the launch dialog. The hold stays
    /// until [`restored`](Self::restored) or [`discard`](Self::discard).
    pub(super) fn take_offer(&mut self) -> Option<RecoveryCopy> {
        if !matches!(self.hold, Hold::Offered(_)) {
            return None;
        }
        match mem::replace(&mut self.hold, Hold::Restoring) {
            Hold::Offered(copy) => Some(copy),
            _ => None,
        }
    }

    /// The offer was discarded: the copy goes, and the check takes over.
    pub(super) fn discard(&mut self) {
        self.hold = Hold::Free;
        self.delete();
    }

    /// The copy has been restored as `sequencer`'s project: the check takes
    /// over, and the copy is written again at once. Its project is the same,
    /// but its origin may not be: one whose file has gone comes back
    /// untitled, and a copy left naming that file would attach to whatever
    /// takes the name next.
    pub(super) fn restored(&mut self, sequencer: &Sequencer, saved: &SavedProject) {
        self.hold = Hold::Free;
        self.write(sequencer, saved, project::fingerprint(sequencer));
    }

    /// Deletes the copy, after any write already queued: the project was
    /// saved, so it is no longer anything to recover.
    pub(crate) fn delete(&mut self) {
        self.copied = None;
        self.send(RecoveryJob::Delete);
    }

    /// A clean quit: deletes the copy and waits for the `"recovery"` thread
    /// to finish, or a copy left behind would claim a crash on the next
    /// launch (`410-crash-recovery.md` decision 13). A copy on hold stays: a
    /// quit while Restore loads its plugins sees only the startup project,
    /// with no changes to ask about, so the next launch offers the copy again.
    pub(crate) fn finish(&mut self) {
        self.delete();
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }

    /// Where jobs go: nowhere in an instance that can't keep the copy, or
    /// while a hold stands.
    fn jobs(&self) -> Option<&Sender<RecoveryJob>> {
        self.jobs
            .as_ref()
            .filter(|_| matches!(self.hold, Hold::Free))
    }

    /// Sends `job` to the `"recovery"` thread, if it may have one.
    fn send(&self, job: RecoveryJob) {
        if let Some(jobs) = self.jobs() {
            jobs.send(job).ok();
        }
    }

    /// Puts `copy` on offer, as a launch after a crash does.
    #[cfg(test)]
    pub(super) fn offer(&mut self, copy: RecoveryCopy) {
        self.hold = Hold::Offered(copy);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use crossbeam_channel::unbounded;
    use undo::Record;

    use crate::core::event_handlers::test_harness::{Harness, add_clip, harness, run_command};
    use crate::core::paths::scratch_dir;
    use crate::core::project::{ProjectAction, ProjectData, TrackOutputData};
    use crate::core::sequencer::test_support::stub_instrument;
    use crate::core::sequencer::{SequencerCommand, SequencerEdit};
    use crate::core::threads::start_recovery_thread;
    use crate::view::display::UiEvent;

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
        h.open.recovery.check(&h.sequencer, &h.open.saved);
    }

    /// Runs `cmd`.
    fn run(h: &mut Harness, cmd: SequencerCommand) {
        let mut record: Record<SequencerEdit> = Record::new();
        run_command(h, &mut record, cmd);
    }

    /// Asks to quit, past the prompt when `discard_changes`.
    fn quit(h: &mut Harness, discard_changes: bool) {
        let cmd = SequencerCommand::ProjectAction {
            action: ProjectAction::Quit,
            discard_changes,
        };
        run(h, cmd);
    }

    /// Answers the launch dialog.
    fn answer(h: &mut Harness, restore: bool) {
        run(h, SequencerCommand::AnswerRecovery { restore });
    }

    /// Puts a copy of a project at 400,000 µs on offer, never saved, with a
    /// plugin on its first track when `plugin`.
    fn offer(h: &mut Harness, plugin: bool) {
        offer_from(h, None, plugin);
    }

    /// [`offer`], of a project from `origin`.
    fn offer_from(h: &mut Harness, origin: Option<ProjectOrigin>, plugin: bool) {
        let mut project = ProjectData {
            tempo_us: 400_000,
            ..ProjectData::default()
        };
        if plugin {
            project.tracks[0].output = TrackOutputData::Instrument {
                bundle_path: "/Synth.clap".to_owned(),
                plugin_id: "synth".to_owned(),
                display_name: "Synth".to_owned(),
                state: String::new(),
            };
        }
        h.open.recovery.offer(RecoveryCopy::new(origin, project));
    }

    /// Whether a quit now prompts, its prompt being all it does.
    fn quit_prompts(h: &mut Harness) -> bool {
        quit(h, false);
        h.ui_events
            .try_iter()
            .any(|event| matches!(event, UiEvent::UnsavedChanges { .. }))
    }

    /// While the offer stands the copy stays as the crash left it, whatever
    /// the startup project does: a change writes nothing over it, and going
    /// back to no changes deletes nothing.
    #[test]
    fn nothing_touches_the_copy_until_the_offer_is_answered() {
        let mut h = harness();
        offer(&mut h, false);
        check(&mut h);
        let tempo = h.sequencer.tempo_us();
        h.sequencer.set_tempo(300_000);
        check(&mut h);
        h.sequencer.set_tempo(tempo);
        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// Discard deletes the copy and hands over to the check.
    #[test]
    fn discard_deletes_the_copy() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, false);
        assert_eq!(jobs(&h), [Job::Delete]);
        assert_eq!(h.sequencer.tempo_us(), 500_000);

        h.sequencer.set_tempo(300_000);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(300_000, None)]);
    }

    /// Restore brings the project back, says so once in the footer, and
    /// writes the copy again at once, from where it now comes from; the
    /// check then has nothing to add.
    #[test]
    fn restore_brings_the_project_back_and_keeps_the_copy() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, true);
        assert_eq!(h.sequencer.tempo_us(), 400_000);
        let lines: Vec<String> = h
            .ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::Status { message } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["Restored your unsaved new project"]);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);

        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// A project whose file has gone comes back untitled, and the copy is
    /// rewritten untitled too: left naming the old file, it would attach to
    /// a new project of that name after the next crash.
    #[test]
    fn a_project_whose_file_has_gone_rewrites_the_copy_untitled() {
        let mut h = harness();
        let gone = format!("stev-gone-{}", std::process::id());
        offer_from(&mut h, Some(ProjectOrigin::new(None, &gone)), false);
        answer(&mut h, true);
        assert_eq!(h.open.saved.name(), None);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
    }

    /// With no baseline a restored project counts as changed, untouched: a
    /// quit asks, and nothing the check does removes the copy. A later change
    /// writes as usual.
    #[test]
    fn a_restored_project_counts_as_changed_until_it_is_saved() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, true);
        jobs(&h);
        assert!(quit_prompts(&mut h));

        h.sequencer.set_tempo(300_000);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(300_000, None)]);

        h.handlers
            .project_saved(&h.sequencer, &mut h.open, None, "Song");
        assert_eq!(jobs(&h), [Job::Delete]);
        assert!(!quit_prompts(&mut h));
    }

    /// Going back to the state it was restored in still counts as changed: there
    /// is no baseline to land on, so the copy isn't deleted.
    #[test]
    fn undoing_back_to_the_restored_state_keeps_the_copy() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, true);
        jobs(&h);
        h.sequencer.set_tempo(300_000);
        check(&mut h);
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        assert_eq!(
            jobs(&h),
            [Job::Write(300_000, None), Job::Write(400_000, None)]
        );
    }

    /// Replacing a restored project past the prompt leaves nothing to
    /// recover, so the next check deletes the copy.
    #[test]
    fn a_restored_project_thrown_away_loses_the_copy() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, true);
        jobs(&h);
        let cmd = SequencerCommand::ProjectAction {
            action: ProjectAction::New,
            discard_changes: true,
        };
        run(&mut h, cmd);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Delete]);
    }

    /// With plugins the project waits on the view, and the hold stays until
    /// it is in: only then does it count as restored.
    #[test]
    fn the_hold_lasts_until_a_staged_project_is_applied() {
        let mut h = harness();
        offer(&mut h, true);
        answer(&mut h, true);
        assert!(h.open.staged.is_some());
        h.sequencer.set_tempo(300_000);
        check(&mut h);
        assert_eq!(jobs(&h), []);

        run(&mut h, SequencerCommand::ApplyStagedProject);
        assert_eq!(h.sequencer.tempo_us(), 400_000);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
        check(&mut h);
        assert_eq!(jobs(&h), []);
        assert!(quit_prompts(&mut h));
    }

    /// A quit while Restore loads its plugins sees the unchanged startup
    /// project, so it doesn't prompt. It must not take the copy with it: the
    /// next launch offers it again.
    #[test]
    fn a_quit_during_the_restore_keeps_the_copy() {
        let mut h = harness();
        offer(&mut h, true);
        answer(&mut h, true);
        quit(&mut h, false);
        assert_eq!(jobs(&h), []);
    }

    /// A save in the gap before a restored project is in leaves the copy:
    /// the hold keeps every job back, not only the check's.
    #[test]
    fn a_save_during_the_restore_keeps_the_copy() {
        let mut h = harness();
        offer(&mut h, true);
        answer(&mut h, true);
        h.handlers
            .project_saved(&h.sequencer, &mut h.open, None, "Song");
        assert_eq!(jobs(&h), []);
    }

    /// A second answer finds no offer, and does nothing.
    #[test]
    fn a_second_answer_does_nothing() {
        let mut h = harness();
        offer(&mut h, false);
        answer(&mut h, true);
        jobs(&h);
        answer(&mut h, false);
        assert_eq!(jobs(&h), []);
    }

    /// A project with an origin and a change handed over as the copy.
    fn copied_project(h: &mut Harness, origin: &ProjectOrigin) {
        h.open.saved = SavedProject::of(&h.sequencer, Some(origin.clone()));
        h.sequencer.set_tempo(400_000);
        check(h);
        jobs(h);
    }

    /// A saved project with a plugin on its first track, its state `[1]`.
    fn saved_with_plugin(h: &mut Harness) {
        h.sequencer.tracks_mut()[0].set_output(stub_instrument(vec![1]));
        h.open.saved = SavedProject::of(&h.sequencer, None);
    }

    /// The view captures the first track's plugin state as `state`.
    fn capture(h: &mut Harness, state: Vec<u8>) {
        let slot = h.sequencer.tracks()[0].slot();
        run(h, SequencerCommand::SetTrackInstrumentState { slot, state });
    }

    /// A new preset reaches the copy: blobs stay out of the fingerprint, so
    /// the capture itself has to count, once (`410-crash-recovery.md`
    /// decision 7).
    #[test]
    fn a_captured_state_rewrites_the_copy() {
        let mut h = harness();
        saved_with_plugin(&mut h);
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        capture(&mut h, vec![2]);
        check(&mut h);
        check(&mut h);
        assert_eq!(
            jobs(&h),
            [Job::Write(400_000, None), Job::Write(400_000, None)]
        );
    }

    /// A capture that brings back the state the project already holds
    /// changes nothing, so it writes nothing.
    #[test]
    fn an_unchanged_state_rewrites_nothing() {
        let mut h = harness();
        saved_with_plugin(&mut h);
        h.sequencer.set_tempo(400_000);
        check(&mut h);
        jobs(&h);
        capture(&mut h, vec![1]);
        check(&mut h);
        assert_eq!(jobs(&h), []);
    }

    /// A sound tweaked in a project with no changes makes no copy, as the
    /// quit prompt doesn't ask either. A later change writes once.
    #[test]
    fn a_capture_alone_makes_no_copy() {
        let mut h = harness();
        saved_with_plugin(&mut h);
        capture(&mut h, vec![2]);
        check(&mut h);
        assert_eq!(jobs(&h), []);

        h.sequencer.set_tempo(400_000);
        check(&mut h);
        check(&mut h);
        assert_eq!(jobs(&h), [Job::Write(400_000, None)]);
    }

    /// The browser renaming the open project reaches the copy at the next
    /// check, though the project itself didn't change, and once.
    #[test]
    fn a_renamed_project_rewrites_the_copy() {
        let mut h = harness();
        let old = ProjectOrigin::new(Some("Songs"), "Song");
        copied_project(&mut h, &old);
        let new = "Tune".to_owned();
        run(&mut h, SequencerCommand::ProjectRenamed { old, new });
        assert_eq!(h.open.saved.name(), Some("Tune"));
        check(&mut h);
        check(&mut h);
        let renamed = ProjectOrigin::new(Some("Songs"), "Tune");
        assert_eq!(jobs(&h), [Job::Write(400_000, Some(renamed))]);
    }

    /// So does renaming the folder it is in.
    #[test]
    fn a_renamed_folder_rewrites_the_copy() {
        let mut h = harness();
        copied_project(&mut h, &ProjectOrigin::new(Some("Songs"), "Song"));
        let (old, new) = ("Songs".to_owned(), "Album".to_owned());
        run(&mut h, SequencerCommand::ProjectFolderRenamed { old, new });
        check(&mut h);
        let renamed = ProjectOrigin::new(Some("Album"), "Song");
        assert_eq!(jobs(&h), [Job::Write(400_000, Some(renamed))]);
    }

    /// A rename elsewhere in the library leaves the project alone: the same
    /// name in another folder is another project.
    #[test]
    fn a_rename_of_another_project_changes_nothing() {
        let mut h = harness();
        copied_project(&mut h, &ProjectOrigin::new(Some("Songs"), "Song"));
        let old = ProjectOrigin::new(Some("Other"), "Song");
        let new = "Tune".to_owned();
        run(&mut h, SequencerCommand::ProjectRenamed { old, new });
        let (old, new) = ("Other".to_owned(), "Album".to_owned());
        run(&mut h, SequencerCommand::ProjectFolderRenamed { old, new });
        check(&mut h);
        assert_eq!(h.open.saved.name(), Some("Song"));
        assert_eq!(jobs(&h), []);
    }

    /// A project never saved has nothing to follow.
    #[test]
    fn a_rename_leaves_an_unsaved_project_untitled() {
        let mut h = harness();
        let old = ProjectOrigin::new(None, "Song");
        let new = "Tune".to_owned();
        run(&mut h, SequencerCommand::ProjectRenamed { old, new });
        assert_eq!(h.open.saved.name(), None);
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
        h.open.saved = SavedProject::of(&h.sequencer, Some(origin.clone()));
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
        let tick = |h: &mut Harness, at| h.open.recovery.tick(at, &h.sequencer, &h.open.saved);
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
            .project_saved(&h.sequencer, &mut h.open, None, "Song");
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
        quit(&mut h, false);
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
        let mut keeper = RecoveryKeeper::new(Some(jobs_tx), Some(worker), write_failed, None);

        h.sequencer.set_tempo(400_000);
        keeper.check(&h.sequencer, &h.open.saved);
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
        let mut keeper = RecoveryKeeper::new(None, None, Arc::new(AtomicBool::new(false)), None);
        h.sequencer.set_tempo(400_000);
        keeper.check(&h.sequencer, &h.open.saved);
        assert_eq!(keeper.copied, None);
        keeper.finish();
    }
}
