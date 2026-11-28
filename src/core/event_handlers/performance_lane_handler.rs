//! The `EventHandlers` chokepoint for raw MIDI-IN while the arranger
//! performance lane is armed: a `NoteOn` jumps the transport to its key
//! (`key_tick`, a bar, half a bar or a beat a key), in the player's hand
//! (`Hand`, placed by octave around the arranger cursor, the keys repeating
//! along the timeline, `560-performance-lane-reach.md`), and the keyboard's
//! previous/next buttons move that hand an octave
//! (`570-lane-octave-buttons.md` § 3). A press while running off a beat
//! waits for the next one and lands on its key's start, so the beat never
//! breaks (`land_due_jump`, `610-launch-quantize.md`), and joins the lane's run
//! (`archive/520-performance-capture.md` decision 2), which `/` renders into clips
//! at the arranger cursor (decision 1). A `NoteOff` does
//! nothing: releasing the key doesn't stop playback, so a performer stops on
//! their own terms, and no loop wrap waits on it (the loop doesn't wrap while
//! the lane is selected, decision 5).
//! Lives here rather than in `Sequencer` because it needs `&mut Transport`.
//! See `110-performance-lane.md`.

use undo::Record;

use crate::core::agent::{format_position, length_label};
use crate::core::midi::message::lane_octave;
use crate::core::sequencer::{DuplicateTimeEdit, UndoStep};
use crate::models::{
    event::{Event, EventType},
    performance_lane::{is_on_beat, key_tick},
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
    /// note capture, and a press of the keyboard's previous/next button
    /// (`lane_octave`) into an octave move of the hand, which wakes the UI
    /// so the lit hand moves before any note is played (`570` § 4). This needs `&mut Transport`, which `Sequencer` doesn't
    /// have, so it lives here (called directly from the sequencer thread's
    /// `midi_in_rx` arm) rather than inside `Sequencer::handle_midi_input`.
    pub(crate) fn handle_performance_lane_midi_input(
        &self,
        sequencer: &mut Sequencer,
        transport: &mut Transport,
        message: &[u8],
    ) {
        if let Some(octaves) = lane_octave(message) {
            if sequencer.move_performance_lane_hand(octaves) {
                self.request_repaint();
            }
            return;
        }
        let event = Event::from_midi(message);
        if event.event_type() != Some(EventType::NoteOn) {
            return;
        }
        let Some(note) = event.note_number() else {
            return;
        };
        let meter = sequencer.meter();
        let started = !transport.is_running();
        // The note's key in the hand: placed around the cursor, moved by the
        // buttons, which arrive in order with the notes, so a press lands
        // where the lane lights it, and the playhead moving never changes
        // what a key means. A note so far below the hand (the keyboard's own
        // octave shifted down) that its key is before bar 1 does nothing.
        let length = sequencer.performance_lane_key_length();
        let key_index = sequencer.performance_lane_hand().key_index(note);
        if key_index < 0 {
            return;
        }
        let target_tick = key_tick(meter, length, key_index);
        // Running off a beat, the press waits for the next one, which the
        // tick pump plays as the key's start (`land_due_jump`). On a beat it
        // jumps now, and stopped there is no beat to keep.
        if !started && !is_on_beat(meter, sequencer.playback_tick()) {
            sequencer.performance_lane_mut().wait_to_press(target_tick);
            return;
        }
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

    /// The jump a lane press or a marker recall left waiting for the beat
    /// (`610-launch-quantize.md`), landed if playback is about to play one:
    /// that tick plays the target instead. Made **synchronously** in the
    /// tick pump, before the tick reaches the sequencer, as a loop wrap is
    /// (`reanchor_playback`): through the transport's events it would land
    /// a tick or more late. The sustain lift, the clock alignment and the
    /// note chase are a jump's, as `Transport::jump_and_chase` gives them.
    pub(super) fn land_due_jump(&self, sequencer: &mut Sequencer, transport: &mut Transport) {
        let (meter, tick) = (sequencer.meter(), sequencer.playback_tick());
        let Some(target) = sequencer.performance_lane_mut().land_due_jump(meter, tick) else {
            return;
        };
        transport.land_jump(target);
        self.reanchor_playback(target, sequencer, transport);
        if sequencer.is_running() {
            sequencer.chase_notes(target);
        }
    }

    /// `/` on the armed lane (`archive/520-performance-capture.md` decision 1): the
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

    use crate::core::event_handlers::test_harness::{
        Harness, harness, muted_metronome, run_command, statuses, transport,
    };
    use crate::core::sequencer::test_support::{clip_at, clip_notes, clip_with_marker_notes, rect};
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::transport::{Transport, TransportCommand};
    use crate::models::performance_lane::KeyLength;

    /// Where pressing key `note` (the hand's note for it, from a cursor at
    /// bar 1) with the lane at `length` (and `meter`) takes playback.
    fn jump(length: KeyLength, meter: Meter, note: u8) -> i32 {
        let mut h = harness();
        let mut transport = transport(&h);
        h.sequencer.set_meter(meter);
        h.sequencer.set_performance_lane_key_length(length);
        press(&mut h, &mut transport, i32::from(note));
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

    /// A note-on of `note` on the armed lane, as the input thread delivers
    /// it.
    fn press_note(h: &mut Harness, transport: &mut Transport, note: u8) {
        h.handlers.handle_performance_lane_midi_input(
            &mut h.sequencer,
            transport,
            &[0x90, note, 100],
        );
    }

    /// A press of the note that lands on key `key_index` in the hand as it
    /// is placed now (`570-lane-octave-buttons.md`): the tests count in
    /// keys, wherever the cursor puts the hand.
    fn press(h: &mut Harness, transport: &mut Transport, key_index: i32) {
        let note = key_index - h.sequencer.performance_lane_hand().key_index(0);
        press_note(h, transport, u8::try_from(note).expect("a MIDI note"));
    }

    /// A press of the keyboard's previous (`cc` 28) or next (29) button.
    fn button(h: &mut Harness, transport: &mut Transport, cc: u8, value: u8) {
        h.handlers.handle_performance_lane_midi_input(
            &mut h.sequencer,
            transport,
            &[0xB0, cc, value],
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
    /// at `cursor`, after a run of E-2 (bar 2) then C-2 (bar 1) for 200
    /// ticks, a beat a key. C-2 is pressed 300 ticks into a beat, so it waits
    /// for the next one and E-2 plays its whole beat (`610-launch-quantize.md`).
    fn played(cursor: i32) -> (Harness, Transport, Record<UndoStep>) {
        let (mut h, mut transport, record) = armed();
        h.sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 7680));
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(3840, 3840));
        h.sequencer.set_cursor_tick(cursor);
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 660 + 200);
        statuses(&h);
        (h, transport, record)
    }

    /// What the run in [`played`] renders on each track, inserted at 7680.
    const RENDER_0: [(i32, i32, i32); 2] = [(7680, 8640, 3840), (8640, 8840, 0)];
    const RENDER_1: [(i32, i32, i32); 1] = [(7680, 8640, 0)];

    #[test]
    fn slash_on_the_armed_lane_inserts_what_played_at_the_cursor() {
        let (mut h, _transport, mut record) = played(7680);
        slash(&mut h, &mut record);

        assert_eq!(
            track_clips(&h, 0),
            [[(0, 7680, 0)].as_slice(), &RENDER_0].concat(),
            "bar 2's first beat, then bar 1 from its start, on the track they came from"
        );
        assert_eq!(
            track_clips(&h, 1),
            [[(3840, 7680, 0)].as_slice(), &RENDER_1].concat(),
            "bar 2 only: nothing there in bar 1"
        );
        assert_eq!(record.len(), 1, "one undo step");
        assert_eq!(statuses(&h), ["Captured 1 beat at bar 3"]);

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
            [(0, 960, 0), (3840 + 1160, 7680 + 1160, 0)],
            "the render at 0, bar 2 pushed along by its 1160 ticks"
        );
    }

    #[test]
    fn a_cursor_inside_a_clip_splits_it_and_pushes_its_second_half() {
        let (mut h, _transport, mut record) = played(5000);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 1),
            [(3840, 5000, 0), (5000, 5960, 0), (6160, 8840, 1160)]
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
        // E-2 at a beat a key: bar 2.
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetPerformanceLaneKeyLength(KeyLength::Bar),
        );
        // C♯-2 at a bar a key: bar 2 again, from its start on the next beat.
        press(&mut h, &mut transport, 1);
        play(&mut h, &mut transport, 660 + 200);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 0),
            [(0, 7680, 0), (7680, 8640, 3840), (8640, 8840, 3840)],
            "both from bar 2, as heard; E-2 at a bar a key would be bar 5"
        );
    }

    /// Where playback is after pressing key `key_index` (a beat a key) with
    /// playback about to play `at`, running.
    fn jump_while_running(at: i32, key_index: u8) -> i32 {
        let (mut h, mut transport, _record) = armed();
        transport.start();
        h.playback_tick.store(at, Ordering::Relaxed);
        press(&mut h, &mut transport, i32::from(key_index));
        h.playback_tick.load(Ordering::Relaxed)
    }

    /// `610-launch-quantize.md`: running, a press on a beat jumps at once
    /// to its key's start; off one it waits, and the next beat's tick plays
    /// the key's start, so the key always starts from the same place and the
    /// beat never breaks.
    #[test]
    fn a_press_while_running_waits_for_the_next_beat() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        assert_eq!(jump_while_running(2 * beat, 4), 4 * beat, "on the beat");
        for at in [2 * beat + 300, 2 * beat - 200, beat - 100, beat + 1] {
            let (mut h, mut transport, _record) = armed();
            transport.start();
            h.playback_tick.store(at, Ordering::Relaxed);
            press(&mut h, &mut transport, 4);
            let playback = |h: &Harness| h.playback_tick.load(Ordering::Relaxed);
            assert_eq!(playback(&h), at, "{at}: waits");
            let to_beat = beat - at.rem_euclid(beat);
            play(&mut h, &mut transport, to_beat);
            assert_eq!(playback(&h), at + to_beat, "{at}: about to play the beat");
            play(&mut h, &mut transport, 1);
            assert_eq!(playback(&h), 4 * beat + 1, "{at}: the beat played the key");
        }
    }

    /// The latest press wins, and a release doesn't cancel it.
    #[test]
    fn a_second_press_while_one_waits_replaces_it() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let (mut h, mut transport, _record) = armed();
        transport.start();
        h.playback_tick.store(beat + 300, Ordering::Relaxed);
        press(&mut h, &mut transport, 4);
        press(&mut h, &mut transport, 8);
        let note = 8 - h.sequencer.performance_lane_hand().key_index(0);
        h.handlers.handle_performance_lane_midi_input(
            &mut h.sequencer,
            &mut transport,
            &[0x80, u8::try_from(note).unwrap(), 0],
        );
        play(&mut h, &mut transport, beat - 300 + 1);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 8 * beat + 1);
    }

    /// A ruler click (`PlayFromTick`) drops a press waiting for the beat;
    /// so does `Space` stopping, and disarming.
    #[test]
    fn moving_playback_or_leaving_the_lane_drops_a_waiting_press() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let waiting = || {
            let (mut h, mut transport, record) = armed();
            transport.start();
            h.playback_tick.store(beat + 300, Ordering::Relaxed);
            press(&mut h, &mut transport, 8);
            (h, transport, record)
        };

        let (mut h, mut transport, _record) = waiting();
        h.handlers.handle_transport_command(
            &TransportCommand::PlayFromTick { tick: 5000 },
            &mut transport,
            &mut h.sequencer,
            &muted_metronome(),
        );
        h.playback_tick.store(5000, Ordering::Relaxed);
        play(&mut h, &mut transport, 2 * beat);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 5000 + 2 * beat);

        let (mut h, mut transport, _record) = waiting();
        space(&mut h, &mut transport);
        assert!(!transport.is_running(), "no run: stops at once");
        transport.start();
        play(&mut h, &mut transport, beat);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 2 * beat + 300);

        let (mut h, mut transport, _record) = waiting();
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        play(&mut h, &mut transport, beat);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 2 * beat + 300);
    }

    /// Where note `note` lands, pressed with the hand as it is now.
    fn lands(h: &mut Harness, transport: &mut Transport, note: u8) -> i32 {
        press_note(h, transport, note);
        h.playback_tick.load(Ordering::Relaxed)
    }

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// `570` § 2: running, a press lands in the hand placed around the
    /// cursor, wherever the playhead is, keeping the beat far along too. 49
    /// keys at a beat a key: from a cursor at bar 73, C3 (note 60) is on it.
    #[test]
    fn a_press_while_running_lands_in_the_hand_around_the_cursor() {
        let from = |cursor: i32, playback: i32, note: u8| {
            let (mut h, mut transport, _record) = armed();
            h.sequencer.set_cursor_tick(cursor);
            transport.start();
            h.playback_tick.store(playback, Ordering::Relaxed);
            lands(&mut h, &mut transport, note)
        };
        assert_eq!(from(72 * BAR, 2 * BAR, 60), 72 * BAR, "playhead at bar 3");
        assert_eq!(from(72 * BAR, 90 * BAR, 60), 72 * BAR, "playhead at bar 91");
        assert_eq!(
            from(72 * BAR, 5 * BAR + 960, 64),
            72 * BAR + BAR,
            "E3 on a beat: a bar on from C3"
        );
    }

    /// A run up the keys lands a beat apart all the way, one stretch: from
    /// bar 59½ the octave placing C3 nearest is a tie, which goes ahead, so
    /// C3 is on bar 61 and C1 on bar 55.
    #[test]
    fn a_run_up_the_keys_lands_unbroken() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let (mut h, mut transport, _record) = armed();
        h.sequencer.set_cursor_tick(58 * BAR + 2 * beat);
        let landed: Vec<i32> = (36..=84)
            .map(|note| lands(&mut h, &mut transport, note))
            .collect();
        assert_eq!(landed[0], 54 * BAR, "C1: bar 55");
        assert_eq!(landed[24], 60 * BAR, "C3: bar 61");
        for pair in landed.windows(2) {
            assert_eq!(pair[1] - pair[0], beat);
        }
    }

    /// From a cursor at bar 1 the hand's lowest key, C1 at home, is bar 1:
    /// never before it.
    #[test]
    fn the_hands_lowest_key_is_bar_one_from_there() {
        let (mut h, mut transport, _record) = armed();
        assert_eq!(lands(&mut h, &mut transport, 36), 0);
    }

    /// A note so far below the hand that its key would be before bar 1 (the
    /// keyboard's own octave shifted down) does nothing.
    #[test]
    fn a_note_before_bar_one_does_nothing() {
        let (mut h, mut transport, _record) = armed();
        press_note(&mut h, &mut transport, 30);
        assert!(!transport.is_running());
    }

    /// Stopped, the hand around the cursor, wherever playback was parked;
    /// moving the cursor back gives the same mapping again.
    #[test]
    fn the_press_that_starts_the_transport_lands_in_the_hand_around_the_cursor() {
        let from_cursor = |cursor: i32| {
            let (mut h, mut transport, _record) = armed();
            h.sequencer.set_cursor_tick(cursor);
            lands(&mut h, &mut transport, 60)
        };
        assert_eq!(from_cursor(9 * BAR), 9 * BAR);
        assert_eq!(from_cursor(28 * BAR), 27 * BAR, "the octave nearest");
        assert_eq!(
            from_cursor(9 * BAR),
            9 * BAR,
            "the same key, the same place"
        );
    }

    /// `570` § 3: the next button moves the hand an octave up, the previous
    /// one back, on the press; a note straight after lands in the moved
    /// hand. At a beat a key in 4/4 an octave is 3 bars.
    #[test]
    fn the_buttons_move_the_hand_an_octave() {
        let (mut h, mut transport, _record) = armed();
        h.sequencer.set_cursor_tick(72 * BAR);
        let home = h.sequencer.performance_lane_hand();
        button(&mut h, &mut transport, 29, 127);
        button(&mut h, &mut transport, 29, 0);
        assert_eq!(
            h.sequencer.performance_lane_hand(),
            home.moved(1).unwrap(),
            "the release does nothing"
        );
        assert!(!transport.is_running(), "a button starts nothing");
        assert_eq!(lands(&mut h, &mut transport, 60), 75 * BAR);
        button(&mut h, &mut transport, 28, 127);
        button(&mut h, &mut transport, 28, 127);
        assert_eq!(lands(&mut h, &mut transport, 60), 69 * BAR);
    }

    /// An octave down that would put the hand's lowest key before bar 1
    /// does nothing.
    #[test]
    fn the_previous_button_does_nothing_at_bar_one() {
        let (mut h, mut transport, _record) = armed();
        let home = h.sequencer.performance_lane_hand();
        button(&mut h, &mut transport, 28, 127);
        assert_eq!(h.sequencer.performance_lane_hand(), home);
        assert_eq!(lands(&mut h, &mut transport, 36), 0);
    }

    /// `570` § 2: any cursor move places the hand around the cursor again,
    /// undoing the buttons, a click on the same spot too; so do the key
    /// length and selecting the lane.
    #[test]
    fn a_cursor_move_after_a_button_places_the_hand_afresh() {
        let (mut h, mut transport, _record) = armed();
        let at = |h: &mut Harness| h.sequencer.performance_lane_hand();
        h.sequencer.set_cursor_tick(72 * BAR);
        let placed = at(&mut h);
        let up = |h: &mut Harness, transport: &mut Transport| {
            button(h, transport, 29, 127);
            assert_ne!(h.sequencer.performance_lane_hand(), placed);
        };

        up(&mut h, &mut transport);
        h.handlers.handle_transport_command(
            &TransportCommand::SetCursorAndSelectClip { tick: 72 * BAR },
            &mut transport,
            &mut h.sequencer,
            &muted_metronome(),
        );
        assert_eq!(at(&mut h), placed, "a click on the cursor's own spot");

        up(&mut h, &mut transport);
        h.handlers.handle_transport_command(
            &TransportCommand::MoveCursor { ticks: BAR },
            &mut transport,
            &mut h.sequencer,
            &muted_metronome(),
        );
        assert_eq!(at(&mut h), placed, "a bar on: the same octave");

        up(&mut h, &mut transport);
        h.sequencer.set_performance_lane_key_length(KeyLength::Beat);
        assert_eq!(at(&mut h), placed, "the key length");

        up(&mut h, &mut transport);
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        assert_eq!(at(&mut h), placed, "selecting the lane");
    }

    /// `Space`.
    fn space(h: &mut Harness, transport: &mut Transport) {
        h.handlers.handle_transport_command(
            &TransportCommand::TogglePlayback,
            transport,
            &mut h.sequencer,
            &muted_metronome(),
        );
    }

    /// `archive/550-phase-jumps.md` § The stop on the beat: `Space` on the armed
    /// lane, off a beat, plays on to the next beat and stops there, so the
    /// run ends on a whole beat.
    #[test]
    fn space_on_the_armed_lane_stops_on_the_next_beat() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let (mut h, mut transport, _record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, beat + 300);
        space(&mut h, &mut transport);
        assert!(transport.is_running(), "waits for the beat");
        play(&mut h, &mut transport, beat - 301);
        assert!(transport.is_running(), "a tick short of it");
        play(&mut h, &mut transport, 1);
        assert!(!transport.is_running());
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 6 * beat);
        let take = h.sequencer.performance_lane_mut().take_run();
        assert_eq!(
            take.map(|take| take.length),
            Some(2 * beat),
            "two whole beats"
        );
    }

    #[test]
    fn space_on_a_beat_stops_at_once() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let (mut h, mut transport, _record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, beat);
        space(&mut h, &mut transport);
        assert!(!transport.is_running());
    }

    #[test]
    fn a_second_space_while_the_stop_waits_stops_at_once() {
        let (mut h, mut transport, _record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        space(&mut h, &mut transport);
        space(&mut h, &mut transport);
        assert!(!transport.is_running());
    }

    #[test]
    fn a_press_while_the_stop_waits_keeps_playing() {
        let beat = Meter::FOUR_FOUR.beat_ticks();
        let (mut h, mut transport, _record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        space(&mut h, &mut transport);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 2 * beat);
        assert!(transport.is_running());
    }

    /// With the lane selected but no key pressed, `Space` plays the
    /// arrangement and stops where it is pressed.
    #[test]
    fn space_on_the_lane_with_no_press_stops_at_once() {
        let (mut h, mut transport, _record) = armed();
        space(&mut h, &mut transport);
        assert!(transport.is_running());
        play(&mut h, &mut transport, 300);
        space(&mut h, &mut transport);
        assert!(!transport.is_running());
    }

    /// Unarmed, `Space` stops where it is pressed, as ever.
    #[test]
    fn space_off_the_lane_stops_at_once() {
        let mut h = harness();
        let mut transport = transport(&h);
        space(&mut h, &mut transport);
        assert!(transport.is_running(), "the first press starts");
        play(&mut h, &mut transport, 300);
        space(&mut h, &mut transport);
        assert!(!transport.is_running());
    }

    /// The press that starts the transport has no beat to keep: it jumps
    /// to its key's start, wherever playback was parked.
    #[test]
    fn the_press_that_starts_the_transport_jumps_to_its_key() {
        let (mut h, mut transport, _record) = armed();
        h.playback_tick
            .store(Meter::FOUR_FOUR.beat_ticks() + 300, Ordering::Relaxed);
        press(&mut h, &mut transport, 4);
        assert!(transport.is_running());
        assert_eq!(
            h.playback_tick.load(Ordering::Relaxed),
            4 * Meter::FOUR_FOUR.beat_ticks()
        );
    }

    #[test]
    fn the_footer_names_a_cursor_off_a_bar_line_as_the_ruler_does() {
        let (mut h, _transport, mut record) = played(4800);
        slash(&mut h, &mut record);
        assert_eq!(statuses(&h), ["Captured 1 beat at 2+1"]);
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
        play(&mut h, &mut transport, 660 + 200);
        slash(&mut h, &mut record);
        assert_eq!(
            track_clips(&h, 1),
            [
                (0, 3840, 0),
                (3840, 4800, 0),
                (3840 * 4 + 1160, 3840 * 5 + 1160, 0)
            ],
            "bar 1's first beat, then 200 ticks of silence, inserted whole"
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
