//! A looping MIDI phrase — the unit a track arranges and the piano roll edits.
//!
//! ## Two timelines
//!
//! A clip sits at [`start_tick`](Clip::start_tick) on the *arrangement*
//! timeline, but its [`events`](Clip::events) are timed in its own
//! *event-tick* space: the half-open window `region.start..region.end`
//! ([`Region`]). The clip loops by wrapping arrangement time through that
//! window. Never convert between the two by hand — go through
//! [`event_tick_from_arrangement_tick`](Clip::event_tick_from_arrangement_tick),
//! [`arrangement_tick_from_event_tick`](Clip::arrangement_tick_from_event_tick)
//! and [`phase_from_event_tick`](Clip::phase_from_event_tick) (all in
//! `cursor_region.rs`), which are what `080-conventions.md` mandates. A non-zero
//! `region.start` is first-class.
//!
//! ## Module split
//!
//! - `mod.rs` — the struct, identity, region, and the simple flags.
//! - `cursor_region.rs` — the cursor, region nudges, the two-timeline mapping,
//!   tempo rescale.
//! - `events.rs` — the event list: add / playback stepping (`tick`/`seek`) /
//!   restore / crop / trim / sort.
//! - `import.rs` — a clip from a MIDI file's events (the MIDI clip import).
//! - `merge.rs` — baking clips into what they play (Merge Clips, `⌘/Ctrl+J`;
//!   the MIDI clip export, `⌘/Ctrl+⇧+E`).
//! - `edits.rs` — in-place edits of the selected events (nudge time / length,
//!   transpose, velocity, delete, duplicate).
//! - `note_edits.rs` — the piano roll's mouse edits by explicit id: insert a
//!   note, move / resize a dragged set, keeping same-pitch notes from
//!   overlapping.
//! - `quantize.rs` — quantize and swing detection (see `070-quantization.md`).
//! - `pairing.rs` — matching `NoteOn`/`NoteOff` for playback and length calc;
//!   `fold_into_loop`, which turns one loop of a take into a clip.
//! - `selection.rs` — the per-clip event [`Selection`].
//!
//! Pure data, unit-tested per file.

mod cursor_region;
mod edits;
mod events;
mod import;
mod merge;
mod note_edits;
mod pairing;
mod quantize;
mod selection;

pub(crate) use cursor_region::{ClipBounds, ClipEdge, EventSpaceRetime, reach_over};
pub(crate) use note_edits::{CopiedNote, NoteBounds, NoteDrag, min_max};

// --- Module imports ---
use std::sync::{Arc, atomic::AtomicI32};

use uuid::Uuid;

use crate::models::{
    event::{Event, EventType},
    region::Region,
    selection::Selection,
};

/// A looping MIDI phrase. See the module docs for the arrangement-tick vs.
/// event-tick distinction.
#[derive(Debug, Clone)]
pub(crate) struct Clip {
    // --- Identity ---
    /// Stable id — selection, undo and the track clip list all key on it.
    id: Uuid,

    // --- Selection state ---
    /// This clip's selected events (see `selection.rs`).
    event_selection: Selection,

    // --- MIDI events, region and swing ---
    /// The clip's events, sorted by tick, timed in event-tick space.
    events: Vec<Event>,
    /// Position of the clip on the *arrangement* timeline — a position.
    start_tick: i32,
    /// The loop window in *event-tick* space; also the clip's playable length.
    region: Region,
    /// Swing amount, 50–75 (50 = straight). Defines the off-beat grid the
    /// quantizer snaps to — see `070-quantization.md`.
    swing_pct: u8,

    // --- Playback and cursor state ---
    /// Edit-cursor position within the clip, shared likewise. Held within
    /// `[region.start, region.end]`.
    cursor_tick: Arc<AtomicI32>,
    /// Ticks accumulated since the last emitted event — playback bookkeeping
    /// for [`tick`](Self::tick). An amount.
    elapsed_delta_ticks: i32,
    /// Index into [`events`](Self::events) of the next event to emit.
    current_event_idx: usize,
    /// Whether the clip is silenced. A muted clip is skipped by track seek /
    /// playback and its open notes are released.
    muted: bool,
}

impl Clip {
    /// An empty clip: new id, zero-length region at tick 0, straight timing,
    /// unmuted.
    pub(crate) fn new() -> Self {
        Clip {
            id: Uuid::new_v4(),
            event_selection: Selection::new(),
            events: Vec::new(),
            start_tick: 0,
            region: Region::new(0, 0),
            swing_pct: 50,
            cursor_tick: Arc::new(AtomicI32::new(0)),
            elapsed_delta_ticks: 0,
            current_event_idx: 0,
            muted: false,
        }
    }

    /// The clip's stable id.
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    /// Assigns a fresh id and returns it — used when a copy must be a distinct
    /// clip (duplicate, paste).
    pub(crate) fn generate_new_id(&mut self) -> Uuid {
        self.id = Uuid::new_v4();
        self.id
    }

    /// The loop window in event-tick space.
    pub(crate) fn region(&self) -> &Region {
        &self.region
    }

    /// Mutable access to the loop window.
    pub(crate) fn region_mut(&mut self) -> &mut Region {
        &mut self.region
    }

    /// Window length `end - start` — the clip's playable length, in ticks.
    pub(crate) fn region_length(&self) -> i32 {
        self.region.end() - self.region.start()
    }

    /// The clip's start on the arrangement timeline.
    pub(crate) fn start_tick(&self) -> i32 {
        self.start_tick
    }

    /// Moves the clip along the arrangement timeline. Event ticks are unchanged
    /// — they are phase-locked to the region, not to `start_tick`.
    pub(crate) fn set_start_tick(&mut self, value: i32) {
        self.start_tick = value;
    }

    /// The clip's end on the arrangement timeline (`start_tick + length`).
    pub(crate) fn end_tick(&self) -> i32 {
        self.start_tick + self.region_length()
    }

    /// Sets the muted flag.
    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Whether the clip is silenced.
    pub(crate) fn is_muted(&self) -> bool {
        self.muted
    }

    /// Swing amount, 50–75.
    pub(crate) fn swing_pct(&self) -> u8 {
        self.swing_pct
    }

    /// Sets the swing amount, clamped to the valid 50–75 range.
    pub(crate) fn set_swing_pct(&mut self, pct: u8) {
        self.swing_pct = pct.clamp(50, 75);
    }

    /// The clip's events, sorted by tick.
    pub(crate) fn events(&self) -> &[Event] {
        &self.events
    }

    /// Whether the clip holds a note (a `NoteOn`). A capture is a take only
    /// when it does: wheel moves alone commit nothing.
    pub(crate) fn has_notes(&self) -> bool {
        self.events
            .iter()
            .any(|event| event.event_type() == Some(EventType::NoteOn))
    }
}
