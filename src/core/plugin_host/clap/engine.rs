//! Plugin instantiation and activation for the CLAP host. Discovery of the
//! available bundles lives in [`super::discovery`].

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::note_ports::{NoteDialect, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags};
use clack_host::prelude::*;
use log::{debug, info, warn};

use crate::core::plugin_host::buffers::AudioIoLayout;
use crate::core::plugin_host::catalog::available_bundles_hint;
use crate::core::plugin_host::ui_wake::UiWake;

use super::discovery::{cached_entry, descriptor_name};
use super::host::{ClapHostMainThread, ClapHostShared, StevClapHost};
use super::params::{ClapParam, ParamValues};

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

/// Whether the plugin takes native note events on its first note input, the
/// port Stev plays into. Logs the dialects every input declares (both calls are
/// `[main-thread]`). A plugin without the `note-ports` extension gets raw MIDI.
fn accepts_note_events(instance: &mut PluginInstance<StevClapHost>, name: &str) -> bool {
    let plugin = instance.plugin_handle();
    let Some(ports) = plugin.get_extension::<PluginNotePorts>() else {
        info!("plugin host: CLAP '{name}' has no note-ports extension");
        return false;
    };
    let count = ports.count(&plugin, true);
    info!("plugin host: CLAP '{name}' has {count} note inputs");
    let mut buffer = NotePortInfoBuffer::new();
    for i in 0..count {
        if let Some(port) = ports.get(&plugin, i, true, &mut buffer) {
            info!(
                "plugin host: CLAP '{name}' note input {i} '{}': {:?}, prefers {:?}",
                String::from_utf8_lossy(port.name),
                port.supported_dialects,
                port.preferred_dialect
            );
        }
    }
    ports
        .get(&plugin, 0, true, &mut buffer)
        .is_some_and(|port| port.supported_dialects.supports(NoteDialect::Clap))
}

/// Reads every parameter and where it stands (both calls are `[main-thread]`).
/// A plugin without the `params` extension has none, and the knobs can't drive
/// it.
fn read_parameters(instance: &mut PluginInstance<StevClapHost>) -> ParamValues {
    let Some(params) = instance.access_shared_handler(|s| s.plugin_params()) else {
        return ParamValues::default();
    };
    let plugin = instance.plugin_handle();
    let mut buffer = ParamInfoBuffer::new();
    let mut read = Vec::new();
    for index in 0..params.count(&plugin) {
        if let Some(info) = params.get_info(&plugin, index, &mut buffer) {
            let plain = params
                .get_value(&plugin, info.id)
                .unwrap_or(info.default_value);
            let param = ClapParam::new(
                String::from_utf8_lossy(info.name).into_owned(),
                info.min_value,
                info.max_value,
                info.default_value,
                info.flags.contains(ParamInfoFlags::IS_STEPPED),
            );
            read.push((info.id.get(), param, plain));
        }
    }
    ParamValues::new(read)
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
    /// The plugin's parameters and where they stand, after any restored state.
    pub(super) params: ParamValues,
    /// Whether notes go to the plugin as native events, with ids, rather than
    /// raw MIDI.
    pub(super) note_events: bool,
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
    wake: Arc<UiWake>,
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
        |_| ClapHostShared::new(Arc::clone(&wake)),
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
            Ok(()) => debug!("clap host: restored {} bytes of plugin state", blob.len()),
            Err(reason) => warn!("clap host: preset for '{name}' not restored ({reason})"),
        }
    }

    // Read the plugin's audio-port layout before activation — a plugin may only
    // change its ports while deactivated, so this is the stable view the voice's
    // buffers must match.
    let io = query_audio_io(&mut instance);
    let params = read_parameters(&mut instance);
    let note_events = accepts_note_events(&mut instance, &name);

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
        params,
        note_events,
        name,
    })
}
