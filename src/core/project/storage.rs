//! Filesystem layout for projects: per-project subfolders of the projects
//! root (`paths::project_dir`), `.stev` file CRUD, project name rules, the
//! `Clips` folder a MIDI clip export lands in, and reading a `.mid` for the
//! MIDI clip import (see `060-persistence.md`).

use std::{
    cmp::Reverse,
    error::Error,
    fs, io,
    iter::once,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    core::{config::PROJECT_NAME_MAX_CHARS, paths::project_dir, time::Meter},
    models::clip::Clip,
};

use super::{dto::ProjectData, smf::read_smf};

/// The extension of the MIDI files the app writes and lists.
const MIDI_EXTENSION: &str = "mid";

/// `folder: None` resolves to `project_dir()` itself — the fallback used when
/// saving a brand-new project before any project folder has been selected
/// (see `060-persistence.md`).
fn resolve_dir(folder: Option<&str>) -> PathBuf {
    match folder {
        Some(f) => project_dir().join(f),
        None => project_dir(),
    }
}

/// The `.stev` file for project `name` in `dir`.
fn stev_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.stev"))
}

/// `(file stem, mtime)` for every `.<extension>` file directly in `dir`,
/// dotfiles excluded.
fn list_files_with_extension(dir: &Path, extension: &str) -> Vec<(String, SystemTime)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()?.to_str()? != extension {
                return None;
            }
            let name = path.file_stem().and_then(|s| s.to_str())?.to_string();
            if name.starts_with('.') {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((name, modified))
        })
        .collect()
}

/// `(project name, mtime)` for every `.stev` file directly in `dir`.
fn list_stev_names(dir: &Path) -> Vec<(String, SystemTime)> {
    list_files_with_extension(dir, "stev")
}

/// The folder under `project_dir()` that holds the MIDI clips: where a clip
/// export lands and what the browser's Clips category lists. Not a project
/// folder, so [`list_project_folders`] leaves it out.
const CLIPS_FOLDER: &str = "Clips";

/// The clips folder ([`CLIPS_FOLDER`]).
fn clips_dir() -> PathBuf {
    project_dir().join(CLIPS_FOLDER)
}

/// Whether a subdirectory of `project_dir()` named `name` is a project
/// folder: not hidden and not the clips folder (in any case — the volume may
/// not tell `clips` from `Clips`).
fn is_project_folder_name(name: &str) -> bool {
    !name.starts_with('.') && !name.eq_ignore_ascii_case(CLIPS_FOLDER)
}

/// Subdirectories of `project_dir()` — each one is a project, created
/// manually by the user (see `060-persistence.md`). Sorted alphabetically;
/// hidden (dot) folders and the clips folder are left out.
pub(crate) fn list_project_folders() -> Vec<String> {
    let dir = project_dir();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_str()?.to_string();
            (entry.file_type().ok()?.is_dir() && is_project_folder_name(&name)).then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// The projects (`.stev` stems) directly in `folder` (`None`: the loose ones
/// at the projects root), newest first.
pub(crate) fn list_projects(folder: Option<&str>) -> Vec<String> {
    let mut projects = list_stev_names(&resolve_dir(folder));
    projects.sort_by_key(|(_, modified)| Reverse(*modified));
    projects.into_iter().map(|(name, _)| name).collect()
}

/// The MIDI clips (`.mid` stems) in the clips folder, alphabetical; none
/// before the first export makes the folder.
pub(crate) fn list_clips() -> Vec<String> {
    let mut clips: Vec<String> = list_files_with_extension(&clips_dir(), MIDI_EXTENSION)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    clips.sort();
    clips
}

/// Writes `data` as pretty JSON to `folder/name.stev` (creating `folder`).
pub(crate) fn save_project(data: &ProjectData, folder: Option<&str>, name: &str) -> io::Result<()> {
    let dir = resolve_dir(folder);
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
    fs::write(stev_path(&dir, name), json)
}

/// Reads and deserializes `folder/name.stev` (`None`: the projects root).
pub(crate) fn load_project(
    folder: Option<&str>,
    name: &str,
) -> Result<ProjectData, Box<dyn Error>> {
    let json = fs::read_to_string(stev_path(&resolve_dir(folder), name))?;
    let data = serde_json::from_str(&json)?;
    Ok(data)
}

/// Removes `folder/name.stev` from disk (`None`: the projects root).
pub(crate) fn delete_project(folder: Option<&str>, name: &str) -> io::Result<()> {
    fs::remove_file(stev_path(&resolve_dir(folder), name))
}

/// Renames project `old` to `new` in `folder` (`None`: the projects root),
/// refusing to replace another file ([`rename_no_replace`]).
pub(crate) fn rename_project(folder: Option<&str>, old: &str, new: &str) -> io::Result<()> {
    let dir = resolve_dir(folder);
    rename_no_replace(&stev_path(&dir, old), &stev_path(&dir, new))
}

/// Renames the clip `old` (its stem) to `new` in the clips folder, refusing
/// to replace another file ([`rename_no_replace`]).
pub(crate) fn rename_clip(old: &str, new: &str) -> io::Result<()> {
    rename_no_replace(&clip_path(old), &clip_path(new))
}

/// Moves `from` to `to` in the same folder unless that would replace another
/// file: `to` is taken when an entry of exactly its name exists, or when it
/// exists at all and isn't `from` under a different case (a case-only rename
/// on a case-insensitive volume, where `to` "exists" because it is `from`).
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let same_but_case = matches!(
        (from.file_name().and_then(|n| n.to_str()), to.file_name().and_then(|n| n.to_str())),
        (Some(a), Some(b)) if a.to_lowercase() == b.to_lowercase()
    );
    // Only a case-only rename needs the folder read: there `to` "exists"
    // whether or not it is another file.
    let exact_entry = || {
        to.parent().is_some_and(|dir| {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| Some(entry.file_name().as_os_str()) == to.file_name())
        })
    };
    if to.exists() && (!same_but_case || exact_entry()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "that name is taken",
        ));
    }
    fs::rename(from, to)
}

/// Characters a project name may not hold: path separators (`/`, and `\` /
/// `:` for Windows and the classic Mac), and the rest of what Windows refuses
/// in a file name, so a project saved on one platform opens on every other.
const PROJECT_NAME_FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// A typed project name as it is saved (the Save As field): trimmed, and
/// `None` when that leaves nothing, starts with a dot (a hidden file the
/// browser would never list), runs past [`PROJECT_NAME_MAX_CHARS`] or holds a
/// control character or one of [`PROJECT_NAME_FORBIDDEN`].
pub(crate) fn project_name_from_input(input: &str) -> Option<String> {
    let name = input.trim();
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name.chars().count() <= PROJECT_NAME_MAX_CHARS
        && !name
            .chars()
            .any(|c| c.is_control() || PROJECT_NAME_FORBIDDEN.contains(&c));
    valid.then(|| name.to_owned())
}

/// Whether `folder/name.stev` exists (`None`: the projects root) — Save As
/// warns before replacing it.
pub(crate) fn project_exists(folder: Option<&str>, name: &str) -> bool {
    stev_path(&resolve_dir(folder), name).exists()
}

/// Writes an exported clip's `.mid` bytes into the clips folder (creating
/// it) under [`clip_export_name`] starting with `base`, never overwriting a
/// file; returns the file name written.
pub(crate) fn save_clip_export(
    bytes: &[u8],
    base: &str,
    track_idx: usize,
    start_tick: i32,
    meter: Meter,
) -> io::Result<String> {
    let dir = clips_dir();
    fs::create_dir_all(&dir)?;
    let name = clip_export_name(base, track_idx, start_tick, meter, |name| {
        dir.join(name).exists()
    });
    fs::write(dir.join(&name), bytes)?;
    Ok(name)
}

/// The clip `name` (its stem) in the clips folder — a browser row's file.
pub(crate) fn clip_path(name: &str) -> PathBuf {
    clips_dir().join(format!("{name}.{MIDI_EXTENSION}"))
}

/// Whether `path` names a MIDI file (`.mid` / `.midi`, any case) — the files
/// a drop from the file manager imports.
pub(crate) fn is_midi_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case(MIDI_EXTENSION) || ext.eq_ignore_ascii_case("midi")
        })
}

/// Reads the `.mid` at `path` as a clip ([`read_smf`], then
/// [`Clip::imported`]) — every track and channel merged, the file's tempo
/// ignored, its length rounded up to whole bars of `meter`. The error says
/// why not, for the footer.
pub(crate) fn load_midi_clip(path: &Path, meter: Meter) -> Result<Clip, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let contents = read_smf(&bytes).map_err(|e| e.to_string())?;
    Clip::imported(contents.events, contents.end_tick, meter)
        .ok_or_else(|| "it has no notes".to_owned())
}

/// `<base>_T<track>_B<bar>.mid` for a clip on track `track_idx` starting at
/// arrangement tick `start_tick`, in bars of `meter` (both 1-based in the
/// name, as on screen); while `taken` says a name is in use, `_2`, `_3`, …
/// before the extension.
fn clip_export_name(
    base: &str,
    track_idx: usize,
    start_tick: i32,
    meter: Meter,
    taken: impl Fn(&str) -> bool,
) -> String {
    let stem = format!(
        "{base}_T{}_B{}",
        track_idx + 1,
        meter.ticks_to_bars(start_tick) + 1
    );
    once(format!("{stem}.{MIDI_EXTENSION}"))
        .chain((2..).map(|n| format!("{stem}_{n}.{MIDI_EXTENSION}")))
        .find(|name| !taken(name))
        .expect("an unbounded list of names has a free one")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_project_name_is_trimmed() {
        assert_eq!(
            project_name_from_input("  Song 2 "),
            Some("Song 2".to_owned())
        );
    }

    /// Nothing the file system or the browser would trip over: blank, a
    /// hidden dot name, a path separator, a Windows-reserved character, a
    /// control character, or a name past the limit.
    #[test]
    fn a_project_name_that_is_no_plain_file_name_is_refused() {
        let too_long = "a".repeat(PROJECT_NAME_MAX_CHARS + 1);
        for input in [
            "", "   ", ".song", "a/b", "a\\b", "a:b", "a?", "a\tb", &too_long,
        ] {
            assert_eq!(project_name_from_input(input), None, "{input:?}");
        }
        let longest = "a".repeat(PROJECT_NAME_MAX_CHARS);
        assert_eq!(project_name_from_input(&longest), Some(longest.clone()));
    }

    /// A fresh, empty scratch folder for one test.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stev-rename-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The file names in `dir`, sorted.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_rename_moves_the_file() {
        let dir = scratch_dir("moves");
        fs::write(dir.join("a.stev"), "a").unwrap();
        rename_no_replace(&dir.join("a.stev"), &dir.join("b.stev")).unwrap();
        assert_eq!(names_in(&dir), ["b.stev"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rename_never_replaces_another_file() {
        let dir = scratch_dir("taken");
        fs::write(dir.join("a.stev"), "a").unwrap();
        fs::write(dir.join("b.stev"), "b").unwrap();
        let err = rename_no_replace(&dir.join("a.stev"), &dir.join("b.stev")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // Nor one that differs from it only in case, where the volume is
        // case-insensitive (macOS by default) — on a case-sensitive one
        // (Linux) `B.stev` is a free name of its own.
        if dir.join("B.stev").exists() {
            assert!(rename_no_replace(&dir.join("a.stev"), &dir.join("B.stev")).is_err());
        }
        assert_eq!(fs::read_to_string(dir.join("b.stev")).unwrap(), "b");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// On a case-insensitive volume `Song.stev` "exists" as soon as
    /// `song.stev` does — it is the same file, and the rename goes ahead.
    #[test]
    fn a_case_only_rename_goes_ahead() {
        let dir = scratch_dir("case");
        fs::write(dir.join("song.stev"), "a").unwrap();
        rename_no_replace(&dir.join("song.stev"), &dir.join("Song.stev")).unwrap();
        assert_eq!(names_in(&dir), ["Song.stev"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_clips_folder_and_hidden_folders_are_no_project_folders() {
        assert!(is_project_folder_name("Song"));
        assert!(!is_project_folder_name(CLIPS_FOLDER));
        assert!(!is_project_folder_name("clips"));
        assert!(!is_project_folder_name(".git"));
    }

    #[test]
    fn midi_files_are_recognised_by_extension_in_any_case() {
        assert!(is_midi_file(Path::new("/a/b.mid")));
        assert!(is_midi_file(Path::new("b.MID")));
        assert!(is_midi_file(Path::new("b.midi")));
        assert!(!is_midi_file(Path::new("b.stev")));
        assert!(!is_midi_file(Path::new("mid")));
    }

    #[test]
    fn clip_export_name_counts_track_and_bar_from_one() {
        let four_four = Meter::FOUR_FOUR;
        let name = clip_export_name("song", 1, four_four.bars_to_ticks(4), four_four, |_| false);
        assert_eq!(name, "song_T2_B5.mid");
    }

    #[test]
    fn clip_export_name_rounds_a_mid_bar_start_down_to_its_bar() {
        let four_four = Meter::FOUR_FOUR;
        let name = clip_export_name(
            "song",
            0,
            four_four.bars_to_ticks(2) + 100,
            four_four,
            |_| false,
        );
        assert_eq!(name, "song_T1_B3.mid");
    }

    /// Bars count in the project's meter: four bars of 3/4 in is bar 5.
    #[test]
    fn clip_export_name_counts_bars_in_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let name = clip_export_name("song", 0, three_four.bars_to_ticks(4), three_four, |_| {
            false
        });
        assert_eq!(name, "song_T1_B5.mid");
    }

    #[test]
    fn clip_export_name_never_reuses_a_taken_name() {
        let taken = ["song_T1_B1.mid", "song_T1_B1_2.mid"];
        let name = clip_export_name("song", 0, 0, Meter::FOUR_FOUR, |name| taken.contains(&name));
        assert_eq!(name, "song_T1_B1_3.mid");
    }
}
