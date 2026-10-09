//! Out-of-process bundle scanning: the child-process mode, and the parent side
//! that drives it.
//!
//! ## Why the scan cannot run in our process
//!
//! A VST3 `bundleEntry` is not a metadata read — it starts the plugin's real
//! runtime, and some of those runtimes demand the **main thread**. Kontakt 8's
//! constructs a full Qt `QApplication`, whose Cocoa platform integration calls
//! HIToolbox's Text Services Manager, which asserts it is on the main queue and
//! traps with `SIGILL` when it is not:
//!
//! ```text
//! scan → ModuleStartup::startup() → ni::qt::Module::Module
//!      → QApplicationPrivate::init → QCocoaIntegration → QCocoaInputContext
//!      → TSMGetInputSourcePropertyWithSetter
//!      → islGetInputSourceListWithAdditions → dispatch_assert_queue → SIGILL
//! ```
//!
//! Our own main thread is not available: it belongs to winit/egui, a scan takes
//! the better part of a minute, and handing it to a second GUI toolkit's event
//! dispatcher is the conflict every serious host scans out-of-process to avoid.
//!
//! ## The design
//!
//! The parent re-invokes **its own binary** with [`SCAN_FLAG`], one child per
//! bundle. The child scans that one bundle on *its* main thread — which is
//! free — writes the result as JSON to a file the parent names, and exits.
//! One child per bundle rather than one for the whole batch because it makes a
//! plugin that crashes or hangs cost exactly itself: the parent records the
//! failure and moves on with the rest of the catalog intact.
//!
//! Results go to a **file**, not stdout, because plugins print to stdout and
//! stderr as they initialise and would otherwise corrupt the payload.
//!
//! The child ends with [`libc::_exit`], never a normal return: a normal exit
//! runs the loaded plugin's static destructors, which is its own reliable way
//! to abort (see `180-vst3-host.md`). The child has nothing worth flushing
//! beyond the result file, which it closes first.
//!
//! See `docs/180-vst3-host.md`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::cache::CachedClass;
use super::discovery::classes_in_bundle;

/// The argument that puts a freshly-launched Stev into scan-child mode.
/// Checked before anything else in `main`.
pub(crate) const SCAN_FLAG: &str = "--scan-vst3-bundle";

/// How long a single bundle gets before the parent gives up on it and records
/// a failure. Generous: the heaviest sampler libraries take several seconds to
/// initialise, and a false timeout costs the user that plugin until they
/// reinstall it. A hung child is killed.
const SCAN_TIMEOUT: Duration = Duration::from_secs(30);

/// What a scan child writes out.
#[derive(Serialize, Deserialize, Default)]
struct ScanResult {
    /// The instrument classes found. Empty is a valid, cacheable answer — the
    /// bundle is an effect.
    classes: Vec<CachedClass>,
}

/// If this process was launched as a scan child, do the scan and never return.
/// Call first thing in `main`, before any app setup: the child must not open
/// audio devices, MIDI ports or a window.
///
/// Returns normally — having done nothing — when the flag is absent.
pub(crate) fn run_if_scan_child() {
    let args: Vec<String> = std::env::args().collect();
    let Some(flag_at) = args.iter().position(|a| a == SCAN_FLAG) else {
        return;
    };
    let (Some(bundle), Some(out_path)) = (args.get(flag_at + 1), args.get(flag_at + 2)) else {
        // Malformed invocation: fail loudly rather than fall through into the
        // app, which would open a second window and a second audio device.
        eprintln!("{SCAN_FLAG} needs <bundle path> <output path>");
        exit_without_destructors(2);
    };

    // This is the whole point of the child: `main` runs on the main thread, so
    // a plugin that requires it during `bundleEntry` gets it.
    let classes = classes_in_bundle(Path::new(bundle));

    let code = match serde_json::to_string(&ScanResult { classes }) {
        Ok(text) => match std::fs::write(out_path, text) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("vst3 scan child: could not write {out_path}: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("vst3 scan child: could not serialise the result: {e}");
            1
        }
    };
    exit_without_destructors(code);
}

/// Ends the child immediately, skipping the loaded plugin's static destructors.
///
/// Running them is not merely wasteful — it is a reliable way to abort, because
/// they tear down the plugin's statics underneath its own still-running worker
/// threads (see `180-vst3-host.md`). The result file is already written and
/// closed by the time this is called, so there is nothing to flush.
fn exit_without_destructors(code: i32) -> ! {
    // SAFETY: `_exit` is async-signal-safe and always valid to call. It ends
    // the process without unwinding, running `atexit` handlers, or invoking
    // static destructors — which is precisely why it is used here.
    unsafe { libc::_exit(code) }
}

/// Scans one bundle in a child process. `Ok(classes)` on a clean scan (an empty
/// vector meaning "no instruments here"), `Err` if the child failed, crashed or
/// timed out — which the caller caches as a failure so it is not retried on
/// every launch.
pub(super) fn scan_bundle_out_of_process(bundle: &Path) -> Result<Vec<CachedClass>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("no path to our own binary: {e}"))?;
    let out_path = result_path(bundle);
    // A stale file from a previous run must not be mistaken for this run's
    // result if the child dies before writing.
    let _ = std::fs::remove_file(&out_path);

    let mut child = Command::new(exe)
        .arg(SCAN_FLAG)
        .arg(bundle)
        .arg(&out_path)
        // Plugins are chatty during initialisation and none of it is ours to
        // relay; the real payload goes to `out_path`.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not spawn a scan child: {e}"))?;

    let status = wait_with_timeout(&mut child, SCAN_TIMEOUT)?;
    let result = std::fs::read_to_string(&out_path)
        .ok()
        .and_then(|text| serde_json::from_str::<ScanResult>(&text).ok());
    let _ = std::fs::remove_file(&out_path);

    match result {
        // A result file means the scan itself completed, whatever the plugin
        // got up to on its way out afterwards.
        Some(result) => Ok(result.classes),
        None => Err(format!("scan child produced no result ({status})")),
    }
}

/// Waits for `child`, killing it if it outlives `timeout`. Returns a short
/// description of how it ended, for the failure message.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(format!("exit status {status}")),
            Ok(None) => {}
            Err(e) => return Err(format!("could not wait for the scan child: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(format!("timed out after {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Where a child writes its result. Includes our own process id so two
/// Stev instances scanning at once cannot read each other's files.
fn result_path(bundle: &Path) -> PathBuf {
    let stem: String = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    std::env::temp_dir().join(format!("stev-vst3-scan-{}-{stem}.json", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_paths_are_per_process_and_per_bundle() {
        let a = result_path(Path::new("/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3"));
        let b = result_path(Path::new("/Library/Audio/Plug-Ins/VST3/Serum2.vst3"));
        assert_ne!(a, b);
        let pid = std::process::id().to_string();
        assert!(a.to_string_lossy().contains(&pid));
        // Spaces and punctuation from the bundle name must not reach the path.
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("Kontakt8")
        );
    }

    #[test]
    fn a_bundle_with_no_usable_name_still_yields_a_path() {
        let path = result_path(Path::new("/"));
        assert!(path.to_string_lossy().contains("stev-vst3-scan-"));
    }

    #[test]
    fn an_empty_class_list_round_trips_as_a_valid_result() {
        // "This bundle is an effect" must survive as a cacheable answer, not
        // decay into a parse failure.
        let text = serde_json::to_string(&ScanResult::default()).unwrap();
        let back: ScanResult = serde_json::from_str(&text).unwrap();
        assert!(back.classes.is_empty());
    }
}
