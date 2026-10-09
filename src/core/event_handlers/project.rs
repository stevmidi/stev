//! `EventHandlers` workflows for project save / load / new-project and the
//! unsaved-changes check ([`SavedProject`]), and the MIDI clip export and
//! import. See `060-persistence.md`.

use undo::Record;

use crate::{
    core::{
        config::MAX_TRACKS,
        input_event::TimeSelectionRect,
        project::{self, ProjectAction, ProjectData},
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
    /// out `ProjectLoaded` + the instrument
    /// list, and returns to the Arranger with track 0 selected. A project with
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
        self.ui_event_tx
            .send(UiEvent::ProjectLoaded {
                clips,
                filename,
                folder,
            })
            .ok();
        self.emit_track_instruments(sequencer);
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
    /// `load_project_workflow`; on failure say why in the footer and fall
    /// back to the Arranger, keeping the current project, its undo history
    /// and `saved`.
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
                self.load_project_workflow(
                    sequencer,
                    undo_record,
                    saved,
                    data,
                    Some(filename.to_owned()),
                    folder.map(str::to_owned),
                );
            }
        }
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
    /// `saved`, and says how it went in the footer.
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

    /// `⌘/Ctrl+⇧+E`: writes the lead clip to a `.mid` next to the project and
    /// says so in the footer — the file name, why nothing was exported, or
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
                Ok(name) => format!("Exported {name}"),
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
