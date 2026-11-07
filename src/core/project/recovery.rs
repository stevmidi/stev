//! The crash recovery copy on disk: one slot in the config directory, holding
//! the open project and where it came from, written and deleted by the
//! `"storage"` thread (`writer`). The copy is never a project file and never
//! written over one. See `410-crash-recovery.md`.

use std::error::Error;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::core::paths::{RECOVERY_FILE_NAME, RECOVERY_LOCK_FILE_NAME, config_dir};

use super::dto::{ProjectData, ProjectSnapshot};
use super::staged::StagedProject;
use super::storage::{ProjectOrigin, UNTITLED, project_exists, replace_file};

/// The copy, on disk.
#[derive(Serialize, Deserialize)]
pub(crate) struct RecoveryCopy {
    /// Where the project came from; `None` for one never saved.
    origin: Option<ProjectOrigin>,
    /// The project.
    project: ProjectData,
}

impl RecoveryCopy {
    /// A copy of `project` from `origin`, for tests that need one without a
    /// disk.
    #[cfg(test)]
    pub(crate) fn new(origin: Option<ProjectOrigin>, project: ProjectData) -> Self {
        RecoveryCopy { origin, project }
    }

    /// The project's name, `None` for one never saved: what the launch dialog
    /// asks about.
    pub(crate) fn name(&self) -> Option<&str> {
        self.origin.as_ref().map(|origin| origin.name.as_str())
    }

    /// The project to restore, through the usual staged load. It keeps its
    /// name and folder while its file is there, and comes back untitled if
    /// the file has gone (`410-crash-recovery.md` decision 10).
    pub(crate) fn into_staged(self) -> StagedProject {
        self.staged_with(|origin| project_exists(origin.folder.as_deref(), &origin.name))
    }

    /// [`into_staged`](Self::into_staged), with `exists` saying whether a
    /// project's file is on disk.
    fn staged_with(self, exists: impl FnOnce(&ProjectOrigin) -> bool) -> StagedProject {
        let (title, recovered) = match self.name() {
            Some(name) => (name.to_owned(), format!("Restored {name}")),
            None => (
                UNTITLED.to_owned(),
                "Restored your unsaved new project".to_owned(),
            ),
        };
        StagedProject {
            data: self.project,
            title,
            file: self
                .origin
                .filter(exists)
                .map(|origin| (origin.folder, origin.name)),
            recovered: Some(recovered),
        }
    }
}

/// The copy's path.
pub(crate) fn recovery_path() -> PathBuf {
    config_dir().join(RECOVERY_FILE_NAME)
}

/// The copy at `path`, `None` if there is none. For the app's startup path,
/// holding the lock: a copy there then means a crash, and one that won't load
/// is the caller's to report (`410-crash-recovery.md` decision 15).
pub(crate) fn read_recovery_copy(path: &Path) -> Result<Option<RecoveryCopy>, Box<dyn Error>> {
    match fs::read_to_string(path) {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Writes the copy to `path`, replacing the one there only once it is whole.
pub(super) fn write_copy(
    path: &Path,
    snapshot: ProjectSnapshot,
    origin: Option<ProjectOrigin>,
) -> io::Result<()> {
    let copy = RecoveryCopy {
        origin,
        project: snapshot.encoded(),
    };
    let json = serde_json::to_string(&copy).map_err(io::Error::other)?;
    drop(copy);
    replace_file(path, &json)
}

/// Deletes the copy at `path`; there being none is no failure.
pub(super) fn delete_copy(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Takes the lock on the file at `path`, made if absent; `None` if another
/// process holds it or it can't be taken.
fn lock_at(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path);
    let file = match file {
        Ok(file) => file,
        Err(e) => {
            warn!("No crash recovery: could not open its lock: {e}");
            return None;
        }
    };
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => {
            info!("No crash recovery: another instance of Stev holds it");
            None
        }
        Err(TryLockError::Error(e)) => {
            warn!("No crash recovery: could not take its lock: {e}");
            None
        }
    }
}

/// The lock that lets this instance keep the recovery copy, or `None` if
/// another instance has it: this one then neither offers nor writes. The
/// system lets go when the process dies, however it dies, so a copy with a
/// free lock means a crash. For the app's startup path alone: a `stev --mcp`
/// taking it could turn recovery off (`410-crash-recovery.md` decision 16).
pub(crate) fn take_recovery_lock() -> Option<File> {
    lock_at(&config_dir().join(RECOVERY_LOCK_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use crate::core::paths::scratch_dir;
    use crate::core::sequencer::test_support::{stub_instrument, test_sequencer};
    use crate::models::track::TrackOutput;

    use super::*;

    /// Writes a project at `tempo_us`, from `origin`, as the copy at `path`.
    fn write(tempo_us: i32, origin: Option<ProjectOrigin>, path: &Path) {
        let sequencer = test_sequencer();
        sequencer.set_tempo(tempo_us);
        write_copy(path, ProjectSnapshot::of(&sequencer), origin).unwrap();
    }

    /// The copy at `path`, as a launch reads it.
    fn read_copy(path: &Path) -> RecoveryCopy {
        read_recovery_copy(path).unwrap().unwrap()
    }

    /// The copy reads back as the project it was, with where it came from.
    #[test]
    fn a_copy_holds_the_project_and_its_origin() {
        let dir = scratch_dir("recovery-a_copy_holds_the_project_and_its_origin");
        let path = dir.join(RECOVERY_FILE_NAME);
        let origin = ProjectOrigin::new(Some("Songs"), "Song");
        write(400_000, Some(origin.clone()), &path);
        let copy = read_copy(&path);
        assert_eq!(copy.origin, Some(origin));
        assert_eq!(copy.project.tempo_us, 400_000);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A project never saved has no origin to record.
    #[test]
    fn a_copy_of_a_project_never_saved_has_no_origin() {
        let dir = scratch_dir("recovery-a_copy_of_a_project_never_saved_has_no_origin");
        let path = dir.join(RECOVERY_FILE_NAME);
        write(400_000, None, &path);
        assert_eq!(read_copy(&path).origin, None);
        fs::remove_dir_all(dir).unwrap();
    }

    /// The blobs reach the copy encoded as a save encodes them.
    #[test]
    fn a_copy_encodes_the_plugin_state_as_a_save_does() {
        let dir = scratch_dir("recovery-a_copy_encodes_the_plugin_state_as_a_save_does");
        let path = dir.join(RECOVERY_FILE_NAME);
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].set_output(stub_instrument(vec![1, 2, 3]));
        write_copy(&path, ProjectSnapshot::of(&sequencer), None).unwrap();
        let restored = read_copy(&path).project;
        restored.apply_to_sequencer(&mut sequencer);
        let TrackOutput::Instrument(instrument) = sequencer.tracks()[1].output() else {
            panic!("track 1 lost its plugin");
        };
        assert_eq!(*instrument.state, [1, 2, 3]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// No copy is no crash, and no failure.
    #[test]
    fn a_launch_with_no_copy_reads_none() {
        let dir = scratch_dir("recovery-a_launch_with_no_copy_reads_none");
        let copy = read_recovery_copy(&dir.join(RECOVERY_FILE_NAME)).unwrap();
        assert!(copy.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    /// A copy that won't load is an error to report, not a copy to offer.
    #[test]
    fn a_broken_copy_is_an_error() {
        let dir = scratch_dir("recovery-a_broken_copy_is_an_error");
        let path = dir.join(RECOVERY_FILE_NAME);
        fs::write(&path, "{\"origin\":").unwrap();
        assert!(read_recovery_copy(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// A copy `name`d, or of a project never saved.
    fn copy(name: Option<&str>) -> RecoveryCopy {
        let origin = name.map(|name| ProjectOrigin::new(Some("Songs"), name));
        RecoveryCopy::new(origin, ProjectData::default())
    }

    /// With its file there, the project keeps its name and folder.
    #[test]
    fn a_restored_project_keeps_its_name() {
        let staged = copy(Some("Song")).staged_with(|_| true);
        assert_eq!(staged.title, "Song");
        assert_eq!(
            staged.file,
            Some((Some("Songs".to_owned()), "Song".to_owned()))
        );
        assert_eq!(staged.recovered.as_deref(), Some("Restored Song"));
    }

    /// With its file gone it comes back untitled, the footer still naming it.
    #[test]
    fn a_restored_project_whose_file_has_gone_is_untitled() {
        let staged = copy(Some("Song")).staged_with(|_| false);
        assert_eq!(staged.file, None);
        assert_eq!(staged.recovered.as_deref(), Some("Restored Song"));
    }

    /// A project never saved comes back untitled, and has no file to look for.
    #[test]
    fn a_restored_new_project_is_untitled() {
        let staged = copy(None).staged_with(|_| panic!("looked for a file"));
        assert_eq!(staged.title, UNTITLED);
        assert_eq!(staged.file, None);
        assert_eq!(
            staged.recovered.as_deref(),
            Some("Restored your unsaved new project")
        );
    }

    /// Only one holder at a time, and the lock goes with its holder.
    #[test]
    fn the_lock_has_one_holder() {
        let dir = scratch_dir("recovery-the_lock_has_one_holder");
        let path = dir.join(RECOVERY_LOCK_FILE_NAME);
        let held = lock_at(&path);
        assert!(held.is_some());
        assert!(lock_at(&path).is_none());
        drop(held);
        assert!(lock_at(&path).is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
