//! The `"sequencer"` thread — the hub, and the tick pump.
//!
//! Its `select!` loop ticks the transport on each [`ClockTick`], re-anchors a
//! loop wrap synchronously before that tick reaches the sequencer (deferring it
//! leaks the next clip's note-on — see `150-clock-position-sync.md`), drains
//! MIDI input, and dispatches every [`SequencerCommand`] / [`TransportCommand`]
//! through `EventHandlers` against the thread-local `Sequencer` / `Transport` /
//! `Metronome`. While an audio export renders, the clock's ticks play nothing
//! and the run is stepped instead, by the offline render's [`ExportStep`]s
//! (`490-offline-export.md` decision 4). It also owns the `Record<UndoStep>` undo stack
//! (`050-undo-redo.md`) and the [`OpenProject`]: what the unsaved-changes
//! check compares against (`060-persistence.md`), a project read from disk
//! while the view loads its plugins (`130-plugin-host.md`), and the
//! [`RecoveryKeeper`] whose check the tick arm runs
//! (`410-crash-recovery.md`).

use crossbeam_channel::{Receiver, never, select};
use undo::Record;

use std::iter;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::core::clock::ClockTick;
use crate::core::event_handlers::{
    EventHandlers, OpenProject, RecoveryKeeper, SavedProject, Storage,
};
use crate::core::metronome::Metronome;
use crate::core::midi::input::RecordedMidi;
use crate::core::project::ProjectData;
use crate::core::sequencer::{ExportStep, Sequencer, SequencerCommand, UndoStep};
use crate::core::time::TapTempo;
use crate::core::transport::{Transport, TransportCommand, TransportEvent};

/// Starts the `"sequencer"` thread — the hub. Its `select!` loop ticks the
/// transport on each [`ClockTick`], drains MIDI input, and dispatches every
/// [`SequencerCommand`] / [`TransportCommand`] via `EventHandlers` against the
/// thread-local `Sequencer` / `Transport` / `Metronome`, and steps an audio
/// export's run for each [`ExportStep`] on `export_step_rx`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_sequencer_thread(
    mut sequencer: Sequencer,
    mut transport: Transport,
    mut metronome: Metronome,
    event_handlers: Arc<EventHandlers>,
    tick_rx: Receiver<ClockTick>,
    midi_in_rx: Receiver<RecordedMidi>,
    transport_event_rx: Receiver<TransportEvent>,
    sequencer_command_rx: Receiver<SequencerCommand>,
    transport_command_rx: Receiver<TransportCommand>,
    mut export_step_rx: Receiver<ExportStep>,
    recovery: RecoveryKeeper,
    storage: Storage,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            ProjectData::default().apply_to_sequencer(&mut sequencer);
            // The view starts from placeholder lanes without track ids.
            event_handlers.emit_tracks(&sequencer, None);

            event_handlers.select_track_workflow(&mut sequencer, 0);

            let mut undo_record: Record<UndoStep> = Record::new();
            let mut tap_tempo = TapTempo::default();
            // The empty startup project counts as saved: quitting before
            // touching anything asks nothing.
            let mut open = OpenProject::new(SavedProject::of(&sequencer, None), recovery, storage);

            loop {
                select! {
                    recv(tick_rx) -> tick => {
                        let Ok(first) = tick else { break };
                        let at = first.at;
                        // This tick, then any extra ticks that arrived while
                        // processing, each run to completion before the next is
                        // taken off the channel.
                        let drained = iter::from_fn(|| tick_rx.try_recv().ok());
                        for tick in iter::once(first).chain(drained) {
                            // An audio export's run is stepped by its render,
                            // in virtual time: the clock's ticks play nothing
                            // meanwhile, and the click gets none.
                            if sequencer.is_exporting_audio() {
                                continue;
                            }
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
                                event_handlers.play_tick(tick.at, &mut sequencer, &mut transport);
                            }
                            if sequencer.take_playback_retimed() {
                                metronome.playback_retimed();
                            }
                            metronome.on_tick(tick.at, click_tick, sequencer.meter(), running);
                        }
                        open.tick(at, &sequencer);
                    }

                    recv(midi_in_rx) -> midi_message => {
                        if let Ok((message, tick)) = midi_message {
                            if sequencer.is_performance_lane_armed() {
                                event_handlers.handle_performance_lane_midi_input(&mut sequencer, &mut transport, &message);
                            } else {
                                let kicked = sequencer.handle_midi_input(&message, tick);
                                // Running, the UI's ~30 fps frames show the
                                // track's meter (and an extra wake would jitter
                                // the playhead, `030`); stopped, nothing else
                                // wakes it for a live note.
                                if kicked && !transport.is_running() {
                                    event_handlers.request_repaint();
                                }
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
                            event_handlers.handle_sequencer_command(&cmd, &mut sequencer, &mut undo_record, &mut tap_tempo, &mut open);
                        }
                    }

                    recv(export_step_rx) -> step => {
                        match step {
                            Ok(step) => {
                                // The run's own seek first. Its command was
                                // queued, on this thread, before anything
                                // could ask for a step, and `select!` takes
                                // the ready channels in no order: so what
                                // waits on the transport's two is handled
                                // here, commands and then the events they
                                // sent, and the tracks are on the
                                // selection's start before a tick is played.
                                for cmd in transport_command_rx.try_iter() {
                                    event_handlers.handle_transport_command(&cmd, &mut transport, &mut sequencer, &metronome);
                                }
                                for event in transport_event_rx.try_iter() {
                                    event_handlers.handle_transport_event(&event, &mut sequencer, &transport);
                                }
                                let stepped = event_handlers.step_audio_export(step.until, &mut sequencer, &mut transport);
                                step.answer.send(stepped).ok();
                            }
                            // No plugin host, so no render to ask: a
                            // disconnected channel would otherwise be ready
                            // on every pass.
                            Err(_) => export_step_rx = never(),
                        }
                    }

                    recv(transport_command_rx) -> command => {
                        if let Ok(cmd) = command {
                            event_handlers.handle_transport_command(&cmd, &mut transport, &mut sequencer, &metronome);
                        }
                    }
                }
            }
        })
        .expect("Failed to spawn sequencer thread")
}
