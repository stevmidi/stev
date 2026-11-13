//! The COM objects Stev hands *to* a hosted VST3 plugin.
//!
//! A plugin is not just called into — it calls back, and it expects three
//! host-provided objects to exist:
//!
//! - [`Vst3Host`] (`IHostApplication`) — passed to `IPluginBase::initialize`.
//!   It is how a plugin learns the host's name and asks the host to create
//!   `IMessage` / `IAttributeList` objects for component↔controller
//!   communication.
//! - [`ComponentHandler`] (`IComponentHandler`, `IComponentHandler2`) — given
//!   to the edit controller. A plugin whose UI moves a parameter, or whose
//!   state changes, reports it here.
//! - [`HostEventList`] (`IEventList`) — the per-block note input the plugin
//!   reads inside `process`.
//!
//! The per-block automation queue lives in [`params`](super::params), which is
//! also where a UI parameter change is carried across to the processor.
//!
//! All three are implemented with `vst3`'s `Class` / `ComWrapper` machinery.
//! See `docs/180-vst3-host.md`.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{Receiver, Sender, unbounded};
use log::debug;
use vst3::Steinberg::Vst::{Event, String128};
use vst3::Steinberg::Vst::{
    IAttributeList_iid, IComponentHandler, IComponentHandler2, IComponentHandler2Trait,
    IComponentHandlerTrait, IEditController, IEditControllerTrait, IEventList, IEventListTrait,
    IHostApplication, IHostApplicationTrait, IMessage_iid, ParamID, ParamValue, RestartFlags_,
};
use vst3::Steinberg::{
    FIDString, FUnknown, TBool, TUID, int32, kInvalidArgument, kNoInterface, kNotImplemented,
    kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use super::message::{HostAttributeList, HostMessage};
use super::params::ParamSender;
use crate::core::plugin_host::editor::ParameterGesture;
use crate::core::time::monotonic_nanos;

/// The host application object, handed to every plugin's `initialize`.
///
/// Its one real job is [`createInstance`](IHostApplicationTrait::createInstance):
/// a plugin cannot allocate the `IMessage` / `IAttributeList` objects its two
/// halves use to talk to each other, so it asks the host for them. Declining is
/// not a harmless simplification — a **dual-component** plugin (separate
/// `IComponent` and `IEditController`) then has no way to get anything from its
/// UI to its processor, and presents as a plugin whose editor works perfectly
/// while its sound never changes. See [`message`](super::message).
pub(super) struct Vst3Host;

impl Class for Vst3Host {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for Vst3Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `name` is a caller-owned `String128` (128 UTF-16 units); the
        // write below stays inside it and always terminates.
        unsafe { write_string128(name, "Stev") }
        kResultOk
    }

    /// Creates one of the two object types a plugin may ask the host for: an
    /// `IMessage` (which carries its own attribute list) or a bare
    /// `IAttributeList`. Anything else is declined.
    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the caller owns all three; `cid`/`iid` are read once and the
        // result is written into `obj` as an owned reference, which is the
        // ownership `createInstance` specifies.
        unsafe {
            let requested = *cid;
            let created: Option<ComPtr<FUnknown>> = if requested == IMessage_iid {
                ComWrapper::new(HostMessage::new()).to_com_ptr::<FUnknown>()
            } else if requested == IAttributeList_iid {
                ComWrapper::new(HostAttributeList::new()).to_com_ptr::<FUnknown>()
            } else {
                None
            };
            let Some(created) = created else {
                return kNotImplemented;
            };
            // Hand back the interface actually asked for, which need not be the
            // one the class is named by.
            match query_interface(created.as_ptr(), &*iid) {
                Some(ptr) => {
                    *obj = ptr;
                    kResultOk
                }
                None => kNoInterface,
            }
        }
    }
}

/// The edit controller's handler. A plugin reports UI-driven parameter changes
/// through this.
///
/// For a **single-component** plugin these are informational: the UI is
/// mutating the same object that renders audio, so the sound already followed.
/// For a **dual-component** plugin they are the only way a knob turn reaches
/// the processor at all, and the host must carry them across — which is what
/// [`ParamSender`] does. See [`params`](super::params).
///
/// Either way a copy goes to the knob panel ([`ParameterGesture`]): a turn's
/// start assigns an armed knob, and a turn of a knob's parameter is a hand
/// turn, which holds off playback and goes into a take
/// (`400-parameter-knobs.md` decisions 3, 6, 9 and 10).
pub(super) struct ComponentHandler {
    /// Carries parameter changes to the audio thread.
    pub(super) params: ParamSender,
    /// The knob panel's copy of every change the plugin reports: sent here,
    /// drained by the editor ([`take_gestures`](Self::take_gestures)).
    gestures: (Sender<ParameterGesture>, Receiver<ParameterGesture>),
    /// Set while the host sets a parameter on the edit controller itself
    /// (`Vst3Editor::set_parameter`): a plugin that reports
    /// `setParamNormalized` back through `performEdit` must not have Stev's
    /// own move read as the user's turn.
    pub(super) host_edit: AtomicBool,
    /// Set by `setDirty(true)`, taken by [`take_gestures`](Self::take_gestures):
    /// a plugin may call it on every edit, and once a frame is enough.
    dirty: AtomicBool,
    /// The plugin's edit controller while it lives
    /// ([`remember_controller`](Self::remember_controller)), not counted:
    /// the controller holds this handler, so a counted reference would be a
    /// cycle. Read to learn where a parameter stands as a turn begins.
    controller: AtomicPtr<IEditController>,
    /// The app's egui context, so a change in the plugin's editor wakes the
    /// reactive UI and the knob follows at once.
    repaint: Arc<OnceLock<egui::Context>>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl ComponentHandler {
    /// Wraps the sending half of the parameter bridge, with a fresh gesture
    /// feed for the knob panel.
    fn new(params: ParamSender, repaint: Arc<OnceLock<egui::Context>>) -> Self {
        Self {
            params,
            gestures: unbounded(),
            host_edit: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            controller: AtomicPtr::new(null_mut()),
            repaint,
        }
    }

    /// Keeps a reference to `controller`, uncounted, for
    /// [`beginEdit`](IComponentHandlerTrait::beginEdit) to read. Must be
    /// [forgotten](Self::forget_controller) before the controller is
    /// terminated.
    pub(super) fn remember_controller(&self, controller: &ComPtr<IEditController>) {
        self.controller
            .store(controller.as_ptr(), Ordering::Release);
    }

    /// The controller is about to go.
    pub(super) fn forget_controller(&self) {
        self.controller.store(null_mut(), Ordering::Release);
    }

    /// Where parameter `id` stands now, normalized, if the controller lives.
    fn parameter_value(&self, id: ParamID) -> Option<f64> {
        let controller = self.controller.load(Ordering::Acquire);
        // SAFETY: stored from a live `ComPtr` and cleared before the
        // controller is terminated, and this runs inside a call the
        // controller itself made, so it is alive for the read.
        let controller = unsafe { ComRef::from_raw(controller) }?;
        // SAFETY: as above; a plain read of the controller's parameter model.
        Some(unsafe { controller.getParamNormalized(id) }.clamp(0.0, 1.0))
    }

    /// Hands `gesture` to the knob panel, unless the host itself is setting
    /// a parameter, and wakes the UI.
    fn report(&self, gesture: ParameterGesture) {
        if self.host_edit.load(Ordering::Relaxed) {
            return;
        }
        self.gestures.0.send(gesture).ok();
        self.wake();
    }

    /// Everything the plugin reported since the last call, oldest first,
    /// then one [`ParameterGesture::ValuesChanged`] if it said it was dirty.
    pub(super) fn take_gestures(&self) -> Vec<ParameterGesture> {
        let mut gestures: Vec<_> = self.gestures.1.try_iter().collect();
        if self.dirty.swap(false, Ordering::Relaxed) {
            gestures.push(ParameterGesture::ValuesChanged);
        }
        gestures
    }

    /// Wakes the reactive UI, so the knob panel follows the plugin at once.
    fn wake(&self) {
        if let Some(ctx) = self.repaint.get() {
            ctx.request_repaint();
        }
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    /// A UI gesture started: the knob panel's cue to assign an armed knob,
    /// or to hold a knob's parameter. Reads where the parameter stands now:
    /// its first `performEdit` moves it before the panel hears of this.
    /// Deliberately *not* an error return either way: a plugin that gets
    /// `kNotImplemented` here may refuse to let its UI move at all.
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        let start = self.parameter_value(id);
        self.report(ParameterGesture::Begin(id, start, monotonic_nanos()));
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.params.send(id, value);
        self.report(ParameterGesture::Move(id, value, monotonic_nanos()));
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.report(ParameterGesture::End(id));
        kResultOk
    }

    /// The plugin is asking to be reconfigured — its parameter list changed,
    /// its values did (a preset), or it wants its state reloaded. Accepted
    /// and otherwise ignored: acting on it would mean tearing the voice down
    /// and rebuilding it mid-playback. The UI is woken, so a knob whose
    /// parameter a preset moved follows at once, and new values are reported:
    /// a preset is a change to the project.
    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        if flags & RestartFlags_::kParamValuesChanged != 0 {
            self.report(ParameterGesture::ValuesChanged);
        } else {
            self.wake();
        }
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    /// The plugin says its state changed (`true`) or was saved (`false`).
    /// A change is reported, once until it is taken, as
    /// [`ParameterGesture::ValuesChanged`], which the unsaved prompt counts
    /// (`060-persistence.md` § Unsaved changes); a save is Stev's own and
    /// needs nothing. Not while the host itself sets a parameter, as for
    /// every report.
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0
            && !self.host_edit.load(Ordering::Relaxed)
            && !self.dirty.swap(true, Ordering::Relaxed)
        {
            self.wake();
        }
        kResultOk
    }

    /// Stev opens a plugin's editor only on `v`, never at the plugin's asking.
    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kNotImplemented
    }

    /// Group edits are accepted and ignored: each change in the group is
    /// still reported through `beginEdit` / `performEdit` / `endEdit`.
    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// The per-block note input a plugin reads during `process`.
///
/// The events live behind an [`UnsafeCell`] rather than a `RefCell` or a lock
/// because this object is reachable from the audio thread and must be `Sync` to
/// live inside a `Send` voice — and because a `RefCell` borrow flag would be
/// checked on the audio thread for an invariant that is already guaranteed
/// structurally. See the `Sync` impl for that guarantee.
pub(super) struct HostEventList {
    /// This block's events, in the order they were queued.
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: the invariant is ownership, not synchronisation. One `HostEventList`
// belongs to exactly one `Vst3Voice` and is never cloned or shared between
// voices. Every access happens on the audio callback thread, inside one
// `render_into`: the mixer's dispatch loop calls `push` (via `queue_midi`),
// then `render_block` hands the list to `process`, which reads it synchronously
// and returns before `clear` runs. The plugin never retains it past that call —
// the VST3 spec scopes `ProcessData` to the `process` invocation — and no two
// of those steps overlap, because a voice is rendered by exactly one thread per
// block (`WorkerPool::for_each` gives each voice to one runner).
unsafe impl Sync for HostEventList {}

impl Class for HostEventList {
    type Interfaces = (IEventList,);
}

impl HostEventList {
    /// An empty list with room for a full note-reset burst plus whatever clip
    /// and live events land in the same block, so pushing never allocates on
    /// the audio thread.
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(capacity)),
        }
    }

    /// Appends one event for the next `process` call.
    pub(super) fn push(&self, event: Event) {
        // SAFETY: see the `Sync` impl — sole ownership, one thread, no
        // overlapping access.
        let events = unsafe { &mut *self.events.get() };
        if events.len() == events.capacity() {
            debug!("vst3: event buffer full ({}); dropping", events.capacity());
            return;
        }
        events.push(event);
    }

    /// Empties the list once the block that queued it has been processed.
    pub(super) fn clear(&self) {
        // SAFETY: see the `Sync` impl.
        unsafe { &mut *self.events.get() }.clear();
    }

    /// Whether anything is queued — the wake-up test for a sleeping voice.
    pub(super) fn is_empty(&self) -> bool {
        // SAFETY: see the `Sync` impl.
        unsafe { &*self.events.get() }.is_empty()
    }
}

impl IEventListTrait for HostEventList {
    unsafe fn getEventCount(&self) -> int32 {
        // SAFETY: see the `Sync` impl. Called by the plugin inside `process`.
        unsafe { &*self.events.get() }.len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        if e.is_null() || index < 0 {
            return kInvalidArgument;
        }
        // SAFETY: see the `Sync` impl for the read; `e` is a caller-owned
        // `Event` the plugin expects us to fill, and the index is bounds-checked
        // against the same slice we just read.
        unsafe {
            let events = &*self.events.get();
            let Some(event) = events.get(index as usize) else {
                return kInvalidArgument;
            };
            *e = *event;
        }
        kResultOk
    }

    /// Plugins may *emit* events (a MIDI-effect output, note expression). This
    /// list is Stev's input only — nothing routes a plugin's output events
    /// anywhere — so additions are declined rather than silently swallowed.
    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        kNotImplemented
    }
}

/// Writes `text` into a VST3 `String128` (128 UTF-16 units, `NUL`-terminated),
/// truncating if it does not fit.
///
/// # Safety
///
/// `dst` must point to a valid, writable `String128`.
unsafe fn write_string128(dst: *mut String128, text: &str) {
    // SAFETY: the caller guarantees `dst` is a valid `String128`.
    write_utf16_terminated(unsafe { &mut *dst }, text.encode_utf16());
}

/// Fills `dst` with `units`, zero-padding the rest and truncating whatever
/// does not fit, so the last slot is always a terminating `NUL`. An empty
/// `dst` is left alone.
pub(super) fn write_utf16_terminated(dst: &mut [u16], mut units: impl Iterator<Item = u16>) {
    let Some((last, body)) = dst.split_last_mut() else {
        return;
    };
    for slot in body {
        *slot = units.next().unwrap_or(0);
    }
    *last = 0;
}

/// Builds the shared host objects one plugin instance needs.
pub(super) struct HostObjects {
    /// `IHostApplication`, passed to `initialize`.
    pub(super) host: ComWrapper<Vst3Host>,
    /// `IComponentHandler`, given to the edit controller.
    pub(super) handler: ComWrapper<ComponentHandler>,
}

impl HostObjects {
    /// Creates a fresh set. One per plugin instance — they are cheap, and
    /// sharing them across instances would only add lifetime questions.
    /// `params` is the sending half of that instance's parameter bridge and
    /// `repaint` the app's UI wake.
    pub(super) fn new(params: ParamSender, repaint: Arc<OnceLock<egui::Context>>) -> Self {
        Self {
            host: ComWrapper::new(Vst3Host),
            handler: ComWrapper::new(ComponentHandler::new(params, repaint)),
        }
    }
}

/// Queries `iid` off a live COM object, returning an owned reference.
///
/// # Safety
///
/// `obj` must be a live object implementing `FUnknown`.
unsafe fn query_interface(obj: *mut FUnknown, iid: &TUID) -> Option<*mut c_void> {
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: the caller guarantees a live object; `iid` is borrowed for the
    // call and `out` is a local the object writes an owned reference into.
    let result = unsafe { ((*(*obj).vtbl).queryInterface)(obj, iid, &mut out) };
    (result == kResultOk && !out.is_null()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::super::params::bridge;
    use super::*;
    use vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOnEvent;
    use vst3::Steinberg::Vst::{Event__type0, NoteOnEvent};

    fn note_on(pitch: i16) -> Event {
        Event {
            busIndex: 0,
            sampleOffset: 0,
            ppqPosition: 0.0,
            flags: 0,
            r#type: kNoteOnEvent as u16,
            __field0: Event__type0 {
                noteOn: NoteOnEvent {
                    channel: 0,
                    pitch,
                    tuning: 0.0,
                    velocity: 1.0,
                    length: 0,
                    noteId: -1,
                },
            },
        }
    }

    #[test]
    fn set_dirty_reports_one_change_a_take_and_ignores_a_save() {
        let (params, _receiver) = bridge();
        let handler = ComponentHandler::new(params, Arc::new(OnceLock::new()));
        // SAFETY: plain calls on a live handler, as a plugin makes them.
        unsafe {
            handler.setDirty(0);
            handler.setDirty(1);
            handler.setDirty(1);
        }
        assert_eq!(
            handler.take_gestures(),
            vec![ParameterGesture::ValuesChanged]
        );
        assert!(handler.take_gestures().is_empty());
    }

    #[test]
    fn the_event_list_reads_back_what_was_pushed_in_order() {
        let list = HostEventList::new(8);
        assert!(list.is_empty());
        list.push(note_on(60));
        list.push(note_on(64));
        // SAFETY: single-threaded test, same access pattern as the audio thread.
        unsafe {
            assert_eq!(list.getEventCount(), 2);
            let mut out: Event = std::mem::zeroed();
            assert_eq!(list.getEvent(0, &mut out), kResultOk);
            assert_eq!(out.__field0.noteOn.pitch, 60);
            assert_eq!(list.getEvent(1, &mut out), kResultOk);
            assert_eq!(out.__field0.noteOn.pitch, 64);
        }
        assert!(!list.is_empty());
        list.clear();
        assert!(list.is_empty());
        // SAFETY: as above.
        unsafe { assert_eq!(list.getEventCount(), 0) };
    }

    #[test]
    fn out_of_range_and_null_reads_are_rejected_not_undefined() {
        let list = HostEventList::new(4);
        list.push(note_on(60));
        // SAFETY: single-threaded test.
        unsafe {
            let mut out: Event = std::mem::zeroed();
            assert_eq!(list.getEvent(1, &mut out), kInvalidArgument);
            assert_eq!(list.getEvent(-1, &mut out), kInvalidArgument);
            assert_eq!(list.getEvent(0, std::ptr::null_mut()), kInvalidArgument);
        }
    }

    #[test]
    fn a_full_event_list_drops_rather_than_reallocating() {
        // Reallocating would allocate on the audio thread; dropping the event
        // is the lesser evil and is what the capacity is sized to avoid.
        let list = HostEventList::new(2);
        list.push(note_on(1));
        list.push(note_on(2));
        list.push(note_on(3));
        // SAFETY: single-threaded test.
        unsafe { assert_eq!(list.getEventCount(), 2) };
    }

    #[test]
    fn the_host_name_is_written_and_terminated() {
        let mut name: String128 = [0xFFFF; 128];
        // SAFETY: `name` is a valid String128.
        unsafe { write_string128(&mut name, "Stev") };
        let text: String = String::from_utf16_lossy(
            &name
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect::<Vec<_>>(),
        );
        assert_eq!(text, "Stev");
        assert_eq!(name[127], 0);
    }

    #[test]
    fn an_overlong_host_name_is_truncated_and_still_terminated() {
        let mut name: String128 = [0xFFFF; 128];
        let long = "x".repeat(500);
        // SAFETY: `name` is a valid String128.
        unsafe { write_string128(&mut name, &long) };
        assert_eq!(name[127], 0);
        assert!(name[..127].iter().all(|&c| c == b'x' as u16));
    }

    #[test]
    fn a_short_utf16_write_is_zero_padded() {
        let mut dst = [0xFFFF_u16; 5];
        write_utf16_terminated(&mut dst, "ab".encode_utf16());
        assert_eq!(dst, [b'a' as u16, b'b' as u16, 0, 0, 0]);
        // Nothing to write into, nothing written — and no panic.
        write_utf16_terminated(&mut [], "ab".encode_utf16());
    }
}
