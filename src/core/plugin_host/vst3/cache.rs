//! On-disk cache of VST3 scan results, so the expensive part of discovery
//! happens once per plugin install rather than once per app start.
//!
//! Scanning a bundle means launching a child process that loads the plugin's
//! executable and runs `bundleEntry` — around half a second each, and several
//! seconds for the heavyweights, for ~60 bundles. That cost is unavoidable the
//! first time and pure waste every time after, since a plugin's class list only
//! changes when the plugin itself does. Each entry is therefore keyed by the
//! bundle's **modification time**: an unchanged bundle is never rescanned, and
//! reinstalling or updating one invalidates just that entry.
//!
//! Effects are cached too, as an entry with no classes. Most installed bundles
//! are effects, so *not* remembering them would leave most of the scan cost in
//! place. Bundles whose scan failed are remembered as well — see
//! [`CachedBundle::failed`]. See `docs/180-vst3-host.md`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::core::paths::{VST3_CACHE_FILE_NAME, cache_dir};

/// Bumped whenever the meaning of a cached field changes, so an old cache is
/// discarded rather than misread. The scan is expensive but reproducible, so
/// throwing the file away is always safe.
const CACHE_VERSION: u32 = 1;

/// One plugin class remembered from a scan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedClass {
    /// The class UID, as [`uid_to_hex`](super::discovery::uid_to_hex) renders it.
    pub(super) plugin_id: String,
    /// Display name.
    pub(super) name: String,
}

/// What one scanned bundle yielded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedBundle {
    /// The bundle's modification time when it was scanned, in nanoseconds since
    /// the Unix epoch. The cache key proper — a bundle whose mtime still
    /// matches is not rescanned.
    pub(super) mtime_ns: u128,
    /// The instrument classes the bundle exposes. Empty for an effect-only
    /// bundle, which is a perfectly good thing to remember: most installed
    /// bundles are effects, and rescanning them every launch is most of the
    /// cost this cache exists to avoid.
    pub(super) classes: Vec<CachedClass>,
    /// Set when the scan of this bundle did not complete — the child process
    /// crashed, hung past its timeout, or reported an error. Such a bundle is
    /// **skipped on later runs** rather than retried, because retrying means
    /// paying its full cost (and possibly its crash) on every single app start.
    /// Updating or reinstalling the plugin changes its mtime and clears this.
    #[serde(default)]
    pub(super) failed: bool,
}

/// The whole cache file.
#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    /// [`CACHE_VERSION`] this file was written by.
    #[serde(default)]
    version: u32,
    /// Scan results, keyed by bundle path.
    #[serde(default)]
    bundles: HashMap<String, CachedBundle>,
}

/// The scan cache, held in memory for the duration of one scan and written back
/// once at the end.
pub(super) struct ScanCache {
    /// Scan results, keyed by bundle path.
    bundles: HashMap<String, CachedBundle>,
    /// Whether anything changed and the file is worth rewriting.
    dirty: bool,
}

impl ScanCache {
    /// Loads the cache, or starts an empty one if it is missing, unreadable, or
    /// written by a different [`CACHE_VERSION`].
    pub(super) fn load() -> Self {
        let bundles = fs::read_to_string(cache_path())
            .ok()
            .and_then(|text| serde_json::from_str::<CacheFile>(&text).ok())
            .filter(|file| file.version == CACHE_VERSION)
            .map(|file| file.bundles)
            .unwrap_or_default();
        Self {
            bundles,
            dirty: false,
        }
    }

    /// The cached result for `bundle`, if one was stored against its current
    /// modification time. `None` means "needs scanning" — including the case
    /// where the bundle's mtime cannot be read at all, since then we cannot
    /// prove a cached entry still applies.
    pub(super) fn get(&self, bundle: &Path) -> Option<&CachedBundle> {
        let mtime = bundle_mtime_ns(bundle)?;
        self.bundles
            .get(&bundle.to_string_lossy().into_owned())
            .filter(|cached| cached.mtime_ns == mtime)
    }

    /// Records a scan result against the bundle's current modification time.
    pub(super) fn insert(&mut self, bundle: &Path, classes: Vec<CachedClass>, failed: bool) {
        let Some(mtime_ns) = bundle_mtime_ns(bundle) else {
            return;
        };
        self.bundles.insert(
            bundle.to_string_lossy().into_owned(),
            CachedBundle {
                mtime_ns,
                classes,
                failed,
            },
        );
        self.dirty = true;
    }

    /// Drops entries for bundles that are no longer installed, so an
    /// uninstalled plugin doesn't linger in the file forever.
    pub(super) fn retain_installed(&mut self, installed: &[PathBuf]) {
        let installed: Vec<String> = installed
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let before = self.bundles.len();
        self.bundles.retain(|path, _| installed.contains(path));
        if self.bundles.len() != before {
            self.dirty = true;
        }
    }

    /// Writes the cache back, if anything changed. A failure here is not worth
    /// reporting: the only cost is a slower scan next time.
    pub(super) fn save(&self) {
        if !self.dirty {
            return;
        }
        let file = CacheFile {
            version: CACHE_VERSION,
            bundles: self.bundles.clone(),
        };
        if let Ok(text) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(cache_path(), text);
        }
    }
}

/// Where the cache file lives — the app's platform cache directory, since it
/// rebuilds itself if lost.
fn cache_path() -> PathBuf {
    cache_dir().join(VST3_CACHE_FILE_NAME)
}

/// A bundle's modification time in nanoseconds since the Unix epoch, or `None`
/// if it cannot be read.
///
/// This is the directory's own mtime, not the executable's: replacing a plugin
/// touches the bundle directory, and it is one `stat` rather than a walk.
fn bundle_mtime_ns(bundle: &Path) -> Option<u128> {
    let modified = fs::metadata(bundle).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str) -> CachedClass {
        CachedClass {
            plugin_id: "0".repeat(32),
            name: name.to_string(),
        }
    }

    fn temp_bundle(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("stev-cache-{}-{tag}.vst3", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_bundle_is_cached_against_its_mtime_and_reused() {
        let bundle = temp_bundle("hit");
        let mut cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        cache.insert(&bundle, vec![class("Synth")], false);
        let hit = cache.get(&bundle).expect("just inserted");
        assert_eq!(hit.classes, vec![class("Synth")]);
        assert!(!hit.failed);
        fs::remove_dir_all(&bundle).ok();
    }

    #[test]
    fn a_changed_mtime_invalidates_the_entry() {
        let bundle = temp_bundle("stale");
        let mut cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        cache.insert(&bundle, vec![class("Synth")], false);
        // Rewrite the stored mtime to something the bundle no longer has —
        // exactly what reinstalling the plugin does.
        let key = bundle.to_string_lossy().into_owned();
        cache.bundles.get_mut(&key).unwrap().mtime_ns = 1;
        assert!(cache.get(&bundle).is_none());
        fs::remove_dir_all(&bundle).ok();
    }

    #[test]
    fn an_effect_only_bundle_is_remembered_as_an_empty_result() {
        // Not caching these would leave most of the scan cost in place, since
        // most installed bundles are effects.
        let bundle = temp_bundle("effect");
        let mut cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        cache.insert(&bundle, Vec::new(), false);
        let hit = cache.get(&bundle).expect("effects are cached too");
        assert!(hit.classes.is_empty());
        fs::remove_dir_all(&bundle).ok();
    }

    #[test]
    fn a_missing_bundle_is_never_a_cache_hit() {
        let cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        assert!(cache.get(Path::new("/nope/does-not-exist.vst3")).is_none());
    }

    #[test]
    fn uninstalled_bundles_are_dropped() {
        let kept = temp_bundle("kept");
        let gone = temp_bundle("gone");
        let mut cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        cache.insert(&kept, vec![class("A")], false);
        cache.insert(&gone, vec![class("B")], false);
        cache.retain_installed(std::slice::from_ref(&kept));
        assert!(cache.get(&kept).is_some());
        assert!(cache.get(&gone).is_none());
        fs::remove_dir_all(&kept).ok();
        fs::remove_dir_all(&gone).ok();
    }

    #[test]
    fn a_cache_from_another_version_is_discarded_not_misread() {
        let file: CacheFile = serde_json::from_str(r#"{"version":0,"bundles":{}}"#).unwrap();
        assert_ne!(file.version, CACHE_VERSION);
    }

    #[test]
    fn a_failed_scan_round_trips() {
        let bundle = temp_bundle("failed");
        let mut cache = ScanCache {
            bundles: HashMap::new(),
            dirty: false,
        };
        cache.insert(&bundle, Vec::new(), true);
        assert!(cache.get(&bundle).expect("stored").failed);
        fs::remove_dir_all(&bundle).ok();
    }
}
