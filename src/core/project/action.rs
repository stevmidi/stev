//! [`ProjectAction`] — what replaces or ends the open project, and so is
//! guarded by the unsaved-changes prompt (`060-persistence.md` § Unsaved
//! changes).

/// Something that throws the open project away: ⌘/Ctrl+N, opening another
/// project from the browser, or quitting. Each goes to the sequencer thread
/// first, which runs it at once when nothing changed since the last save or
/// load, or asks the view to prompt (`UiEvent::UnsavedChanges`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectAction {
    /// Start an empty project.
    New,
    /// Load `folder/filename.stev`.
    Open {
        /// Project file name, without extension.
        filename: String,
        /// Project folder, or the projects root when `None`.
        folder: Option<String>,
    },
    /// Close the window and quit the app.
    Quit,
}
