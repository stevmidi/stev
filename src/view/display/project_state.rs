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
/// The default folder is the open project's folder; with no saved project
/// open, the last one's. Nothing else moves it — not the browser's
/// selection, nor whether the panel is shown, nor a paste or move of another
/// project. Opening a project makes its folder the default, and so does
/// saving one ([`adopt`](Self::adopt)); the browser moving the open project
/// to another folder takes the default with it
/// ([`follow_file`](Self::follow_file)), and a rename of its folder
/// keeps it the default under the new name; deleting the default folder
/// falls back to the projects root
/// ([`follow_library`](Self::follow_library)); and with none set, the
/// projects root (`None`) is the default.
pub(super) struct ProjectViewState {
    /// Where the open project is on disk, `None` before it is first saved:
    /// what ⌘/Ctrl+S saves to. A copy of the sequencer's record, never
    /// changed by the view on its own: private, so the only writers are
    /// [`adopt`](Self::adopt) and [`follow_file`](Self::follow_file), both
    /// fed by the sequencer's events.
    open: Option<ProjectOrigin>,
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
    /// reports it moved or renamed by the browser — its folder the default,
    /// so a move to another folder takes the default along — or `None`:
    /// deleted, so it goes on untitled, or a new project never saved, which
    /// leaves the default where the last one was. Whether the default
    /// changed, to be persisted.
    pub(super) fn follow_file(&mut self, origin: Option<ProjectOrigin>) -> bool {
        let changed = match &origin {
            Some(origin) if origin.folder != self.default_folder => {
                self.default_folder.clone_from(&origin.folder);
                true
            }
            _ => false,
        };
        self.open = origin;
        changed
    }

    /// Follows `change`, made by the browser on disk: the default folder
    /// renamed stays the default under its new name, and deleted falls back
    /// to the projects root, whatever is open (a project open from it or
    /// not; one open in it follows to the new name in
    /// [`follow_file`](Self::follow_file) too). The open project isn't
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

    /// Where the open project is on disk, `None` before it is first saved.
    pub(super) fn open(&self) -> Option<&ProjectOrigin> {
        self.open.as_ref()
    }

    /// The open project's name, `None` before it is first saved.
    pub(super) fn name(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.name.as_str())
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
    fn the_open_project_is_named_and_recognised() {
        let mut project = ProjectViewState::new(Some("last".to_owned()));
        assert_eq!(project.name(), None);
        assert!(!project.is_open(Some("last"), "a"));

        project.follow_file(Some(ProjectOrigin::new(None, "a")));
        assert_eq!(project.name(), Some("a"));
        assert!(project.is_open(None, "a"));
        assert!(!project.is_open(Some("last"), "a"));
    }

    /// The rules: none set is the projects root, an open or a save sets it,
    /// a rename of it renames it, deleting it falls back to the root.
    #[test]
    fn the_default_folder_follows_its_rules_and_nothing_else() {
        let mut project = ProjectViewState::new(None);
        assert_eq!(project.default_folder, None);

        // Opened, or saved: its folder is the default.
        assert!(project.adopt(ProjectOrigin::new(Some("a"), "song")));
        assert_eq!(project.default_folder.as_deref(), Some("a"));
        assert!(!project.adopt(ProjectOrigin::new(Some("a"), "other")));
        assert!(project.adopt(ProjectOrigin::new(None, "loose")));
        assert_eq!(project.default_folder, None);

        project.adopt(ProjectOrigin::new(Some("a"), "song"));

        // A rename keeps it the default under its new name; another folder's
        // changes nothing.
        let renamed = |old: &str, new: &str| LibraryChange::FolderRenamed {
            old: old.into(),
            new: new.into(),
        };
        assert!(!project.follow_library(&renamed("b", "c")));
        assert!(project.follow_library(&renamed("a", "z")));
        // Deletes of projects leave it, and so do moves and renames of
        // any project but the open one.
        let song = ProjectOrigin::new(Some("z"), "song");
        assert!(!project.follow_library(&LibraryChange::Deleted(song)));
        let moved = |old: ProjectOrigin, new: ProjectOrigin| LibraryChange::Moved { old, new };
        let other = moved(
            ProjectOrigin::new(Some("z"), "other"),
            ProjectOrigin::new(Some("b"), "other"),
        );
        assert!(!project.follow_library(&other));
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

    /// The default folder is the open project's folder: the sequencer's
    /// report of a move to another folder takes it along, a rename in
    /// place keeps it, and a delete (untitled) leaves it.
    #[test]
    fn a_move_of_the_open_project_takes_the_default_folder_along() {
        let mut project = ProjectViewState::new(None);
        let song = |folder: Option<&str>, name: &str| ProjectOrigin::new(folder, name);
        project.adopt(song(Some("Ideas"), "riff"));

        assert!(project.follow_file(Some(song(Some("Jams"), "riff"))));
        assert_eq!(project.default_folder.as_deref(), Some("Jams"));
        assert!(!project.follow_file(Some(song(Some("Jams"), "lick"))));
        assert!(project.follow_file(Some(song(None, "lick"))));
        assert_eq!(project.default_folder, None);
        assert!(project.follow_file(Some(song(Some("Ideas"), "lick"))));
        assert!(!project.follow_file(None));
        assert_eq!(project.default_folder.as_deref(), Some("Ideas"));
    }
}
