//! The `EventHandlers` chokepoint for raw MIDI-IN while the arranger
//! performance lane is armed: a `NoteOn` on a mapped key jumps the transport to
//! the start of that key (`key_tick`, a bar, half a bar or a beat a key) and
//! joins the lane's run, which `/` keeps (`520-performance-capture.md`
//! decision 8). A `NoteOff` does nothing: releasing the key doesn't stop
//! playback, so a performer stops on their own terms, and no loop wrap waits
//! on it (the loop doesn't wrap while the lane is selected, decision 7).
//! Lives here rather than in `Sequencer` because it needs `&mut Transport`.
//! See `110-performance-lane.md`.

use undo::Record;

use crate::core::sequencer::{SetPerformancePadEdit, UndoStep};
use crate::models::{
    event::{Event, EventType},
    performance_lane::{key_index_from_note, key_tick, length_label},
};

use super::*;

/// The footer's line for `/` on the lane with nothing in the run.
const NOTHING_TO_KEEP: &str = "Nothing played on the lane to keep";

impl EventHandlers {
    /// Chokepoint for raw MIDI-IN while the performance lane is armed
    /// (selected) — routes a `NoteOn` into a transport jump instead of normal
    /// note capture. This needs `&mut Transport`, which `Sequencer` doesn't
    /// have, so it lives here (called directly from the sequencer thread's
    /// `midi_in_rx` arm) rather than inside `Sequencer::handle_midi_input`.
    pub(crate) fn handle_performance_lane_midi_input(
        &self,
        sequencer: &mut Sequencer,
        transport: &mut Transport,
        message: &[u8],
    ) {
        let event = Event::from_midi(message);
        if event.event_type() != Some(EventType::NoteOn) {
            return;
        }
        let Some(note) = event.note_number() else {
            return;
        };
        let Some(key_index) = key_index_from_note(note) else {
            return;
        };
        let target_tick = key_tick(
            sequencer.meter(),
            sequencer.performance_lane_key_length(),
            key_index,
        );
        let started = !transport.is_running();
        sequencer.performance_lane_mut().press(
            note,
            event.velocity().unwrap_or(100),
            target_tick,
            started,
        );
        if started {
            transport.start();
        }
        transport.jump_and_chase(target_tick);
        // The trigger came from a physical MIDI key, not a window input
        // event, so nothing else wakes the reactive UI up to notice `running`
        // flipped true and start animating.
        self.request_repaint();
    }

    /// `/` on the armed lane: the run so far, ending now or where playback
    /// stopped, goes to the selected pad as a performance, replacing what it
    /// held, one undo step (`520-performance-capture.md` decisions 6 and 8).
    /// The footer names it, or says there was nothing to keep: the run is
    /// empty, or playback moved since (a ruler click, `Space`) and ended it.
    pub(super) fn capture_performance_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
    ) {
        let Some(clip) = sequencer.performance_lane_mut().take_run() else {
            self.send_status_ui_event(NOTHING_TO_KEEP.to_owned());
            return;
        };
        let pad = sequencer.selected_pad();
        let length = length_label(clip.region_length(), sequencer.meter());
        let edit = SetPerformancePadEdit::new(sequencer, pad, Some(clip));
        if self.record_edit(sequencer, undo_record, edit) {
            self.send_status_ui_event(format!("Captured {length} to pad {}", pad + 1));
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

    use std::time::Instant;

    use undo::Record;

    use crate::core::config::PERFORMANCE_LANE_BASE_NOTE;
    use crate::core::event_handlers::test_harness::{Harness, harness, run_command, statuses};
    use crate::core::sequencer::test_support::clip_notes;
    use crate::core::sequencer::{SequencerCommand, UndoStep};
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
            h.performance_lane_armed.clone(),
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

    /// The armed lane, a transport beside it and an undo record: what `/`
    /// on the lane works against.
    fn armed() -> (Harness, Transport, Record<UndoStep>) {
        let mut h = harness();
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        let transport = transport(&h);
        (h, transport, Record::new())
    }

    /// Presses lane key `key_index` (a beat a key).
    fn press(h: &mut Harness, transport: &mut Transport, key_index: u8) {
        let note = PERFORMANCE_LANE_BASE_NOTE + key_index;
        h.handlers.handle_performance_lane_midi_input(
            &mut h.sequencer,
            transport,
            &[0x90, note, 100],
        );
    }

    /// Plays `ticks` ticks, as the clock's would.
    fn play(h: &mut Harness, transport: &mut Transport, ticks: i32) {
        for _ in 0..ticks {
            h.handlers
                .play_tick(Instant::now(), &mut h.sequencer, transport);
        }
    }

    /// `/`.
    fn slash(h: &mut Harness, record: &mut Record<UndoStep>) {
        run_command(h, record, SequencerCommand::Commit { frame: None });
    }

    /// The `(tick, note, length)` of each note on pad `pad`.
    fn pad_notes(h: &Harness, pad: usize) -> Option<Vec<(i32, u8, i32)>> {
        h.sequencer.performance_pads().get(pad).map(clip_notes)
    }

    #[test]
    fn slash_on_the_armed_lane_keeps_the_run_on_the_selected_pad() {
        let (mut h, mut transport, mut record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 200);
        slash(&mut h, &mut record);

        assert_eq!(
            pad_notes(&h, 0),
            Some(vec![
                (0, PERFORMANCE_LANE_BASE_NOTE + 4, 300),
                (300, 60, 200)
            ])
        );
        assert!(
            h.sequencer.tracks().iter().all(|t| t.clips().is_empty()),
            "no clip on a track"
        );
        assert_eq!(statuses(&h), ["Captured under a beat to pad 1"]);

        record.undo(&mut h.sequencer);
        assert_eq!(pad_notes(&h, 0), None, "undo empties the pad");
    }

    #[test]
    fn a_capture_goes_to_the_selected_pad_and_replaces_it() {
        let (mut h, mut transport, mut record) = armed();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(5),
        );
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 100);
        slash(&mut h, &mut record);
        press(&mut h, &mut transport, 2);
        play(&mut h, &mut transport, 50);
        slash(&mut h, &mut record);

        assert_eq!(pad_notes(&h, 5), Some(vec![(0, 62, 50)]));
        record.undo(&mut h.sequencer);
        assert_eq!(
            pad_notes(&h, 5),
            Some(vec![(0, 60, 100)]),
            "the old one back"
        );
    }

    #[test]
    fn slash_with_nothing_in_the_run_does_nothing() {
        let (mut h, _transport, mut record) = armed();
        slash(&mut h, &mut record);
        assert!(record.is_empty());
        assert_eq!(pad_notes(&h, 0), None);
        assert_eq!(statuses(&h), [super::NOTHING_TO_KEEP]);
    }

    #[test]
    fn leaving_the_lane_drops_the_run() {
        let (mut h, mut transport, mut record) = armed();
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 100);
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        slash(&mut h, &mut record);
        assert!(record.is_empty());
    }

    #[test]
    fn delete_on_the_selected_pad_empties_it() {
        let (mut h, mut transport, mut record) = armed();
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 100);
        slash(&mut h, &mut record);
        run_command(&mut h, &mut record, SequencerCommand::ClearPerformancePad);
        assert_eq!(pad_notes(&h, 0), None);
        record.undo(&mut h.sequencer);
        assert!(pad_notes(&h, 0).is_some());
    }

    #[test]
    fn playback_never_wraps_while_the_lane_is_selected() {
        let (mut h, mut transport, _record) = armed();
        // The loop is 0..3840 (the harness's), so beat 4 is inside it.
        press(&mut h, &mut transport, 3);
        play(&mut h, &mut transport, 1000);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 3 * 960 + 1000);

        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        h.playback_tick.store(3839, Ordering::Relaxed);
        play(&mut h, &mut transport, 1);
        assert_eq!(
            h.playback_tick.load(Ordering::Relaxed),
            0,
            "a track: it wraps"
        );
    }
}
