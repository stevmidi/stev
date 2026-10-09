//! Carrying a UI parameter change from a plugin's edit controller to its audio
//! processor.
//!
//! **Only a dual-component plugin needs this, and for one it is essential.**
//! When `IComponent` and `IEditController` are the same object, moving a knob
//! in the UI mutates the very state `process` reads, and the host is not
//! involved. When they are separate objects they share nothing, and the host is
//! the only path between them:
//!
//! ```text
//! plugin UI → IComponentHandler::performEdit  (main thread)
//!           → ParamSender  ──rtrb ring──►  ParamReceiver  (audio thread)
//!           → IParameterChanges in the next ProcessData
//!           → the processor finally hears about it
//! ```
//!
//! The same [`HostParameterChanges`] carries **MIDI CCs**, which VST3 has no
//! other way to express — see [`events`](super::events). Those are pushed
//! straight in on the audio thread rather than crossing the ring.
//!
//! Without it such a plugin looks entirely healthy — editor opens, preset
//! browser works, every control in the UI moves — and its sound never changes.
//!
//! The ring is `rtrb` for the same reason every other audio-thread feed in the
//! host is: a `crossbeam_channel` may allocate or free inside `pop`, which is
//! not safe on the callback thread.
//!
//! See `docs/180-vst3-host.md`.

use std::cell::{Cell, UnsafeCell};
use std::sync::Mutex;

use rtrb::{Consumer, Producer, RingBuffer};
use vst3::Steinberg::Vst::{
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, ParamID,
    ParamValue,
};
use vst3::Steinberg::{int32, kInvalidArgument, kNotImplemented, kResultOk, tresult};
use vst3::{Class, ComWrapper};

/// Capacity of the UI→audio parameter ring. A preset change in a large synth
/// can push a parameter per control at once, so this is sized for a whole
/// panel's worth rather than for incremental knob turns.
const PARAM_RING_CAPACITY: usize = 4096;

/// Distinct parameters that can change in a single block. A block is ~5 ms; a
/// human moving controls cannot exceed this. A preset loaded from the plugin's
/// own UI can — thousands of `performEdit`s at once — so
/// [`drain_ui`](ParamReceiver::drain_ui) stops when a block is full and leaves
/// the rest in the ring for the following blocks. That also caps what a burst
/// of distinct parameters costs one block in [`HostParameterChanges::push`]'s
/// dedupe scan.
const MAX_PARAMS_PER_BLOCK: usize = 512;

/// The sending half, held by `ComponentHandler` on the main thread.
///
/// The `Mutex` guards the producer, not the audio thread: `performEdit` is
/// specified as a UI-thread call, but plugins have been known to report from
/// their own worker threads, and a `RefCell` would panic where this merely
/// serialises. The audio thread never touches it — it owns the consumer half.
pub(super) struct ParamSender {
    /// Producer end of the ring.
    tx: Mutex<Producer<(ParamID, ParamValue)>>,
}

impl ParamSender {
    /// Queues one parameter change for the next processed block. Dropped if the
    /// ring is full, which costs that one change rather than blocking the UI.
    pub(super) fn send(&self, id: ParamID, value: ParamValue) {
        let Ok(mut tx) = self.tx.lock() else {
            return;
        };
        if tx.push((id, value)).is_err() {
            dprintln!("vst3: parameter ring full; dropped a change to {id}");
        }
    }
}

/// The receiving half, owned by the voice on the audio thread.
pub(super) struct ParamReceiver {
    /// Consumer end of the ring.
    rx: Consumer<(ParamID, ParamValue)>,
    /// The `IParameterChanges` handed to the plugin each block.
    changes: ComWrapper<HostParameterChanges>,
}

impl ParamReceiver {
    /// Empties this block's automation queue, ready for the next one.
    ///
    /// Called at the *end* of a block rather than the start, because two
    /// different producers fill it and they run at different points: MIDI CCs
    /// arrive during the mixer's event dispatch, before `render_block`, while
    /// UI changes are drained inside it. Resetting at the start of
    /// `render_block` would throw the CCs away.
    pub(super) fn reset(&mut self) {
        self.changes.reset();
    }

    /// Records one change directly — the path a MIDI CC takes, having been
    /// mapped to a parameter id by [`MidiMap`](super::events::MidiMap). A CC
    /// that finds the block full is dropped: it has no ring to wait in.
    pub(super) fn push(&mut self, id: ParamID, value: ParamValue) {
        if !self.changes.push(id, value) {
            dprintln!("vst3: more than {MAX_PARAMS_PER_BLOCK} parameters in one block");
        }
    }

    /// Adds what the UI has queued since the last block, up to a full block.
    /// A change that doesn't fit stays at the head of the ring (it is peeked,
    /// not popped) and goes out in a later block, in order.
    ///
    /// Every change lands at sample offset 0. `performEdit` carries no timing
    /// information — it is a UI gesture, not an automation curve — so the
    /// honest placement is "as early in this block as possible".
    pub(super) fn drain_ui(&mut self) {
        while let Ok(&(id, value)) = self.rx.peek() {
            if !self.changes.push(id, value) {
                break;
            }
            let _ = self.rx.pop();
        }
    }

    /// Whether UI changes are waiting — a sleeping voice must wake to deliver
    /// them, or the processor (and the state saved from it) lags the editor.
    pub(super) fn ui_pending(&self) -> bool {
        !self.rx.is_empty()
    }

    /// The automation queue the plugin reads this block.
    pub(super) fn changes(&self) -> &ComWrapper<HostParameterChanges> {
        &self.changes
    }
}

/// Creates a bridge, returning both halves.
pub(super) fn bridge() -> (ParamSender, ParamReceiver) {
    let (tx, rx) = RingBuffer::new(PARAM_RING_CAPACITY);
    (
        ParamSender { tx: Mutex::new(tx) },
        ParamReceiver {
            rx,
            changes: ComWrapper::new(HostParameterChanges::new()),
        },
    )
}

/// One parameter's changes within a block. VST3 models this as a queue of
/// (sample offset, value) points; Stev only ever produces one, at offset 0.
struct HostParamValueQueue {
    /// The parameter this queue is for, and its value. Behind an `UnsafeCell`
    /// for the same reason `HostEventList`'s events are — see the `Sync` impl.
    state: UnsafeCell<(ParamID, ParamValue)>,
}

// SAFETY: identical invariant to `HostEventList`. One queue belongs to one
// voice's `HostParameterChanges`, is never shared between voices, and is only
// touched on the audio callback thread inside one `render_into`: filled by
// `push` / `drain_ui`, read synchronously by `process`, reset before the next
// block. The
// VST3 spec scopes `ProcessData` to the `process` call, so the plugin cannot
// retain it.
unsafe impl Sync for HostParamValueQueue {}

impl Class for HostParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for HostParamValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        // SAFETY: see the `Sync` impl.
        unsafe { (*self.state.get()).0 }
    }

    unsafe fn getPointCount(&self) -> int32 {
        1
    }

    unsafe fn getPoint(
        &self,
        index: int32,
        sample_offset: *mut int32,
        value: *mut ParamValue,
    ) -> tresult {
        if index != 0 || sample_offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: see the `Sync` impl for the read; both out-parameters are
        // caller-owned locals of the matching type.
        unsafe {
            *sample_offset = 0;
            *value = (*self.state.get()).1;
        }
        kResultOk
    }

    /// Input queues are read-only from the plugin's side; a plugin writing
    /// automation does it on `outputParameterChanges`, which Stev does not
    /// supply.
    unsafe fn addPoint(&self, _offset: int32, _value: ParamValue, _index: *mut int32) -> tresult {
        kNotImplemented
    }
}

/// The per-block `IParameterChanges` a plugin reads inside `process`.
pub(super) struct HostParameterChanges {
    /// Pre-allocated queues, so filling this never allocates on the audio
    /// thread. One per distinct parameter changed in a block.
    queues: Vec<ComWrapper<HostParamValueQueue>>,
    /// How many of [`queues`](Self::queues) hold a change this block.
    used: Cell<usize>,
}

// SAFETY: as for `HostParamValueQueue` — sole ownership by one voice, accessed
// only on the audio thread within a single block.
unsafe impl Sync for HostParameterChanges {}

impl Class for HostParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl HostParameterChanges {
    /// An empty set with its queue pool pre-allocated.
    fn new() -> Self {
        Self {
            queues: (0..MAX_PARAMS_PER_BLOCK)
                .map(|_| {
                    ComWrapper::new(HostParamValueQueue {
                        state: UnsafeCell::new((0, 0.0)),
                    })
                })
                .collect(),
            used: Cell::new(0),
        }
    }

    /// Marks every queue unused, ready for a fresh block.
    fn reset(&self) {
        self.used.set(0);
    }

    /// Records a change, reusing this block's queue for a parameter that has
    /// already changed — a knob dragged across a block boundary produces
    /// several `performEdit`s, and the plugin should see the latest value once,
    /// not a queue per event. Returns `false`, recording nothing, when `id` is
    /// new and every queue is taken.
    fn push(&self, id: ParamID, value: ParamValue) -> bool {
        let used = self.used.get();
        for queue in self.queues.iter().take(used) {
            // SAFETY: see the `Sync` impl.
            let state = unsafe { &mut *queue.state.get() };
            if state.0 == id {
                state.1 = value;
                return true;
            }
        }
        let Some(queue) = self.queues.get(used) else {
            return false;
        };
        // SAFETY: see the `Sync` impl.
        unsafe { *queue.state.get() = (id, value) };
        self.used.set(used + 1);
        true
    }
}

impl IParameterChangesTrait for HostParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used.get() {
            return std::ptr::null_mut();
        }
        self.queues
            .get(index as usize)
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map(|r| r.as_ptr())
            .unwrap_or(std::ptr::null_mut())
    }

    /// Only a plugin writing its *own* automation calls this, on an output
    /// queue. Stev supplies no output parameter changes.
    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        _index: *mut int32,
    ) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value a queue currently holds.
    fn point(changes: &HostParameterChanges, index: usize) -> (ParamID, ParamValue) {
        // SAFETY: single-threaded test, same access pattern as the audio thread.
        unsafe { *changes.queues[index].state.get() }
    }

    /// Drains the UI ring and returns the resulting queue — the shape the old
    /// single-step API had, kept here so the tests read the same way the audio
    /// thread does.
    fn drain(rx: &mut ParamReceiver) -> &ComWrapper<HostParameterChanges> {
        rx.reset();
        rx.drain_ui();
        rx.changes()
    }

    #[test]
    fn a_change_crosses_from_the_ui_half_to_the_audio_half() {
        let (tx, mut rx) = bridge();
        tx.send(7, 0.25);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test.
        unsafe { assert_eq!(changes.getParameterCount(), 1) };
        assert_eq!(point(changes, 0), (7, 0.25));
    }

    #[test]
    fn repeated_changes_to_one_parameter_collapse_to_the_latest() {
        // A knob drag produces many `performEdit`s; the plugin should see one
        // queue holding the value it ended on.
        let (tx, mut rx) = bridge();
        tx.send(3, 0.1);
        tx.send(3, 0.5);
        tx.send(3, 0.9);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test.
        unsafe { assert_eq!(changes.getParameterCount(), 1) };
        assert_eq!(point(changes, 0), (3, 0.9));
    }

    #[test]
    fn distinct_parameters_get_distinct_queues() {
        let (tx, mut rx) = bridge();
        tx.send(1, 0.1);
        tx.send(2, 0.2);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test.
        unsafe { assert_eq!(changes.getParameterCount(), 2) };
        assert_eq!(point(changes, 0), (1, 0.1));
        assert_eq!(point(changes, 1), (2, 0.2));
    }

    #[test]
    fn each_block_starts_empty() {
        // A change must reach the plugin once, not every block thereafter.
        let (tx, mut rx) = bridge();
        tx.send(1, 0.1);
        // SAFETY: single-threaded test.
        unsafe { assert_eq!(drain(&mut rx).getParameterCount(), 1) };
        unsafe { assert_eq!(drain(&mut rx).getParameterCount(), 0) };
    }

    #[test]
    fn every_point_lands_at_the_head_of_the_block() {
        // `performEdit` carries no timing, so offset 0 is the honest placement.
        let (tx, mut rx) = bridge();
        tx.send(9, 0.75);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test with valid out-parameters.
        unsafe {
            assert!(!changes.getParameterData(0).is_null());
            let mut offset = -1i32;
            let mut value = 0.0f64;
            assert_eq!(
                changes.queues[0].getPoint(0, &mut offset, &mut value),
                kResultOk
            );
            assert_eq!(offset, 0);
            assert!((value - 0.75).abs() < 1e-12);
            assert_eq!(changes.queues[0].getPointCount(), 1);
            assert_eq!(changes.queues[0].getParameterId(), 9);
        }
    }

    #[test]
    fn an_out_of_range_queue_index_yields_null_not_a_stale_queue() {
        let (tx, mut rx) = bridge();
        tx.send(1, 0.5);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test.
        unsafe {
            assert!(!changes.getParameterData(0).is_null());
            // Index 1 exists in the pool but holds no change this block.
            assert!(changes.getParameterData(1).is_null());
            assert!(changes.getParameterData(-1).is_null());
        }
    }

    #[test]
    fn a_bad_point_request_is_rejected_rather_than_answered() {
        let (tx, mut rx) = bridge();
        tx.send(1, 0.5);
        let changes = drain(&mut rx);
        // SAFETY: single-threaded test; passing nulls is the case under test.
        unsafe {
            let mut offset = 0i32;
            let mut value = 0.0f64;
            assert_eq!(
                changes.queues[0].getPoint(1, &mut offset, &mut value),
                kInvalidArgument
            );
            assert_eq!(
                changes.queues[0].getPoint(0, std::ptr::null_mut(), &mut value),
                kInvalidArgument
            );
        }
    }

    #[test]
    fn a_ui_burst_past_one_block_carries_over_to_the_next() {
        // A preset loaded from the plugin's own UI: more distinct parameters
        // than one block holds. The excess waits in the ring, in order.
        let (tx, mut rx) = bridge();
        let total = MAX_PARAMS_PER_BLOCK as u32 + 10;
        for id in 0..total {
            tx.send(id, 0.5);
        }
        // SAFETY: single-threaded test.
        unsafe {
            assert_eq!(
                drain(&mut rx).getParameterCount(),
                MAX_PARAMS_PER_BLOCK as int32
            );
            assert!(rx.ui_pending());
            assert_eq!(drain(&mut rx).getParameterCount(), 10);
        }
        assert_eq!(point(rx.changes(), 0).0, MAX_PARAMS_PER_BLOCK as u32);
        assert!(!rx.ui_pending());
    }

    #[test]
    fn a_full_block_still_takes_repeat_changes_to_its_parameters() {
        // Only a *new* parameter needs a free queue; a later value for one
        // already in the block replaces it in place.
        let (tx, mut rx) = bridge();
        for id in 0..MAX_PARAMS_PER_BLOCK as u32 {
            tx.send(id, 0.1);
        }
        tx.send(0, 0.9);
        let changes = drain(&mut rx);
        assert_eq!(point(changes, 0), (0, 0.9));
        assert!(!rx.ui_pending());
    }

    #[test]
    fn a_cc_that_finds_the_block_full_is_dropped() {
        let (_tx, mut rx) = bridge();
        for id in 0..(MAX_PARAMS_PER_BLOCK as u32 + 1) {
            rx.push(id, 0.5);
        }
        // SAFETY: single-threaded test.
        unsafe {
            assert_eq!(
                rx.changes().getParameterCount(),
                MAX_PARAMS_PER_BLOCK as int32
            )
        };
    }
}
