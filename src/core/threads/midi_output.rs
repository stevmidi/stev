//! The `"midiout"` thread: the single owner of the outbound MIDI port once
//! the `"midiwatcher"` has opened it.
//!
//! Holds clip messages in a [`MidiOutQueue`] until their tick instant plus the
//! user's `midi_out_offset_ms`, so external gear lands with the audio path
//! rather than ahead of it; live thru, previews and the note-off safety net
//! carry no instant and go straight out. Every write is recorded with the
//! [`NoteLogger`], which is what makes dropping the queue on a stop safe.
//! See `160-midi-out-offset.md`.

use crossbeam_channel::{Receiver, Sender, select};

use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::core::midi::out_queue::{MidiOutMessage, MidiOutQueue, deadline_for};
use crate::core::midi::output::MidiOutputConnection;
use crate::core::note_logger::{NoteLogger, NoteLoggerCommand};

/// How long the `"midiout"` thread blocks when its delay queue is empty.
/// Nothing depends on the wakeup — it exists only so the `select!` always has a
/// timeout arm, and a once-a-second tick on an otherwise idle thread costs
/// nothing.
const MIDI_OUT_IDLE_TIMEOUT: Duration = Duration::from_secs(1);

/// Writes one message to the port and records note on/off with the logger, so
/// `ReleaseNotes` releases exactly the notes that actually reached the device —
/// which is what makes dropping the delay queue on a stop safe.
fn send_midi_out(connection: &mut MidiOutputConnection, logger: &mut NoteLogger, msg: Vec<u8>) {
    if let Err(e) = connection.send(&msg) {
        eprintln!("Failed to send MIDI message: {}", e);
    } else {
        logger.log_clip_event(&msg);
    }
}

/// Drains the output port, holding clip messages in a [`MidiOutQueue`] until
/// their tick instant plus `midi_out_offset_ms` so external gear lands with the
/// audio path instead of ahead of it. Live thru, previews and the note-off
/// safety net carry no instant and go straight out. See `160-midi-out-offset.md`.
///
/// Starts with no port open. The `"midiwatcher"` opens ports and hands each
/// connection over on `midi_out_connection_rx`; this thread swaps it in.
pub(crate) fn start_midi_output_thread(
    midi_out_rx: Receiver<MidiOutMessage>,
    midi_out_tx: Sender<MidiOutMessage>,
    note_logger_command_rx: Receiver<NoteLoggerCommand>,
    midi_out_connection_rx: Receiver<MidiOutputConnection>,
    midi_out_offset_ms: Arc<AtomicI32>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("midiout".to_string())
        .spawn(move || {
            let mut midi_out = MidiOutputConnection::new();
            let mut logger = NoteLogger::new(midi_out_tx);
            let mut queue = MidiOutQueue::new();

            loop {
                // Flush what has come due, then block only as long as the next
                // deadline allows — an empty queue waits the idle timeout.
                let now = Instant::now();
                while let Some(msg) = queue.pop_due(now) {
                    send_midi_out(&mut midi_out, &mut logger, msg);
                }
                let timeout = queue
                    .next_deadline()
                    .map_or(MIDI_OUT_IDLE_TIMEOUT, |deadline| {
                        deadline.saturating_duration_since(now)
                    });

                select! {
                    recv(midi_out_rx) -> message => {
                        if let Ok(msg) = message {
                            let offset = midi_out_offset_ms.load(Ordering::Relaxed);
                            match deadline_for(msg.at, offset, Instant::now()) {
                                Some(deadline) => queue.push(deadline, msg.bytes),
                                None => send_midi_out(&mut midi_out, &mut logger, msg.bytes),
                            }
                        }
                    }
                    recv(note_logger_command_rx) -> command => {
                        if let Ok(cmd) = command {
                            match cmd {
                                NoteLoggerCommand::ReleaseNotes => {
                                    // Drop the queue *before* releasing: a
                                    // scheduled note-on belongs to a playback
                                    // that has just ended, and letting one out
                                    // after the release pass would hang a note
                                    // the logger no longer knows about.
                                    queue.clear();
                                    logger.release_notes();
                                }
                                NoteLoggerCommand::LogInputEvent(msg) => {
                                    logger.log_input_event(&msg);
                                }
                            }
                        }
                    }
                    recv(midi_out_connection_rx) -> connection => {
                        if let Ok(connection) = connection {
                            // Pending messages were addressed to the old port.
                            queue.clear();
                            midi_out = connection;
                        }
                    }
                    default(timeout) => {}
                }
            }
        })
        .expect("Failed to spawn midiout thread")
}
