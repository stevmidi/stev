//! Project persistence: serializable DTOs (`dto`), `.stev` filesystem layout
//! (`storage`), the `.mid` reader / writer of the MIDI clip import and
//! export (`smf`), the [`ProjectAction`]s the unsaved-changes prompt
//! guards (`action`), and the [`StagedProject`] an open with plugins hands
//! the view first (`staged`). See `060-persistence.md`.

mod action;
mod dto;
mod smf;
mod staged;
mod storage;

pub(crate) use action::ProjectAction;
pub(crate) use dto::ProjectData;
#[cfg(test)]
pub(crate) use dto::TrackOutputData;
#[cfg(all(test, debug_assertions))]
pub(crate) use dto::{ClipData, EventData};
pub(crate) use smf::write_smf;
pub(crate) use staged::StagedProject;
#[cfg(target_os = "macos")]
pub(crate) use storage::is_project_file;
pub(crate) use storage::{
    MIX_FILE_NAME, clip_path, copy_project_in, create_audio_export_dir, delete_project,
    is_midi_file, library_project, list_clips, list_project_folders, list_projects, load_midi_clip,
    load_project, project_exists, project_name_from_input, rename_clip, rename_project,
    save_clip_export, save_project, stem_file_name,
};
