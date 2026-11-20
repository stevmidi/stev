//! The `EventHandlers` chokepoint for raw MIDI-IN while the arranger
//! performance lane is armed: a `NoteOn` on a mapped key jumps the transport to
//! the start of that key (`key_tick`, a bar, half a bar or a beat a key) and
//! joins the lane's run (`520-performance-capture.md` decision 2), which `/`
//! renders into clips at the arranger cursor (decision 1). A `NoteOff` does
//! nothing: releasing the key doesn't stop playback, so a performer stops on
//! their own terms, and no loop wrap waits on it (the loop doesn't wrap while
//! the lane is selected, decision 5).
//! Lives here rather than in `Sequencer` because it needs `&mut Transport`.
//! See `110-performance-lane.md`.

use undo::Record;

use crate::core::agent::{format_position, length_label};
use crate::core::sequencer::{DuplicateTimeEdit, UndoStep};
use crate::models::{
    event::{Event, EventType},
    performance_lane::{key_index_from_note, key_tick},
};

use super::*;

/// The footer's line for `/` on the lane with nothing in the run.
const NOTHING_TO_KEEP: &str = "Nothing played on the lane to keep";

/// The footer's line for `/` on the lane when the run played only gaps and
/// the arrangement's end.
const NOTHING_PLAYED: &str = "Nothing to keep: the run played no clips";

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
        sequencer.performance_lane_mut().press(target_tick, started);
        if started {
            transport.start();
        }
        transport.jump_and_chase(target_tick);
        // The trigger came from a physical MIDI key, not a window input
        // event, so nothing else wakes the reactive UI up to notice `running`
        // flipped true and start animating.
        self.request_repaint();
    }

    /// `/` on the armed lane (`520-performance-capture.md` decision 1): the
    /// run so far, ending now or where playback stopped, rendered from the
    /// arrangement as it is now and inserted at the arranger cursor, as
    /// Paste Time inserts, one undo step. Nothing is selected and the
    /// clipboard is untouched. The footer names what landed and where, or
    /// says there was nothing to keep: the run is empty (or playback moved
    /// since, a ruler click or `Space`, and ended it), or it played no clips.
    pub(super) fn capture_performance_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
    ) {
        let Some(run) = sequencer.performance_lane_mut().take_run() else {
            self.send_status_ui_event(NOTHING_TO_KEEP.to_owned());
            return;
        };
        let Some(render) = sequencer.render_performance(&run) else {
            self.send_status_ui_event(NOTHING_PLAYED.to_owned());
            return;
        };
        let meter = sequencer.meter();
        let length = length_label(render.span(), meter);
        let cursor = sequencer.cursor_tick();
        let at = match format_position(cursor, meter) {
            bar if cursor.rem_euclid(meter.bar_ticks()) == 0 => format!("bar {bar}"),
            position => position,
        };
        let edit = DuplicateTimeEdit::capturing(sequencer, &render);
        if self.record_edit(sequencer, undo_record, edit) {
            self.send_status_ui_event(format!("Captured {length} at {at}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use std::time::Instant;

    use undo::Record;

    use crate::core::config::PERFORMANCE_LANE_BASE_NOTE;
    use crate::core::event_handlers::test_harness::{
        Harness, harness, run_command, statuses, transport,
    };
    use crate::core::sequencer::test_support::{clip_at, clip_notes, clip_with_marker_notes, rect};
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::transport::Transport;
    use crate::models::performance_lane::KeyLength;

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
        // The tests count in beats, not the default half bar.
        h.sequencer.set_performance_lane_key_length(KeyLength::Beat);
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

    /// The `(start, end, window start)` of each clip on track `track`.
    fn track_clips(h: &Harness, track: usize) -> Vec<(i32, i32, i32)> {
        h.sequencer.tracks()[track]
            .clips()
            .iter()
            .map(|clip| (clip.start_tick(), clip.end_tick(), clip.region().start()))
            .collect()
    }

    /// The armed lane over an arrangement of a clip over bars 1–2 on track 0
    /// (a note every 500 ticks) and one over bar 2 on track 1, the cursor
    /// at `cursor`, after a run of E3 (bar 2) for 300 ticks then C3 (bar 1)
    /// for 200, a beat a key.
    fn played(cursor: i32) -> (Harness, Transport, Record<UndoStep>) {
        let (mut h, mut transport, record) = armed();
        h.sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 7680));
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(3840, 3840));
        h.sequencer.set_cursor_tick(cursor);
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 200);
        statuses(&h);
        (h, transport, record)
    }

    /// What the run in [`played`] renders on each track, inserted at 7680.
    const RENDER_0: [(i32, i32, i32); 2] = [(7680, 7980, 3840), (7980, 8180, 0)];
    const RENDER_1: [(i32, i32, i32); 1] = [(7680, 7980, 0)];

    #[test]
    fn slash_on_the_armed_lane_inserts_what_played_at_the_cursor() {
        let (mut h, _transport, mut record) = played(7680);
        slash(&mut h, &mut record);

        assert_eq!(
            track_clips(&h, 0),
            [[(0, 7680, 0)].as_slice(), &RENDER_0].concat(),
            "bar 2's start, then bar 1's, on the track they came from"
        );
        assert_eq!(
            track_clips(&h, 1),
            [[(3840, 7680, 0)].as_slice(), &RENDER_1].concat(),
            "bar 2 only: nothing there in bar 1"
        );
        assert_eq!(record.len(), 1, "one undo step");
        assert_eq!(statuses(&h), ["Captured under a beat at bar 3"]);

        // Nothing selected, the lane kept, the clipboard untouched.
        assert!(h.sequencer.is_performance_lane_armed());
        assert_eq!(h.sequencer.selected_track_index(), None);
        assert!(h.sequencer.selected_clip().is_none());
        assert!(h.sequencer.marquee().is_none());
        assert!(h.sequencer.clip_clipboard().is_none());
        assert_eq!(h.sequencer.cursor_tick(), 7680, "the cursor stays");
    }

    #[test]
    fn the_capture_pushes_what_is_after_the_cursor_along_by_the_runs_length() {
        let (mut h, _transport, mut record) = played(0);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 1),
            [(0, 300, 0), (3840 + 500, 7680 + 500, 0)],
            "the render at 0, bar 2 pushed along by its 500 ticks"
        );
    }

    #[test]
    fn a_cursor_inside_a_clip_splits_it_and_pushes_its_second_half() {
        let (mut h, _transport, mut record) = played(5000);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 1),
            [(3840, 5000, 0), (5000, 5300, 0), (5500, 8180, 1160)]
        );
    }

    /// The regression this design's revision is for: the render is the
    /// arrangement as it was at the `/`, so an edit after it changes nothing
    /// already inserted.
    #[test]
    fn an_edit_after_the_capture_leaves_the_inserted_clips_alone() {
        let (mut h, _transport, mut record) = played(7680);
        slash(&mut h, &mut record);
        let notes = |h: &Harness| -> Vec<_> {
            h.sequencer.tracks()[0]
                .clips()
                .iter()
                .map(clip_notes)
                .collect()
        };
        let before = notes(&h);

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::TransposeClips {
                rect: rect(0, 7680, 0, 0),
                semitones: 2,
            },
        );
        let after = notes(&h);
        assert_ne!(after[0], before[0], "the source moved");
        assert_eq!(after[1..], before[1..], "the render didn't");
        assert_eq!(track_clips(&h, 0)[1..], RENDER_0);
    }

    /// Regression: the render re-derived each key's jump from the key length
    /// at the `/`, so a length changed in the middle of a run rendered
    /// stretches nobody heard. Each press keeps where it jumped.
    #[test]
    fn a_key_length_changed_mid_run_renders_what_each_press_played() {
        let (mut h, mut transport, mut record) = armed();
        h.sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 7680));
        h.sequencer.set_cursor_tick(7680);
        // E3 at a beat a key: bar 2.
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetPerformanceLaneKeyLength(KeyLength::Bar),
        );
        // C♯3 at a bar a key: bar 2 again.
        press(&mut h, &mut transport, 1);
        play(&mut h, &mut transport, 200);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 0),
            [(0, 7680, 0), (7680, 7980, 3840), (7980, 8180, 3840)],
            "both from bar 2, as heard; E3 at a bar a key would be bar 5"
        );
    }

    #[test]
    fn the_footer_names_a_cursor_off_a_bar_line_as_the_ruler_does() {
        let (mut h, _transport, mut record) = played(4800);
        slash(&mut h, &mut record);
        assert_eq!(statuses(&h), ["Captured under a beat at 2+1"]);
    }

    #[test]
    fn a_stretch_past_the_arrangement_is_silence_that_still_takes_its_room() {
        let (mut h, mut transport, mut record) = armed();
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(0, 3840));
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(3840 * 4, 3840));
        h.sequencer.set_cursor_tick(3840);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 300);
        // Bar 9: nothing there.
        press(&mut h, &mut transport, 32);
        play(&mut h, &mut transport, 200);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 1),
            [
                (0, 3840, 0),
                (3840, 4140, 0),
                (3840 * 4 + 500, 3840 * 5 + 500, 0)
            ],
            "bar 1's start, then 200 ticks of silence, inserted whole"
        );
    }

    #[test]
    fn a_run_that_plays_no_clips_inserts_nothing_and_says_so() {
        let (mut h, mut transport, mut record) = armed();
        h.sequencer.tracks_mut()[0].add_clip(&clip_at(3840, 3840));
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 300);
        statuses(&h);
        slash(&mut h, &mut record);
        assert!(record.is_empty());
        assert_eq!(track_clips(&h, 0), [(3840, 7680, 0)]);
        assert_eq!(statuses(&h), [super::NOTHING_PLAYED]);
    }

    #[test]
    fn undo_closes_the_gap_redo_brings_it_back_and_the_run_is_spent() {
        let (mut h, _transport, mut record) = played(0);
        slash(&mut h, &mut record);
        let inserted = (track_clips(&h, 0), track_clips(&h, 1));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(track_clips(&h, 0), [(0, 7680, 0)]);
        assert_eq!(track_clips(&h, 1), [(3840, 7680, 0)]);

        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!((track_clips(&h, 0), track_clips(&h, 1)), inserted);

        // The undo doesn't give the run back: no presses since, nothing kept.
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        statuses(&h);
        slash(&mut h, &mut record);
        assert_eq!(track_clips(&h, 1), [(3840, 7680, 0)]);
        assert_eq!(statuses(&h), [super::NOTHING_TO_KEEP]);
    }

    #[test]
    fn slash_with_nothing_in_the_run_does_nothing() {
        let (mut h, _transport, mut record) = armed();
        slash(&mut h, &mut record);
        assert!(record.is_empty());
        assert_eq!(statuses(&h), [super::NOTHING_TO_KEEP]);
    }

    #[test]
    fn leaving_the_lane_drops_the_run() {
        let (mut h, mut transport, mut record) = played(7680);
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        play(&mut h, &mut transport, 1);
        slash(&mut h, &mut record);
        assert!(record.is_empty());
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
