//! Project persistence: serializable DTOs (`dto`), `.stev` filesystem layout
//! (`storage`), the `.mid` reader / writer of the MIDI clip import and
//! export (`smf`), and the [`ProjectAction`]s the unsaved-changes prompt
//! guards (`action`). See `060-persistence.md`.

mod action;
mod dto;
mod smf;
mod storage;

pub(crate) use action::ProjectAction;
pub(crate) use dto::ProjectData;
#[cfg(all(test, debug_assertions))]
pub(crate) use dto::{ClipData, EventData};
pub(crate) use smf::write_smf;
pub(crate) use storage::{
    FolderListing, delete_project, is_midi_file, list_folder, list_project_folders, load_midi_clip,
    load_project, midi_file_path, project_exists, project_name_from_input, rename_midi_file,
    rename_project, save_clip_export, save_project,
};
