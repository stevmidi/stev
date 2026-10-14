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

use std::error::Error;
use std::hint::spin_loop;
use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::core::midi::out_queue::{MidiOutMessage, MidiOutQueue, deadline_for};
use crate::core::midi::output::MidiOutputConnection;
use crate::core::note_logger::{NoteLogger, NoteLoggerCommand};
use crate::core::thread_priority::{TimingThread, raise_timing_priority};

/// How long the `"midiout"` thread blocks when its delay queue is empty.
/// Nothing depends on the wakeup — it exists only so the `select!` always has a
/// timeout arm, and a once-a-second tick on an otherwise idle thread costs
/// nothing.
const MIDI_OUT_IDLE_TIMEOUT: Duration = Duration::from_secs(1);

/// How close a deadline must be before the loop stops trusting `select!`'s
/// timeout, a condvar wait that the timing probe saw overshoot by up to
/// ~1.3 ms on an idle Mac (`260-porting.md` § Timing).
const PRECISE_WINDOW: Duration = Duration::from_millis(2);

/// The final stretch before a deadline that is spun rather than slept: a
/// `thread::sleep` can itself overshoot by a few hundred µs.
const SPIN_TAIL: Duration = Duration::from_micros(500);

/// The longest single sleep inside [`PRECISE_WINDOW`], so a live-thru message
/// arriving meanwhile waits at most this long for the channels to be polled.
const POLL_SLICE: Duration = Duration::from_micros(250);

/// How the `"midiout"` loop waits before its next pass.
#[derive(Debug, PartialEq)]
enum Wait {
    /// Block in `select!` for up to this long: the next deadline is far off,
    /// or nothing is queued.
    Block(Duration),
    /// Poll the channels, then sleep this long: the deadline is near.
    Sleep(Duration),
    /// Poll the channels, then go round again at once: the deadline is in the
    /// spin tail.
    Spin,
}

/// How to wait at `now` with `next_deadline` the earliest queued message. A
/// far deadline blocks until [`PRECISE_WINDOW`] before it; inside the window
/// the loop sleeps in [`POLL_SLICE`]s down to [`SPIN_TAIL`], then spins, so
/// the message leaves on time without the channels going unpolled.
fn plan_wait(next_deadline: Option<Instant>, now: Instant) -> Wait {
    let Some(deadline) = next_deadline else {
        return Wait::Block(MIDI_OUT_IDLE_TIMEOUT);
    };
    let remaining = deadline.saturating_duration_since(now);
    if remaining > PRECISE_WINDOW {
        Wait::Block(remaining - PRECISE_WINDOW)
    } else if remaining > SPIN_TAIL {
        Wait::Sleep((remaining - SPIN_TAIL).min(POLL_SLICE))
    } else {
        Wait::Spin
    }
}

/// Where the `"midiout"` loop writes: the live port in the app, a recorder or
/// a loopback port in the timing probe (`260-porting.md` § Timing), so the
/// probe measures the very loop the app runs.
pub(crate) trait MidiSink {
    /// Writes raw MIDI bytes.
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>>;
}

impl MidiSink for MidiOutputConnection {
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        MidiOutputConnection::send(self, message)
    }
}

/// Writes one message to the port and records note on/off with the logger, so
/// `ReleaseNotes` releases exactly the notes that actually reached the device —
/// which is what makes dropping the delay queue on a stop safe.
fn send_midi_out<S: MidiSink>(sink: &mut S, logger: &mut NoteLogger, msg: Vec<u8>) {
    if let Err(e) = sink.send(&msg) {
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
            run_midi_output_loop(
                MidiOutputConnection::new(),
                midi_out_rx,
                NoteLogger::new(midi_out_tx),
                note_logger_command_rx,
                midi_out_connection_rx,
                midi_out_offset_ms,
            );
        })
        .expect("Failed to spawn midiout thread")
}

/// The `"midiout"` loop itself, writing to `sink` until a new one arrives on
/// `sink_rx`. Returns the current sink once `midi_out_rx` disconnects, which
/// the app never does (the logger holds a sender to it); the timing probe uses
/// it to end a run and read its recorder back.
pub(crate) fn run_midi_output_loop<S: MidiSink>(
    mut sink: S,
    midi_out_rx: Receiver<MidiOutMessage>,
    mut logger: NoteLogger,
    note_logger_command_rx: Receiver<NoteLoggerCommand>,
    sink_rx: Receiver<S>,
    midi_out_offset_ms: Arc<AtomicI32>,
) -> S {
    raise_timing_priority(TimingThread::MidiOut);
    let mut queue = MidiOutQueue::new();

    loop {
        // Flush what has come due, then wait as `plan_wait` says: blocking
        // while the next deadline is far off, polling once it's near.
        let now = Instant::now();
        while let Some(msg) = queue.pop_due(now) {
            send_midi_out(&mut sink, &mut logger, msg);
        }
        let wait = plan_wait(queue.next_deadline(), now);
        let timeout = match wait {
            Wait::Block(timeout) => timeout,
            Wait::Sleep(_) | Wait::Spin => Duration::ZERO,
        };

        select! {
            recv(midi_out_rx) -> message => {
                let Ok(msg) = message else {
                    return sink;
                };
                let offset = midi_out_offset_ms.load(Ordering::Relaxed);
                match deadline_for(msg.at, offset, Instant::now()) {
                    Some(deadline) => queue.push(deadline, msg.bytes),
                    None => send_midi_out(&mut sink, &mut logger, msg.bytes),
                }
            }
            recv(note_logger_command_rx) -> command => {
                if let Ok(cmd) = command {
                    match cmd {
                        NoteLoggerCommand::ReleaseNotes => {
                            // Drop the queue *before* releasing: a scheduled
                            // note-on belongs to a playback that has just
                            // ended, and letting one out after the release
                            // pass would hang a note the logger no longer
                            // knows about.
                            queue.clear();
                            logger.release_notes();
                        }
                        NoteLoggerCommand::LogInputEvent(msg) => {
                            logger.log_input_event(&msg);
                        }
                    }
                }
            }
            recv(sink_rx) -> connection => {
                if let Ok(connection) = connection {
                    // Pending messages were addressed to the old port.
                    queue.clear();
                    sink = connection;
                }
            }
            default(timeout) => match wait {
                Wait::Sleep(slice) => thread::sleep(slice),
                Wait::Spin => spin_loop(),
                Wait::Block(_) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_queued_blocks_for_the_idle_timeout() {
        assert_eq!(
            plan_wait(None, Instant::now()),
            Wait::Block(MIDI_OUT_IDLE_TIMEOUT)
        );
    }

    #[test]
    fn a_far_deadline_blocks_until_the_precise_window() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(10);
        assert_eq!(
            plan_wait(Some(deadline), now),
            Wait::Block(Duration::from_millis(10) - PRECISE_WINDOW)
        );
    }

    #[test]
    fn a_near_deadline_sleeps_in_slices_down_to_the_spin_tail() {
        let now = Instant::now();
        assert_eq!(
            plan_wait(Some(now + PRECISE_WINDOW), now),
            Wait::Sleep(POLL_SLICE)
        );
        let close = SPIN_TAIL + Duration::from_micros(100);
        assert_eq!(
            plan_wait(Some(now + close), now),
            Wait::Sleep(Duration::from_micros(100))
        );
    }

    #[test]
    fn the_spin_tail_and_a_passed_deadline_spin() {
        let now = Instant::now();
        assert_eq!(plan_wait(Some(now + SPIN_TAIL), now), Wait::Spin);
        assert_eq!(
            plan_wait(Some(now - Duration::from_millis(1)), now),
            Wait::Spin
        );
    }
}
