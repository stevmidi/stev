//! One MIDI message placed on a clip's timeline.
//!
//! An `Event` is the atom the piano roll draws and the edit operations move,
//! transpose and delete. It carries the raw MIDI bytes plus the timeline
//! bookkeeping the sequencer needs: an absolute `tick` (a position, in the
//! clip's event-tick space), a `delta_ticks` gap from the previous event (kept
//! so playback can step the list without re-scanning), and a `length` giving a
//! `NoteOn` its paired `NoteOff` distance for rendering. Pure data — no I/O, no
//! channels — and unit-tested at the bottom of the file.

use uuid::Uuid;

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

/// A single timed MIDI message within a clip. See the module docs.
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

    // --- MIDI data ---
    /// The raw MIDI message. `[status, data1, data2]` for note events; kept as
    /// a `Vec` because CC / sysex / realtime also pass through here.
    midi_message: Vec<u8>,

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
        Event {
            id: Uuid::new_v4(),
            tick,
            delta_ticks: 0,
            length,
            midi_message,
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
            midi_message: midi_message.to_vec(),
            muted: false,
        }
    }

    /// [`from_midi`](Self::from_midi) but placed at `tick` and given a real id —
    /// a recorded note.
    pub(crate) fn from_midi_with_tick(midi_message: &[u8], tick: i32) -> Self {
        Event::new(tick, 0, midi_message.to_vec())
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

    /// The raw MIDI bytes.
    pub(crate) fn midi_message(&self) -> &[u8] {
        &self.midi_message
    }

    /// Consumes the event for its raw MIDI bytes — the owned buffer, not a
    /// copy, for playback's send of an event it already owns.
    pub(crate) fn into_midi_message(self) -> Vec<u8> {
        self.midi_message
    }

    /// MIDI data byte 1 (the note number for note events), or `None` if the
    /// message is too short.
    pub(crate) fn note_number(&self) -> Option<u8> {
        self.midi_message.get(1).copied()
    }

    /// Overwrites the note number in place. No-op on a message with no data
    /// byte 1.
    pub(crate) fn set_note_number(&mut self, note_number: u8) {
        if self.midi_message.len() > 1 {
            self.midi_message[1] = note_number;
        }
    }

    /// MIDI data byte 2 (velocity for note events), or `None` if the message is
    /// too short.
    pub(crate) fn velocity(&self) -> Option<u8> {
        self.midi_message.get(2).copied()
    }

    /// Overwrites the velocity in place. No-op on a message with no data
    /// byte 2.
    pub(crate) fn set_velocity(&mut self, velocity: u8) {
        if self.midi_message.len() > 2 {
            self.midi_message[2] = velocity;
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
        let &status = self.midi_message.first()?;
        let vel = self.midi_message.get(2).copied().unwrap_or(0);

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
        let &status = self.midi_message.first()?;

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

    fn note_on(tick: i32, note: u8, vel: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, vel])
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
