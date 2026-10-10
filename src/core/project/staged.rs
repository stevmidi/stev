//! [`StagedProject`] — a project read from disk whose plugins load before it
//! replaces the open one (`130-plugin-host.md` § Project persistence).

use std::fmt;

use super::ProjectData;

/// A project opened from disk with plugins on its tracks, on its round trip
/// through the view: the sequencer thread reads it and hands it over
/// (`UiEvent::StageProject`) without applying it; the view loads its plugins
/// behind the restore panel while the open project stays as it is, then hands
/// it back (`InputEvent::ApplyStagedProject`) for the sequencer to apply. So
/// the screen goes from the old project to the new one, whole, in one step.
#[derive(Clone)]
pub(crate) struct StagedProject {
    /// The project, as read.
    pub(crate) data: ProjectData,
    /// Its file name, without extension.
    pub(crate) filename: String,
    /// Its folder, or the projects root when `None`.
    pub(crate) folder: Option<String>,
}

/// Names the file only: the data is a whole project.
impl fmt::Debug for StagedProject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagedProject")
            .field("filename", &self.filename)
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}
