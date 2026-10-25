//! The app's log: the backend behind the `log` macros, writing every line to
//! stderr and to `stev.log` in the logs folder (`~/Library/Logs/stev/` on
//! macOS), plus a panic hook that writes a crash there too. From a `.app`,
//! stderr goes nowhere, so the file is what a bug report attaches.
//!
//! The file is rotated once at launch: the previous run's log is kept as
//! `stev.1.log`, so the run that crashed survives the relaunch after it.
//! Release builds keep `info!` and above (`log`'s `release_max_level_info`);
//! `debug!` compiles away there. Other crates log
//! at `warn!` and above. See `080-conventions.md` § Logging.
//!
//! Never from the audio thread above `debug!`: a log line takes the file's
//! lock. The overrun journal (`core::audio::journal`) is the pattern there.

use std::backtrace::Backtrace;
use std::env::consts::{ARCH, OS};
use std::fs::{self, File};
use std::io;
use std::panic;
use std::path::{Path, PathBuf};
use std::thread;

use chrono::Local;
use fern::Dispatch;
use log::{LevelFilter, error, info, warn};

use crate::core::paths::logs_dir;
use crate::core::update_check::CURRENT_VERSION;

/// File name of this run's log, in the logs folder.
const LOG_FILE_NAME: &str = "stev.log";

/// File name the previous run's log is kept under.
const PREVIOUS_LOG_FILE_NAME: &str = "stev.1.log";

/// How both log files in the logs folder, this one and the overrun journal,
/// write a line's local time, so their lines compare side by side.
pub(crate) const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// This crate's name, the first segment of every target it logs under.
const CRATE_NAME: &str = env!("CARGO_CRATE_NAME");

/// This run's log file (`stev.log` in the logs folder), for the About tab's
/// "Show log in Finder".
pub(crate) fn log_path() -> PathBuf {
    logs_dir().join(LOG_FILE_NAME)
}

/// Installs the logger and the panic hook. Called once, early in `main`,
/// after the VST3 scan child has split off: a child installs no logger, so
/// it never rotates the running app's log away. An unwritable logs folder
/// leaves stderr alone, and says so there.
pub(crate) fn init() {
    let path = log_path();
    let dispatch = Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {:<5} [{}] {}: {message}",
                Local::now().format(TIMESTAMP_FORMAT),
                record.level(),
                thread::current().name().unwrap_or("unnamed"),
                short_target(record.target()),
            ))
        })
        .level(LevelFilter::Warn)
        // `Debug`, capped at `Info` in release by `release_max_level_info`.
        .level_for(CRATE_NAME, LevelFilter::Debug)
        .chain(io::stderr());
    let (dispatch, file_error) = match rotate_and_create(&path) {
        Ok(file) => (dispatch.chain(file), None),
        Err(e) => (dispatch, Some(e)),
    };
    // Fails only if a logger is already set, which `main` never does.
    let _ = dispatch.apply();
    install_panic_hook();

    info!("Stev {CURRENT_VERSION} ({OS}, {ARCH})");
    match file_error {
        None => info!("log file: {}", path.display()),
        Some(e) => warn!("no log file at {}: {e}", path.display()),
    }
}

/// Keeps the log at `path`, if any, as the previous run's (replacing the one
/// before it), and creates a fresh, empty one in its place.
fn rotate_and_create(path: &Path) -> io::Result<File> {
    // No log yet on a first run; any other failure leaves `create` to
    // truncate the old one instead.
    let _ = fs::rename(path, path.with_file_name(PREVIOUS_LOG_FILE_NAME));
    File::create(path)
}

/// `target` without this crate's own name in front: `core::settings` rather
/// than `stev::core::settings`. Another crate's target is left whole.
fn short_target(target: &str) -> &str {
    target
        .strip_prefix(CRATE_NAME)
        .and_then(|rest| rest.strip_prefix("::"))
        .unwrap_or(target)
}

/// Writes every panic to the log, with its thread and a backtrace, instead of
/// the default hook's stderr alone (the log's stderr output still prints it).
fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let thread = thread::current();
        let name = thread.name().unwrap_or("unnamed");
        error!("thread '{name}' {info}\n{}", Backtrace::force_capture());
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty scratch folder for one test.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stev-log-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn launches_keep_exactly_the_previous_run() {
        let dir = scratch_dir("rotate");
        let path = dir.join(LOG_FILE_NAME);
        let previous = dir.join(PREVIOUS_LOG_FILE_NAME);

        // First run: no log yet.
        rotate_and_create(&path).unwrap();
        assert!(path.exists());
        assert!(!previous.exists());
        fs::write(&path, "run 1").unwrap();

        rotate_and_create(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        assert_eq!(fs::read_to_string(&previous).unwrap(), "run 1");
        fs::write(&path, "run 2").unwrap();

        // The run before the previous one is gone.
        rotate_and_create(&path).unwrap();
        assert_eq!(fs::read_to_string(&previous).unwrap(), "run 2");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_targets_drop_the_crate_name() {
        assert_eq!(short_target("stev::core::settings"), "core::settings");
        assert_eq!(short_target("stev"), "stev");
        assert_eq!(short_target("egui_glow::painter"), "egui_glow::painter");
        // A crate whose name merely starts with ours keeps it.
        assert_eq!(short_target("stevedore::x"), "stevedore::x");
    }
}
