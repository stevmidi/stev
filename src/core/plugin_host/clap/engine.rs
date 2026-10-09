//! Plugin instantiation and activation for the CLAP host. Discovery of the
//! available bundles lives in [`super::discovery`].

use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::prelude::*;

use crate::core::plugin_host::buffers::AudioIoLayout;
use crate::core::plugin_host::catalog::available_bundles_hint;

use super::discovery::{cached_entry, descriptor_name};
use super::host::{ClapHostMainThread, ClapHostShared, StevClapHost};

/// Queries a freshly instantiated plugin's audio-port layout via the
/// `audio-ports` extension (both calls are `[main-thread]`). A plugin without
/// the extension has no audio ports of its own — it still gets a stereo output.
fn query_audio_io(instance: &mut PluginInstance<StevClapHost>) -> AudioIoLayout {
    let Some(ports) = instance.plugin_handle().get_extension::<PluginAudioPorts>() else {
        return AudioIoLayout::new(Vec::new(), Vec::new());
    };

    let inputs = port_channel_counts(instance, ports, true);
    let outputs = port_channel_counts(instance, ports, false);
    AudioIoLayout::new(inputs, outputs)
}

/// Channel count of every input or output port the plugin exposes, in order.
fn port_channel_counts(
    instance: &mut PluginInstance<StevClapHost>,
    ports: PluginAudioPorts,
    is_input: bool,
) -> Vec<u16> {
    let plugin = instance.plugin_handle();
    let count = ports.count(&plugin, is_input);
    let mut buffer = AudioPortInfoBuffer::new();
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        if let Some(info) = ports.get(&plugin, i, is_input, &mut buffer) {
            out.push(info.channel_count.min(u32::from(u16::MAX)) as u16);
        }
    }
    out
}

/// A loaded, activated CLAP instrument, ready to be handed to the audio thread.
pub(super) struct LoadedPlugin {
    /// Session-lived. `PluginInstance` is `!Send` and must stay on the eframe
    /// main thread (it drives the plugin editor); dropping it destroys the
    /// plugin.
    pub(super) instance: PluginInstance<StevClapHost>,
    /// The stopped audio processor — `Send`, wrapped in a `ClapVoice` and
    /// handed to the mixer in the shared `"audio-engine"`.
    pub(super) processor: StoppedPluginAudioProcessor<StevClapHost>,
    /// The plugin's declared audio-port layout, for [`ClapVoice`](super::ClapVoice)'s
    /// buffer allocation.
    pub(super) io: AudioIoLayout,
    /// Display name, for the editor window title.
    pub(super) name: String,
}

/// Loads the plugin `plugin_id` from the `.clap` bundle at `bundle_path` and
/// activates it for the given audio config.
///
/// [`StevClapHost`] is the [`HostHandlers`] implementation: it declares the
/// log, gui and timer host extensions so the plugin editor can be hosted (see
/// `super::host`).
pub(super) fn load(
    bundle_path: &Path,
    plugin_id: &str,
    sample_rate: f64,
    max_frames: u32,
    repaint: Arc<OnceLock<egui::Context>>,
    state: Option<&[u8]>,
) -> Result<LoadedPlugin, String> {
    let path_str = bundle_path.display();

    // Reuses the process-wide entry cache (see `discovery::ENTRY_CACHE`): if
    // the catalog scan or an earlier track's load already opened this
    // bundle, this skips straight past the dylib load + CLAP `init()`.
    let entry = cached_entry(bundle_path).map_err(|e| {
        format!(
            "load CLAP bundle '{path_str}': {e}{}",
            available_bundles_hint()
        )
    })?;

    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| format!("'{path_str}' exposes no plugin factory"))?;

    let (id, name) = factory
        .plugin_descriptors()
        .find_map(|d| {
            let id = d.id()?;
            (id.to_string_lossy() == plugin_id)
                .then(|| (id.to_owned(), descriptor_name(d, plugin_id)))
        })
        .ok_or_else(|| format!("plugin id '{plugin_id}' not found in '{path_str}'"))?;

    let host_info = HostInfo::new(
        "Stev",
        "Stev",
        "https://github.com/stevmidi/stev",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| format!("host info: {e}"))?;

    let mut instance = PluginInstance::<StevClapHost>::new(
        |_| ClapHostShared::new(Arc::clone(&repaint)),
        |shared| ClapHostMainThread::new(shared),
        &entry,
        &id,
        &host_info,
    )
    .map_err(|e| format!("instantiate '{name}': {e}"))?;

    // Restore the persisted preset before activation — this is when a fresh
    // instance is expected to receive its state (matches how DAWs reload a
    // plugin). Any failure here just leaves the plugin at its own default.
    if let Some(blob) = state.filter(|b| !b.is_empty()) {
        let restored = instance
            .access_shared_handler(|s| s.plugin_state())
            .ok_or_else(|| format!("'{name}' has no state extension"))
            .and_then(|state_ext| {
                state_ext
                    .load(&instance.plugin_handle(), &mut Cursor::new(blob))
                    .map_err(|e| format!("state.load: {e}"))
            });
        match restored {
            Ok(()) => dprintln!("clap host: restored {} bytes of plugin state", blob.len()),
            Err(reason) => eprintln!("clap host: preset for '{name}' not restored ({reason})"),
        }
    }

    // Read the plugin's audio-port layout before activation — a plugin may only
    // change its ports while deactivated, so this is the stable view the voice's
    // buffers must match.
    let io = query_audio_io(&mut instance);

    let configuration = PluginAudioConfiguration {
        sample_rate,
        min_frames_count: 1,
        max_frames_count: max_frames,
    };
    let processor = instance
        .activate(|_, _| (), configuration)
        .map_err(|e| format!("activate '{name}': {e}"))?;

    Ok(LoadedPlugin {
        instance,
        processor,
        io,
        name,
    })
}
