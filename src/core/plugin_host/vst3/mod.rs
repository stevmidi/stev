//! The VST3 format module: everything specific to hosting a VST3 plugin,
//! behind the plugin host's shared traits.
//!
//! - [`module`] loads a `.vst3` bundle and gets at its plugin factory.
//! - [`discovery`] enumerates the instrument classes for the catalog.
//! - [`child`] runs the *scan's* bundle load in a **separate process** — some
//!   plugins' `bundleEntry` requires a main thread of its own — and [`cache`]
//!   remembers the result so that only happens once per plugin install.
//! - [`component`] instantiates and activates one plugin, and owns the COM
//!   dance VST3 requires to wire a component to its edit controller.
//! - [`voice`] is the `Send` audio-thread half, [`editor`] the `!Send`
//!   main-thread half, and [`events`] turns MIDI into VST3 note events.
//! - [`host`] is the set of COM objects handed *to* the plugin.
//!
//! See `docs/180-vst3-host.md`.

mod cache;
pub(crate) mod child;
mod component;
mod discovery;
mod editor;
mod events;
mod host;
mod message;
mod module;
mod params;
mod state;
mod stream;
mod voice;

use crate::core::audio::MAX_FRAMES;
use crate::core::plugin_host::catalog::PluginCatalogEntry;
use crate::core::plugin_host::{LoadedInstrument, PluginAudioHandle};

use editor::Vst3Editor;
use voice::Vst3Voice;

/// VST3's half of [`load_instrument`](crate::core::plugin_host::load_instrument):
/// instantiates and activates `entry`'s plugin on the main thread, then splits
/// it into the `Send` voice the mixer takes and the `!Send` editor the UI keeps.
pub(super) fn load(
    handle: &PluginAudioHandle,
    track: usize,
    entry: &PluginCatalogEntry,
    state: Option<&[u8]>,
) -> Result<LoadedInstrument, String> {
    let loaded = component::load(
        &entry.bundle_path,
        &entry.plugin_id,
        handle.sample_rate,
        MAX_FRAMES as u32,
        state,
    )?;
    let voice = Vst3Voice::new(
        loaded.processor,
        MAX_FRAMES,
        &loaded.io,
        loaded.params,
        loaded.midi_map,
        loaded.tail_samples,
        handle.sample_rate,
    );
    let editor = Vst3Editor::new(loaded.main, &entry.name, track);
    Ok((Box::new(voice), Box::new(editor)))
}

/// VST3's half of the shared catalog scan — see
/// [`catalog::scan_catalog`](crate::core::plugin_host::catalog::scan_catalog),
/// which merges every format's results and sorts them.
pub(super) fn scan_catalog() -> Vec<PluginCatalogEntry> {
    discovery::scan_catalog()
}
