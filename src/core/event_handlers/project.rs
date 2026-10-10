//! `EventHandlers` workflows for project save / load / new-project and the
//! unsaved-changes check ([`SavedProject`]), and the MIDI clip export and
//! import. See `060-persistence.md`.

use undo::Record;

use crate::{
    core::{
        config::MAX_TRACKS,
        input_event::TimeSelectionRect,
        project::{self, ProjectAction, ProjectData, StagedProject},
        sequencer::{ExportRefusal, PasteClipsEdit, SequencerEdit},
    },
    models::{clip::Clip, track::TrackOutput},
};

use super::*;

impl EventHandlers {
    /// Tells `Display` which tracks host CLAP instruments, so it can rebuild its
    /// editors after a project load / new-project.
    fn emit_track_instruments(&self, sequencer: &Sequencer) {
        let specs: Vec<_> = sequencer
            .tracks()
            .iter()
            .filter_map(|track| match track.output() {
                TrackOutput::Instrument(r) => Some((track.slot(), r.clone())),
                TrackOutput::MidiOut { .. } => None,
            })
            .collect();
        self.ui_event_tx
            .send(UiEvent::TrackInstrumentsChanged { specs })
            .ok();
    }

    /// Applies already-loaded `ProjectData` to the sequencer (`filename` is
    /// `None` for a new project; `folder` is where it was loaded from), fans
    /// out the instrument list + `ProjectLoaded`, and returns to the Arranger
    /// with track 0 selected. The instrument list goes first, so a staged
    /// load's plugins are swapped in no later than the new clips arrive. A project with
    /// more than `MAX_TRACKS` tracks loads the first ones and the footer says so.
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
        *saved = SavedProject::of(sequencer);
        self.emit_track_instruments(sequencer);
        self.ui_event_tx
            .send(UiEvent::ProjectLoaded {
                clips,
                filename,
                folder,
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
        saved: &mut SavedProject,
        folder: Option<&str>,
        filename: &str,
    ) {
        match project::load_project(folder, filename) {
            Err(e) => {
                self.send_status_ui_event(format!("Could not open {filename}: {e}"));
                self.set_view_state(ViewState::Arranger);
            }
            Ok(data) => {
                let project = StagedProject {
                    data,
                    filename: filename.to_owned(),
                    folder: folder.map(str::to_owned),
                };
                self.open_read_project_workflow(sequencer, undo_record, saved, project);
            }
        }
    }

    /// A project just read from disk: with plugins on its tracks, stops the
    /// transport and stages it with the view (`UiEvent::StageProject`), which
    /// loads them while the open project stays as it is and sends it back to
    /// `apply_staged_project_workflow`; without, applies it at once.
    fn open_read_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        saved: &mut SavedProject,
        project: StagedProject,
    ) {
        if project.data.instrument_specs().is_empty() {
            self.apply_staged_project_workflow(sequencer, undo_record, saved, project);
            return;
        }
        self.send_transport(TransportCommand::Stop);
        self.ui_event_tx
            .send(UiEvent::StageProject(Box::new(project)))
            .ok();
        self.request_repaint();
    }

    /// Applies a project read from disk — at once, or once the view has
    /// loaded the plugins of a staged one (`open_read_project_workflow`). No
    /// unsaved-changes check: it ran before the project was read, and the
    /// view takes no input while a staged one loads.
    pub(super) fn apply_staged_project_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        saved: &mut SavedProject,
        project: StagedProject,
    ) {
        let StagedProject {
            data,
            filename,
            folder,
        } = project;
        self.load_project_workflow(sequencer, undo_record, saved, data, Some(filename), folder);
    }

    /// Runs `action` — unless `discard_changes` is false and the project
    /// changed since `saved`: then the view is asked to prompt
    /// (`UiEvent::UnsavedChanges`) and nothing else happens. A new project or
    /// a load becomes the new `saved`; quitting tells the view to close the
    /// window (`UiEvent::QuitApproved`). Both replies wake the view, which
    /// may be waiting on no input of its own (a window close).
    pub(super) fn project_action_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        saved: &mut SavedProject,
        action: &ProjectAction,
        discard_changes: bool,
    ) {
        if !discard_changes && saved.has_changes(sequencer) {
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
                    saved,
                    ProjectData::default(),
                    None,
                    None,
                );
            }
            ProjectAction::Open { filename, folder } => {
                self.open_project_workflow(
                    sequencer,
                    undo_record,
                    saved,
                    folder.as_deref(),
                    filename,
                );
            }
            ProjectAction::Quit => {
                self.ui_event_tx.send(UiEvent::QuitApproved).ok();
                self.request_repaint();
            }
        }
    }

    /// Writes the project to `folder/filename.stev`, which on success becomes
    /// `saved` and has the browser re-read the tree, and says how it went in
    /// the footer.
    pub(super) fn save_project_workflow(
        &self,
        sequencer: &Sequencer,
        saved: &mut SavedProject,
        folder: Option<&str>,
        filename: &str,
    ) {
        let data = ProjectData::from_sequencer(sequencer);
        let message = match project::save_project(&data, folder, filename) {
            Ok(()) => {
                *saved = SavedProject {
                    fingerprint: data.fingerprint(),
                };
                self.ui_event_tx.send(UiEvent::ProjectFilesChanged).ok();
                format!("Saved {filename}")
            }
            Err(e) => format!("Could not save {filename}: {e}"),
        };
        self.send_status_ui_event(message);
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

    /// `⌘/Ctrl+⇧+E`: writes the lead clip to a `.mid` next to the project
    /// (a shown browser re-reads the tree) and says so in the footer — the file name, why nothing was exported, or
    /// the write error. Never silent.
    pub(super) fn export_clip_workflow(
        &self,
        sequencer: &Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        folder: Option<&str>,
        name: &str,
    ) {
        let message = match sequencer.export_lead_clip(time_bounds) {
            Err(ExportRefusal::SeveralClips) => "Select a single clip to export".to_owned(),
            Err(ExportRefusal::NoClip) => "No clip to export".to_owned(),
            Ok(export) => match project::save_clip_export(
                &export.bytes,
                folder,
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

/// The project as it was last saved or loaded, as a
/// [`ProjectData::fingerprint`] — the `"sequencer"` thread keeps one beside
/// the undo record, and the unsaved-changes check compares the live project
/// against it. Comparing what would be written, not counting edits, makes
/// every change count (volume, the loop region, a track's plugin — not just
/// undoable edits; plugin presets aside, which `fingerprint` leaves out) and
/// undoing back to the saved state count as no change.
/// See `060-persistence.md` § Unsaved changes.
pub(crate) struct SavedProject {
    /// The saved project's fingerprint.
    fingerprint: u64,
}

impl SavedProject {
    /// `sequencer`'s project as it stands, taken as saved — after a load, a
    /// new project, and at startup.
    pub(crate) fn of(sequencer: &Sequencer) -> Self {
        SavedProject {
            fingerprint: ProjectData::from_sequencer(sequencer).fingerprint(),
        }
    }

    /// Whether `sequencer`'s project would save differently from this one.
    fn has_changes(&self, sequencer: &Sequencer) -> bool {
        ProjectData::from_sequencer(sequencer).fingerprint() != self.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::event_handlers::test_harness::harness;
    use crate::core::project::{ProjectData, StagedProject};
    use crate::core::sequencer::SequencerEdit;
    use crate::core::transport::TransportCommand;
    use crate::models::track::{InstrumentRef, TrackOutput};
    use crate::view::display::UiEvent;

    /// A two-track project at 90 BPM, with a plugin on track 1 when
    /// `plugin`.
    fn project(plugin: bool) -> StagedProject {
        let mut data = ProjectData {
            tempo_us: 666_667,
            ..ProjectData::default()
        };
        data.tracks.truncate(2);
        if plugin {
            data.tracks[1].output = serde_json::from_str(
                r#"{"type":"Instrument","bundle_path":"/Synth.clap","plugin_id":"synth","display_name":"Synth"}"#,
            )
            .unwrap();
        }
        StagedProject {
            data,
            filename: "Song".to_owned(),
            folder: None,
        }
    }

    /// An open with plugins leaves the open project alone and hands the
    /// read one to the view; handed back, it is applied, the plugin list
    /// reaching the view before the new clips.
    #[test]
    fn a_project_with_plugins_is_staged_before_it_is_applied() {
        let mut h = harness();
        let mut record: Record<SequencerEdit> = Record::new();
        let tempo = h.sequencer.tempo_us();

        h.handlers.open_read_project_workflow(
            &mut h.sequencer,
            &mut record,
            &mut h.saved,
            project(true),
        );
        assert_eq!(h.sequencer.tempo_us(), tempo);
        assert_eq!(h.sequencer.tracks().len(), 4);
        assert!(
            h.transport_commands
                .try_iter()
                .any(|cmd| matches!(cmd, TransportCommand::Stop))
        );
        let staged = h
            .ui_events
            .try_iter()
            .find_map(|event| match event {
                UiEvent::StageProject(project) => Some(project),
                _ => None,
            })
            .expect("staged with the view");
        assert_eq!(staged.filename, "Song");

        h.handlers.apply_staged_project_workflow(
            &mut h.sequencer,
            &mut record,
            &mut h.saved,
            *staged,
        );
        assert_eq!(h.sequencer.tempo_us(), 666_667);
        assert_eq!(h.sequencer.tracks().len(), 2);
        let order: Vec<_> = h
            .ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::TrackInstrumentsChanged { specs } => Some(Some(specs)),
                UiEvent::ProjectLoaded { .. } => Some(None),
                _ => None,
            })
            .collect();
        let [Some(specs), None] = order.as_slice() else {
            panic!("plugin list, then the project: {order:?}");
        };
        assert_eq!(
            specs.as_slice(),
            [(
                1,
                InstrumentRef {
                    bundle_path: "/Synth.clap".into(),
                    plugin_id: "synth".to_owned(),
                    display_name: "Synth".to_owned(),
                    state: Vec::new(),
                }
            )]
        );
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
            &mut h.saved,
            project(false),
        );
        assert_eq!(h.sequencer.tempo_us(), 666_667);
        assert!(
            !h.ui_events
                .try_iter()
                .any(|event| matches!(event, UiEvent::StageProject(_)))
        );
    }
}
