//! The ambient "what's open" project state: the open project's file, as the
//! sequencer reports it, and the default folder Save As starts in.
//!
//! Grouped out of [`Display`](super::Display) as `Display::project`. The
//! sequencer's `SavedProject` origin is the one record of which file is open;
//! `open` is the view's copy of it, written only from the sequencer's
//! `ProjectLoaded`, `ProjectSaved` and `OpenProjectFile` (#22). See
//! `020-views-and-state.md` and `060-persistence.md`.

use crate::core::project::{LibraryChange, ProjectOrigin};

/// The open project and the default folder, grouped out of
/// [`Display`](super::Display).
///
/// The default folder follows four rules, and nothing else moves it — not
/// the browser's selection, nor whether the panel is shown: opening a
/// project makes its folder the default, and so does saving one
/// ([`adopt`](Self::adopt)); deleting the default folder falls back to the
/// projects root ([`follow_library`](Self::follow_library)); and with none
/// set, the projects root (`None`) is the default. A rename of the folder
/// keeps it the default under its new name.
pub(super) struct ProjectViewState {
    /// Where the open project is on disk, `None` before it is first saved:
    /// what ⌘/Ctrl+S saves to. A copy of the sequencer's record, never
    /// changed by the view on its own.
    pub(super) open: Option<ProjectOrigin>,
    /// The folder Save As starts in, persisted to `settings.json` as
    /// `last_project_folder` so it's remembered across sessions. `None`: the
    /// projects root.
    pub(super) default_folder: Option<String>,
}

impl ProjectViewState {
    /// Seeded with the folder remembered from the last session
    /// (`SettingsData.last_project_folder`, `None` on a first run).
    pub(super) fn new(last_project_folder: Option<String>) -> Self {
        ProjectViewState {
            open: None,
            default_folder: last_project_folder,
        }
    }

    /// Takes `origin` as the open project, as the sequencer reports it just
    /// loaded or saved: its folder becomes the default. Whether the default
    /// changed, to be persisted.
    pub(super) fn adopt(&mut self, origin: ProjectOrigin) -> bool {
        let changed = self.default_folder != origin.folder;
        self.default_folder.clone_from(&origin.folder);
        self.open = Some(origin);
        changed
    }

    /// Takes `origin` as where the open project is now, as the sequencer
    /// reports it moved or renamed by the browser — or `None`, deleted, so
    /// it goes on untitled. The default folder stays: only an open or a save
    /// sets it, and a deleted folder falls back on its own
    /// ([`follow_library`](Self::follow_library)).
    pub(super) fn follow_file(&mut self, origin: Option<ProjectOrigin>) {
        self.open = origin;
    }

    /// Follows `change`, made by the browser on disk: the default folder
    /// renamed stays the default under its new name, and deleted falls back
    /// to the projects root, whatever is open. The open project isn't
    /// followed here: the sequencer's record is the one, and it reports back
    /// (`UiEvent::OpenProjectFile`). Whether the default changed.
    pub(super) fn follow_library(&mut self, change: &LibraryChange) -> bool {
        let is_default = |folder: &str| self.default_folder.as_deref() == Some(folder);
        let default = match change {
            LibraryChange::FolderRenamed { old, new } if is_default(old) => Some(new.clone()),
            LibraryChange::FolderDeleted(folder) if is_default(folder) => None,
            _ => return false,
        };
        self.default_folder = default;
        true
    }

    /// The open project's name, `None` before it is first saved.
    pub(super) fn name(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.name.as_str())
    }

    /// The current folder: the open project's, else the default one.
    pub(super) fn folder(&self) -> Option<&str> {
        match &self.open {
            Some(open) => open.folder.as_deref(),
            None => self.default_folder.as_deref(),
        }
    }

    /// Whether `name` in `folder` is the open project.
    pub(super) fn is_open(&self, folder: Option<&str>, name: &str) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.folder.as_deref() == folder && open.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_folder_is_the_open_projects_else_the_default() {
        let mut project = ProjectViewState::new(Some("last".to_owned()));
        assert_eq!(project.folder(), Some("last"));
        assert_eq!(project.name(), None);
        assert!(!project.is_open(Some("last"), "a"));

        project.open = Some(ProjectOrigin::new(None, "a"));
        assert_eq!(project.folder(), None);
        assert_eq!(project.name(), Some("a"));
        assert!(project.is_open(None, "a"));
        assert!(!project.is_open(Some("last"), "a"));
    }

    /// The four rules: none set is the projects root, an open or a save sets
    /// it, deleting it falls back to the root.
    #[test]
    fn only_an_open_or_a_save_sets_the_default_folder() {
        let mut project = ProjectViewState::new(None);
        assert_eq!(project.default_folder, None);

        // Opened, or saved: its folder is the default.
        assert!(project.adopt(ProjectOrigin::new(Some("a"), "song")));
        assert_eq!(project.default_folder.as_deref(), Some("a"));
        assert!(!project.adopt(ProjectOrigin::new(Some("a"), "other")));
        assert!(project.adopt(ProjectOrigin::new(None, "loose")));
        assert_eq!(project.default_folder, None);

        // A move of the open project leaves it.
        project.adopt(ProjectOrigin::new(Some("a"), "song"));
        project.follow_file(Some(ProjectOrigin::new(Some("b"), "song")));
        assert_eq!(project.default_folder.as_deref(), Some("a"));

        // A rename keeps it the default under its new name; another folder's
        // changes nothing.
        let renamed = |old: &str, new: &str| LibraryChange::FolderRenamed {
            old: old.into(),
            new: new.into(),
        };
        assert!(!project.follow_library(&renamed("b", "c")));
        assert!(project.follow_library(&renamed("a", "z")));
        // Moves and deletes of projects leave it.
        let song = ProjectOrigin::new(Some("z"), "song");
        assert!(!project.follow_library(&LibraryChange::Deleted(song)));
        assert_eq!(project.default_folder.as_deref(), Some("z"));

        // Deleted, it falls back to the root, a project open or not.
        let deleted = |folder: &str| LibraryChange::FolderDeleted(folder.into());
        assert!(!project.follow_library(&deleted("c")));
        assert!(project.follow_library(&deleted("z")));
        assert_eq!(project.default_folder, None);

        // The open project's file deleted: untitled, the default unmoved.
        project.adopt(ProjectOrigin::new(Some("a"), "song"));
        project.follow_file(None);
        assert_eq!(project.name(), None);
        assert_eq!(project.default_folder.as_deref(), Some("a"));
    }
}
