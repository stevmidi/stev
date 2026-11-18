//! The `EventHandlers` chokepoint for raw MIDI-IN while the arranger
//! performance lane is armed: a `NoteOn` on a mapped key jumps the transport to
//! the start of that key (`key_tick`, a bar, half a bar or a beat a key) and
//! suspends loop-wrap; the `NoteOff` resumes loop-wrap but
//! deliberately leaves the transport running — releasing the key does not
//! stop playback, so a performer stops on their own terms. Lives here rather
//! than in `Sequencer` because it needs `&mut Transport`. Live-only. See
//! `110-performance-lane.md`.

use crate::models::{
    event::{Event, EventType},
    performance_lane::{key_index_from_note, key_tick},
};

use super::*;

impl EventHandlers {
    /// Chokepoint for raw MIDI-IN NoteOn/NoteOff while the performance lane
    /// is armed (selected) — routes triggers into transport jumps instead
    /// of normal note capture. This needs `&mut Transport`, which
    /// `Sequencer` doesn't have, so it lives here (called directly from the
    /// sequencer thread's `midi_in_rx` arm) rather than inside
    /// `Sequencer::handle_midi_input`.
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
                let Some(key_index) = key_index_from_note(note) else {
                    return;
                };

                sequencer.performance_lane_mut().begin_live_trigger(note);

                let target_tick = key_tick(
                    sequencer.meter(),
                    sequencer.performance_lane_key_length(),
                    key_index,
                );
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use crossbeam_channel::unbounded;

    use crate::core::config::PERFORMANCE_LANE_BASE_NOTE;
    use crate::core::event_handlers::test_harness::{Harness, harness};
    use crate::core::time::Meter;
    use crate::core::transport::Transport;
    use crate::models::performance_lane::KeyLength;

    /// A transport over `h`'s shared state, as the `"sequencer"` thread holds
    /// one beside the sequencer.
    fn transport(h: &Harness) -> Transport {
        Transport::new(
            unbounded().0,
            unbounded().0,
            h.playback_tick.clone(),
            h.cursor_tick.clone(),
            h.running.clone(),
            h.region.0.clone(),
            h.region.1.clone(),
            Arc::new(AtomicBool::new(true)),
        )
    }

    /// Where pressing key `key_index` with the lane at `length` (and `meter`)
    /// takes playback.
    fn jump(length: KeyLength, meter: Meter, key_index: u8) -> i32 {
        let mut h = harness();
        let mut transport = transport(&h);
        h.sequencer.set_meter(meter);
        h.sequencer.set_performance_lane_key_length(length);
        let note = PERFORMANCE_LANE_BASE_NOTE + key_index;
        h.handlers.handle_performance_lane_midi_input(
            &mut h.sequencer,
            &mut transport,
            &[0x90, note, 100],
        );
        assert!(transport.is_running(), "a key starts the transport");
        h.playback_tick.load(Ordering::Relaxed)
    }

    #[test]
    fn a_key_jumps_by_the_chosen_length() {
        let meter = Meter::FOUR_FOUR;
        assert_eq!(
            jump(KeyLength::Beat, meter, 2),
            2 * meter.beat_ticks(),
            "D3: beat 3"
        );
        assert_eq!(
            jump(KeyLength::HalfBar, meter, 2),
            meter.bars_to_ticks(1),
            "D3: bar 2"
        );
        assert_eq!(
            jump(KeyLength::Bar, meter, 2),
            meter.bars_to_ticks(2),
            "D3: bar 3"
        );
    }

    #[test]
    fn a_half_bar_key_jumps_a_beat_in_an_odd_meter() {
        let waltz = Meter::new(3, 4).unwrap();
        assert_eq!(jump(KeyLength::HalfBar, waltz, 2), 2 * waltz.beat_ticks());
    }
}
