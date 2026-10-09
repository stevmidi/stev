//! The CLAP format module: everything specific to hosting a CLAP plugin,
//! behind the plugin host's shared [`InstrumentVoice`](super::voice::InstrumentVoice)
//! and [`InstrumentEditor`](super::editor::InstrumentEditor) traits.
//!
//! - [`discovery`] scans the installed `.clap` bundles for the catalog.
//! - [`engine`] loads and activates one plugin on the main thread.
//! - [`voice`] is the `Send` audio-thread half, driven by the shared mixer.
//! - [`editor`] is the `!Send` main-thread half, with the plugin's own window.
//! - [`host`] is the `clack-host` `HostHandlers` implementation the plugin
//!   calls back into.
//!
//! See `130-plugin-host.md`.

mod discovery;
mod editor;
mod engine;
mod host;
mod voice;

use std::sync::Arc;

use crate::core::audio::MAX_FRAMES;
use crate::core::plugin_host::catalog::PluginCatalogEntry;
use crate::core::plugin_host::{LoadedInstrument, PluginAudioHandle};

pub(super) use editor::ClapEditor;
use voice::ClapVoice;

/// CLAP's half of the shared catalog scan — see
/// [`catalog::scan_catalog`](crate::core::plugin_host::catalog::scan_catalog),
/// which merges every format's results and sorts them.
pub(super) fn scan_catalog() -> Vec<PluginCatalogEntry> {
    discovery::scan_catalog()
}

/// CLAP's half of [`load_instrument`](crate::core::plugin_host::load_instrument):
/// loads and activates `entry`'s plugin on the main thread, then splits it into
/// the `Send` voice the mixer takes and the `!Send` editor the UI keeps.
pub(super) fn load(
    handle: &PluginAudioHandle,
    track: usize,
    entry: &PluginCatalogEntry,
    state: Option<&[u8]>,
) -> Result<LoadedInstrument, String> {
    let loaded = engine::load(
        &entry.bundle_path,
        &entry.plugin_id,
        handle.sample_rate,
        MAX_FRAMES as u32,
        Arc::clone(&handle.repaint),
        state,
    )?;

    let voice = ClapVoice::new(loaded.processor, MAX_FRAMES, &loaded.io);
    let editor = ClapEditor::new(loaded.instance, &loaded.name, track);
    Ok((Box::new(voice), Box::new(editor)))
}
