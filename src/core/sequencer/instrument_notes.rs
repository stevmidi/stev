//! Per-track record of which notes each hosted instrument plugin currently has
//! sounding, so transport stop / seek can release exactly those instead of
//! blasting all 128 keys — and, crucially, *without* cutting a note the player
//! is holding on the physical keyboard (start/stop mid-improvisation must not
//! kill the note under their fingers).
//!
//! The plugin MIDI route (`Sequencer`'s `instrument_midi_tx` / `MidiInputForwarder`'s
//! own ring of the same name → the audio engine's instrument mixer) never passes through
//! the `"midiout"` thread, so the
//! [`NoteLogger`] note-off safety net does not cover it — this is the
//! equivalent for instrument tracks (and mirrors its "don't release a
//! live-held note" rule). Plugin-routed events are always on channel 0, so only
//! the note number is tracked.
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
    /// Notes currently down on the physical keyboard.
    live_held: [bool; 128],
    /// The instrument track the live keyboard is routed to (the armed track, if
    /// it hosts a plugin). `release_for` must not release a clip note that is
    /// also held live *on this track*.
    live_track: Option<usize>,
}

impl InstrumentNotes {
    /// Nothing held, no live track.
    pub(super) fn new() -> Self {
        Self {
            held: [[false; 128]; MAX_TRACKS],
            live_held: [false; 128],
            live_track: None,
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

    /// Records a live-keyboard MIDI message. `target` is the instrument track it
    /// is being routed to right now (`None` if the armed track has no plugin).
    pub(super) fn observe_live(&mut self, target: Option<usize>, msg: &[u8]) {
        self.live_track = target;
        if let Some((note, on)) = parse_note(msg) {
            self.live_held[note] = on;
        }
    }

    /// Note numbers to release on `track` to silence its clip-driven notes:
    /// every note recorded as held, minus any that is also under the player's
    /// fingers on this same track. Released notes are cleared; a skipped
    /// live-overlapped note stays recorded so a later stop still catches it.
    pub(super) fn release_for(&mut self, track: usize) -> Vec<u8> {
        let protect_live = self.live_track == Some(track);
        let Some(slot) = self.held.get_mut(track) else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        for (note, held) in slot.iter_mut().enumerate() {
            if !*held {
                continue;
            }
            if protect_live && self.live_held[note] {
                continue;
            }
            notes.push(note as u8);
            *held = false;
        }
        notes
    }

    /// Drops clip tracking (new / loaded project). Live state is left alone — it
    /// mirrors physical keys that may still be down.
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
    fn a_note_held_live_on_the_target_track_is_not_released() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(1, &[0x90, 60, 100]);
        notes.observe_clip(1, &[0x90, 64, 100]);
        // Player is holding C4 (60) live on track 1.
        notes.observe_live(Some(1), &[0x90, 60, 100]);

        // Only the non-live clip note is released.
        assert_eq!(notes.release_for(1), vec![64]);
        // C4 is still recorded — a later stop (after the key is up) catches it.
        notes.observe_live(Some(1), &[0x80, 60, 0]);
        assert_eq!(notes.release_for(1), vec![60]);
    }

    #[test]
    fn live_hold_on_a_different_track_does_not_protect() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(0, &[0x90, 60, 100]);
        notes.observe_live(Some(1), &[0x90, 60, 100]); // live goes to track 1
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
    fn clear_drops_clip_tracking_only() {
        let mut notes = InstrumentNotes::new();
        notes.observe_clip(3, &[0x90, 48, 100]);
        notes.observe_live(Some(3), &[0x90, 50, 100]);
        notes.clear();
        assert!(notes.release_for(3).is_empty());
        // Live note still remembered.
        notes.observe_clip(3, &[0x90, 50, 100]);
        assert!(notes.release_for(3).is_empty());
    }
}
