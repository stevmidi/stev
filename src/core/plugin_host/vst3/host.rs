//! The COM objects Stev hands *to* a hosted VST3 plugin.
//!
//! A plugin is not just called into — it calls back, and it expects three
//! host-provided objects to exist:
//!
//! - [`Vst3Host`] (`IHostApplication`) — passed to `IPluginBase::initialize`.
//!   It is how a plugin learns the host's name and asks the host to create
//!   `IMessage` / `IAttributeList` objects for component↔controller
//!   communication.
//! - [`ComponentHandler`] (`IComponentHandler`) — given to the edit controller.
//!   A plugin whose UI moves a parameter reports it here.
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

use vst3::Steinberg::Vst::{Event, String128};
use vst3::Steinberg::Vst::{
    IAttributeList_iid, IComponentHandler, IComponentHandlerTrait, IEventList, IEventListTrait,
    IHostApplication, IHostApplicationTrait, IMessage_iid, ParamID, ParamValue,
};
use vst3::Steinberg::{
    FUnknown, TUID, int32, kInvalidArgument, kNoInterface, kNotImplemented, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComWrapper};

use super::message::{HostAttributeList, HostMessage};
use super::params::ParamSender;

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
pub(super) struct ComponentHandler {
    /// Carries parameter changes to the audio thread.
    params: ParamSender,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl ComponentHandler {
    /// Wraps the sending half of the parameter bridge.
    fn new(params: ParamSender) -> Self {
        Self { params }
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    /// A UI gesture started. Nothing to do — Stev has no automation to
    /// latch — but it is deliberately *not* an error return: a plugin that gets
    /// `kNotImplemented` here may refuse to let its UI move at all.
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.params.send(id, value);
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    /// The plugin is asking to be reconfigured — its parameter list changed, or
    /// it wants its state reloaded. Accepted and ignored: acting on it would
    /// mean tearing the voice down and rebuilding it mid-playback, and nothing
    /// Stev shows depends on the parameter list.
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
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
            dprintln!("vst3: event buffer full ({}); dropping", events.capacity());
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
    /// `params` is the sending half of that instance's parameter bridge.
    pub(super) fn new(params: ParamSender) -> Self {
        Self {
            host: ComWrapper::new(Vst3Host),
            handler: ComWrapper::new(ComponentHandler::new(params)),
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
