//! Where the app keeps things on disk: the user's library under Documents
//! (`Stev/`, holding only user content — `Projects/` and the audio export's
//! `Exports/` today, sibling root folders for forks), app settings in the platform config directory,
//! rebuildable caches in the platform cache directory, and logs in the
//! platform's logs folder. See `060-persistence.md`.

use std::{fs, path::PathBuf};

/// Name of the app's folder inside the platform config and cache directories.
const APP_DIR_NAME: &str = "stev";

/// Name of the user's library folder under Documents.
const LIBRARY_DIR_NAME: &str = "Stev";

/// Name of the projects root folder inside the library.
const PROJECTS_DIR_NAME: &str = "Projects";

/// Name of the folder inside the library that every audio export's own
/// folder goes in.
const EXPORTS_DIR_NAME: &str = "Exports";

/// File name of the app settings.
pub(crate) const SETTINGS_FILE_NAME: &str = "settings.json";

/// File name of the VST3 catalog cache.
#[cfg(target_os = "macos")]
pub(crate) const VST3_CACHE_FILE_NAME: &str = "vst3-catalog-cache.json";

/// `dir`, created if absent (best effort — a failure surfaces on first use).
fn ensured(dir: PathBuf) -> PathBuf {
    let _ = fs::create_dir_all(&dir);
    dir
}

/// The `Stev` library under Documents (or home, or `.`), created if
/// absent. Only user content lives here.
pub(crate) fn library_dir() -> PathBuf {
    let base = dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    ensured(base.join(LIBRARY_DIR_NAME))
}

/// The projects root (`Stev/Projects/`), created if absent: each
/// subfolder is one project.
pub(crate) fn project_dir() -> PathBuf {
    ensured(library_dir().join(PROJECTS_DIR_NAME))
}

/// The audio exports' folder (`Stev/Exports/`), beside the projects root,
/// whichever project an export came from. Not created here: the first
/// export makes it (`storage::create_audio_export_dir`).
pub(crate) fn exports_dir() -> PathBuf {
    library_dir().join(EXPORTS_DIR_NAME)
}

/// The app's folder in the platform directory `base`, created if absent;
/// the library when the platform has none.
fn app_dir(base: Option<PathBuf>) -> PathBuf {
    base.map_or_else(library_dir, |d| ensured(d.join(APP_DIR_NAME)))
}

/// The app's settings directory (`~/Library/Application Support/stev`,
/// `~/.config/stev`, `%APPDATA%\stev`).
pub(crate) fn config_dir() -> PathBuf {
    app_dir(dirs::config_dir())
}

/// The app's cache directory (`~/Library/Caches/stev`). Only the VST3
/// catalog cache lives there, so it exists on macOS alone, like the plugin
/// host.
#[cfg(target_os = "macos")]
pub(crate) fn cache_dir() -> PathBuf {
    app_dir(dirs::cache_dir())
}

/// The app's logs folder, beside the user's other apps' logs: `stev.log`
/// (`core::logging`) and the audio overrun journal. `~/Library/Logs/stev`
/// on macOS, the local-data directory's `stev/logs` elsewhere, falling back
/// to the current directory. Not created here: each writer makes it when it
/// first writes.
pub(crate) fn logs_dir() -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs").join(APP_DIR_NAME))
    } else {
        dirs::data_local_dir().map(|d| d.join(APP_DIR_NAME).join("logs"))
    };
    dir.unwrap_or_else(|| PathBuf::from("."))
}
