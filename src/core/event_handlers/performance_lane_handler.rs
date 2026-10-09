//! The `EventHandlers` chokepoint for raw MIDI-IN while the arranger
//! performance lane is armed: a `NoteOn` on a mapped key jumps the transport to
//! that bar and suspends loop-wrap; the `NoteOff` resumes loop-wrap but
//! deliberately leaves the transport running — releasing the key does not
//! stop playback, so a performer stops on their own terms. Lives here rather
//! than in `Sequencer` because it needs `&mut Transport`. Live-only. See
//! `110-performance-lane.md`.

use crate::models::{
    event::{Event, EventType},
    performance_lane::bar_index_from_note,
};

use super::*;

impl EventHandlers {
    /// Chokepoint for raw MIDI-IN NoteOn/NoteOff while the performance lane
    /// is armed (selected) — routes triggers into transport jumps instead
    /// of normal note capture. This needs `&mut Transport`, which
    /// `Sequencer` doesn't have, so it lives here (called directly from the
    /// sequencer thread's `midi_in_rx` arm) rather than inside
    /// `Sequencer::handle_midi_input_dispatch`.
    ///
    /// Live input only — nothing here is recorded or persisted.
    pub(crate) fn handle_performance_lane_midi_input(
        &self,
        sequencer: &mut Sequencer,
        transport: &mut Transport,
        message: &[u8],
    ) {
        let event = Event::from_midi(message);
        match event.event_type() {
            Some(EventType::NoteOn) => {
                let Some(note) = event.note_number() else {
                    return;
                };
                let Some(bar_index) = bar_index_from_note(note) else {
                    return;
                };

                sequencer.performance_lane_mut().begin_live_trigger(note);

                let target_tick = time::bars_to_ticks(bar_index);
                transport.suspend_loop_wrap();
                if !transport.is_running() {
                    transport.start();
                }
                transport.jump_and_chase(target_tick);
                // The trigger came from a physical MIDI key, not a window
                // input event, so nothing else wakes the reactive UI up to
                // notice `running` flipped true and start animating.
                self.request_repaint();
            }
            Some(EventType::NoteOff) => {
                let Some(note) = event.note_number() else {
                    return;
                };
                if sequencer.performance_lane_mut().end_live_trigger(note) {
                    transport.resume_loop_wrap();
                }
            }
            None => {}
        }
    }
}
