//! Instantiating and activating one VST3 plugin: the COM dance from a class UID
//! to a processor ready to be handed a block.
//!
//! VST3 splits a plugin in two — an `IComponent`/`IAudioProcessor` that makes
//! sound and an `IEditController` that owns the UI and the parameter model —
//! and expects the host to create both, initialise both, and wire them to each
//! other. CLAP has no equivalent: `clack-host` hands back one instance already
//! split into a `!Send` handle and a `Send` processor. Here that split is ours
//! to make and ours to keep, which is the main reason this module exists.
//!
//! Runs entirely on the **eframe main thread**, which is not incidental: some
//! plugins' module initialisation demands it (see `180-vst3-host.md`), and the
//! `!Send` half must be created where it will live.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::Path;

use vst3::Steinberg::Vst::{
    BusDirections_::{kInput, kOutput},
    MediaTypes_::{kAudio, kEvent},
    ProcessModes_::kRealtime,
    SymbolicSampleSizes_::kSample32,
};
use vst3::Steinberg::Vst::{
    IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait, IHostApplication, IMidiMapping,
    ProcessSetup, SpeakerArrangement,
};
use vst3::Steinberg::{
    IPluginBaseTrait, IPluginFactory, IPluginFactoryTrait, TUID, int32, kResultOk,
};
use vst3::{ComPtr, Interface};

use crate::core::plugin_host::buffers::AudioIoLayout;

use super::discovery::uid_from_hex;
use super::events::MidiMap;
use super::host::HostObjects;
use super::module::cached_module;
use super::params::{ParamReceiver, bridge};
use super::state;
use super::voice::check_bus_capacity;

/// One instantiated, activated plugin, split into the halves that live on
/// different threads.
pub(super) struct LoadedPlugin {
    /// The `!Send` half: the component and its edit controller, plus the host
    /// objects they hold pointers to. Stays on the main thread.
    pub(super) main: MainThreadHalf,
    /// The `Send` half: the audio processor the voice calls `process` on.
    ///
    /// Note that `ComPtr` is unconditionally `Send` in these bindings, so the
    /// compiler will *not* stop this being used from the wrong thread — the
    /// split is upheld by construction (only this field leaves the main thread)
    /// rather than by the type system, which is why the two halves are separate
    /// structs and [`MainThreadHalf`] is explicitly `!Send`.
    pub(super) processor: ComPtr<IAudioProcessor>,
    /// The plugin's declared audio bus layout.
    pub(super) io: AudioIoLayout,
    /// The audio-thread end of the UI→processor parameter bridge. Only a
    /// dual-component plugin ever has anything to carry — see
    /// [`params`](super::params).
    pub(super) params: ParamReceiver,
    /// Which parameter each MIDI controller drives, queried once here because
    /// it needs the controller, which does not leave the main thread.
    pub(super) midi_map: MidiMap,
    /// How many samples of output the plugin may still produce after its last
    /// input — `getTailSamples`. `u32::MAX` (`kInfiniteTail`) means "assume it
    /// never stops". The voice needs it to know when idling is safe; see
    /// [`voice`](super::voice).
    pub(super) tail_samples: u32,
}

/// Everything about a loaded plugin that may only be touched on the eframe main
/// thread.
pub(super) struct MainThreadHalf {
    /// The plugin component. Deactivated and terminated on teardown.
    pub(super) component: ComPtr<IComponent>,
    /// The edit controller, when the plugin has one that could be obtained.
    /// `None` costs only the editor UI; the plugin still makes sound.
    pub(super) controller: Option<ComPtr<IEditController>>,
    /// Whether [`controller`](Self::controller) is the **same object** as
    /// [`component`](Self::component) — i.e. the plugin is single-component.
    ///
    /// This is not a curiosity: it changes what state handling is correct. On
    /// the same object, `IEditController::setState` *is* `IComponent::setState`,
    /// so treating the two halves independently applies the plugin's state
    /// twice. See [`super::state`].
    pub(super) controller_is_component: bool,
    /// The host objects handed to the plugin. Kept alive here because the
    /// plugin holds raw pointers to them for as long as it is initialised.
    pub(super) _host: HostObjects,
    /// Makes this half `!Send`, so it cannot accidentally be moved to the audio
    /// thread with the processor. VST3 gives no such guarantee itself.
    _not_send: PhantomData<*const ()>,
}

/// Instantiates `plugin_id` from `bundle_path`, wires it up and activates it.
///
/// Runs on the eframe main thread. Any failure is returned as `Err` and leaves
/// nothing loaded.
pub(super) fn load(
    bundle_path: &Path,
    plugin_id: &str,
    sample_rate: f64,
    max_frames: u32,
    saved_state: Option<&[u8]>,
) -> Result<LoadedPlugin, String> {
    let cid = uid_from_hex(plugin_id)
        .ok_or_else(|| format!("vst3: '{plugin_id}' is not a valid class id"))?;
    // The module must outlive every instance made from it, so this is the
    // *cached* entry point — the opposite policy from the catalog scan, which
    // unloads as soon as it has read the metadata. See `module.rs`.
    let module = cached_module(bundle_path)?;
    let (param_tx, param_rx) = bridge();
    let hosts = HostObjects::new(param_tx);

    // SAFETY: every call below is on a live COM object obtained from this
    // module's factory, in the order the VST3 lifecycle requires: create,
    // initialize, connect, configure buses, setupProcessing, setActive. Every
    // out-parameter is a local of the matching type, and every `tresult` is
    // checked before the value it wrote is used.
    unsafe {
        let component: ComPtr<IComponent> = create_instance(module.factory(), &cid)
            .ok_or_else(|| format!("vst3: could not create '{plugin_id}'"))?;

        let host_ptr = hosts
            .host
            .to_com_ptr::<IHostApplication>()
            .ok_or("vst3: host object does not expose IHostApplication")?;
        if component.initialize(host_ptr.as_ptr().cast()) != kResultOk {
            return Err(format!("vst3: '{plugin_id}' failed to initialize"));
        }

        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| format!("vst3: '{plugin_id}' exposes no IAudioProcessor"))?;

        let (controller, controller_is_component) =
            obtain_controller(&component, module.factory(), &host_ptr);
        if let Some(controller) = &controller {
            connect(&component, controller);
            if let Some(handler) = hosts.handler.to_com_ptr() {
                controller.setComponentHandler(handler.as_ptr());
            }
        }

        // Before activation, and before the buses are configured, so a preset
        // that changes the plugin's own bus layout is honoured by the query
        // below rather than overwritten by it.
        if let Some(bytes) = saved_state.filter(|b| !b.is_empty()) {
            state::apply(
                &component,
                controller.as_ref(),
                controller_is_component,
                bytes,
            );
        }

        // Needs the controller, so it happens here rather than in the voice.
        let midi_map = controller
            .as_ref()
            .and_then(|c| c.cast::<IMidiMapping>())
            // SAFETY (covered by this function's `unsafe` block): a live
            // mapping obtained from the initialised controller.
            .map(|mapping| MidiMap::query(&mapping))
            .unwrap_or_else(MidiMap::empty);

        let io = configure_buses(&component, &processor);
        check_bus_capacity(&io).map_err(|e| format!("vst3: '{plugin_id}': {e}"))?;
        // Asked once: it is a property of this `setupProcessing`, not of any
        // particular block.
        let tail_samples = processor.getTailSamples();

        let mut setup = ProcessSetup {
            processMode: kRealtime as int32,
            symbolicSampleSize: kSample32 as int32,
            maxSamplesPerBlock: max_frames as int32,
            sampleRate: sample_rate,
        };
        if processor.setupProcessing(&mut setup) != kResultOk {
            return Err(format!(
                "vst3: '{plugin_id}' rejected {sample_rate} Hz / {max_frames} frames"
            ));
        }

        if component.setActive(1) != kResultOk {
            return Err(format!("vst3: '{plugin_id}' refused to activate"));
        }
        // Left on for the plugin's lifetime rather than toggled with the
        // transport: Stev plays live notes while stopped, so a voice is
        // never truly idle in the sense `setProcessing(false)` describes.
        processor.setProcessing(1);

        Ok(LoadedPlugin {
            main: MainThreadHalf {
                component,
                controller,
                controller_is_component,
                _host: hosts,
                _not_send: PhantomData,
            },
            processor,
            io,
            params: param_rx,
            midi_map,
            tail_samples,
        })
    }
}

/// Creates one instance of class `cid` from `factory`, cast to `I`.
///
/// # Safety
///
/// `factory` must be a live plugin factory.
unsafe fn create_instance<I: Interface>(
    factory: &ComPtr<IPluginFactory>,
    cid: &TUID,
) -> Option<ComPtr<I>> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    // SAFETY: the caller guarantees a live factory; `cid` and the interface id
    // are both borrowed for the duration of the call, and `obj` is a local the
    // factory writes an owned reference into.
    let result = unsafe {
        factory.createInstance(
            cid.as_ptr().cast(),
            I::IID.as_ptr().cast(),
            &mut obj as *mut *mut c_void,
        )
    };
    if result != kResultOk || obj.is_null() {
        return None;
    }
    // SAFETY: `createInstance` returns an already-`addRef`'d object, which is
    // exactly what `from_raw` adopts.
    unsafe { ComPtr::from_raw(obj.cast::<I>()) }
}

/// Gets the plugin's edit controller, and reports whether it turned out to be
/// the *same object* as the component.
///
/// The caller needs that second fact, not just the controller: single- and
/// dual-component plugins need different state handling, and this is the only
/// place the difference is observable.
///
/// # Safety
///
/// `component` must be an initialised component and `factory` its factory.
unsafe fn obtain_controller(
    component: &ComPtr<IComponent>,
    factory: &ComPtr<IPluginFactory>,
    host: &ComPtr<IHostApplication>,
) -> (Option<ComPtr<IEditController>>, bool) {
    // Single-component: one object implements both interfaces.
    if let Some(controller) = component.cast::<IEditController>() {
        return (Some(controller), true);
    }
    let mut cid: TUID = [0; 16];
    // SAFETY: the caller guarantees a live component; `cid` is a local of the
    // matching type for the out-parameter.
    if unsafe { component.getControllerClassId(&mut cid) } != kResultOk {
        return (None, false);
    }
    // SAFETY: the caller guarantees a live factory.
    let Some(controller) = (unsafe { create_instance::<IEditController>(factory, &cid) }) else {
        return (None, false);
    };
    // SAFETY: freshly created, not yet initialised — this is the required call.
    if unsafe { controller.initialize(host.as_ptr().cast()) } != kResultOk {
        return (None, false);
    }
    (Some(controller), false)
}

/// Wires the component and controller to each other, so the pair can exchange
/// the messages a split plugin relies on (a preset change reaching the UI, for
/// instance). Both directions, as the spec requires. A plugin that exposes no
/// connection points simply isn't wired.
///
/// # Safety
///
/// Both must be live, initialised objects.
unsafe fn connect(component: &ComPtr<IComponent>, controller: &ComPtr<IEditController>) {
    let (Some(from), Some(to)) = (
        component.cast::<IConnectionPoint>(),
        controller.cast::<IConnectionPoint>(),
    ) else {
        return;
    };
    // SAFETY: the caller guarantees both are live; each is handed the other's
    // pointer, which outlives the connection because both are held in the same
    // `MainThreadHalf`.
    unsafe {
        from.connect(to.as_ptr());
        to.connect(from.as_ptr());
    }
}

/// Negotiates bus arrangements, activates every bus, and reports the resulting
/// channel layout.
///
/// Three things happen here that all matter:
///
/// - **Every audio bus is activated.** A bus the host never activates may be
///   left unallocated by the plugin, and `process` then writes through a null
///   channel pointer. This is the same crash the CLAP host hit with aux output
///   ports, arrived at from the other direction.
/// - **Every event input bus is activated**, or an instrument receives no notes
///   at all and is simply silent — with no error anywhere to explain it.
/// - **The layout is read back after negotiation, not assumed.**
///   `setBusArrangements` is a request; a plugin may decline it and keep its own
///   layout, so the buffers are sized from what `getBusInfo` reports afterwards.
///
/// # Safety
///
/// Both must belong to the same initialised, not-yet-activated plugin.
unsafe fn configure_buses(
    component: &ComPtr<IComponent>,
    processor: &ComPtr<IAudioProcessor>,
) -> AudioIoLayout {
    // SAFETY: the caller guarantees live objects; every index below is drawn
    // from the plugin's own reported bus count, and every out-parameter is a
    // local of the matching type.
    unsafe {
        let mut inputs = bus_arrangements(component, kInput as i32);
        let mut outputs = bus_arrangements(component, kOutput as i32);
        processor.setBusArrangements(
            inputs.as_mut_ptr(),
            inputs.len() as int32,
            outputs.as_mut_ptr(),
            outputs.len() as int32,
        );

        for (media, dir) in [
            (kAudio as i32, kInput as i32),
            (kAudio as i32, kOutput as i32),
            (kEvent as i32, kInput as i32),
            (kEvent as i32, kOutput as i32),
        ] {
            for index in 0..component.getBusCount(media, dir) {
                component.activateBus(media, dir, index, 1);
            }
        }

        AudioIoLayout::new(
            channel_counts(component, kInput as i32),
            channel_counts(component, kOutput as i32),
        )
    }
}

/// The speaker arrangement to request for each audio bus in `dir` — whatever
/// channel count the plugin already declares, expressed as a mask.
///
/// # Safety
///
/// `component` must be live.
unsafe fn bus_arrangements(component: &ComPtr<IComponent>, dir: int32) -> Vec<SpeakerArrangement> {
    // SAFETY: the caller guarantees a live component.
    unsafe { channel_counts(component, dir) }
        .into_iter()
        .map(speaker_arrangement)
        .collect()
}

/// Channel count of every audio bus in `dir`, in bus order.
///
/// # Safety
///
/// `component` must be live.
unsafe fn channel_counts(component: &ComPtr<IComponent>, dir: int32) -> Vec<u16> {
    // SAFETY: the caller guarantees a live component; indices come from its own
    // `getBusCount`, and `info` is a local of the matching type.
    unsafe {
        (0..component.getBusCount(kAudio as i32, dir))
            .map(|index| {
                let mut info = std::mem::zeroed();
                if component.getBusInfo(kAudio as i32, dir, index, &mut info) == kResultOk {
                    info.channelCount.clamp(0, i32::from(u16::MAX)) as u16
                } else {
                    0
                }
            })
            .collect()
    }
}

/// The low `channels` bits set — VST3's speaker arrangement is a channel
/// bitmask, and mono/stereo are its first one and two bits (`kSpeakerL`,
/// `kSpeakerL | kSpeakerR`). Good enough for the instrument layouts Stev
/// hosts; a surround bus would want a real arrangement constant.
fn speaker_arrangement(channels: u16) -> SpeakerArrangement {
    if channels >= 64 {
        return SpeakerArrangement::MAX;
    }
    (1u64 << channels) - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_arrangements_are_channel_masks() {
        assert_eq!(speaker_arrangement(0), 0);
        // kSpeakerL
        assert_eq!(speaker_arrangement(1), 1);
        // kSpeakerL | kSpeakerR
        assert_eq!(speaker_arrangement(2), 3);
        assert_eq!(speaker_arrangement(6), 0b11_1111);
    }

    #[test]
    fn an_absurd_channel_count_cannot_overflow_the_shift() {
        // `1 << 64` is undefined behaviour in C and a panic in debug Rust; a
        // plugin reporting nonsense must not take the app with it.
        assert_eq!(speaker_arrangement(64), SpeakerArrangement::MAX);
        assert_eq!(speaker_arrangement(u16::MAX), SpeakerArrangement::MAX);
    }
}
