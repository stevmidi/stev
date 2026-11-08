//! One MIDI message or plugin parameter move placed on a clip's timeline.
//!
//! An `Event` is the atom the piano roll draws and the edit operations move,
//! transpose and delete. It carries its payload — raw MIDI bytes, or a
//! [`ParamMove`] (`400-parameter-knobs.md` decision 11) — plus the timeline
//! bookkeeping the sequencer needs: an absolute `tick` (a position, in the
//! clip's event-tick space), a `delta_ticks` gap from the previous event (kept
//! so playback can step the list without re-scanning), and a `length` giving a
//! `NoteOn` its paired `NoteOff` distance for rendering. Pure data — no I/O, no
//! channels — and unit-tested at the bottom of the file.
//!
//! Keeping parameter moves in the same list is what lets every clip edit
//! carry them with no code of its own. Whatever reads MIDI skips them by
//! type: a parameter move's [`midi_message`](Event::midi_message) is empty,
//! so it is no note, no controller message and no channel message.

use uuid::Uuid;

use crate::core::midi::message::{Controller, parse_controller, rewrite_channel};
use crate::models::knobs::PluginParam;

/// The two note edges the sequencer cares about. Derived from the MIDI status
/// byte and velocity by [`Event::event_type`]; everything else (CC, sysex,
/// realtime) is `None`.
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum EventType {
    /// A `0x90` status with velocity > 0.
    NoteOn,
    /// A `0x80` status, or a `0x90` with velocity 0 (running-status note off).
    NoteOff,
}

/// A plugin parameter moved to a value: what a knob records
/// (`400-parameter-knobs.md` decision 11).
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) struct ParamMove {
    /// The parameter, with the plugin it was recorded on.
    pub(crate) param: PluginParam,
    /// Where it moved to, normalized to `0.0..=1.0`.
    pub(crate) value: f64,
}

/// What an [`Event`] carries.
#[derive(Debug, Clone)]
enum Payload {
    /// The raw MIDI message. `[status, data1, data2]` for note events; kept
    /// as a `Vec` because CC / sysex / realtime also pass through here.
    Midi(Vec<u8>),
    /// A plugin parameter's move.
    Param(ParamMove),
}

/// A single timed event within a clip. See the module docs.
#[derive(Debug, Clone)]
pub(crate) struct Event {
    // --- Identity ---
    /// Stable id — survives edits, and is what selection and the undo system
    /// key against. [`Uuid::nil`] for a throwaway `Event` built straight from
    /// live MIDI bytes ([`from_midi`](Self::from_midi)).
    id: Uuid,

    // --- Timing ---
    /// Absolute position on the clip's event-tick timeline. A position, not an
    /// amount (`080-conventions.md`).
    tick: i32,
    /// Ticks since the previous event in the clip's sorted list — an amount,
    /// cached so playback advances event-to-event without rescanning. Kept in
    /// step via [`set_delta_ticks`](Self::set_delta_ticks).
    delta_ticks: i32,
    /// For a `NoteOn`, ticks to its matching `NoteOff` — an amount, used for
    /// rendering the note bar and for length edits. `0` until
    /// `Clip::calculate_note_lengths` pairs them up.
    length: i32,

    // --- Data ---
    /// What the event plays.
    payload: Payload,

    // --- Editing ---
    /// Whether this event is muted — set on a `NoteOn` and its paired
    /// `NoteOff` together by `Clip::toggle_muted_for_selected_events`, and
    /// checked at the playback emission sites so a muted note never sounds.
    muted: bool,
}

impl Event {
    // --- Constructors ---
    /// A fresh event with a new [`id`](Self::id). `delta_ticks` starts at `0`
    /// and is filled in by the clip's sort pass; `length` is supplied by the
    /// caller (usually `0`, then filled in by the clip's length pass).
    pub(crate) fn new(tick: i32, length: i32, midi_message: Vec<u8>) -> Self {
        Event::with_payload(tick, length, Payload::Midi(midi_message))
    }

    /// [`new`](Self::new), for either kind of payload.
    fn with_payload(tick: i32, length: i32, payload: Payload) -> Self {
        Event {
            id: Uuid::new_v4(),
            tick,
            delta_ticks: 0,
            length,
            payload,
            muted: false,
        }
    }

    /// A throwaway event straight off the wire — nil id, no timeline position.
    /// Used where only the MIDI bytes matter (channel routing, thru).
    pub(crate) fn from_midi(midi_message: &[u8]) -> Self {
        Event {
            id: Uuid::nil(),
            tick: 0,
            delta_ticks: 0,
            length: 0,
            payload: Payload::Midi(midi_message.to_vec()),
            muted: false,
        }
    }

    /// [`from_midi`](Self::from_midi) but placed at `tick` and given a real id —
    /// a recorded note.
    pub(crate) fn from_midi_with_tick(midi_message: &[u8], tick: i32) -> Self {
        Event::new(tick, 0, midi_message.to_vec())
    }

    /// A parameter move at `tick`, with a real id.
    pub(crate) fn from_param_move(tick: i32, param_move: ParamMove) -> Self {
        Event::with_payload(tick, 0, Payload::Param(param_move))
    }

    /// A move of `controller` to `value` at `tick`, on `channel` for one a
    /// MIDI message carries.
    pub(crate) fn controller_move_at(
        tick: i32,
        controller: Controller,
        value: f64,
        channel: u8,
    ) -> Self {
        if let Controller::Param(param) = controller {
            return Event::from_param_move(tick, ParamMove { param, value });
        }
        let mut message = controller.message(value as u16);
        rewrite_channel(&mut message, channel);
        Event::from_midi_with_tick(&message, tick)
    }

    /// The same payload as a new, unmuted event at `tick`, with no length:
    /// a move restamped, or a bake's copy.
    pub(crate) fn copy_at(&self, tick: i32) -> Self {
        Event::with_payload(tick, 0, self.payload.clone())
    }

    /// [`copy_at`](Self::copy_at) with the id given rather than a fresh
    /// random one: a bake that writes many events takes its ids from one
    /// sequence (`merge.rs`'s `FreshIds`), since a v4 id per event is most of
    /// a whole-project merge's time.
    pub(crate) fn copy_with_id(&self, tick: i32, id: Uuid) -> Self {
        Event {
            id,
            tick,
            delta_ticks: 0,
            length: 0,
            payload: self.payload.clone(),
            muted: false,
        }
    }

    // --- Identity accessor ---
    /// The stable id — see the [`id`](Self::id) field.
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    // --- Accessors and mutators ---
    /// Absolute position on the clip's event-tick timeline.
    pub(crate) fn tick(&self) -> i32 {
        self.tick
    }

    /// Moves the event to an absolute tick. Callers are responsible for
    /// re-sorting and refreshing `delta_ticks` afterwards.
    pub(crate) fn set_tick(&mut self, value: i32) {
        self.tick = value;
    }

    /// Ticks since the previous event — see the [`delta_ticks`](Self::delta_ticks)
    /// field.
    pub(crate) fn delta_ticks(&self) -> i32 {
        self.delta_ticks
    }

    /// Recomputes `delta_ticks` as `self.tick - prev_tick` — call with the tick
    /// of the event now preceding this one in the sorted list.
    pub(crate) fn set_delta_ticks(&mut self, prev_tick: i32) {
        self.delta_ticks = self.tick - prev_tick;
    }

    /// Sets the note length (ticks to the paired `NoteOff`).
    pub(crate) fn set_length(&mut self, value: i32) {
        self.length = value;
    }

    /// `tick + length` — where a `NoteOn`'s note bar ends.
    pub(crate) fn end_tick(&self) -> i32 {
        self.tick + self.length
    }

    /// The raw MIDI bytes — none for a parameter move, which nothing reading
    /// MIDI recognises.
    pub(crate) fn midi_message(&self) -> &[u8] {
        match &self.payload {
            Payload::Midi(message) => message,
            Payload::Param(_) => &[],
        }
    }

    /// Consumes the event for its raw MIDI bytes — the owned buffer, not a
    /// copy, for playback's send of an event it already owns. Empty for a
    /// parameter move.
    pub(crate) fn into_midi_message(self) -> Vec<u8> {
        match self.payload {
            Payload::Midi(message) => message,
            Payload::Param(_) => Vec::new(),
        }
    }

    /// The parameter move this event is, if it is one.
    pub(crate) fn param_move(&self) -> Option<ParamMove> {
        match self.payload {
            Payload::Param(param_move) => Some(param_move),
            Payload::Midi(_) => None,
        }
    }

    /// Puts a parameter move on `param` instead. No-op on a MIDI event.
    pub(crate) fn set_param(&mut self, param: PluginParam) {
        if let Payload::Param(param_move) = &mut self.payload {
            param_move.param = param;
        }
    }

    /// The controller this event moves and the value it moves it to: a
    /// MIDI controller's in its own units ([`parse_controller`]), a
    /// parameter's normalized. `None` for anything else.
    pub(crate) fn controller_move(&self) -> Option<(Controller, f64)> {
        match &self.payload {
            Payload::Midi(message) => {
                parse_controller(message).map(|(controller, value)| (controller, f64::from(value)))
            }
            Payload::Param(ParamMove { param, value }) => Some((Controller::Param(*param), *value)),
        }
    }

    /// MIDI data byte 1 (the note number for note events), or `None` if the
    /// message is too short.
    pub(crate) fn note_number(&self) -> Option<u8> {
        self.midi_message().get(1).copied()
    }

    /// Overwrites the note number in place. No-op on a message with no data
    /// byte 1.
    pub(crate) fn set_note_number(&mut self, note_number: u8) {
        if let Payload::Midi(message) = &mut self.payload
            && let Some(byte) = message.get_mut(1)
        {
            *byte = note_number;
        }
    }

    /// MIDI data byte 2 (velocity for note events), or `None` if the message is
    /// too short.
    pub(crate) fn velocity(&self) -> Option<u8> {
        self.midi_message().get(2).copied()
    }

    /// Overwrites the velocity in place. No-op on a message with no data
    /// byte 2.
    pub(crate) fn set_velocity(&mut self, velocity: u8) {
        if let Payload::Midi(message) = &mut self.payload
            && let Some(byte) = message.get_mut(2)
        {
            *byte = velocity;
        }
    }

    /// Whether this event is muted — see the [`muted`](Self::muted) field.
    pub(crate) fn is_muted(&self) -> bool {
        self.muted
    }

    /// Sets the muted flag directly.
    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    // --- MIDI status helpers ---
    /// Classifies the message as a note edge, or `None` for anything else
    /// (CC, sysex, realtime). Treats a zero-velocity `NoteOn` as a `NoteOff`.
    pub(crate) fn event_type(&self) -> Option<EventType> {
        let message = self.midi_message();
        let &status = message.first()?;
        let vel = message.get(2).copied().unwrap_or(0);

        match status & 0xF0 {
            0x90 if vel > 0 => Some(EventType::NoteOn),
            // A zero-velocity note-on is a note-off by MIDI convention.
            0x80 | 0x90 => Some(EventType::NoteOff),
            _ => None,
        }
    }

    /// Whether this is a note edge (a `NoteOn` or `NoteOff`) — what every
    /// note-only consumer keeps, since a clip also holds wheel moves.
    pub(crate) fn is_note_edge(&self) -> bool {
        self.event_type().is_some()
    }

    /// The MIDI channel (0–15) in the status byte's low nibble, or `None` for
    /// System Common / Realtime messages (`0xF0..=0xFF`), which carry none.
    pub(crate) fn midi_channel(&self) -> Option<u8> {
        let &status = self.midi_message().first()?;

        // System Common / Realtime messages are 0xF0..0xFF and have no MIDI channel.
        if status >= 0xF0 {
            return None;
        }

        Some(status & 0x0F)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::knobs::test_param;

    fn note_on(tick: i32, note: u8, vel: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, vel])
    }

    /// Nothing reading MIDI sees a parameter move: no bytes, no note edge,
    /// no channel, and a note edit passes it by.
    #[test]
    fn a_parameter_move_is_nothing_to_a_midi_reader() {
        let param_move = ParamMove {
            param: test_param(714),
            value: 0.25,
        };
        let mut event = Event::from_param_move(96, param_move);
        assert!(event.midi_message().is_empty());
        assert!(!event.is_note_edge());
        assert_eq!(event.midi_channel(), None);
        assert_eq!(event.note_number(), None);
        event.set_note_number(60);
        event.set_velocity(100);
        assert_eq!(event.param_move(), Some(param_move));
        assert_eq!(event.into_midi_message(), Vec::<u8>::new());
    }

    #[test]
    fn a_controller_move_reads_midi_and_parameters_alike() {
        let bend = Event::new(0, 0, vec![0xE3, 0x00, 0x40]);
        assert_eq!(
            bend.controller_move(),
            Some((Controller::PitchBend, 8192.0))
        );
        let param = Event::controller_move_at(10, Controller::Param(test_param(7)), 0.5, 3);
        assert_eq!(
            param.controller_move(),
            Some((Controller::Param(test_param(7)), 0.5))
        );
        assert_eq!(note_on(0, 60, 100).controller_move(), None);
    }

    #[test]
    fn a_built_controller_move_lands_on_its_channel() {
        let event = Event::controller_move_at(10, Controller::MOD_WHEEL, 64.0, 3);
        assert_eq!(event.tick(), 10);
        assert_eq!(event.midi_message(), &[0xB3, 0x01, 64]);
    }

    /// A copy is a new event: its own id, unmuted, with the same payload.
    #[test]
    fn a_copy_keeps_the_payload_at_a_new_tick() {
        let mut event = Event::from_param_move(
            5,
            ParamMove {
                param: test_param(1),
                value: 0.75,
            },
        );
        event.set_muted(true);
        let copy = event.copy_at(50);
        assert_eq!(copy.tick(), 50);
        assert_ne!(copy.id(), event.id());
        assert!(!copy.is_muted());
        assert_eq!(copy.param_move(), event.param_move());
        event.set_param(test_param(2));
        assert_eq!(
            event.param_move().map(|moved| moved.param),
            Some(test_param(2))
        );
    }

    #[test]
    fn end_tick_is_tick_plus_length() {
        let mut e = Event::new(100, 50, vec![0x90, 60, 100]);
        // set_length is the only way to set it post-construction
        e.set_length(50);
        assert_eq!(e.end_tick(), 150);
    }

    #[test]
    fn end_tick_zero_length_equals_tick() {
        let e = Event::new(100, 0, vec![0x90, 60, 100]);
        assert_eq!(e.end_tick(), 100);
    }

    #[test]
    fn into_midi_message_yields_the_raw_bytes() {
        assert_eq!(note_on(0, 60, 100).into_midi_message(), vec![0x90, 60, 100]);
    }

    #[test]
    fn event_type_note_on_positive_velocity() {
        let e = note_on(0, 60, 100);
        assert_eq!(e.event_type(), Some(EventType::NoteOn));
    }

    #[test]
    fn event_type_note_on_on_non_zero_channel() {
        let e = Event::new(0, 0, vec![0x91, 60, 100]); // channel 1
        assert_eq!(e.event_type(), Some(EventType::NoteOn));
    }

    #[test]
    fn event_type_note_on_zero_velocity_is_note_off() {
        let e = Event::new(0, 0, vec![0x90, 60, 0]);
        assert_eq!(e.event_type(), Some(EventType::NoteOff));
    }

    #[test]
    fn event_type_0x80_status_is_note_off() {
        let e = Event::new(0, 0, vec![0x80, 60, 0]);
        assert_eq!(e.event_type(), Some(EventType::NoteOff));
    }

    #[test]
    fn event_type_cc_returns_none() {
        let e = Event::new(0, 0, vec![0xB0, 7, 100]);
        assert_eq!(e.event_type(), None);
    }

    #[test]
    fn event_type_empty_message_returns_none() {
        let e = Event::new(0, 0, vec![]);
        assert_eq!(e.event_type(), None);
    }

    #[test]
    fn midi_channel_extracts_lower_nibble() {
        let e = Event::new(0, 0, vec![0x91, 60, 100]); // channel 1
        assert_eq!(e.midi_channel(), Some(1));
    }

    #[test]
    fn midi_channel_channel_zero() {
        let e = note_on(0, 60, 100); // 0x90 → channel 0
        assert_eq!(e.midi_channel(), Some(0));
    }

    #[test]
    fn midi_channel_sysex_returns_none() {
        let e = Event::new(0, 0, vec![0xF0, 0x7E, 0x00]);
        assert_eq!(e.midi_channel(), None);
    }

    #[test]
    fn note_number_extracts_second_byte() {
        let e = note_on(0, 60, 100);
        assert_eq!(e.note_number(), Some(60));
    }

    #[test]
    fn note_number_empty_message_returns_none() {
        let e = Event::new(0, 0, vec![]);
        assert_eq!(e.note_number(), None);
    }

    #[test]
    fn set_note_number_updates_second_byte() {
        let mut e = note_on(0, 60, 100);
        e.set_note_number(72);
        assert_eq!(e.note_number(), Some(72));
    }

    #[test]
    fn set_note_number_ignores_short_messages() {
        let mut e = Event::new(0, 0, vec![0xF8]);
        e.set_note_number(70);
        assert_eq!(e.midi_message(), &[0xF8]);
    }

    #[test]
    fn delta_ticks_computed_from_previous() {
        let mut e = Event::new(100, 0, vec![0x90, 60, 100]);
        e.set_delta_ticks(60);
        assert_eq!(e.delta_ticks(), 40); // 100 - 60
    }

    #[test]
    fn new_events_start_unmuted() {
        let e = note_on(0, 60, 100);
        assert!(!e.is_muted());
    }

    #[test]
    fn set_muted_toggles_the_flag() {
        let mut e = note_on(0, 60, 100);
        e.set_muted(true);
        assert!(e.is_muted());
        e.set_muted(false);
        assert!(!e.is_muted());
    }
}
