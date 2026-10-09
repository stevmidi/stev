//! The installed-plugin catalog the Track modal offers, across every hosted
//! plugin format.
//!
//! [`scan_catalog`] merges each format's own scan into one list sorted by
//! display name. Every format's scan is the same shape — walk the standard
//! install roots for bundles with the format's extension, then ask each bundle
//! what plugins it exposes — so the bundle walk lives here, and each format
//! calls [`installed_bundles`].
//!
//! Scanning loads each bundle's dylib just to read its metadata, so it is not
//! free: the caller runs it on the `"plugin-catalog-scan"` thread
//! ([`start_plugin_catalog_scan`](super::start_plugin_catalog_scan)).

use std::path::{Path, PathBuf};

use super::{clap, vst3};

/// A plugin format Stev can host. A track's instrument is one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum PluginFormat {
    /// CLAP — hosted through `clack-host`. See `130-plugin-host.md`.
    #[default]
    Clap,
    /// VST3 — hosted through the raw `vst3` bindings. See `180-vst3-host.md`.
    Vst3,
}

impl PluginFormat {
    /// Short name for the UI and for log lines.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PluginFormat::Clap => "CLAP",
            PluginFormat::Vst3 => "VST3",
        }
    }

    /// The bundle extension this format's plugins are installed as, without
    /// the leading dot.
    fn bundle_extension(self) -> &'static str {
        match self {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
        }
    }
}

/// Every plugin format the host can load, for the scans and listings that have
/// to cover all of them.
pub(crate) const ALL_FORMATS: [PluginFormat; 2] = [PluginFormat::Clap, PluginFormat::Vst3];

/// One selectable plugin: a bundle plus the plugin id within it (a bundle can
/// expose several — e.g. u-he `Repro-1.clap` exposes `Repro-1` and `Repro-5`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginCatalogEntry {
    /// Which format's host loads this plugin.
    pub(crate) format: PluginFormat,
    /// Path to the plugin bundle.
    pub(crate) bundle_path: PathBuf,
    /// Plugin id within the bundle.
    pub(crate) plugin_id: String,
    /// Display name.
    pub(crate) name: String,
}

impl PluginCatalogEntry {
    /// Whether this is the plugin `plugin_id` in the bundle at `bundle_path`
    /// — a plugin's identity, whatever its display name.
    pub(crate) fn is(&self, bundle_path: &Path, plugin_id: &str) -> bool {
        self.bundle_path == bundle_path && self.plugin_id == plugin_id
    }
}

/// The standard macOS plug-in install roots for a format's `subdir` under
/// `Audio/Plug-Ins` — system-wide plus per-user. The subdirectory is the
/// format's [`label`](PluginFormat::label) (`CLAP`, `VST3`).
fn install_roots(subdir: &str) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Library/Audio/Plug-Ins").join(subdir)];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(
            PathBuf::from(home)
                .join("Library/Audio/Plug-Ins")
                .join(subdir),
        );
    }
    roots
}

/// Recursively collects bundle paths with `extension` under `dir` (bundles are
/// leaves — we don't descend into them). Depth-capped to keep it cheap; vendors
/// nest bundles one level deep (`CLAP/u-he/Repro-1.clap`).
fn collect_bundles(dir: &Path, extension: &str, depth: u8, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        } else if path.is_dir() {
            collect_bundles(&path, extension, depth - 1, out);
        }
    }
}

/// Every bundle of `format` installed on this machine, deduplicated and sorted.
pub(crate) fn installed_bundles(format: PluginFormat) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in install_roots(format.label()) {
        collect_bundles(
            &root,
            format.bundle_extension(),
            BUNDLE_SCAN_DEPTH,
            &mut found,
        );
    }
    found.sort();
    found.dedup();
    found
}

/// How deep under an install root a bundle may be nested before the scan gives
/// up. Vendors nest one level (`CLAP/u-he/Repro-1.clap`); four is generous.
const BUNDLE_SCAN_DEPTH: u8 = 4;

/// Scans every hosted format, calling `deliver` with the catalog so far after
/// each format completes.
///
/// It reports per format rather than once at the end because the formats are
/// nowhere near equally expensive: a CLAP scan is close to instant, while
/// scanning ~60 installed VST3 bundles takes the better part of a minute (see
/// [`super::vst3::scan_catalog`]). Waiting for the slow one
/// would leave the Track modal empty of *every* plugin for that whole time.
/// Each call hands over the full list so far, already sorted, so the caller
/// just replaces what it holds.
///
/// Sorting by name across formats — rather than grouping by format — is
/// deliberate: the user is looking for a plugin by its name, and several are
/// installed in both formats, so the two land adjacent and the format tag in
/// the Track modal is what tells them apart.
pub(crate) fn scan_catalog(deliver: impl FnMut(Vec<PluginCatalogEntry>)) {
    // Cheapest format first, so something reaches the modal quickly.
    merge_scans([clap::scan_catalog, vst3::scan_catalog], deliver);
}

/// Runs each of `scans` in turn, delivering the merged, sorted list after
/// each one. Each scan only *starts* after the previous one's delivery —
/// which is the point: the formats are handed over as functions rather than
/// as their results, so a slow scan cannot hold back a fast one's list.
fn merge_scans<const N: usize>(
    scans: [fn() -> Vec<PluginCatalogEntry>; N],
    mut deliver: impl FnMut(Vec<PluginCatalogEntry>),
) {
    let mut out = Vec::new();
    for scan in scans {
        out.extend(scan());
        out.sort_by_cached_key(|e| (e.name.to_lowercase(), e.format));
        deliver(out.clone());
    }
}

/// A newline-indented list of every installed bundle, across formats —
/// appended to a load error so a wrong bundle path is easy to diagnose.
pub(crate) fn available_bundles_hint() -> String {
    let mut lines = String::new();
    for format in ALL_FORMATS {
        for path in installed_bundles(format) {
            lines.push_str(&format!("\n  {}", path.display()));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\nAvailable plugin bundles:{lines}")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn install_roots_cover_system_and_user() {
        let roots = install_roots("CLAP");
        assert!(roots.contains(&PathBuf::from("/Library/Audio/Plug-Ins/CLAP")));
        if std::env::var_os("HOME").is_some() {
            assert_eq!(roots.len(), 2);
            assert!(roots[1].ends_with("Library/Audio/Plug-Ins/CLAP"));
        }
    }

    #[test]
    fn every_format_has_a_distinct_extension_and_label() {
        // Guards against a copy-pasted arm when another format lands.
        for (i, a) in ALL_FORMATS.iter().enumerate() {
            for b in &ALL_FORMATS[i + 1..] {
                assert_ne!(a.bundle_extension(), b.bundle_extension());
                assert_ne!(a.label(), b.label());
            }
        }
    }

    thread_local! {
        /// Deliveries seen when each fake scan starts, for the ordering test.
        static SCAN_LOG: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    }

    fn entry(format: PluginFormat, name: &str) -> PluginCatalogEntry {
        PluginCatalogEntry {
            format,
            bundle_path: PathBuf::new(),
            plugin_id: name.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn each_scan_is_delivered_before_the_next_one_starts() {
        fn fast() -> Vec<PluginCatalogEntry> {
            SCAN_LOG.with(|log| log.borrow_mut().push(0));
            vec![
                entry(PluginFormat::Clap, "zeta"),
                entry(PluginFormat::Clap, "Alpha"),
            ]
        }
        fn slow() -> Vec<PluginCatalogEntry> {
            SCAN_LOG.with(|log| log.borrow_mut().push(1));
            vec![
                entry(PluginFormat::Vst3, "alpha"),
                entry(PluginFormat::Vst3, "Mid"),
            ]
        }

        let mut deliveries: Vec<Vec<String>> = Vec::new();
        merge_scans([fast, slow], |list| {
            SCAN_LOG.with(|log| log.borrow_mut().push(10 + deliveries.len()));
            deliveries.push(list.into_iter().map(|e| e.name).collect());
        });

        // Regression: the slow scan must not run before the fast one's list
        // has been handed over.
        assert_eq!(
            SCAN_LOG.with(|log| log.borrow().clone()),
            vec![0, 10, 1, 11]
        );
        // Sorted case-insensitively by name; a tie keeps CLAP first.
        assert_eq!(deliveries[0], vec!["Alpha", "zeta"]);
        assert_eq!(deliveries[1], vec!["Alpha", "alpha", "Mid", "zeta"]);
    }

    #[test]
    fn collect_bundles_finds_nested_bundles_and_stops_at_the_depth_cap() {
        let root = std::env::temp_dir().join(format!("stev-catalog-{}", std::process::id()));
        let nested = root.join("vendor");
        std::fs::create_dir_all(nested.join("Deep.clap")).unwrap();
        std::fs::create_dir_all(root.join("Top.clap")).unwrap();
        std::fs::create_dir_all(root.join("Other.vst3")).unwrap();

        let mut found = Vec::new();
        collect_bundles(&root, "clap", 4, &mut found);
        found.sort();
        assert_eq!(found, vec![root.join("Top.clap"), nested.join("Deep.clap")]);

        // Depth 1 sees only the root's own entries, not the vendor subdirectory.
        let mut shallow = Vec::new();
        collect_bundles(&root, "clap", 1, &mut shallow);
        assert_eq!(shallow, vec![root.join("Top.clap")]);

        // The extension filter keeps the formats apart.
        let mut vst3 = Vec::new();
        collect_bundles(&root, "vst3", 4, &mut vst3);
        assert_eq!(vst3, vec![root.join("Other.vst3")]);

        std::fs::remove_dir_all(&root).ok();
    }
}
