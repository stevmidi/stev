//! [`StagedProject`] — a project read from disk whose plugins load before it
//! replaces the open one (`130-plugin-host.md` § Project persistence).

use super::ProjectData;

/// A project read from disk, with where it came from. One with plugins on
/// its tracks waits in the `"sequencer"` thread while the view loads them
/// (`UiEvent::StageProject`) and is applied when the view says they are in
/// (`InputEvent::ApplyStagedProject`), so the screen goes from the open
/// project to this one, whole, in one step.
pub(crate) struct StagedProject {
    /// The project, as read.
    pub(crate) data: ProjectData,
    /// Its file name, without extension.
    pub(crate) filename: String,
    /// Its folder, or the projects root when `None`.
    pub(crate) folder: Option<String>,
}
