//! The `"sequencer"` thread — the hub, and the tick pump.
//!
//! Its `select!` loop ticks the transport on each [`ClockTick`], re-anchors a
//! loop wrap synchronously before that tick reaches the sequencer (deferring it
//! leaks the next clip's note-on — see `150-clock-position-sync.md`), drains
//! MIDI input, and dispatches every [`SequencerCommand`] / [`TransportCommand`]
//! through `EventHandlers` against the thread-local `Sequencer` / `Transport` /
//! `Metronome`. It also owns the `Record<SequencerEdit>` undo stack
//! (`050-undo-redo.md`) and the [`SavedProject`] the unsaved-changes check
//! compares against (`060-persistence.md`).

use crossbeam_channel::{Receiver, select};
use undo::Record;

use std::iter;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::core::clock::ClockTick;
use crate::core::event_handlers::{EventHandlers, SavedProject};
use crate::core::metronome::Metronome;
use crate::core::midi::input::InputTicks;
use crate::core::project::ProjectData;
use crate::core::sequencer::{Sequencer, SequencerCommand, SequencerEdit};
use crate::core::time::TapTempo;
use crate::core::transport::{TickOutcome, Transport, TransportCommand, TransportEvent};

/// Starts the `"sequencer"` thread — the hub. Its `select!` loop ticks the
/// transport on each [`ClockTick`], drains MIDI input, and dispatches every
/// [`SequencerCommand`] / [`TransportCommand`] via `EventHandlers` against the
/// thread-local `Sequencer` / `Transport` / `Metronome`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_sequencer_thread(
    mut sequencer: Sequencer,
    mut transport: Transport,
    mut metronome: Metronome,
    event_handlers: Arc<EventHandlers>,
    tick_rx: Receiver<ClockTick>,
    midi_in_rx: Receiver<(Vec<u8>, InputTicks)>,
    transport_event_rx: Receiver<TransportEvent>,
    sequencer_command_rx: Receiver<SequencerCommand>,
    transport_command_rx: Receiver<TransportCommand>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            ProjectData::default().apply_to_sequencer(&mut sequencer);

            event_handlers.select_track_workflow(&mut sequencer, 0);

            let mut undo_record: Record<SequencerEdit> = Record::new();
            let mut tap_tempo = TapTempo::default();
            // The empty startup project counts as saved: quitting before
            // touching anything asks nothing.
            let mut saved = SavedProject::of(&sequencer);

            loop {
                select! {
                    recv(tick_rx) -> tick => {
                        let Ok(first) = tick else { break };
                        // This tick, then any extra ticks that arrived while
                        // processing, each run to completion before the next is
                        // taken off the channel.
                        let drained = iter::from_fn(|| tick_rx.try_recv().ok());
                        for tick in iter::once(first).chain(drained) {
                            // The click counts from playback while running —
                            // read before the advance, so the start tick and
                            // a wrap's region start are each counted once —
                            // and from the free-running clock while stopped
                            // (`Metronome`'s module docs).
                            let running = transport.is_running();
                            let click_tick = if running {
                                sequencer.playback_tick()
                            } else {
                                tick.tick
                            };
                            if running {
                                // A loop wrap must be re-anchored here, before
                                // this tick reaches the sequencer — deferring it
                                // to `transport_event_rx` lets a burst of ticks
                                // drained in one wakeup play past the region end
                                // into the next clip (stuck-note bug).
                                if let TickOutcome::Wrapped(anchor) = transport.tick() {
                                    event_handlers.reanchor_playback(
                                        anchor,
                                        &mut sequencer,
                                        &transport,
                                    );
                                }
                                sequencer.tick(tick.at);

                                if sequencer.should_end_live_recording() {
                                    event_handlers.end_live_recording_workflow(&mut sequencer, &mut undo_record);
                                }
                            }
                            metronome.on_tick(tick.at, click_tick, sequencer.meter(), running);
                        }
                    }

                    recv(midi_in_rx) -> midi_message => {
                        if let Ok((message, ticks)) = midi_message {
                            if sequencer.is_performance_lane_armed() {
                                event_handlers.handle_performance_lane_midi_input(&mut sequencer, &mut transport, &message);
                            } else {
                                sequencer.handle_midi_input_dispatch(&message, ticks);
                            }
                        }
                    }

                    recv(transport_event_rx) -> transport_event => {
                        if let Ok(event) = transport_event {
                            event_handlers.handle_transport_event(&event, &mut sequencer, &transport);
                        }
                    }

                    recv(sequencer_command_rx) -> command => {
                        if let Ok(cmd) = command {
                            event_handlers.handle_sequencer_command(&cmd, &mut sequencer, &mut undo_record, &mut tap_tempo, &mut saved);
                        }
                    }

                    recv(transport_command_rx) -> command => {
                        if let Ok(cmd) = command {
                            event_handlers.handle_transport_command(&cmd, &mut transport, &mut sequencer, &metronome, &mut undo_record);
                        }
                    }
                }
            }
        })
        .expect("Failed to spawn sequencer thread")
}
