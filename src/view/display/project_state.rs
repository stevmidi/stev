//! The ambient "what's open" project state: the current project folder and
//! name a later ⌘/Ctrl+N/S reads.
//!
//! Grouped out of [`Display`](super::Display) as `Display::project`; the
//! browser panel (`browser.rs`) sets the folder, `ProjectLoaded` the name. See
//! `020-views-and-state.md` and `060-persistence.md`.

/// The current project folder and name, grouped out of
/// [`Display`](super::Display).
pub(super) struct ProjectViewState {
    /// The ambient "active" project folder — persisted to `settings.json` so
    /// it's remembered across sessions, and used by ⌘/Ctrl+N/S to place new
    /// projects. `None` only until the user has ever picked a folder.
    pub(super) project_current_folder: Option<String>,
    /// Name of the currently open project, `None` if unsaved.
    pub(super) project_current_name: Option<String>,
}

impl ProjectViewState {
    /// Seeded with the folder remembered from the last session
    /// (`SettingsData.last_project_folder`, `None` on a first run).
    pub(super) fn new(last_project_folder: Option<String>) -> Self {
        ProjectViewState {
            project_current_folder: last_project_folder,
            project_current_name: None,
        }
    }
}
