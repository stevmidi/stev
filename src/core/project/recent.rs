//! The recent projects: the projects opened or saved most recently, newest
//! first, kept in `settings.json` (`SettingsData::recent_projects`) and
//! listed by the browser's Recent category (`archive/350-recent-projects.md`).
//! The list is pure here; `Display` reads and writes it.

use crate::core::config::RECENT_PROJECTS_MAX;

use super::storage::{Followed, LibraryChange, ProjectOrigin};

/// Puts `project` first — just opened or saved — dropping its older entry
/// and anything past [`RECENT_PROJECTS_MAX`].
pub(crate) fn note_recent(list: &mut Vec<ProjectOrigin>, project: ProjectOrigin) {
    list.retain(|entry| *entry != project);
    list.insert(0, project);
    list.truncate(RECENT_PROJECTS_MAX);
}

/// Takes `project` off the list; the file is left alone.
pub(crate) fn remove_recent(list: &mut Vec<ProjectOrigin>, project: &ProjectOrigin) {
    list.retain(|entry| entry != project);
}

/// Follows `change`, made by the browser on disk: a moved or renamed
/// entry keeps its place in the list, a deleted one is dropped.
pub(crate) fn follow_recent(list: &mut Vec<ProjectOrigin>, change: &LibraryChange) {
    list.retain_mut(|entry| entry.follow(change) != Followed::Gone);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[ProjectOrigin]) -> Vec<String> {
        list.iter()
            .map(|entry| format!("{}/{}", entry.folder.as_deref().unwrap_or(""), entry.name))
            .collect()
    }

    #[test]
    fn a_noted_project_goes_first_once_and_the_list_is_capped() {
        let mut list = Vec::new();
        note_recent(&mut list, ProjectOrigin::new(Some("a"), "x"));
        note_recent(&mut list, ProjectOrigin::new(None, "y"));
        note_recent(&mut list, ProjectOrigin::new(Some("a"), "x"));
        assert_eq!(names(&list), vec!["a/x", "/y"]);
        // The same name in another folder is another project.
        note_recent(&mut list, ProjectOrigin::new(Some("b"), "x"));
        assert_eq!(names(&list), vec!["b/x", "a/x", "/y"]);

        for n in 0..RECENT_PROJECTS_MAX {
            note_recent(&mut list, ProjectOrigin::new(None, &n.to_string()));
        }
        assert_eq!(list.len(), RECENT_PROJECTS_MAX);
        assert_eq!(list[0], ProjectOrigin::new(None, "9"));
        assert!(!list.contains(&ProjectOrigin::new(None, "y")));
    }

    #[test]
    fn a_removed_project_leaves_the_rest_in_order() {
        let mut list = vec![
            ProjectOrigin::new(None, "a"),
            ProjectOrigin::new(Some("f"), "b"),
            ProjectOrigin::new(None, "c"),
        ];
        remove_recent(&mut list, &ProjectOrigin::new(Some("f"), "b"));
        assert_eq!(names(&list), vec!["/a", "/c"]);
    }

    #[test]
    fn library_changes_keep_the_place_in_the_list_or_drop_the_entry() {
        let mut list = vec![
            ProjectOrigin::new(Some("f"), "a"),
            ProjectOrigin::new(None, "a"),
            ProjectOrigin::new(Some("f"), "b"),
            ProjectOrigin::new(Some("h"), "c"),
        ];
        let moved = |old: ProjectOrigin, new: ProjectOrigin| LibraryChange::Moved { old, new };
        // A rename is a move within the folder.
        let change = moved(
            ProjectOrigin::new(Some("f"), "a"),
            ProjectOrigin::new(Some("f"), "z"),
        );
        follow_recent(&mut list, &change);
        assert_eq!(names(&list), vec!["f/z", "/a", "f/b", "h/c"]);
        let change = LibraryChange::FolderRenamed {
            old: "f".into(),
            new: "g".into(),
        };
        follow_recent(&mut list, &change);
        assert_eq!(names(&list), vec!["g/z", "/a", "g/b", "h/c"]);
        // A move to another folder.
        let change = moved(
            ProjectOrigin::new(Some("g"), "b"),
            ProjectOrigin::new(None, "b"),
        );
        follow_recent(&mut list, &change);
        assert_eq!(names(&list), vec!["g/z", "/a", "/b", "h/c"]);
        // Deletes drop their entries: the same name elsewhere stays.
        follow_recent(
            &mut list,
            &LibraryChange::Deleted(ProjectOrigin::new(None, "a")),
        );
        assert_eq!(names(&list), vec!["g/z", "/b", "h/c"]);
        follow_recent(&mut list, &LibraryChange::FolderDeleted("g".into()));
        assert_eq!(names(&list), vec!["/b", "h/c"]);
    }
}
