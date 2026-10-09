//! The MIDI-out note-off safety net.
//!
//! Lives on the `"midiout"` thread. It tracks which `[note, channel]` slots
//! clip playback has sounding, and which the live keyboard is holding; on a
//! transport stop / seek ([`NoteLoggerCommand::ReleaseNotes`]) it sends a
//! Note Off for every clip note that is *not* also held live — so a
//! discontinuity never strands an external note, without cutting one the player
//! is holding. Only covers the port MIDI route; the instrument-plugin route has
//! its own equivalent (`sequencer/instrument_notes.rs`).

use crossbeam_channel::Sender;

use crate::core::midi::message::{Midi3, parse_note};
use crate::core::midi::out_queue::MidiOutMessage;

/// A message to the `NoteLogger` (on the `"midiout"` thread).
pub(crate) enum NoteLoggerCommand {
    /// Send a Note Off for every held clip note not also held live.
    ReleaseNotes,
    /// Record a live-input note edge (so `ReleaseNotes` spares it).
    LogInputEvent(Midi3),
}

/// Tracks held notes per `[note, channel]` slot for [`NoteLoggerCommand::ReleaseNotes`].
pub(crate) struct NoteLogger {
    /// `[note, channel]` slots clip playback currently has sounding.
    clip_note_log: [bool; 128 * 16],
    /// `[note, channel]` slots the live keyboard currently holds.
    input_note_log: [bool; 128 * 16],
    /// Where the synthetic Note Offs go.
    midi_out_tx: Sender<MidiOutMessage>,
}

impl NoteLogger {
    /// An empty logger writing releases to `midi_out_tx`.
    pub(crate) fn new(midi_out_tx: Sender<MidiOutMessage>) -> Self {
        NoteLogger {
            clip_note_log: [false; 128 * 16],
            input_note_log: [false; 128 * 16],
            midi_out_tx,
        }
    }

    /// Records a clip-playback note edge; anything else is ignored.
    pub(crate) fn log_clip_event(&mut self, msg: &[u8]) {
        Self::log_edge(&mut self.clip_note_log, msg);
    }

    /// Records a live-input note edge; anything else is ignored.
    pub(crate) fn log_input_event(&mut self, msg: &[u8]) {
        Self::log_edge(&mut self.input_note_log, msg);
    }

    /// Sets (note-on) or clears (note-off) `msg`'s `[note, channel]` slot in `log`.
    fn log_edge(log: &mut [bool; 128 * 16], msg: &[u8]) {
        if let Some((note, on)) = parse_note(msg) {
            let channel = usize::from(msg[0] & 0x0F);
            log[channel * 128 + note] = on;
        }
    }

    /// Sends a Note Off for every clip note held but not also held live, and
    /// clears those slots.
    pub(crate) fn release_notes(&mut self) {
        for i in 0..self.clip_note_log.len() {
            if self.clip_note_log[i] && !self.input_note_log[i] {
                let channel = (i / 128) as u8;
                let note = (i % 128) as u8;
                let status = 0x80 | channel;
                let note_off = vec![status, note, 0];
                self.midi_out_tx.send(MidiOutMessage::now(note_off)).ok();
                self.clip_note_log[i] = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use super::NoteLogger;

    #[test]
    fn release_notes_offs_clip_notes_but_spares_live_held_ones() {
        let (tx, rx) = unbounded();
        let mut logger = NoteLogger::new(tx);
        logger.log_clip_event(&[0x92, 60, 100]); // held by a clip
        logger.log_clip_event(&[0x92, 64, 100]);
        logger.log_clip_event(&[0x92, 64, 0]); // velocity-0 = released
        logger.log_clip_event(&[0x93, 67, 100]);
        logger.log_input_event(&[0x93, 67, 100]); // also held live
        logger.log_clip_event(&[0xB2, 7, 100]); // not a note: ignored

        logger.release_notes();

        let sent: Vec<Vec<u8>> = rx.try_iter().map(|msg| msg.bytes).collect();
        assert_eq!(sent, vec![vec![0x82, 60, 0]]);
    }
}
