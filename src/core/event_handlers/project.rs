//! `EventHandlers` workflows for project save / load / new-project and the
//! unsaved-changes check ([`SavedProject`]), and the MIDI clip export and
//! import. See `060-persistence.md`.

use std::{collections::VecDeque, fmt::Display, path::Path, time::Instant};

use undo::Record;

use crate::{
    core::{
        config::MAX_TRACKS,
        input_event::TimeSelectionRect,
        project::{
            self, Followed, LibraryChange, ProjectAction, ProjectData, ProjectOrigin, StagedProject,
        },
        sequencer::{ExportRefusal, PasteClipsEdit, SequencerEdit},
    },
    models::{clip::Clip, track::TrackOutput},
};

use super::*;

impl EventHandlers {
    /// Applies already-loaded `ProjectData` to the sequencer (`filename` is
    /// `None` for a new project; `folder` is where it was loaded from), fans
    /// out `ProjectLoaded` — the clips and the plugin tracks, so the view
    /// swaps both in at once — and returns to the Arranger with track 0
    /// selected. A project with more than `MAX_TRACKS` tracks loads the first
    /// ones and the footer says so.
    /// Clears `undo_record`: its edits name tracks and clips of the project
    /// being replaced, so undoing one would replay it against the new one.
    /// The loaded project becomes `saved`.
    fn load_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        saved: &mut SavedProject,
        data: ProjectData,
        filename: Option<String>,
        folder: Option<String>,
    ) {
        self.send_transport(TransportCommand::Stop);
        undo_record.clear();

        let saved_tracks = data.tracks.len();
        let clips = data.apply_to_sequencer(sequencer);
        let origin = filename
            .as_deref()
            .map(|name| ProjectOrigin::new(folder.as_deref(), name));
        *saved = SavedProject::of(sequencer, origin.clone());
        let instruments = sequencer
            .tracks()
            .iter()
            .filter_map(|track| match track.output() {
                TrackOutput::Instrument(r) => Some((track.slot(), r.clone())),
                TrackOutput::MidiOut { .. } => None,
            })
            .collect();
        self.ui_event_tx
            .send(UiEvent::ProjectLoaded {
                clips,
                instruments,
                origin,
            })
            .ok();
        self.emit_tracks(sequencer, None);

        self.set_view_state(ViewState::Arranger);

        self.select_track_workflow(sequencer, 0);
        self.announce_lead_clip_after_load(sequencer);

        if saved_tracks > MAX_TRACKS {
            let message = format!("Loaded the first {MAX_TRACKS} of {saved_tracks} tracks");
            self.send_status_ui_event(message);
        }
    }

    /// Load a project from disk by folder/filename: on success hand off to
    /// `open_read_project_workflow`; on failure say why in the footer and
    /// fall back to the Arranger, keeping the current project, its undo
    /// history and `saved`.
    fn open_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        folder: Option<&str>,
        filename: &str,
    ) {
        match project::load_project(folder, filename) {
            Err(e) => self.open_failed(filename, e),
            Ok(data) => {
                let project = StagedProject {
                    data,
                    title: filename.to_owned(),
                    file: Some((folder.map(str::to_owned), filename.to_owned())),
                    recovered: None,
                };
                self.open_read_project_workflow(sequencer, undo_record, open, project);
            }
        }
    }

    /// Opens the `.stev` at `path` (from Finder): one in the library as the
    /// browser opens it — a template untitled
    /// (`new_from_template_workflow`); one outside copied into the projects root first
    /// (`project::copy_project_in`), which the footer says and a shown
    /// browser lists, then opened as that copy. A file that won't load is never copied: the footer says
    /// why, and the open project stays.
    fn open_file_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        path: &Path,
    ) {
        if project::is_template_file(path) {
            self.new_from_template_workflow(sequencer, undo_record, open, path);
            return;
        }
        if let Some((folder, filename)) = project::library_project(path) {
            self.open_project_workflow(sequencer, undo_record, open, folder.as_deref(), &filename);
            return;
        }
        match project::copy_project_in(path) {
            Err(e) => self.open_failed(&project::file_stem(path), e),
            Ok(filename) => {
                self.ui_event_tx.send(UiEvent::ProjectFilesChanged).ok();
                self.send_status_ui_event(format!("Copied {filename} into Projects"));
                self.open_project_workflow(sequencer, undo_record, open, None, &filename);
            }
        }
    }

    /// Opens the template at `path` as a new project: read, stripped of
    /// anything a template doesn't keep (`ProjectData::into_template`: clips,
    /// mutes, the loop region), then staged or applied like any project, but
    /// untitled — with no name or file, so ⌘S
    /// asks where to save and the template is never written to. On failure
    /// the footer says why and the open project stays.
    fn new_from_template_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        path: &Path,
    ) {
        let name = project::file_stem(path);
        match project::load_project_file(path) {
            Err(e) => self.open_failed(&name, e),
            Ok(data) => {
                let project = StagedProject {
                    data: data.into_template(),
                    title: name,
                    file: None,
                    recovered: None,
                };
                self.open_read_project_workflow(sequencer, undo_record, open, project);
            }
        }
    }

    /// A project or template `name` that couldn't be read: the footer says
    /// why, and the open project stays.
    fn open_failed(&self, name: &str, e: impl Display) {
        self.send_status_ui_event(format!("Could not open {name}: {e}"));
        self.set_view_state(ViewState::Arranger);
    }

    /// A project just read from disk: without plugins, applies it at once.
    /// With, stops the transport, keeps it in `open.staged` and asks the view to
    /// load its plugins (`UiEvent::StageProject`) while the open project
    /// stays as it is; the view's `ApplyStagedProject` then applies it
    /// (`apply_staged_project_workflow`).
    fn open_read_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        project: StagedProject,
    ) {
        let specs = project.data.instrument_specs();
        if specs.is_empty() {
            open.staged = Some(project);
            self.apply_staged_project_workflow(sequencer, undo_record, open);
            return;
        }
        self.send_transport(TransportCommand::Stop);
        self.ui_event_tx
            .send(UiEvent::StageProject {
                name: project.title.clone(),
                specs,
            })
            .ok();
        open.staged = Some(project);
        self.request_repaint();
    }

    /// Applies the project in `open.staged`, if any — at once, or once the view
    /// has loaded its plugins (`open_read_project_workflow`). No
    /// unsaved-changes check: it ran before the project was read, and the
    /// view takes no input while a staged one loads. A project restored from
    /// the recovery copy takes no baseline (`SavedProject::clear_baseline`),
    /// ends `recovery`'s hold on the copy and says so in the footer.
    pub(super) fn apply_staged_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
    ) {
        let Some(StagedProject {
            data,
            file,
            recovered,
            ..
        }) = open.staged.take()
        else {
            return;
        };
        let (folder, filename) = file.unzip();
        self.load_project_workflow(
            sequencer,
            undo_record,
            &mut open.saved,
            data,
            filename,
            folder.flatten(),
        );
        if let Some(message) = recovered {
            open.saved.clear_baseline();
            open.recovery.restored(sequencer, &open.saved);
            self.send_status_ui_event(message);
        }
    }

    /// The launch dialog's answer: Restore stages the recovery copy like any
    /// project read from disk, and the copy stays until the project is saved
    /// (`410-crash-recovery.md` decision 13); Discard deletes it.
    pub(super) fn answer_recovery_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        restore: bool,
    ) {
        match open.recovery.take_offer() {
            Some(copy) if restore => {
                self.open_read_project_workflow(sequencer, undo_record, open, copy.into_staged());
            }
            Some(_) => open.recovery.discard(),
            None => {}
        }
    }

    /// Runs `action` — unless a save is on its way, which holds it in
    /// `open.held` until the outcome is in (`project_written_workflow`,
    /// `420-large-projects.md` decision 6), or it is Finder opening the
    /// project already open
    /// with unsaved changes, which does nothing, or `discard_changes` is false and the project
    /// changed since `saved`: then the view is asked to prompt
    /// (`UiEvent::UnsavedChanges`) and nothing else happens. A new project or
    /// a load becomes the new `saved`; quitting tells the view to close the
    /// window (`UiEvent::QuitApproved`), once the copy is deleted and the
    /// `"storage"` thread finished (`OpenProject::finish`), since the process ends soon after. Both replies wake the
    /// view, which may be waiting on no input of its own (a window close). An
    /// open with plugins is held in `open.staged` until the view has loaded
    /// them.
    pub(super) fn project_action_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        open: &mut OpenProject,
        action: &ProjectAction,
        discard_changes: bool,
    ) {
        if open.is_saving() {
            open.held.push((action.clone(), discard_changes));
            return;
        }
        // As a macOS document does, the project already open with unsaved
        // changes stays as it is when Finder opens it again: no reload, so no
        // prompt offering to throw them away. With none it reloads, which
        // picks up a file changed on disk. The browser's open always reloads,
        // a way to revert.
        if let ProjectAction::OpenFile { path } = action
            && let Some((folder, name)) = project::library_project(path)
            && open.saved.origin() == Some(&ProjectOrigin::new(folder.as_deref(), &name))
            && open.saved.has_changes(sequencer)
        {
            return;
        }
        if !discard_changes && open.saved.has_changes(sequencer) {
            self.ui_event_tx
                .send(UiEvent::UnsavedChanges {
                    action: action.clone(),
                })
                .ok();
            self.request_repaint();
            return;
        }
        match action {
            ProjectAction::New => {
                self.load_project_workflow(
                    sequencer,
                    undo_record,
                    &mut open.saved,
                    ProjectData::default(),
                    None,
                    None,
                );
            }
            ProjectAction::Open { filename, folder } => {
                self.open_project_workflow(
                    sequencer,
                    undo_record,
                    open,
                    folder.as_deref(),
                    filename,
                );
            }
            ProjectAction::OpenFile { path } => {
                self.open_file_workflow(sequencer, undo_record, open, path);
            }
            ProjectAction::NewFromTemplate { path } => {
                self.new_from_template_workflow(sequencer, undo_record, open, path);
            }
            ProjectAction::Quit => {
                open.finish();
                self.ui_event_tx.send(UiEvent::QuitApproved).ok();
                self.request_repaint();
            }
        }
    }

    /// After a load or a new project: re-selects the clip under the cursor as
    /// the lead clip and always announces it. `ProjectLoaded` makes the view
    /// drop its lead clip, and neither the track select (a no-op when track 0
    /// was already selected) nor a lead that happens to keep its id would
    /// tell it again — so a docked clip panel showed "no clip" until the
    /// cursor moved. See `archive/210-docked-clip-panel.md`.
    fn announce_lead_clip_after_load(&self, sequencer: &mut Sequencer) {
        self.sync_clip_selection_to_cursor_workflow(sequencer);
        self.send_lead_clip_changed_ui_event(sequencer.current_clip_view());
    }

    /// `⌘/Ctrl+⇧+E`: writes the lead clip to a `.mid` in the `Clips` folder
    /// (a shown browser re-reads the tree) and says so in the footer — the
    /// file name, why nothing was exported, or the write error. Never silent.
    pub(super) fn export_clip_workflow(
        &self,
        sequencer: &Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        name: &str,
    ) {
        let message = match sequencer.export_lead_clip(time_bounds) {
            Err(ExportRefusal::SeveralClips) => "Select a single clip to export".to_owned(),
            Err(ExportRefusal::NoClip) => "No clip to export".to_owned(),
            Ok(export) => match project::save_clip_export(
                &export.bytes,
                name,
                export.track_idx,
                export.start_tick,
                sequencer.meter(),
            ) {
                Ok(name) => {
                    self.ui_event_tx.send(UiEvent::ProjectFilesChanged).ok();
                    format!("Exported {name}")
                }
                Err(e) => format!("Export failed: {e}"),
            },
        };
        self.send_status_ui_event(message);
    }

    /// Puts an imported `.mid`'s clip on `target` — or, `None`, the selected
    /// track at the cursor — as one undoable step (`PasteClipsEdit::importing`)
    /// and says so in the footer. Never silent: with no track to put it on,
    /// the footer says that instead.
    pub(super) fn import_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        clip: &Clip,
        name: &str,
        target: Option<(usize, i32)>,
    ) {
        let target = target.or_else(|| {
            sequencer
                .selected_track_index()
                .map(|track_idx| (track_idx, sequencer.cursor_tick()))
        });
        let edit = target.and_then(|(track_idx, tick)| {
            PasteClipsEdit::importing(sequencer, track_idx, tick, clip.clone())
        });
        let message = if self.record_edit(sequencer, undo_record, edit) {
            format!("Imported {name}")
        } else {
            "Select a track to import onto".to_owned()
        };
        self.send_status_ui_event(message);
    }
}

/// What the `"sequencer"` thread keeps about the open project as a file,
/// beside the sequencer and the undo record: what it was last saved as, what
/// is on its way in or out, and its crash recovery copy.
pub(crate) struct OpenProject {
    /// What the unsaved-changes check compares against.
    pub(crate) saved: SavedProject,
    /// A project read from disk, waiting on the view to load its plugins
    /// (`130-plugin-host.md` § Project persistence).
    pub(crate) staged: Option<StagedProject>,
    /// The crash recovery copy.
    pub(crate) recovery: RecoveryKeeper,
    /// The way to the `"storage"` thread, for saves.
    pub(super) storage: Storage,
    /// Saves handed to the `"storage"` thread, a template's too, their
    /// outcomes still to come, oldest first (`420-large-projects.md`
    /// decision 5).
    pub(super) saving: VecDeque<PendingSave>,
    /// Project actions that came while a save was on its way, with their
    /// `discard_changes`, to run once the last outcome is in (decision 6).
    pub(super) held: Vec<(ProjectAction, bool)>,
}

impl OpenProject {
    /// The project `saved`, nothing staged or on its way, its copy kept by
    /// `recovery` and its saves going to `storage`.
    pub(crate) fn new(saved: SavedProject, recovery: RecoveryKeeper, storage: Storage) -> Self {
        OpenProject {
            saved,
            staged: None,
            recovery,
            storage,
            saving: VecDeque::new(),
            held: Vec::new(),
        }
    }

    /// Once per pass of the tick arm, `at` being the tick's instant: the
    /// recovery check, when one is due and no save is on its way. A save
    /// deletes the copy once its file is safe, so a copy written behind it
    /// would only be deleted again; the check runs on the first tick after
    /// the outcome instead.
    pub(crate) fn tick(&mut self, at: Instant, sequencer: &Sequencer) {
        if !self.is_saving() {
            self.recovery.tick(at, sequencer, &self.saved);
        }
    }

    /// Whether a save of the project is on its way to the disk. A
    /// template's isn't counted: it changes neither `saved` nor the copy, so
    /// nothing has to wait for it.
    pub(super) fn is_saving(&self) -> bool {
        self.saving
            .iter()
            .any(|pending| matches!(pending, PendingSave::Project { .. }))
    }

    /// A clean quit: the recovery copy deleted, and the `"storage"` thread
    /// waited for until it has run what it has and ended
    /// (`410-crash-recovery.md` decision 13).
    pub(super) fn finish(&mut self) {
        self.recovery.finish();
        self.storage.finish();
    }
}

/// The project as it was last saved or loaded, as a
/// [`fingerprint`](project::fingerprint) — the `"sequencer"` thread keeps one
/// beside the undo record, and the unsaved-changes check and the crash
/// recovery check compare the live project against it. Comparing what would
/// be written, not counting edits, makes
/// every change count (volume, the loop region, a track's plugin — not just
/// undoable edits; plugin presets aside, which `fingerprint` leaves out) and
/// undoing back to the saved state count as no change.
/// See `060-persistence.md` § Unsaved changes.
pub(crate) struct SavedProject {
    /// The saved project's fingerprint; `None` for a project restored from
    /// the crash recovery copy, which matches nothing until it is saved.
    fingerprint: Option<u64>,
    /// Where it is on disk; `None` for a project never saved (a new one, a
    /// template opened). The agent bridge's `get_project` names it, and the
    /// recovery copy records it.
    origin: Option<ProjectOrigin>,
}

impl SavedProject {
    /// `sequencer`'s project as it stands, at `origin`, taken as saved —
    /// after a save, a load, a new project, and at startup.
    pub(crate) fn of(sequencer: &Sequencer, origin: Option<ProjectOrigin>) -> Self {
        SavedProject {
            fingerprint: Some(project::fingerprint(sequencer)),
            origin,
        }
    }

    /// `fingerprint`'s project, at `origin`, taken as saved: a save's
    /// outcome, the project as it was at the handover.
    pub(super) fn saved_as(fingerprint: u64, origin: ProjectOrigin) -> Self {
        SavedProject {
            fingerprint: Some(fingerprint),
            origin: Some(origin),
        }
    }

    /// Drops the baseline of a project just restored from the recovery copy:
    /// it counts as changed until it is saved, so the quit prompt asks.
    /// Comparing with the file on disk wouldn't do: a copy that differs only
    /// in a plugin blob has the same fingerprint, and the next check would
    /// delete it (`410-crash-recovery.md` decision 11).
    fn clear_baseline(&mut self) {
        self.fingerprint = None;
    }

    /// Follows `change`, made by the browser on disk, if it touched this
    /// project: moved or renamed, the origin names it where it is; deleted
    /// (it, or its folder), the project goes on untitled and counts as
    /// changed until it is saved, since the only copy of it on disk is in
    /// the Trash and the quit prompt has to ask. Whether it touched it.
    pub(super) fn follow(&mut self, change: &LibraryChange) -> bool {
        let Some(origin) = &mut self.origin else {
            return false;
        };
        match origin.follow(change) {
            Followed::Untouched => false,
            Followed::Moved => true,
            Followed::Gone => {
                self.origin = None;
                self.clear_baseline();
                true
            }
        }
    }

    /// The project's file name, `None` before it is first saved.
    pub(crate) fn name(&self) -> Option<&str> {
        self.origin.as_ref().map(|origin| origin.name.as_str())
    }

    /// Where the project is on disk, `None` before it is first saved.
    pub(super) fn origin(&self) -> Option<&ProjectOrigin> {
        self.origin.as_ref()
    }

    /// Whether a project with `fingerprint` would save as this one did.
    pub(super) fn is_fingerprint(&self, fingerprint: u64) -> bool {
        self.fingerprint == Some(fingerprint)
    }

    /// Whether `sequencer`'s project would save differently from this one.
    pub(super) fn has_changes(&self, sequencer: &Sequencer) -> bool {
        !self.is_fingerprint(project::fingerprint(sequencer))
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::event_handlers::test_harness::harness;
    use crate::core::project::{ProjectData, StagedProject, TrackOutputData};
    use crate::core::sequencer::SequencerEdit;
    use crate::core::transport::TransportCommand;
    use crate::models::track::{InstrumentRef, TrackOutput};
    use crate::view::display::UiEvent;

    /// The plugin `project(true)` puts on track 1.
    fn synth() -> InstrumentRef {
        InstrumentRef {
            bundle_path: "/Synth.clap".into(),
            plugin_id: "synth".to_owned(),
            display_name: "Synth".to_owned(),
            state: Vec::new().into(),
        }
    }

    /// A two-track project at 90 BPM, with [`synth`] on track 1 when
    /// `plugin`.
    fn project(plugin: bool) -> StagedProject {
        let mut data = ProjectData {
            tempo_us: 666_667,
            ..ProjectData::default()
        };
        data.tracks.truncate(2);
        if plugin {
            data.tracks[1].output = TrackOutputData::Instrument {
                bundle_path: "/Synth.clap".to_owned(),
                plugin_id: "synth".to_owned(),
                display_name: "Synth".to_owned(),
                state: String::new(),
            };
        }
        StagedProject {
            data,
            title: "Song".to_owned(),
            file: Some((None, "Song".to_owned())),
            recovered: None,
        }
    }

    /// An open with plugins leaves the open project alone, keeps the read
    /// one and asks the view for its plugins; applied, its `ProjectLoaded`
    /// names them.
    #[test]
    fn a_project_with_plugins_is_staged_before_it_is_applied() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();
        let tempo = h.sequencer.tempo_us();

        h.handlers.open_read_project_workflow(
            &mut h.sequencer,
            &mut record,
            &mut h.open,
            project(true),
        );
        assert_eq!(h.sequencer.tempo_us(), tempo);
        assert_eq!(h.sequencer.tracks().len(), 4);
        assert!(h.open.staged.is_some());
        assert!(
            h.transport_commands
                .try_iter()
                .any(|cmd| matches!(cmd, TransportCommand::Stop))
        );
        let asked = h.ui_events.try_iter().find_map(|event| match event {
            UiEvent::StageProject { name, specs } => Some((name, specs)),
            _ => None,
        });
        assert_eq!(asked, Some(("Song".to_owned(), vec![(1, synth())])));

        h.handlers
            .apply_staged_project_workflow(&mut h.sequencer, &mut record, &mut h.open);
        assert!(h.open.staged.is_none());
        assert_eq!(h.sequencer.tempo_us(), 666_667);
        assert_eq!(h.sequencer.tracks().len(), 2);
        let loaded = h.ui_events.try_iter().find_map(|event| match event {
            UiEvent::ProjectLoaded { instruments, .. } => Some(instruments),
            _ => None,
        });
        assert_eq!(loaded, Some(vec![(1, synth())]));
        assert!(matches!(
            h.sequencer.tracks()[1].output(),
            TrackOutput::Instrument(_)
        ));
    }

    /// Without plugins there is nothing to wait for: applied at once.
    #[test]
    fn a_project_without_plugins_is_applied_at_once() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();

        h.handlers.open_read_project_workflow(
            &mut h.sequencer,
            &mut record,
            &mut h.open,
            project(false),
        );
        assert!(h.open.staged.is_none());
        assert_eq!(h.sequencer.tempo_us(), 666_667);
        assert!(
            !h.ui_events
                .try_iter()
                .any(|event| matches!(event, UiEvent::StageProject { .. }))
        );
    }
}
