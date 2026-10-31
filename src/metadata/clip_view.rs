//! [`ClipView`] — the render-side snapshot of one clip's piano roll, the
//! payload of `UiEvent::ClipEntered` / `EventsUpdated`.
//!
//! It shares the clip's *position* atoms (cursor / region) so the cursor and
//! region shading track live without a `UiEvent`, but takes a flat copy of the
//! events ([`EventMetadata`]) and of its controller moves
//! ([`ControllerMoves`]). The
//! `Display` piano-roll renderer works from
//! this alone. The arranger's counterpart is
//! [`ClipMetadata`](crate::metadata::clip_metadata::ClipMetadata). See
//! `020-views-and-state.md`.

use std::sync::{Arc, atomic::AtomicI32};

use uuid::Uuid;

use crate::core::midi::message::{Controller, parse_controller};
use crate::models::{clip::Clip, event::EventType};

/// One `NoteOn` as the piano roll draws it.
#[derive(Debug, Clone)]
pub(crate) struct EventMetadata {
    /// The event's id.
    pub(crate) id: Uuid,
    /// Note onset, event ticks.
    pub(crate) start_tick: i32,
    /// Note release, event ticks.
    pub(crate) end_tick: i32,
    /// MIDI note number.
    pub(crate) note_number: u8,
    /// MIDI velocity.
    pub(crate) velocity: u8,
    /// Whether the note is muted.
    pub(crate) muted: bool,
}

/// One controller move as a lane draws it: `(tick, value)`, event ticks.
pub(crate) type ControllerMove = (i32, u16);

/// The clip's unmuted controller moves — pitch bend, the mod wheel, any
/// other CC — per controller, each list in tick order: what plays, so a
/// muted move is left out (`ControllerValues::apply`). See
/// `380-wheel-lane.md`, `390-knob-panel.md` decision 11.
#[derive(Debug, Clone, Default)]
pub(crate) struct ControllerMoves(Vec<(Controller, Vec<ControllerMove>)>);

impl ControllerMoves {
    /// `controller`'s moves; none when the clip has none.
    pub(crate) fn of(&self, controller: Controller) -> &[ControllerMove] {
        self.0
            .iter()
            .find(|(moved, _)| *moved == controller)
            .map_or(&[], |(_, moves)| moves)
    }

    /// Adds a move of `controller` to `value` at `tick`, after every move
    /// added before it.
    fn push(&mut self, controller: Controller, tick: i32, value: u16) {
        match self.0.iter_mut().find(|(moved, _)| *moved == controller) {
            Some((_, moves)) => moves.push((tick, value)),
            None => self.0.push((controller, vec![(tick, value)])),
        }
    }
}

/// A whole piano-roll snapshot. See the module docs.
#[derive(Debug, Clone)]
pub(crate) struct ClipView {
    /// Shared edit-cursor position within the clip.
    pub(crate) cursor_tick: Arc<AtomicI32>,
    /// Shared region start (event ticks).
    pub(crate) region_start: Arc<AtomicI32>,
    /// Shared region end (event ticks).
    pub(crate) region_end: Arc<AtomicI32>,
    /// The clip's `NoteOn` events, flattened.
    pub(crate) events: Vec<EventMetadata>,
    /// The clip's controller moves, for the controller lanes.
    pub(crate) controller_moves: ControllerMoves,
    /// The clip's whole extent in event ticks (`Clip::event_extent`): the
    /// span `⌘A` marks in a controller lane.
    pub(crate) extent: (i32, i32),
    /// The clip's id.
    pub(crate) clip_id: Uuid,
    /// The event selection.
    pub(crate) selected_event_ids: Vec<Uuid>,
}
impl ClipView {
    /// Snapshots `clip` into a `ClipView`, sharing its position atoms and
    /// copying its `NoteOn`s and controller moves.
    pub(crate) fn from_clip(clip: &Clip) -> ClipView {
        let events: Vec<EventMetadata> = clip
            .events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| EventMetadata {
                id: event.id(),
                start_tick: event.tick(),
                end_tick: event.end_tick(),
                note_number: event.note_number().unwrap_or(0),
                velocity: event.velocity().unwrap_or(64),
                muted: event.is_muted(),
            })
            .collect();

        let mut controller_moves = ControllerMoves::default();
        for event in clip.events().iter().filter(|event| !event.is_muted()) {
            if let Some((controller, value)) = parse_controller(event.midi_message()) {
                controller_moves.push(controller, event.tick(), value);
            }
        }

        ClipView {
            cursor_tick: clip.cursor_tick_atomic(),
            region_start: clip.region().start_atomic(),
            region_end: clip.region().end_atomic(),
            events,
            controller_moves,
            extent: clip.event_extent(),
            clip_id: clip.id(),
            selected_event_ids: clip.selected_event_ids(),
        }
    }
}
