//! The piano roll's render model for one note bar — the
//! [`ClipShape`](crate::shapes::clip_shape::ClipShape) analogue for events.
//!
//! `Display` keeps a `Vec<EventShape>` it reconciles against the open clip's
//! events, so the per-frame paint reads plain geometry. The note colour is
//! the lead clip's track's, which `Display` looks up at paint time.

use uuid::Uuid;

use crate::metadata::clip_view::EventMetadata;

/// One note bar as the piano roll draws it. See the module docs.
#[derive(Debug)]
pub(crate) struct EventShape {
    /// The event's id.
    event_id: Uuid,
    /// Note onset, in event ticks.
    start_tick: i32,
    /// Note release, in event ticks.
    end_tick: i32,
    /// MIDI note number — the vertical position.
    note_number: u8,
    /// MIDI velocity — drives the bar's intensity.
    velocity: u8,
    /// Whether the note is in the event selection.
    is_selected: bool,
    /// Whether the note is muted.
    is_muted: bool,
}

impl EventShape {
    /// The shape of `event`, unselected.
    pub(crate) fn from_metadata(event: EventMetadata) -> Self {
        EventShape {
            event_id: event.id,
            start_tick: event.start_tick,
            end_tick: event.end_tick,
            note_number: event.note_number,
            velocity: event.velocity,
            is_selected: false,
            is_muted: event.muted,
        }
    }

    /// Sets the selected flag.
    pub(crate) fn set_selected(&mut self, selected: bool) {
        self.is_selected = selected;
    }

    /// Moves the bar to `start`..`end` on row `note_number` — a released note
    /// drag showing where its edit lands until the clip's next event snapshot
    /// rebuilds the list.
    pub(crate) fn set_span(&mut self, start_tick: i32, end_tick: i32, note_number: u8) {
        self.start_tick = start_tick;
        self.end_tick = end_tick;
        self.note_number = note_number;
    }

    /// Whether this shape represents the event `event_id` — the reconcile key.
    pub(crate) fn matches(&self, event_id: Uuid) -> bool {
        self.event_id == event_id
    }

    /// The event's id.
    pub(crate) fn event_id(&self) -> Uuid {
        self.event_id
    }

    /// Note onset, in event ticks.
    pub(crate) fn start_tick(&self) -> i32 {
        self.start_tick
    }

    /// Note release, in event ticks.
    pub(crate) fn end_tick(&self) -> i32 {
        self.end_tick
    }

    /// MIDI note number.
    pub(crate) fn note_number(&self) -> u8 {
        self.note_number
    }

    /// Whether the shape is marked selected.
    pub(crate) fn is_selected(&self) -> bool {
        self.is_selected
    }

    /// Whether the note is muted.
    pub(crate) fn is_muted(&self) -> bool {
        self.is_muted
    }

    /// MIDI velocity.
    pub(crate) fn velocity(&self) -> u8 {
        self.velocity
    }
}
