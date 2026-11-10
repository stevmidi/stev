//! Project persistence: serializable DTOs (`dto`), `.stev` filesystem layout
//! (`storage`), the `.mid` reader / writer of the MIDI clip import and
//! export (`smf`), the recent projects list (`recent`), the [`ProjectAction`]s the unsaved-changes prompt
//! guards (`action`), the [`StagedProject`] an open with plugins hands
//! the view first (`staged`), the [`fingerprint()`] that says what counts as a
//! change (`fingerprint`), the crash recovery copy on disk (`recovery`), and
//! the `"storage"` thread's jobs, saves and the copy's (`writer`).
//! See `060-persistence.md`, `410-crash-recovery.md` and
//! `420-large-projects.md`.

mod action;
mod dto;
mod fingerprint;
mod recent;
mod recovery;
#[cfg(test)]
mod recovery_timing;
mod smf;
mod staged;
mod storage;
#[cfg(test)]
mod stress;
mod writer;

pub(crate) use action::ProjectAction;
#[cfg(test)]
pub(crate) use dto::TrackOutputData;
#[cfg(all(test, debug_assertions))]
pub(crate) use dto::{ClipData, EventData};
pub(crate) use dto::{ProjectData, ProjectSnapshot};
pub(crate) use fingerprint::{FingerprintCache, fingerprint};
pub(crate) use recent::{follow_recent, note_recent, remove_recent};
pub(crate) use recovery::{RecoveryCopy, read_recovery_copy, recovery_path, take_recovery_lock};
pub(crate) use smf::write_smf;
pub(crate) use staged::StagedProject;
#[cfg(target_os = "macos")]
pub(crate) use storage::is_project_file;
pub(crate) use storage::{
    Followed, LibraryChange, MIX_FILE_NAME, ProjectOrigin, UNTITLED, clip_path, copy_project,
    copy_project_in, create_audio_export_dir, create_project_folder, delete_clip, delete_project,
    delete_project_folder, delete_template, file_stem, is_midi_file, is_template_file,
    library_project, list_clips, list_project_folders, list_projects, list_templates,
    load_midi_clip, load_project, load_project_file, project_exists,
    project_folder_name_from_input, project_name_from_input, rename_clip, rename_project,
    rename_project_folder, rename_template, save_clip_export, stem_file_name, template_exists,
    template_path,
};
#[cfg(test)]
pub(crate) use stress::{STRESS_BLOB_BYTES, STRESS_EVENTS, fill_stress_project};
pub(crate) use writer::{SaveOutcome, SaveTarget, StorageJob, run_storage_jobs, write_save};
