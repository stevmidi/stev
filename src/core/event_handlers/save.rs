//! `EventHandlers` workflows for saving: the project and templates handed to
//! the `"storage"` thread as a snapshot ([`Storage`]), the outcome coming
//! back, and the project actions held while a save is on its way. See
//! `420-large-projects.md` decisions 3 to 6 and `060-persistence.md`.

use std::thread::JoinHandle;

use crate::core::project::{
    self, ProjectAction, ProjectOrigin, ProjectSnapshot, SaveOutcome, SaveTarget, StorageJob,
};

use super::*;

/// The `"sequencer"` thread's way to the `"storage"` thread, the project's
/// one writer. The [`RecoveryKeeper`] sends the copy's jobs down the same
/// channel, so saves and the copy's writes and deletes run in one order.
pub(crate) struct Storage {
    /// Down to the `"storage"` thread; `None` once [`finish`](Self::finish)
    /// has run.
    jobs: Option<Sender<StorageJob>>,
    /// The `"storage"` thread, for [`finish`](Self::finish) to wait on;
    /// `None` in tests, which run the jobs themselves.
    worker: Option<JoinHandle<()>>,
}

impl Storage {
    /// Sends to `jobs`, with `worker` to wait on at the end.
    pub(crate) fn new(jobs: Sender<StorageJob>, worker: Option<JoinHandle<()>>) -> Self {
        Storage {
            jobs: Some(jobs),
            worker,
        }
    }

    /// Sends `job`, behind every job already sent. False if the thread has
    /// gone, when no outcome will come.
    fn send(&self, job: StorageJob) -> bool {
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok())
    }

    /// Waits for the `"storage"` thread to run what it has and end: once
    /// every sender is gone, the [`RecoveryKeeper`]'s too.
    pub(super) fn finish(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }
}

/// A save handed to the `"storage"` thread, its outcome still to come.
/// Outcomes come back in the order the saves went, so the oldest is the
/// next one's.
pub(crate) enum PendingSave {
    /// The open project, to `origin`.
    Project {
        /// The project's fingerprint at the handover: what it counts as
        /// saved as, if the save succeeds (`420-large-projects.md`
        /// decision 5).
        fingerprint: u64,
        /// Where it goes, as the job names it: a browser rename made while
        /// it is on its way doesn't follow, since the file is written under
        /// this name whatever the rename did, and that is the file the
        /// project then counts as saved to.
        origin: ProjectOrigin,
        /// Whether it deletes the recovery copy once its file is safe.
        deletes_copy: bool,
    },
    /// The template of this name.
    Template(String),
}

impl PendingSave {
    /// What the footer calls it: the project's name, or `template` and its.
    fn label(&self) -> String {
        match self {
            PendingSave::Project { origin, .. } => origin.name.clone(),
            PendingSave::Template(name) => format!("template {name}"),
        }
    }
}

/// Why a save failed when the `"storage"` thread has gone: only after a
/// quit, or a panic there.
const WRITER_GONE: &str = "the writer has stopped";

impl EventHandlers {
    /// Hands the project, as it stands, to the `"storage"` thread to be
    /// written to `folder/filename.stev`; the outcome comes back as
    /// `SequencerCommand::ProjectWritten` (`project_written_workflow`).
    /// Nothing counts as saved until then.
    pub(super) fn save_project_workflow(
        &self,
        sequencer: &Sequencer,
        open: &mut OpenProject,
        folder: Option<&str>,
        filename: &str,
    ) {
        let origin = ProjectOrigin::new(folder, filename);
        let deletes_copy = open.recovery.deleted_by_save();
        let job = StorageJob::Save {
            snapshot: ProjectSnapshot::of(sequencer),
            target: SaveTarget::Project(origin.clone()),
            delete_copy: deletes_copy,
        };
        let pending = PendingSave::Project {
            fingerprint: project::fingerprint(sequencer),
            origin,
            deletes_copy,
        };
        self.hand_over(open, job, pending);
    }

    /// Hands the project, without its clips, to the `"storage"` thread to be
    /// written as the template `name`, stripped there
    /// (`ProjectData::into_template`). The open project stays as it was: its
    /// name, its file and `saved` are untouched, so a project with unsaved
    /// changes still has them. See `archive/340-templates.md`.
    pub(super) fn save_template_workflow(
        &self,
        sequencer: &Sequencer,
        open: &mut OpenProject,
        name: &str,
    ) {
        let job = StorageJob::Save {
            snapshot: ProjectSnapshot::template_of(sequencer),
            target: SaveTarget::Template(name.to_owned()),
            delete_copy: false,
        };
        self.hand_over(open, job, PendingSave::Template(name.to_owned()));
    }

    /// Sends a save's `job` and keeps it as `pending` until its outcome. If
    /// the thread has gone, the footer says the save failed at once, so
    /// nothing waits on an outcome that won't come.
    fn hand_over(&self, open: &mut OpenProject, job: StorageJob, pending: PendingSave) {
        if open.storage.send(job) {
            open.saving.push_back(pending);
        } else {
            let label = pending.label();
            self.send_status_ui_event(format!("Could not save {label}: {WRITER_GONE}"));
        }
    }

    /// The oldest save's `outcome`, from the `"storage"` thread, says how it
    /// went in the footer. A project saved becomes `saved` as it was at the
    /// handover and tells the view it is the open project
    /// (`UiEvent::ProjectSaved`); a failed one changes nothing. Anything
    /// written has the browser re-read the tree. Once no save is left on its
    /// way, returns the project actions held meanwhile, for the caller to run
    /// in order as they came: on success they find nothing unsaved, on
    /// failure they prompt again (`420-large-projects.md` decision 6).
    pub(super) fn project_written_workflow(
        &self,
        open: &mut OpenProject,
        outcome: &SaveOutcome,
    ) -> Vec<(ProjectAction, bool)> {
        let Some(pending) = open.saving.pop_front() else {
            return Vec::new();
        };
        let label = pending.label();
        let message = match outcome {
            Err(e) => format!("Could not save {label}: {e}"),
            Ok(()) => {
                if let PendingSave::Project {
                    fingerprint,
                    origin,
                    deletes_copy,
                } = pending
                {
                    if deletes_copy {
                        open.recovery.copy_deleted();
                    }
                    self.ui_event_tx
                        .send(UiEvent::ProjectSaved {
                            origin: origin.clone(),
                        })
                        .ok();
                    open.saved = SavedProject::saved_as(fingerprint, origin);
                }
                self.ui_event_tx.send(UiEvent::ProjectFilesChanged).ok();
                format!("Saved {label}")
            }
        };
        self.send_status_ui_event(message);
        if open.is_saving() {
            Vec::new()
        } else {
            std::mem::take(&mut open.held)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::core::event_handlers::test_harness::{
        Harness, harness, library_changed, quit, run_once, save_as, statuses, written,
    };
    use crate::core::project::LibraryChange;
    use crate::core::sequencer::SequencerCommand;
    use crate::core::sequencer::test_support::clip_at;

    use super::*;

    /// The saves handed to the `"storage"` thread since the last call: each
    /// one's file or template name, the tempo it holds, its clip count, and
    /// whether it deletes the copy.
    fn saves(h: &Harness) -> Vec<(String, i32, usize, bool)> {
        h.storage_jobs
            .try_iter()
            .filter_map(|job| match job {
                StorageJob::Save {
                    snapshot,
                    target,
                    delete_copy,
                } => {
                    let name = match target {
                        SaveTarget::Project(origin) => origin.name,
                        SaveTarget::Template(name) => name,
                    };
                    let data = snapshot.encoded();
                    let clips = data.tracks.iter().map(|track| track.clips.len()).sum();
                    Some((name, data.tempo_us, clips, delete_copy))
                }
                _ => None,
            })
            .collect()
    }

    /// What reached the view since the last call.
    #[derive(Debug, Default, PartialEq)]
    struct Ui {
        /// The footer lines.
        lines: Vec<String>,
        /// It was told of a project saved.
        saved: bool,
        /// It was asked to prompt.
        prompted: bool,
        /// It was told to quit.
        quit: bool,
    }

    /// Everything that reached the view since the last call.
    fn ui(h: &Harness) -> Ui {
        let mut ui = Ui::default();
        for event in h.ui_events.try_iter() {
            match event {
                UiEvent::Status { message } => ui.lines.push(message),
                UiEvent::ProjectSaved { .. } => ui.saved = true,
                UiEvent::UnsavedChanges { .. } => ui.prompted = true,
                UiEvent::QuitApproved => ui.quit = true,
                _ => {}
            }
        }
        ui
    }

    /// A save hands the project over as it stands, and nothing counts as
    /// saved until the outcome comes back.
    #[test]
    fn a_save_counts_only_once_it_is_written() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        assert_eq!(saves(&h), [("Song".to_owned(), 400_000, 0, true)]);
        assert!(h.open.saved.has_changes(&h.sequencer));
        assert_eq!(h.open.saved.name(), None);
        ui(&h);

        written(&mut h, None);
        assert!(!h.open.saved.has_changes(&h.sequencer));
        assert_eq!(h.open.saved.name(), Some("Song"));
        let lines = vec!["Saved Song".to_owned()];
        let saved = true;
        assert_eq!(
            ui(&h),
            Ui {
                lines,
                saved,
                ..Ui::default()
            }
        );
    }

    /// An edit made while the save is on its way still counts as unsaved:
    /// the project is saved as it was at the handover.
    #[test]
    fn an_edit_during_a_save_stays_unsaved() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        h.sequencer.set_tempo(300_000);
        written(&mut h, None);
        assert!(h.open.saved.has_changes(&h.sequencer));
        h.sequencer.set_tempo(400_000);
        assert!(!h.open.saved.has_changes(&h.sequencer));
    }

    /// A failed save says why and changes nothing.
    #[test]
    fn a_failed_save_changes_nothing() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        ui(&h);
        written(&mut h, Some("disk full"));
        assert!(h.open.saved.has_changes(&h.sequencer));
        assert_eq!(h.open.saved.name(), None);
        let lines = vec!["Could not save Song: disk full".to_owned()];
        assert_eq!(
            ui(&h),
            Ui {
                lines,
                ..Ui::default()
            }
        );
    }

    /// Save then Quit: the quit waits for the outcome, then finds nothing
    /// unsaved and goes ahead.
    #[test]
    fn a_quit_behind_a_save_waits_for_it() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        quit(&mut h, false);
        assert!(h.open.is_saving());
        let held = ui(&h);
        assert!(!held.quit && !held.prompted);

        written(&mut h, None);
        assert!(ui(&h).quit);
    }

    /// A quit behind a save that fails prompts again.
    #[test]
    fn a_quit_behind_a_failed_save_prompts_again() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        quit(&mut h, false);
        written(&mut h, Some("disk full"));
        assert!(ui(&h).prompted);
    }

    /// A held action waits for every save on its way, not just the first.
    #[test]
    fn held_actions_wait_for_the_last_save() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        save_as(&mut h, "Song");
        quit(&mut h, false);
        written(&mut h, None);
        assert!(!ui(&h).quit);
        assert!(h.open.is_saving());
        written(&mut h, None);
        assert!(ui(&h).quit);
    }

    /// The recovery check waits while a save is on its way: the save deletes
    /// the copy once the file is safe, and a copy written behind it would
    /// only be deleted again.
    #[test]
    fn the_recovery_check_waits_for_a_save() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        save_as(&mut h, "Song");
        h.storage_jobs.try_iter().for_each(drop);
        h.sequencer.set_tempo(300_000);
        h.open.tick(Instant::now(), &h.sequencer);
        assert_eq!(h.storage_jobs.try_iter().count(), 0);

        written(&mut h, None);
        h.open.tick(Instant::now(), &h.sequencer);
        assert_eq!(h.storage_jobs.try_iter().count(), 1);
    }

    /// A template goes to the `"storage"` thread without its clips, and its
    /// outcome leaves the open project as it was.
    #[test]
    fn a_template_leaves_the_project_as_it_was() {
        let mut h = harness();
        h.sequencer.set_tempo(400_000);
        h.sequencer.tracks_mut()[0].add_clip(&clip_at(0, 1920));
        let template = SequencerCommand::SaveTemplate {
            name: "Band".to_owned(),
        };
        run_once(&mut h, template);
        assert_eq!(saves(&h), [("Band".to_owned(), 400_000, 0, false)]);
        ui(&h);
        written(&mut h, None);
        assert!(h.open.saved.has_changes(&h.sequencer));
        let lines = vec!["Saved template Band".to_owned()];
        assert_eq!(
            ui(&h),
            Ui {
                lines,
                ..Ui::default()
            }
        );
    }

    /// A browser rename of the file a save is writing doesn't follow into
    /// the save: the job writes under the old name, so that is the file the
    /// project counts as saved to, and the renamed one keeps what it held.
    #[test]
    fn a_rename_during_a_save_keeps_the_name_written() {
        let mut h = harness();
        h.open.saved = SavedProject::saved_as(0, ProjectOrigin::new(None, "Song"));
        save_as(&mut h, "Song");
        let old = ProjectOrigin::new(None, "Song");
        let new = ProjectOrigin::new(None, "Tune");
        library_changed(&mut h, LibraryChange::Moved { old, new });
        assert_eq!(h.open.saved.name(), Some("Tune"));
        written(&mut h, None);
        assert_eq!(h.open.saved.name(), Some("Song"));
    }

    /// A save that failed leaves the copy, and the keeper knows it: undoing
    /// back to the saved state then deletes it, as without the save.
    #[test]
    fn a_failed_save_leaves_the_copy_to_the_check() {
        let mut h = harness();
        let tempo = h.sequencer.tempo_us();
        h.sequencer.set_tempo(400_000);
        h.open.tick(Instant::now(), &h.sequencer);
        save_as(&mut h, "Song");
        written(&mut h, Some("disk full"));
        h.storage_jobs.try_iter().for_each(drop);
        h.sequencer.set_tempo(tempo);
        h.open.recovery.tick(
            Instant::now() + Duration::from_secs(60),
            &h.sequencer,
            &h.open.saved,
        );
        assert!(
            h.storage_jobs
                .try_iter()
                .any(|job| matches!(job, StorageJob::DeleteCopy))
        );
    }

    /// A template's write holds nothing up: it changes neither `saved` nor
    /// the copy, so a quit behind it goes ahead at once.
    #[test]
    fn a_template_holds_nothing_up() {
        let mut h = harness();
        let template = SequencerCommand::SaveTemplate {
            name: "Band".to_owned(),
        };
        run_once(&mut h, template);
        assert!(!h.open.is_saving());
        quit(&mut h, false);
        assert!(ui(&h).quit);
    }

    /// With the `"storage"` thread gone a save says so at once, and holds
    /// nothing up.
    #[test]
    fn a_save_with_no_writer_fails_at_once() {
        let mut h = harness();
        h.open.storage.finish();
        statuses(&h);
        save_as(&mut h, "Song");
        assert!(!h.open.is_saving());
        assert_eq!(
            statuses(&h),
            ["Could not save Song: the writer has stopped"]
        );
    }
}
