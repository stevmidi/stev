//! Per-track record of which clip notes each hosted instrument plugin currently
//! has sounding, so transport stop / seek can release exactly those instead of
//! blasting all 128 keys, which would cut every note the player is holding.
//!
//! The plugin MIDI route (`Sequencer`'s `instrument_midi_tx` / `MidiInputForwarder`'s
//! own ring of the same name → the audio engine's instrument mixer) never passes through
//! the `"midiout"` thread, so the
//! [`NoteLogger`] note-off safety net does not cover it — this is the
//! equivalent for instrument tracks. Unlike it, a clip note is released even
//! when its pitch is held live: the off carries the clip's own note id
//! ([`NoteSource`](super::instrument_event::NoteSource)), so a plugin keying
//! voices by id plays the live key on. Plugin-routed events are always on
//! channel 0, so only the note number is tracked.
//!
//! Every `track` here is an engine slot
//! ([`Track::slot`](crate::models::track::Track::slot)), the same index the
//! plugin-bound events carry — never a track's position.
//!
//! [`NoteLogger`]: crate::core::note_logger::NoteLogger

use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::parse_note;

/// Per-track record of the notes each hosted plugin is holding. See the module
/// docs.
#[derive(Debug)]
pub(super) struct InstrumentNotes {
    /// `held[track][note]` — a NoteOn routed to that track's plugin from a clip
    /// (or chase) with no matching NoteOff yet.
    held: [[bool; 128]; MAX_TRACKS],
}

impl InstrumentNotes {
    /// Nothing held.
    pub(super) fn new() -> Self {
        Self {
            held: [[false; 128]; MAX_TRACKS],
        }
    }

    /// Records a clip / chase MIDI message just routed to `track`'s plugin.
    pub(super) fn observe_clip(&mut self, track: usize, msg: &[u8]) {
        let Some(slot) = self.held.get_mut(track) else {
            return;
        };
        if let Some((note, on)) = parse_note(msg) {
            slot[note] = on;
        }
    }

    /// Note numbers to release on `track` to silence its clip-driven notes:
    /// every note recorded as held. Released notes are cleared.
    pub(super) fn release_for(&mut self, track: usize) -> Vec<u8> {
        let Some(slot) = self.held.get_mut(track) else {
            return Vec::new();
        };
        let notes = (0..=127).filter(|&note| slot[usize::from(note)]).collect();
        slot.fill(false);
        notes
    }

    /// Drops clip tracking (new / loaded project).
    pub(super) fn clear(&mut self) {
        self.held = [[false; 128]; MAX_TRACKS];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_clip_note_on_then_releases_it_once() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(2, &[0x90, 60, 100]);
        notes.observe_clip(2, &[0x90, 64, 100]);
        assert_eq!(notes.release_for(2), vec![60, 64]);
        // Cleared by release_for.
        assert!(notes.release_for(2).is_empty());
    }

    #[test]
    fn note_off_and_zero_velocity_note_on_both_clear() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(0, &[0x90, 60, 100]);
        notes.observe_clip(0, &[0x90, 62, 100]);
        notes.observe_clip(0, &[0x80, 60, 0]); // explicit note-off
        notes.observe_clip(0, &[0x90, 62, 0]); // running-status note-off
        assert!(notes.release_for(0).is_empty());
    }

    #[test]
    fn tracking_is_per_track() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(0, &[0x90, 60, 100]);
        notes.observe_clip(1, &[0x90, 72, 100]);
        assert_eq!(notes.release_for(1), vec![72]);
        assert_eq!(notes.release_for(0), vec![60]);
    }

    #[test]
    fn non_note_messages_and_bad_indices_are_ignored() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(0, &[0xB0, 0x7B, 0]); // CC
        notes.observe_clip(0, &[0x90]); // truncated
        notes.observe_clip(999, &[0x90, 60, 100]); // out-of-range track
        assert!(notes.release_for(0).is_empty());
    }

    #[test]
    fn clear_drops_clip_tracking() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(3, &[0x90, 48, 100]);
        notes.clear();
        assert!(notes.release_for(3).is_empty());
    }
}
