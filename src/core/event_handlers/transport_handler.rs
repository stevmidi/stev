//! `handle_transport_command` and `handle_transport_event` — the `match`es over
//! [`TransportCommand`] and
//! [`TransportEvent`]. Every playback
//! discontinuity ends up in `reanchor_playback`: re-seek tracks, realign the
//! clock's *phase*, flush sounding notes. See `150-clock-position-sync.md`.

use undo::Record;

use crate::core::sequencer::SequencerEdit;

use super::*;

impl EventHandlers {
    /// Reacts to a [`TransportEvent`]:
    /// re-anchor after a plain reset, additionally chase notes after a
    /// reset-with-chase, or release notes and reset the wheels on stop.
    pub(crate) fn handle_transport_event(
        &self,
        event: &TransportEvent,
        sequencer: &mut Sequencer,
        transport: &Transport,
    ) {
        match event {
            TransportEvent::PlaybackTickReset(new_tick) => {
                self.reanchor_playback(*new_tick, sequencer, transport);
            }

            TransportEvent::PlaybackTickResetWithChase(new_tick) => {
                self.reanchor_playback(*new_tick, sequencer, transport);
                if sequencer.is_running() {
                    sequencer.chase_notes(*new_tick);
                }
            }

            TransportEvent::Stopped => {
                self.release_all_notes(sequencer);
                // Only on a stop: every jump while running chases the
                // wheels instead (`Track::seek`), so a loop wrap mid-bend
                // doesn't dip through centre.
                sequencer.reset_wheels();
            }
        }
    }

    /// Re-anchors the sequencer after the playback position jumped to
    /// `new_tick` — re-seek every track, realign the free-running clock, and
    /// flush sounding notes (MIDI-out `NoteLogger` + CLAP `instrument_notes`) so
    /// nothing hangs across the discontinuity. That flush drops the
    /// `"midiout"` delay queue, so the re-seek resends every wheel value a
    /// track sent (`Sequencer::invalidate_wheels`) rather than trust it arrived. A loop wrap leaves the clock
    /// phases equal in principle and a few ticks of command latency apart in
    /// practice, so the realign only nudges the phase there — it must never move
    /// the clock out of the loop it has free-run to, or running capture keeps
    /// every cycle of a take instead of the last one.
    ///
    /// A loop wrap calls this **synchronously** from the `"sequencer"` thread's
    /// tick pump (`Transport::tick` → [`TickOutcome::Wrapped`]), before the next
    /// `Sequencer::tick`, so playback never runs past the region end into the
    /// following clip. Every other jump routes here via
    /// [`TransportEvent::PlaybackTickReset`].
    ///
    /// [`TickOutcome::Wrapped`]: crate::core::transport::TickOutcome::Wrapped
    pub(crate) fn reanchor_playback(
        &self,
        new_tick: i32,
        sequencer: &mut Sequencer,
        transport: &Transport,
    ) {
        sequencer.invalidate_wheels();
        sequencer.reset_to_tick(new_tick);
        transport.align_clock_with_playback();
        self.release_all_notes(sequencer);
    }

    /// Applies one [`TransportCommand`]
    /// against the thread-local `transport` (and, where a command has playback
    /// side effects, the `sequencer` and `metronome`). `undo_record` is only
    /// for the stop-family commands that end a live take, which records the
    /// completed clip as an undoable commit.
    pub(crate) fn handle_transport_command(
        &self,
        cmd: &TransportCommand,
        transport: &mut Transport,
        sequencer: &mut Sequencer,
        metronome: &Metronome,
        undo_record: &mut Record<SequencerEdit>,
    ) {
        match cmd {
            TransportCommand::SetRegion { start, end } => {
                transport.region_mut().set_region(*start, *end);
                dprintln!("Set region: {:?} - {:?}", start, end);

                // Region changed without resetting playback ticks; request a
                // phase sync now so the next loop boundary is already aligned.
                transport.align_clock_with_playback_if_running();
            }
            TransportCommand::SetRegionOrToggleLoop { start, end } => {
                let region_changed = transport.set_region_or_toggle_loop(*start, *end);
                dprintln!(
                    "Set region to clip or toggle loop: {} - {} (region_changed={}, loop_enabled={})",
                    start,
                    end,
                    region_changed,
                    transport.is_loop_enabled()
                );

                if region_changed {
                    // Region changed without resetting playback ticks; request a
                    // phase sync now so the next loop boundary is already aligned.
                    transport.align_clock_with_playback_if_running();
                }
            }
            TransportCommand::LoopOver { start, end } => {
                transport.loop_over(*start, *end);
                dprintln!("Loop over: {} - {}", start, end);
            }
            TransportCommand::ToggleLoop => {
                transport.toggle_loop_enabled();
                dprintln!("Toggle loop: loop_enabled={}", transport.is_loop_enabled());
            }
            TransportCommand::MoveCursor { ticks } => {
                transport.move_cursor(*ticks);
                self.sync_clip_selection_to_cursor_workflow(sequencer);
            }
            TransportCommand::SetCursorAndSelectClip { tick } => {
                transport.set_cursor_tick(*tick);
                self.sync_clip_selection_to_cursor_workflow(sequencer);
            }
            TransportCommand::PlayFromTick { tick } => {
                if sequencer.is_running() && sequencer.is_recording() {
                    self.end_live_recording_workflow(sequencer, undo_record);
                }
                sequencer.reset_capture();
                transport.stop();
                transport.jump_and_chase(*tick);
                transport.start();
            }
            TransportCommand::TogglePlayback => {
                if transport.is_running() {
                    if sequencer.is_recording() {
                        self.end_live_recording_workflow(sequencer, undo_record);
                    }
                    transport.stop();
                } else {
                    sequencer.reset_capture();
                    transport.restart_from_cursor();
                    transport.start();
                }
            }
            TransportCommand::Stop => {
                if sequencer.is_recording() {
                    self.end_live_recording_workflow(sequencer, undo_record);
                }
                sequencer.reset_capture();
                transport.stop();
                transport.reset_playback_to_cursor();
            }
            TransportCommand::ToggleMetronomeMute => {
                metronome.toggle_mute();
            }
        }
    }
}
