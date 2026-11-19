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
    clip::Clip,
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
        // A live press takes over from the replay (`520-performance-capture.md`
        // decision 4) and begins a run of its own.
        sequencer.stop_performance_replay();
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

    /// `⌘C` on the selected pad: its performance and its render to the
    /// clipboard, a copy, not an edit (`520-performance-capture.md`
    /// decision 5). The footer names it, and says so when the performance
    /// plays nothing on the timeline (nothing for the arranger to paste).
    /// Nothing on an empty pad.
    pub(super) fn copy_performance_pad_workflow(&self, sequencer: &mut Sequencer) {
        let Some(length) = sequencer.copy_performance_pad_to_clipboard() else {
            return;
        };
        self.prime_os_clipboard();
        let pad = sequencer.selected_pad() + 1;
        let length = length_label(length, sequencer.meter());
        let plays = match sequencer.clip_clipboard() {
            Some(_) => "",
            None => ", which plays no clips",
        };
        self.send_status_ui_event(format!("Copied pad {pad} · {length}{plays}"));
    }

    /// `⌘V` on the selected pad: a copy of the copied performance replaces
    /// what it holds, lit or not, one undo step, a clip of its own so
    /// editing one pad never changes the other (decision 5). Nothing when
    /// the last copy was clips or notes.
    pub(super) fn paste_performance_pad_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
    ) {
        let Some(clip) = sequencer.performance_clipboard().map(Clip::detached_copy) else {
            return;
        };
        let edit = SetPerformancePadEdit::new(sequencer, sequencer.selected_pad(), Some(clip));
        self.record_edit(sequencer, undo_record, edit);
    }

    /// Selects pad `pad` (the view did already, on the click). With the
    /// lane selected the clip view follows it: the old performance's notes
    /// are deselected while it is still the one shown, then the new one is
    /// shown (`520-performance-capture.md` decisions 2 and 6).
    pub(super) fn select_performance_pad_workflow(&self, sequencer: &mut Sequencer, pad: usize) {
        if pad == sequencer.selected_pad() {
            return;
        }
        let shown = sequencer.is_performance_lane_armed();
        if shown {
            self.clear_event_selection_workflow(sequencer);
        }
        sequencer.select_pad(pad);
        if shown {
            self.show_performance_workflow(sequencer);
        }
    }

    /// Shows the clip view what it now shows with the lane selected: the
    /// selected pad's performance, none on an empty pad
    /// (`UiEvent::LeadClipChanged`), framed fresh, as a new lead clip is.
    pub(super) fn show_performance_workflow(&self, sequencer: &mut Sequencer) {
        self.send_lead_clip_changed_ui_event(sequencer.current_clip_view());
        self.publish_event_selection(sequencer);
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

    use rtrb::RingBuffer;
    use uuid::Uuid;

    use crate::core::config::PERFORMANCE_LANE_BASE_NOTE;
    use crate::core::metronome::Metronome;

    use crate::core::event_handlers::test_harness::{
        Harness, add_clip, harness, lead_changes, run_command, statuses,
    };
    use crate::core::sequencer::test_support::{clip_at, clip_notes};
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::transport::{Transport, TransportCommand};
    use crate::models::{clip::Clip, performance_lane::KeyLength};

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

    /// The armed lane with a performance captured on pad 1: E3 for 300
    /// ticks, then C3 for 200.
    fn captured() -> (Harness, Record<UndoStep>, Uuid) {
        let (mut h, mut transport, mut record) = armed();
        press(&mut h, &mut transport, 4);
        play(&mut h, &mut transport, 300);
        press(&mut h, &mut transport, 0);
        play(&mut h, &mut transport, 200);
        slash(&mut h, &mut record);
        let id = h.sequencer.performance_pads().get(0).map(Clip::id).unwrap();
        (h, record, id)
    }

    #[test]
    fn the_clip_view_follows_the_lane_and_its_pads() {
        let (mut h, mut record, id) = captured();
        assert_eq!(
            lead_changes(&h.ui_events),
            [None, Some(id)],
            "the lane selected on empty pad 1, then the capture onto it"
        );

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(1),
        );
        assert_eq!(lead_changes(&h.ui_events), [None], "an empty pad");
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(0),
        );
        assert_eq!(lead_changes(&h.ui_events), [Some(id)]);

        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        assert_eq!(
            lead_changes(&h.ui_events),
            [None],
            "a track with no clip at the cursor"
        );
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        assert_eq!(lead_changes(&h.ui_events), [Some(id)], "back on the lane");

        // Unarmed, a pad's selection shows nothing.
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        assert_eq!(lead_changes(&h.ui_events), [None]);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(1),
        );
        assert!(lead_changes(&h.ui_events).is_empty());
    }

    #[test]
    fn a_note_edit_on_a_performance_undoes_with_the_arranger_left_alone() {
        let (mut h, mut record, _) = captured();
        run_command(&mut h, &mut record, SequencerCommand::SelectAllEvents);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::TransposeSelectedEvents(2),
        );
        assert_eq!(
            pad_notes(&h, 0),
            Some(vec![(0, 66, 300), (300, 62, 200)]),
            "the pad's performance, nothing on a track"
        );
        assert!(h.sequencer.tracks().iter().all(|t| t.clips().is_empty()));

        h.sequencer.set_cursor_tick(1920);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(pad_notes(&h, 0), Some(vec![(0, 64, 300), (300, 60, 200)]));
        assert_eq!(
            h.sequencer.cursor_tick(),
            1920,
            "the pads aren't the arrangement: no view put back"
        );
    }

    #[test]
    fn only_a_performances_notes_paste_into_a_performance() {
        let (mut h, mut record, _) = captured();
        // A track's note copied...
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .select_clip_workflow(&mut h.sequencer, Some((0, clip_id)));
        run_command(&mut h, &mut record, SequencerCommand::SelectAllEvents);
        run_command(&mut h, &mut record, SequencerCommand::CopyNotes);
        // ...doesn't paste into the performance.
        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        let before = pad_notes(&h, 0);
        run_command(&mut h, &mut record, SequencerCommand::PasteNotes);
        assert_eq!(pad_notes(&h, 0), before);

        // The performance's own notes do, at its cursor.
        run_command(&mut h, &mut record, SequencerCommand::SelectAllEvents);
        run_command(&mut h, &mut record, SequencerCommand::CopyNotes);
        h.sequencer.set_selected_clip_cursor_tick(250);
        run_command(&mut h, &mut record, SequencerCommand::PasteNotes);
        assert_ne!(pad_notes(&h, 0), before);
    }

    /// The `(start, end, window start)` of each clip on track `track`.
    fn track_clips(h: &Harness, track: usize) -> Vec<(i32, i32, i32)> {
        h.sequencer.tracks()[track]
            .clips()
            .iter()
            .map(|clip| (clip.start_tick(), clip.end_tick(), clip.region().start()))
            .collect()
    }

    /// The captured performance (E3, bar 2, for 300 ticks, then C3, bar 1,
    /// for 200) over an arrangement of a clip over bars 1–2 on track 0 and
    /// one bar 2 on track 1, copied from its pad.
    fn copied() -> (Harness, Record<UndoStep>) {
        let (mut h, mut record, _) = captured();
        h.sequencer.tracks_mut()[0].add_clip(&clip_at(0, 7680));
        h.sequencer.tracks_mut()[1].add_clip(&clip_at(3840, 3840));
        statuses(&h);
        run_command(&mut h, &mut record, SequencerCommand::CopyPerformancePad);
        (h, record)
    }

    #[test]
    fn a_pads_copy_pastes_what_it_plays_on_the_tracks_it_played_from() {
        let (mut h, mut record) = copied();
        assert_eq!(statuses(&h), ["Copied pad 1 · under a beat"]);
        assert_eq!(record.len(), 1, "the capture only: a copy isn't an edit");

        h.sequencer.set_cursor_tick(7680);
        run_command(&mut h, &mut record, SequencerCommand::PasteClips);
        assert_eq!(
            track_clips(&h, 0),
            [(0, 7680, 0), (7680, 7980, 3840), (7980, 8180, 0)],
            "bar 2's start, then bar 1's"
        );
        assert_eq!(
            track_clips(&h, 1),
            [(3840, 7680, 0), (7680, 7980, 0)],
            "bar 2 only: nothing there in bar 1"
        );

        record.undo(&mut h.sequencer);
        assert_eq!(track_clips(&h, 0), [(0, 7680, 0)]);
        assert!(
            h.sequencer.performance_pads().get(0).is_some(),
            "the pad keeps it"
        );
    }

    #[test]
    fn paste_time_inserts_the_performances_length() {
        let (mut h, mut record) = copied();
        h.sequencer.set_cursor_tick(7680);
        run_command(&mut h, &mut record, SequencerCommand::PasteTime);
        assert_eq!(
            track_clips(&h, 1),
            [(3840, 7680, 0), (7680, 7980, 0)],
            "nothing after the cursor to push"
        );

        record.undo(&mut h.sequencer);
        h.sequencer.set_cursor_tick(0);
        run_command(&mut h, &mut record, SequencerCommand::PasteTime);
        assert_eq!(
            track_clips(&h, 1),
            [(0, 300, 0), (3840 + 500, 7680 + 500, 0)],
            "the render at 0, bar 2 pushed along by its 500 ticks"
        );
    }

    #[test]
    fn a_copied_performance_pastes_onto_another_pad_as_a_clip_of_its_own() {
        let (mut h, mut record) = copied();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(3),
        );
        run_command(&mut h, &mut record, SequencerCommand::PastePerformancePad);
        assert_eq!(pad_notes(&h, 3), pad_notes(&h, 0));
        let ids = [0, 3].map(|pad| h.sequencer.performance_pads().get(pad).map(Clip::id));
        assert_ne!(ids[0], ids[1], "a clip of its own");

        record.undo(&mut h.sequencer);
        assert_eq!(pad_notes(&h, 3), None, "one undo step");

        // A clip copy since: no performance to paste.
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::CopyClips { start: 0, end: 960 },
        );
        run_command(&mut h, &mut record, SequencerCommand::PastePerformancePad);
        assert_eq!(pad_notes(&h, 3), None);
    }

    #[test]
    fn copying_an_empty_pad_keeps_the_clipboard() {
        let (mut h, mut record) = copied();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(2),
        );
        statuses(&h);
        run_command(&mut h, &mut record, SequencerCommand::CopyPerformancePad);
        assert!(statuses(&h).is_empty());
        assert!(h.sequencer.performance_clipboard().is_some());
        assert_eq!(
            h.sequencer.clip_clipboard().map(|copy| copy.span()),
            Some(500)
        );
    }

    /// Regression: `⌥Space` in the clip view played the arrangement from the
    /// performance's cursor taken as an arrangement tick. A performance has
    /// no place there: `⌥Space` replays it (`PlayPerformance`, routed by the
    /// input handler), and the clip cursor's play does nothing.
    #[test]
    fn option_space_on_a_performance_plays_nothing_from_the_arrangement() {
        let (mut h, mut record, _) = captured();
        h.transport_commands.try_iter().for_each(drop);
        run_command(&mut h, &mut record, SequencerCommand::PlayFromClipCursor);
        assert_eq!(h.transport_commands.try_iter().count(), 0);
    }

    /// `cmd` through the transport's handler, as the `"sequencer"` thread
    /// runs it.
    fn transport_command(h: &mut Harness, transport: &mut Transport, cmd: TransportCommand) {
        let metronome = Metronome::new(Arc::new(AtomicBool::new(true)), RingBuffer::new(1).0);
        h.handlers
            .handle_transport_command(&cmd, transport, &mut h.sequencer, &metronome);
    }

    /// The captured performance (E3, bar 2, for 300 ticks, then C3, bar 1,
    /// for 200) on the armed lane, stopped with playback at `parked`.
    fn ready_to_replay(parked: i32) -> (Harness, Transport) {
        let (mut h, _record, _) = captured();
        let mut transport = transport(&h);
        transport_command(&mut h, &mut transport, TransportCommand::Stop);
        h.playback_tick.store(parked, Ordering::Relaxed);
        (h, transport)
    }

    fn playback(h: &Harness) -> i32 {
        h.playback_tick.load(Ordering::Relaxed)
    }

    #[test]
    fn a_replay_plays_the_path_the_presses_did_and_loops() {
        let (mut h, mut transport) = ready_to_replay(7000);
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        assert!(transport.is_running(), "⌥Space starts the transport");

        play(&mut h, &mut transport, 1);
        assert_eq!(playback(&h), 3840 + 1, "E3: bar 2");
        play(&mut h, &mut transport, 299);
        assert_eq!(playback(&h), 3840 + 300);
        play(&mut h, &mut transport, 1);
        assert_eq!(playback(&h), 1, "C3: bar 1");
        play(&mut h, &mut transport, 199);
        play(&mut h, &mut transport, 1);
        assert_eq!(playback(&h), 3840 + 1, "round again at its length");
        assert!(
            h.sequencer.performance_pads().get(0).is_some(),
            "a replay isn't a capture"
        );
    }

    #[test]
    fn a_replay_starts_from_the_clip_cursor_in_the_performance() {
        let (mut h, mut transport) = ready_to_replay(0);
        h.sequencer.set_selected_clip_cursor_tick(400);
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        play(&mut h, &mut transport, 1);
        assert_eq!(playback(&h), 100 + 1, "100 ticks into C3");
    }

    #[test]
    fn stopping_ends_the_replay() {
        let (mut h, mut transport) = ready_to_replay(0);
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        play(&mut h, &mut transport, 10);
        transport_command(&mut h, &mut transport, TransportCommand::TogglePlayback);
        assert!(!transport.is_running());
        transport_command(&mut h, &mut transport, TransportCommand::TogglePlayback);
        let from = playback(&h);
        play(&mut h, &mut transport, 400);
        assert_eq!(playback(&h), from + 400, "the arrangement, straight on");
    }

    #[test]
    fn a_live_press_takes_over_from_the_replay() {
        let (mut h, mut transport) = ready_to_replay(0);
        let mut record = Record::new();
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        play(&mut h, &mut transport, 10);
        press(&mut h, &mut transport, 8);
        play(&mut h, &mut transport, 400);
        assert_eq!(playback(&h), 2 * 3840 + 400, "the press's key, straight on");

        // The press began a run of its own.
        slash(&mut h, &mut record);
        assert_eq!(pad_notes(&h, 0), Some(vec![(0, 68, 400)]));
    }

    #[test]
    fn leaving_the_lane_ends_the_replay_and_playback_runs_on() {
        let (mut h, mut transport) = ready_to_replay(0);
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        play(&mut h, &mut transport, 10);
        h.handlers.select_track_workflow(&mut h.sequencer, 0);
        play(&mut h, &mut transport, 300);
        assert_eq!(playback(&h), 3840 + 310);
        assert!(transport.is_running());
    }

    #[test]
    fn option_space_on_an_empty_pad_plays_nothing() {
        let (mut h, mut transport) = ready_to_replay(0);
        let mut record = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SelectPerformancePad(4),
        );
        transport_command(&mut h, &mut transport, TransportCommand::PlayPerformance);
        assert!(!transport.is_running());
        assert_eq!(
            statuses(&h).last().map(String::as_str),
            Some("Nothing on pad 5 to play")
        );
    }
}
