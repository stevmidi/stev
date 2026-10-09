//! CLAP's half of the plugin catalog: enumerate the instrument plugins each
//! installed `.clap` bundle exposes, so the Track modal can offer a pick list.
//! The bundle walk itself is shared with every other format — see
//! [`catalog`](crate::core::plugin_host::catalog).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use clack_host::entry::PluginEntryError;
use clack_host::plugin::PluginDescriptor;
use clack_host::prelude::*;

use crate::core::plugin_host::catalog::{PluginCatalogEntry, PluginFormat, installed_bundles};

/// Process-wide cache of loaded CLAP entries, keyed by bundle path. Loading a
/// bundle (dylib load + CLAP `init()`) is the expensive part of both
/// [`scan_catalog`] and [`super::engine::load`]; `PluginEntry` is cheap to
/// clone (it's a handle around a ref-counted, `Send + Sync` loaded library —
/// "Entries and factories are all thread-safe by the CLAP spec"), so caching
/// it lets a bundle be loaded at most once per app run, shared between
/// discovery and every later instrument load for that bundle. Hosting several
/// plugin instances off one shared entry/factory is the standard CLAP usage
/// pattern, not a new safety assumption. Never evicted for the process
/// lifetime — a plugin installed while the app is running still won't be
/// picked up until restart, same as before this cache existed.
static ENTRY_CACHE: LazyLock<Mutex<HashMap<PathBuf, PluginEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A plugin's display name: its descriptor's `name`, or `plugin_id` when that
/// is missing or blank. Shared by the catalog and the loader so the Track modal
/// and the editor title agree.
pub(super) fn descriptor_name(descriptor: &PluginDescriptor, plugin_id: &str) -> String {
    descriptor
        .name()
        .map(|c| c.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| plugin_id.to_owned())
}

/// Returns the cached entry for `bundle_path`, loading (and caching) it if
/// this is the first request for this bundle in the process's lifetime.
pub(super) fn cached_entry(bundle_path: &Path) -> Result<PluginEntry, PluginEntryError> {
    let mut cache = ENTRY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.get(bundle_path) {
        return Ok(entry.clone());
    }
    // SAFETY: we trust the CLAP bundles installed on this machine to be
    // compliant entry points — acceptable for a local PoC.
    let entry = unsafe { PluginEntry::load(bundle_path) }?;
    cache.insert(bundle_path.to_path_buf(), entry.clone());
    Ok(entry)
}

/// Scans every installed `.clap` bundle and returns one entry per plugin
/// descriptor. Bundles that fail to load or expose no factory are skipped
/// silently. This loads each bundle's dylib to read its metadata, so it is not
/// free — the caller runs it on a background thread
/// ([`start_plugin_catalog_scan`](crate::core::plugin_host::start_plugin_catalog_scan));
/// every entry it opens stays in [`ENTRY_CACHE`] afterward so a later
/// [`load`](super::engine::load) of the same bundle skips straight to
/// `instantiate()`. Sorting across formats is the shared
/// [`scan_catalog`](crate::core::plugin_host::catalog::scan_catalog)'s job.
pub(super) fn scan_catalog() -> Vec<PluginCatalogEntry> {
    let mut out = Vec::new();
    for bundle in installed_bundles(PluginFormat::Clap) {
        let Ok(entry) = cached_entry(&bundle) else {
            continue;
        };
        let Some(factory) = entry.get_plugin_factory() else {
            continue;
        };
        for descriptor in factory.plugin_descriptors() {
            let Some(id) = descriptor.id() else {
                continue;
            };
            let plugin_id = id.to_string_lossy().into_owned();
            let name = descriptor_name(descriptor, &plugin_id);
            out.push(PluginCatalogEntry {
                format: PluginFormat::Clap,
                bundle_path: bundle.clone(),
                plugin_id,
                name,
            });
        }
    }
    out
}
