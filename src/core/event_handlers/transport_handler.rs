//! `handle_transport_command` and `handle_transport_event` — the `match`es over
//! [`TransportCommand`] and
//! [`TransportEvent`]. Every playback
//! discontinuity ends up in `reanchor_playback`: re-seek tracks, realign the
//! clock's *phase*, flush sounding notes. See `150-clock-position-sync.md`.

use log::debug;

use super::*;

impl EventHandlers {
    /// Reacts to a [`TransportEvent`]:
    /// re-anchor after a plain reset, additionally chase notes after a
    /// reset-with-chase, or release notes and reset the wheels and the
    /// sustain pedal on stop.
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
                // doesn't dip through centre. After the note releases, so
                // the pedal lifting lets nothing hang.
                sequencer.reset_controllers();
            }
        }
    }

    /// Re-anchors the sequencer after the playback position jumped to
    /// `new_tick` — re-seek every track, realign the free-running clock, and
    /// flush sounding notes (MIDI-out `NoteLogger` + CLAP `instrument_notes`) so
    /// nothing hangs across the discontinuity. That flush drops the
    /// `"midiout"` delay queue, so the re-seek resends every controller value a
    /// track sent rather than trust it arrived; and a track whose playback
    /// has the sustain pedal lifts it now, before the re-seek's chase presses
    /// it again at the next tick where playback lands down, or the released
    /// notes would ring on into the jump target (`Sequencer::jump_controllers`,
    /// `archive/530-sustain-capture.md` decision 4). A loop wrap leaves the clock
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
        sequencer.jump_controllers();
        sequencer.reset_to_tick(new_tick);
        transport.align_clock_with_playback();
        self.release_all_notes(sequencer);
    }

    /// Applies one [`TransportCommand`]
    /// against the thread-local `transport` (and, where a command has playback
    /// side effects, the `sequencer` and `metronome`).
    pub(crate) fn handle_transport_command(
        &self,
        cmd: &TransportCommand,
        transport: &mut Transport,
        sequencer: &mut Sequencer,
        metronome: &Metronome,
    ) {
        // A running audio export keeps the transport to itself
        // (`audio_export.rs`).
        if self.audio_export_transport_command(cmd, sequencer) {
            return;
        }
        match cmd {
            TransportCommand::SetRegion { start, end } => {
                transport.region_mut().set_region(*start, *end);
                debug!("Set region: {:?} - {:?}", start, end);

                // Region changed without resetting playback ticks; request a
                // phase sync now so the next loop boundary is already aligned.
                transport.align_clock_with_playback_if_running();
            }
            TransportCommand::SetRegionOrToggleLoop { start, end } => {
                let region_changed = transport.set_region_or_toggle_loop(*start, *end);
                debug!(
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
                debug!("Loop over: {} - {}", start, end);
            }
            TransportCommand::ToggleLoop => {
                transport.toggle_loop_enabled();
                debug!("Toggle loop: loop_enabled={}", transport.is_loop_enabled());
            }
            TransportCommand::SetLoop { enabled } => {
                transport.set_loop_enabled(*enabled);
            }
            TransportCommand::MoveCursor { ticks } => {
                transport.move_cursor(*ticks);
                // A cursor move places the lane's hand (`570` § 2).
                sequencer.place_performance_lane_hand();
                self.sync_clip_selection_to_cursor_workflow(sequencer);
            }
            TransportCommand::SetCursorAndSelectClip { tick } => {
                self.set_cursor_workflow(*tick, transport, sequencer);
            }
            TransportCommand::PlayFromTick { tick } => {
                play_from_tick(*tick, transport, sequencer);
            }
            TransportCommand::RecallMarker { number } => {
                self.recall_marker_workflow(*number, transport, sequencer);
            }
            TransportCommand::StartExport { tick } => {
                transport.set_loop_enabled(false);
                play_from_tick(*tick, transport, sequencer);
            }
            TransportCommand::TogglePlayback => {
                if transport.is_running() {
                    // On the armed lane, off a beat, the stop waits for the
                    // next one (`550-phase-jumps.md` § The stop on the beat).
                    let (meter, tick) = (sequencer.meter(), sequencer.playback_tick());
                    let armed = sequencer.is_performance_lane_armed();
                    if sequencer
                        .performance_lane_mut()
                        .stop_now(meter, tick, armed)
                    {
                        transport.stop();
                    }
                } else {
                    // `restart_from_cursor` starts playback from the cursor.
                    sequencer.begin_running_take(sequencer.cursor_tick());
                    transport.restart_from_cursor();
                    transport.start();
                }
            }
            TransportCommand::Stop => {
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

impl EventHandlers {
    /// Places the arranger cursor at `tick`, as a click in the lanes or a
    /// stopped marker recall does: the lane's hand is placed around it
    /// (`570` § 2) and the lead clip follows it.
    pub(super) fn set_cursor_workflow(
        &self,
        tick: i32,
        transport: &mut Transport,
        sequencer: &mut Sequencer,
    ) {
        transport.set_cursor_tick(tick);
        sequencer.place_performance_lane_hand();
        self.sync_clip_selection_to_cursor_workflow(sequencer);
    }
}

/// Stops, moves playback to `tick` with the notes sounding there chased, and
/// starts again, leaving the cursor: `PlayFromTick`, and the seek of an audio
/// export's run.
fn play_from_tick(tick: i32, transport: &mut Transport, sequencer: &mut Sequencer) {
    sequencer.begin_running_take(tick);
    transport.stop();
    transport.jump_and_chase(tick);
    transport.start();
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::core::sequencer::test_support::{cc_at, clip_at, midi_out_bytes, note_off, note_on};

    use super::super::test_harness::{Harness, harness, transport};
    use super::*;

    const PEDAL_DOWN: [u8; 3] = [0xB0, 64, 127];
    const PEDAL_UP: [u8; 3] = [0xB0, 64, 0];

    /// Track 0 (MIDI Out, channel 0) holds a clip over the loop `[0, 3840)`:
    /// a note at 0 and the pedal pressed at `down`, never lifted. Played
    /// from 0 up to 3000, what it sent dropped.
    fn pedalled_from(down: i32) -> Harness {
        let mut h = harness();
        let mut clip = clip_at(0, 3840);
        for event in [note_on(0), cc_at(down, 64, 127), note_off(480)] {
            clip.add_event(event);
        }
        clip.sort_events_by_tick();
        h.sequencer.tracks_mut()[0].add_clip(&clip);
        h.sequencer.reset_to_tick(0);
        tick(&mut h, 3000);
        pedal_moves(&h);
        h
    }

    /// Plays `ticks` ticks.
    fn tick(h: &mut Harness, ticks: usize) {
        let at = Instant::now();
        for _ in 0..ticks {
            h.sequencer.tick(at);
        }
    }

    /// The pedal moves sent to MIDI Out since the last call.
    fn pedal_moves(h: &Harness) -> Vec<Vec<u8>> {
        midi_out_bytes(&h.midi_out)
            .into_iter()
            .filter(|bytes| bytes.get(..2) == Some(&[0xB0, 64]))
            .collect()
    }

    /// A jump into a stretch where the pedal is down, from one where it is
    /// down too: the pedal lifts at once, so the notes the jump releases
    /// stop ringing, and the chase presses it again at the next tick
    /// (`archive/530-sustain-capture.md` decision 4).
    #[test]
    fn a_jump_under_a_held_pedal_lifts_it_then_presses_it_again() {
        let mut h = pedalled_from(100);
        let transport = transport(&h);

        h.handlers
            .reanchor_playback(1000, &mut h.sequencer, &transport);
        assert_eq!(pedal_moves(&h), vec![PEDAL_UP.to_vec()], "lifted now");
        tick(&mut h, 1);
        assert_eq!(pedal_moves(&h), vec![PEDAL_DOWN.to_vec()]);
    }

    /// A loop wrap from the pedal down to where it is up sends the one lift.
    #[test]
    fn a_wrap_onto_a_lifted_pedal_sends_one_up() {
        let mut h = pedalled_from(100);
        let transport = transport(&h);

        h.handlers
            .reanchor_playback(0, &mut h.sequencer, &transport);
        tick(&mut h, 50);
        assert_eq!(pedal_moves(&h), vec![PEDAL_UP.to_vec()]);
    }

    /// The clip's own press at the loop start follows the lift on a wrap.
    #[test]
    fn a_wrap_onto_a_press_lifts_then_plays_the_press() {
        let mut h = pedalled_from(0);
        let transport = transport(&h);

        h.handlers
            .reanchor_playback(0, &mut h.sequencer, &transport);
        tick(&mut h, 50);
        assert_eq!(
            pedal_moves(&h),
            vec![PEDAL_UP.to_vec(), PEDAL_DOWN.to_vec()]
        );
    }

    /// A stop lifts the pedal a clip left down, once.
    #[test]
    fn a_stop_lifts_the_clips_pedal() {
        let mut h = pedalled_from(100);
        let transport = transport(&h);

        h.handlers
            .handle_transport_event(&TransportEvent::Stopped, &mut h.sequencer, &transport);
        assert_eq!(pedal_moves(&h), vec![PEDAL_UP.to_vec()]);
        h.handlers
            .handle_transport_event(&TransportEvent::Stopped, &mut h.sequencer, &transport);
        assert!(pedal_moves(&h).is_empty());
    }

    /// The player's own pedal on the armed track is theirs: neither a jump
    /// nor a stop lifts it.
    #[test]
    fn the_players_pedal_is_left_down_by_a_jump_and_a_stop() {
        let mut h = pedalled_from(100);
        let transport = transport(&h);
        h.sequencer.handle_midi_input(&PEDAL_DOWN, 3000);
        pedal_moves(&h);

        h.handlers
            .reanchor_playback(1000, &mut h.sequencer, &transport);
        assert!(pedal_moves(&h).is_empty());
        h.handlers
            .handle_transport_event(&TransportEvent::Stopped, &mut h.sequencer, &transport);
        assert!(pedal_moves(&h).is_empty());
    }
}
