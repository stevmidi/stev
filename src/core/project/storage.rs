//! Filesystem layout for projects: per-project subfolders of the projects
//! root (`paths::project_dir`), `.stev` file CRUD, project name rules, the
//! project templates in `paths::templates_dir`, the
//! `Clips` folder a MIDI clip export lands in, the folder and file names of
//! the audio export (`130-plugin-host.md` § Audio export), and reading a `.mid`
//! for the MIDI clip import (see `060-persistence.md`).

use std::{
    cmp::Reverse,
    error::Error,
    fs,
    io::{self, Write},
    iter::once,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    core::{
        config::PROJECT_NAME_MAX_CHARS,
        paths::{exports_dir, project_dir, templates_dir},
        time::{Meter, format_bpm},
    },
    models::clip::Clip,
};

#[cfg(target_os = "macos")]
use objc2_foundation::{NSFileManager, NSURL};

use super::{dto::ProjectData, smf::read_smf};

/// The extension of the MIDI files the app writes and lists.
const MIDI_EXTENSION: &str = "mid";

/// The extension of a project file.
const PROJECT_EXTENSION: &str = "stev";

/// The name a project copied into the library takes when its file name is no
/// project name ([`project_name_from_input`]).
const COPIED_PROJECT_FALLBACK_NAME: &str = "Untitled";

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
    dir.join(format!("{name}.{PROJECT_EXTENSION}"))
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
    list_files_with_extension(dir, PROJECT_EXTENSION)
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

/// Subdirectories of `project_dir()` — the project folders, made in the
/// browser (⌘⇧N) or in Finder (see `060-persistence.md`). Sorted
/// alphabetically; hidden (dot) folders and the clips folder are left out.
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
    sorted_stems(&clips_dir(), MIDI_EXTENSION)
}

/// The project templates (`.stev` stems) in the templates folder,
/// alphabetical; none before the folder exists (`archive/340-templates.md`).
pub(crate) fn list_templates() -> Vec<String> {
    list_templates_in(&templates_dir())
}

/// [`list_templates`] in the templates folder `dir`.
fn list_templates_in(dir: &Path) -> Vec<String> {
    sorted_stems(dir, PROJECT_EXTENSION)
}

/// The stems of the `.<extension>` files directly in `dir`, alphabetical,
/// dotfiles excluded.
fn sorted_stems(dir: &Path, extension: &str) -> Vec<String> {
    let mut stems: Vec<String> = list_files_with_extension(dir, extension)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    stems.sort();
    stems
}

/// The template `name` (its stem) in the templates folder — a browser
/// row's file.
pub(crate) fn template_path(name: &str) -> PathBuf {
    stev_path(&templates_dir(), name)
}

/// Writes `data` as pretty JSON to `folder/name.stev` (creating `folder`).
pub(crate) fn save_project(data: &ProjectData, folder: Option<&str>, name: &str) -> io::Result<()> {
    write_project_in(&resolve_dir(folder), data, name)
}

/// Writes `data` as the template `name` in the templates folder, made on
/// the first save, replacing one of that name. `data` is saved as given:
/// the caller strips it (`ProjectData::into_template`).
pub(crate) fn save_template(data: &ProjectData, name: &str) -> io::Result<()> {
    write_project_in(&templates_dir(), data, name)
}

/// Writes `data` as pretty JSON to `dir/name.stev` (creating `dir`).
fn write_project_in(dir: &Path, data: &ProjectData, name: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let json = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
    fs::write(stev_path(dir, name), json)
}

/// Reads and deserializes `folder/name.stev` (`None`: the projects root).
pub(crate) fn load_project(
    folder: Option<&str>,
    name: &str,
) -> Result<ProjectData, Box<dyn Error>> {
    load_project_file(&stev_path(&resolve_dir(folder), name))
}

/// Reads and deserializes the `.stev` at `path`, wherever it is — a
/// template ([`template_path`]).
pub(crate) fn load_project_file(path: &Path) -> Result<ProjectData, Box<dyn Error>> {
    let (_, data) = read_project(path)?;
    Ok(data)
}

/// The `.stev` at `path`: its JSON as written, and the project it holds.
fn read_project(path: &Path) -> Result<(String, ProjectData), Box<dyn Error>> {
    let json = fs::read_to_string(path)?;
    let data = serde_json::from_str(&json)?;
    Ok((json, data))
}

/// Where the `.stev` at `path` sits in the library, named as the browser
/// names it: `(folder, name)` for one directly in the projects root (folder
/// `None`) or in one of its project folders; `None` anywhere else — outside
/// the library, deeper, or in the clips folder or a hidden one. Both paths
/// are compared resolved, so a symlink on either side doesn't matter.
pub(crate) fn library_project(path: &Path) -> Option<(Option<String>, String)> {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    project_in(&canonical(path), &canonical(&project_dir()))
}

/// Whether the `.stev` at `path` is a template: directly in the templates
/// folder, compared resolved as [`library_project`] does. Finder opens one
/// as the browser does, untitled (`archive/340-templates.md`).
pub(crate) fn is_template_file(path: &Path) -> bool {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    is_template_in(&canonical(path), &canonical(&templates_dir()))
}

/// [`is_template_file`] with the templates folder `dir`, both paths as given.
fn is_template_in(path: &Path, dir: &Path) -> bool {
    is_project_file(path)
        && path.parent() == Some(dir)
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| !stem.starts_with('.'))
}

/// [`library_project`] with the projects root `root`, both paths as given.
fn project_in(path: &Path, root: &Path) -> Option<(Option<String>, String)> {
    if !is_project_file(path) {
        return None;
    }
    let name = path.file_stem()?.to_str()?;
    if name.starts_with('.') {
        return None;
    }
    let parent = path.parent()?;
    let folder = if parent == root {
        None
    } else {
        let folder = parent.file_name()?.to_str()?;
        if parent.parent()? != root || !is_project_folder_name(folder) {
            return None;
        }
        Some(folder.to_owned())
    };
    Some((folder, name.to_owned()))
}

/// Copies the project at `path`, from outside the library, into the projects
/// root, and returns its name there. The file is read and parsed first, so
/// one that won't load is never copied. Its name is the file's
/// (or [`COPIED_PROJECT_FALLBACK_NAME`] when that is no project name), with
/// `_2`, `_3`, … after it while the name is taken: nothing is overwritten.
pub(crate) fn copy_project_in(path: &Path) -> Result<String, Box<dyn Error>> {
    copy_project_into(path, &project_dir())
}

/// [`copy_project_in`] with the projects root `root`.
fn copy_project_into(path: &Path, root: &Path) -> Result<String, Box<dyn Error>> {
    let (json, _) = read_project(path)?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(project_name_from_input)
        .unwrap_or_else(|| COPIED_PROJECT_FALLBACK_NAME.to_owned());
    let name = first_free_name(
        |n| n.map_or_else(|| stem.clone(), |n| format!("{stem}_{n}")),
        |name| stev_path(root, name).exists(),
    );
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stev_path(root, &name))?
        .write_all(json.as_bytes())?;
    Ok(name)
}

/// `path`'s file name without its extension, for the footer.
pub(crate) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
}

/// Whether `path` names a project file (`.stev`, any case): what Finder
/// hands over when one is double-clicked (macOS), and what
/// [`library_project`] places.
pub(crate) fn is_project_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(PROJECT_EXTENSION))
}

/// Deletes `folder/name.stev` (`None`: the projects root) — to the Trash on
/// macOS ([`delete_file`]).
pub(crate) fn delete_project(folder: Option<&str>, name: &str) -> io::Result<()> {
    delete_file(&stev_path(&resolve_dir(folder), name))
}

/// Deletes the clip `name` (its stem) from the clips folder — to the Trash on
/// macOS ([`delete_file`]).
pub(crate) fn delete_clip(name: &str) -> io::Result<()> {
    delete_file(&clip_path(name))
}

/// Deletes the template `name` (its stem) from the templates folder — to the
/// Trash on macOS ([`delete_file`]).
pub(crate) fn delete_template(name: &str) -> io::Result<()> {
    delete_file(&template_path(name))
}

/// Moves the file at `path` to the Trash.
#[cfg(target_os = "macos")]
fn delete_file(path: &Path) -> io::Result<()> {
    let url = NSURL::from_file_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
    trash(&url)
}

/// Removes the file at `path` for good: off macOS there is no Trash.
#[cfg(not(target_os = "macos"))]
fn delete_file(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

/// Moves the file or folder at `url` to the Trash.
#[cfg(target_os = "macos")]
fn trash(url: &NSURL) -> io::Result<()> {
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(url, None)
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))
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

/// Renames the template `old` (its stem) to `new` in the templates folder,
/// refusing to replace another file ([`rename_no_replace`]).
pub(crate) fn rename_template(old: &str, new: &str) -> io::Result<()> {
    rename_no_replace(&template_path(old), &template_path(new))
}

/// Makes the project folder `name` under the projects root (and the root
/// itself when it is missing). Refuses a name that is taken — in any case,
/// where the volume doesn't tell them apart — and the clips folder's.
pub(crate) fn create_project_folder(name: &str) -> io::Result<()> {
    create_folder_in(&project_dir(), name)
}

/// [`create_project_folder`] with the projects root `root`.
fn create_folder_in(root: &Path, name: &str) -> io::Result<()> {
    refuse_clips_folder_name(name)?;
    fs::create_dir_all(root)?;
    fs::create_dir(root.join(name)).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => name_taken(),
        _ => e,
    })
}

/// Renames the project folder `old` to `new`, refusing to replace anything
/// ([`rename_no_replace`]) and the clips folder's name.
pub(crate) fn rename_project_folder(old: &str, new: &str) -> io::Result<()> {
    rename_folder_in(&project_dir(), old, new)
}

/// [`rename_project_folder`] with the projects root `root`.
fn rename_folder_in(root: &Path, old: &str, new: &str) -> io::Result<()> {
    refuse_clips_folder_name(new)?;
    rename_no_replace(&root.join(old), &root.join(new))
}

/// Moves the project folder `name`, with everything in it, to the Trash.
#[cfg(target_os = "macos")]
pub(crate) fn delete_project_folder(name: &str) -> io::Result<()> {
    let url = NSURL::from_directory_path(resolve_dir(Some(name)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a folder path"))?;
    trash(&url)
}

/// Removes the project folder `name` when it is empty. Off macOS there is
/// no Trash to move a full one to, and nothing is deleted for good.
#[cfg(not(target_os = "macos"))]
pub(crate) fn delete_project_folder(name: &str) -> io::Result<()> {
    fs::remove_dir(resolve_dir(Some(name)))
}

/// Refuses `name` for a project folder when it is the clips folder's, which
/// the browser lists apart ([`is_project_folder_name`]).
fn refuse_clips_folder_name(name: &str) -> io::Result<()> {
    if is_project_folder_name(name) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "that name is the clips folder's",
        ))
    }
}

/// The error for a name another file or folder already has.
fn name_taken() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "that name is taken")
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
        return Err(name_taken());
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
        && !name.chars().any(is_forbidden_in_name);
    valid.then(|| name.to_owned())
}

/// The project folder name typed in `input`: [`project_name_from_input`]'s
/// rule, and not the clips folder's name (in any case) — what Save As's new
/// folder takes.
pub(crate) fn project_folder_name_from_input(input: &str) -> Option<String> {
    project_name_from_input(input).filter(|name| is_project_folder_name(name))
}

/// Whether a file system refuses `c` in a name: a control character or one
/// of [`PROJECT_NAME_FORBIDDEN`].
fn is_forbidden_in_name(c: char) -> bool {
    c.is_control() || PROJECT_NAME_FORBIDDEN.contains(&c)
}

/// The first of `name(None)`, `name(Some(2))`, `name(Some(3))`, … that
/// `taken` doesn't claim: the never-overwrite rule every export's name
/// follows.
fn first_free_name(name: impl Fn(Option<u32>) -> String, taken: impl Fn(&str) -> bool) -> String {
    once(None)
        .chain((2..).map(Some))
        .map(name)
        .find(|name| !taken(name))
        .expect("an unbounded list of names has a free one")
}

/// Whether `folder/name.stev` exists (`None`: the projects root) — Save As
/// warns before replacing it.
pub(crate) fn project_exists(folder: Option<&str>, name: &str) -> bool {
    stev_path(&resolve_dir(folder), name).exists()
}

/// Whether the template `name` exists — Save as template warns before
/// replacing it.
pub(crate) fn template_exists(name: &str) -> bool {
    template_path(name).exists()
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

/// The file every audio export writes the summed mix to.
pub(crate) const MIX_FILE_NAME: &str = "Mix.wav";

/// Makes a new, empty folder for one audio export inside the exports folder
/// (creating that too), named by [`audio_export_dir_name`]; returns its path.
/// Never reuses a folder: a name taken on disk moves on to the next number.
pub(crate) fn create_audio_export_dir(
    base: &str,
    tempo_us: i32,
    meter: Meter,
    start_tick: i32,
) -> io::Result<PathBuf> {
    let exports = exports_dir();
    fs::create_dir_all(&exports)?;
    let name = audio_export_dir_name(base, tempo_us, meter, start_tick, |name| {
        exports.join(name).exists()
    });
    let dir = exports.join(name);
    fs::create_dir(&dir)?;
    Ok(dir)
}

/// `<base> <tempo>bpm <n>-<d> bar <bar>` (e.g. `Sketch 120bpm 4-4 bar 9`) for
/// an export starting at arrangement tick `start_tick`, in bars of `meter`
/// (1-based, as on screen). A DAW import needs the tempo, the meter and
/// where to drop the files, and a WAV carries none of them. The tempo shows a
/// tenth only when it has one (`120.5bpm`). While `taken` says a name is in
/// use, ` 2`, ` 3`, … go after it.
fn audio_export_dir_name(
    base: &str,
    tempo_us: i32,
    meter: Meter,
    start_tick: i32,
    taken: impl Fn(&str) -> bool,
) -> String {
    let bpm = format_bpm(tempo_us);
    let bpm = bpm.strip_suffix(".0").unwrap_or(&bpm);
    let stem = format!(
        "{base} {bpm}bpm {}-{} bar {}",
        meter.numerator(),
        meter.denominator(),
        meter.ticks_to_bars(start_tick) + 1
    );
    first_free_name(
        |n| n.map_or_else(|| stem.clone(), |n| format!("{stem} {n}")),
        taken,
    )
}

/// `NN Name.wav` for the stem of the track at 1-based `number`, zero-padded
/// so the files sort in the arrangement's order. `name` is the track's name
/// (or its plugin's, for a track never named), with every character a file
/// system refuses ([`PROJECT_NAME_FORBIDDEN`], control characters) replaced
/// by `_`. A blank `name` leaves just the number: `NN.wav`.
pub(crate) fn stem_file_name(number: usize, name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .map(|c| if is_forbidden_in_name(c) { '_' } else { c })
        .collect();
    if name.is_empty() {
        format!("{number:02}.wav")
    } else {
        format!("{number:02} {name}.wav")
    }
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
    first_free_name(
        |n| match n {
            None => format!("{stem}.{MIDI_EXTENSION}"),
            Some(n) => format!("{stem}_{n}.{MIDI_EXTENSION}"),
        },
        taken,
    )
}

#[cfg(test)]
mod tests {
    use crate::core::time::{bpm_tenths_to_tempo_us, bpm_to_tempo_us};

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

    /// The templates folder lists its `.stev` files alphabetically, dotfiles
    /// and other files left out — and nothing before it exists.
    #[test]
    fn templates_list_alphabetically_and_none_before_the_folder_exists() {
        let dir = scratch_dir("templates");
        let templates = dir.join("Templates");
        assert!(list_templates_in(&templates).is_empty());
        fs::create_dir(&templates).unwrap();
        for file in ["Synths.stev", "Band.stev", ".hidden.stev", "Riff.mid"] {
            fs::write(templates.join(file), "{}").unwrap();
        }
        assert_eq!(list_templates_in(&templates), vec!["Band", "Synths"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The first template saved makes the folder, and saving under a taken
    /// name replaces that template.
    #[test]
    fn saving_a_template_makes_the_folder_and_replaces_one_of_that_name() {
        let dir = scratch_dir("save-template");
        let templates = dir.join("Templates");
        let mut data = ProjectData::default();
        write_project_in(&templates, &data, "Band").unwrap();
        assert_eq!(list_templates_in(&templates), vec!["Band"]);
        data.tempo_us = bpm_to_tempo_us(90);
        write_project_in(&templates, &data, "Band").unwrap();
        assert_eq!(list_templates_in(&templates), vec!["Band"]);
        let saved = load_project_file(&stev_path(&templates, "Band")).unwrap();
        assert_eq!(saved.tempo_us, data.tempo_us);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A `.stev` directly in the templates folder is a template; one deeper,
    /// beside it, hidden or of another kind is not.
    #[test]
    fn a_template_file_is_one_directly_in_the_templates_folder() {
        let dir = Path::new("/docs/Stev/Templates");
        let is_template = |path: &str| is_template_in(Path::new(path), dir);
        assert!(is_template("/docs/Stev/Templates/Band.stev"));
        assert!(is_template("/docs/Stev/Templates/Band.STEV"));
        for not in [
            "/docs/Stev/Templates/Old/Band.stev",
            "/docs/Stev/Projects/Band.stev",
            "/docs/Stev/Templates/.Band.stev",
            "/docs/Stev/Templates/Band.mid",
        ] {
            assert!(!is_template(not), "{not}");
        }
    }

    /// A project in the root or one project folder down is named as the
    /// browser names it; anything else is outside the library.
    #[test]
    fn a_project_file_is_placed_in_the_library_or_outside_it() {
        let root = Path::new("/docs/Stev/Projects");
        let place = |path: &str| project_in(Path::new(path), root);
        assert_eq!(
            place("/docs/Stev/Projects/Song.stev"),
            Some((None, "Song".to_owned()))
        );
        assert_eq!(
            place("/docs/Stev/Projects/Album/Song.stev"),
            Some((Some("Album".to_owned()), "Song".to_owned()))
        );
        // As Finder names it: the extension in any case.
        assert_eq!(
            place("/docs/Stev/Projects/Song.STEV"),
            Some((None, "Song".to_owned()))
        );
        for outside in [
            "/Downloads/Song.stev",
            "/docs/Stev/Song.stev",
            "/docs/Stev/Projects/Album/Old/Song.stev",
            "/docs/Stev/Projects/Clips/Song.stev",
            "/docs/Stev/Projects/.hidden/Song.stev",
            "/docs/Stev/Projects/.Song.stev",
            "/docs/Stev/Projects/Song.mid",
        ] {
            assert_eq!(place(outside), None, "{outside}");
        }
    }

    #[test]
    fn a_project_copied_in_keeps_its_name_and_never_overwrites() {
        let dir = scratch_dir("copy-in");
        let root = dir.join("Projects");
        fs::create_dir(&root).unwrap();
        let source = dir.join("Song.stev");
        let json = serde_json::to_string(&ProjectData::default()).unwrap();
        fs::write(&source, &json).unwrap();
        fs::write(root.join("Song.stev"), "taken").unwrap();

        assert_eq!(copy_project_into(&source, &root).unwrap(), "Song_2");
        assert_eq!(copy_project_into(&source, &root).unwrap(), "Song_3");
        assert_eq!(fs::read_to_string(root.join("Song_3.stev")).unwrap(), json);
        assert_eq!(fs::read_to_string(root.join("Song.stev")).unwrap(), "taken");
        assert_eq!(fs::read_to_string(&source).unwrap(), json);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that is no project is refused before anything is written.
    #[test]
    fn a_file_that_wont_load_is_not_copied_in() {
        let dir = scratch_dir("copy-in-broken");
        let root = dir.join("Projects");
        fs::create_dir(&root).unwrap();
        let source = dir.join("Broken.stev");
        fs::write(&source, "not json").unwrap();

        assert!(copy_project_into(&source, &root).is_err());
        assert!(copy_project_into(&dir.join("Missing.stev"), &root).is_err());
        assert!(names_in(&root).is_empty());
        fs::remove_dir_all(&dir).unwrap();
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
    fn a_new_folder_is_made_with_the_projects_root() {
        let dir = scratch_dir("new-folder");
        let root = dir.join("Projects");
        create_folder_in(&root, "Album").unwrap();
        assert_eq!(names_in(&root), ["Album"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Not a name already taken — by a folder, a file, or one differing only
    /// in case where the volume is case-insensitive — nor the clips folder's.
    #[test]
    fn a_new_folder_never_takes_a_name_in_use() {
        let dir = scratch_dir("new-folder-taken");
        fs::create_dir(dir.join("Album")).unwrap();
        fs::write(dir.join("Notes"), "n").unwrap();
        for name in ["Album", "Notes"] {
            let err = create_folder_in(&dir, name).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{name}");
        }
        if dir.join("ALBUM").exists() {
            assert!(create_folder_in(&dir, "ALBUM").is_err());
        }
        for name in [CLIPS_FOLDER, "clips"] {
            let err = create_folder_in(&dir, name).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert_eq!(names_in(&dir), ["Album", "Notes"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_rename_moves_it_with_its_projects_but_never_onto_another() {
        let dir = scratch_dir("rename-folder");
        fs::create_dir(dir.join("Album")).unwrap();
        fs::write(dir.join("Album").join("Song.stev"), "s").unwrap();
        fs::create_dir(dir.join("Other")).unwrap();

        rename_folder_in(&dir, "Album", "Record").unwrap();
        assert_eq!(names_in(&dir), ["Other", "Record"]);
        assert_eq!(names_in(&dir.join("Record")), ["Song.stev"]);

        let err = rename_folder_in(&dir, "Record", "Other").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        let err = rename_folder_in(&dir, "Record", CLIPS_FOLDER).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(names_in(&dir), ["Other", "Record"]);
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
    fn a_new_folder_name_is_a_project_name_but_never_the_clips_folder() {
        assert_eq!(
            project_folder_name_from_input("  Album "),
            Some("Album".to_owned())
        );
        assert_eq!(project_folder_name_from_input("clips"), None);
        assert_eq!(project_folder_name_from_input(CLIPS_FOLDER), None);
        assert_eq!(project_folder_name_from_input("a/b"), None);
        assert_eq!(project_folder_name_from_input("   "), None);
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
    fn audio_export_dir_name_carries_tempo_meter_and_start_bar() {
        let four_four = Meter::FOUR_FOUR;
        let name = audio_export_dir_name(
            "Sketch",
            bpm_to_tempo_us(120),
            four_four,
            four_four.bars_to_ticks(8),
            |_| false,
        );
        assert_eq!(name, "Sketch 120bpm 4-4 bar 9");
    }

    #[test]
    fn audio_export_dir_name_from_bar_one_still_says_so() {
        let name =
            audio_export_dir_name("Sketch", bpm_to_tempo_us(90), Meter::FOUR_FOUR, 0, |_| {
                false
            });
        assert_eq!(name, "Sketch 90bpm 4-4 bar 1");
    }

    #[test]
    fn audio_export_dir_name_keeps_a_tempo_tenth_and_an_eighth_meter() {
        let six_eight = Meter::parse("6/8").unwrap();
        let name = audio_export_dir_name(
            "Waltz",
            bpm_tenths_to_tempo_us(1_205),
            six_eight,
            six_eight.bars_to_ticks(2),
            |_| false,
        );
        assert_eq!(name, "Waltz 120.5bpm 6-8 bar 3");
    }

    #[test]
    fn audio_export_dir_name_never_reuses_a_folder() {
        let taken = ["Sketch 120bpm 4-4 bar 1", "Sketch 120bpm 4-4 bar 1 2"];
        let name =
            audio_export_dir_name("Sketch", bpm_to_tempo_us(120), Meter::FOUR_FOUR, 0, |n| {
                taken.contains(&n)
            });
        assert_eq!(name, "Sketch 120bpm 4-4 bar 1 3");
    }

    #[test]
    fn stem_file_names_sort_in_track_order() {
        assert_eq!(stem_file_name(1, "Bass"), "01 Bass.wav");
        assert_eq!(stem_file_name(12, "Pads"), "12 Pads.wav");
    }

    #[test]
    fn a_blank_stem_name_leaves_the_number() {
        assert_eq!(stem_file_name(4, "  "), "04.wav");
    }

    #[test]
    fn stem_file_names_replace_what_a_file_system_refuses() {
        assert_eq!(
            stem_file_name(3, " Lead: A/B\\C\u{7}D? "),
            "03 Lead_ A_B_C_D_.wav"
        );
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
