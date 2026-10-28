//! [`ProjectAction`] — what replaces or ends the open project, and so is
//! guarded by the unsaved-changes prompt (`060-persistence.md` § Unsaved
//! changes).

use std::path::PathBuf;

/// Something that throws the open project away: ⌘/Ctrl+N, opening another
/// project or a template from the browser or from Finder, or quitting. Each goes to the sequencer thread
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
    /// Open the `.stev` at `path`, handed over by Finder (macOS): as
    /// [`Open`](Self::Open) when it is in the library, else copied into the
    /// projects root first (`060-persistence.md` § Opening from Finder).
    /// Only macOS's AppKit glue sends it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    OpenFile {
        /// The project file, anywhere.
        path: PathBuf,
    },
    /// Start a new, untitled project from the template at `path`: ⌘N with a
    /// different starting point. The template is only read
    /// (`340-templates.md`).
    NewFromTemplate {
        /// The template's `.stev`.
        path: PathBuf,
    },
    /// Close the window and quit the app.
    Quit,
}
