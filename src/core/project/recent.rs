//! The recent projects: the projects opened or saved most recently, newest
//! first, kept in `settings.json` (`SettingsData::recent_projects`) and
//! listed by the browser's Recent category (`archive/350-recent-projects.md`).
//! The list is pure here; `Display` reads and writes it.

use crate::core::config::RECENT_PROJECTS_MAX;

use super::storage::ProjectOrigin;

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

/// Follows a rename of the project `old` to `new` (its name), keeping its
/// place in the list.
pub(crate) fn rename_recent(list: &mut [ProjectOrigin], old: &ProjectOrigin, new: &str) {
    for entry in list.iter_mut().filter(|entry| *entry == old) {
        entry.name = new.to_owned();
    }
}

/// Follows a rename of the project folder `old` to `new`.
pub(crate) fn rename_recent_folder(list: &mut [ProjectOrigin], old: &str, new: &str) {
    for entry in list {
        if entry.folder.as_deref() == Some(old) {
            entry.folder = Some(new.to_owned());
        }
    }
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
    fn renames_keep_the_place_in_the_list() {
        let mut list = vec![
            ProjectOrigin::new(Some("f"), "a"),
            ProjectOrigin::new(None, "a"),
            ProjectOrigin::new(Some("f"), "b"),
        ];
        rename_recent(&mut list, &ProjectOrigin::new(Some("f"), "a"), "z");
        assert_eq!(names(&list), vec!["f/z", "/a", "f/b"]);
        rename_recent_folder(&mut list, "f", "g");
        assert_eq!(names(&list), vec!["g/z", "/a", "g/b"]);
    }
}
