//! `EventHandlers` workflows for a clip appearing on / leaving a track through
//! `CommitClipEdit` — `/`'s `Commit` binding, and the undo/redo of it. See
//! `050-undo-redo.md`, `100-running-capture.md`.

use std::slice;

use undo::Record;

use crate::core::agent::span_label;
use crate::core::input_event::CaptureFrame;
use crate::core::sequencer::{
    CaptureFraming, CommitClipEdit, InsertCaptureEdit, PassRefusal, PassStep, SequencerEdit,
    UndoStep, ViewSnapshot, top_step,
};

use super::*;

impl EventHandlers {
    // --- Clip operations ---

    /// `/` (`SequencerCommand::Commit`): into the lead clip if there is one,
    /// otherwise a new clip — the last pass while running, the detected
    /// phrase while stopped. Stopped into the lead clip opens a take session
    /// instead, which inserts on Enter.
    ///
    /// The view's `frame`, resolved by [`Sequencer::capture_framing`]
    /// (`470-marquee-capture.md`), frames a new clip instead of the loop or
    /// punches in a running insert. It is refused with a footer line when it
    /// can't, otherwise the footer names the span captured or punched in. It
    /// is never cleared: the view keeps it.
    pub(super) fn commit_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        frame: Option<CaptureFrame>,
    ) {
        let framing = match sequencer.capture_framing(frame) {
            Ok(framing) => framing,
            Err(refusal) => {
                self.send_status_ui_event(refusal.message(sequencer.meter()));
                return;
            }
        };
        let into_lead_clip = sequencer.selected_clip_id().is_some();
        let running = sequencer.is_running();
        let edit = match (into_lead_clip, running) {
            (true, true) => InsertCaptureEdit::from_running_capture(sequencer, framing.punch_in())
                .map(SequencerEdit::from),
            (true, false) => {
                if self.view_state() == ViewState::Arranger {
                    sequencer.sync_selected_clip_cursor_with_arranger();
                }
                self.open_take_framing_workflow(sequencer);
                None
            }
            (false, true) => CommitClipEdit::from_running_capture(sequencer, framing.new_clip())
                .map(SequencerEdit::from),
            (false, false) => {
                #[cfg(debug_assertions)]
                sequencer.write_stopped_capture_fixture();
                CommitClipEdit::from_stopped_capture(sequencer, framing.new_clip())
                    .map(SequencerEdit::from)
            }
        };
        if !self.record_edit(sequencer, undo_record, edit) {
            return;
        }
        let meter = sequencer.meter();
        match framing {
            CaptureFraming::NewClip((start, end)) => {
                let span = span_label(start, end, meter);
                self.send_status_ui_event(format!("Captured {span} (marquee)"));
            }
            CaptureFraming::PunchIn {
                span: (start, end),
                ruler_origin,
            } => {
                let span = span_label(start - ruler_origin, end - ruler_origin, meter);
                self.send_status_ui_event(format!("Inserted {span} (marquee)"));
            }
            CaptureFraming::Unframed => {}
        }
        if !into_lead_clip && !running {
            // The project's first clip: loop over it.
            self.loop_sole_clip_workflow(sequencer, true);
        }
    }

    /// `,`/`.` (`SequencerCommand::StepPass`, `540-earlier-pass.md`): the
    /// take the running `/` landed, a new clip or an insert into the lead
    /// clip, shows the pass before or after, while that commit is the newest
    /// undo step with nothing to redo (decision 5, read from the record).
    /// The step merges into the commit's undo step (decision 4), whose view
    /// snapshots stay as they were: a step changes no view state. The footer says which pass, or why nothing changed.
    pub(super) fn step_pass_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        step: PassStep,
    ) {
        let top = top_step(undo_record).filter(|_| !undo_record.can_redo());
        let stepped = match top.map(UndoStep::sequencer_edit) {
            Some(SequencerEdit::CommitClip(commit)) => commit.step_pass(sequencer, step),
            Some(SequencerEdit::InsertCapture(insert)) => insert.step_pass(sequencer, step),
            _ => Err(PassRefusal::NoRunningTake),
        };
        let (edit, label) = match stepped {
            Ok(stepped) => stepped,
            Err(refusal) => {
                self.send_status_ui_event(refusal.message().to_owned());
                return;
            }
        };
        let step = UndoStep::new(edit.into(), ViewSnapshot::of(sequencer));
        let (result, _) = undo_record.edit(sequencer, step);
        self.handle_edit_result(sequencer, result);
        self.send_status_ui_event(label);
    }

    /// A committed clip is on its track (`EditResult::ClipCommitted` — every
    /// commit and its redo): adds its shape, selects it, and re-seeks the
    /// sequencer.
    pub(super) fn commit_clip_workflow(&self, sequencer: &mut Sequencer, clip: &ClipMetadata) {
        self.send_clips_added_ui_event(slice::from_ref(clip));
        self.select_clip_workflow(sequencer, Some((clip.track_idx, clip.clip_id)));
        sequencer.reset();
    }

    /// Undo of a clip commit (`EditResult::ClipUncommitted`): the clip has
    /// already been lifted off its track. If it was the lead clip, it stops
    /// being it (telling the view the lead clip is gone), then its shape
    /// goes. The clip view stays where it is, docked or maximized, on "No
    /// clip at the cursor" if it showed this clip, as after undoing a delete
    /// that took the lead clip: only the user moves the keyboard
    /// (`440-undo-view-state.md` decision 6).
    pub(super) fn uncommit_clip_workflow(&self, sequencer: &mut Sequencer, clip: &ClipMetadata) {
        sequencer.reset();

        if sequencer.selected_clip_id() == Some(clip.clip_id) {
            self.select_clip_workflow(sequencer, None);
        }

        self.send_clips_removed_ui_event(slice::from_ref(clip));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::agent::span_label;
    use crate::core::event_handlers::test_harness::{
        Harness, add_clip, harness, play, run_command, statuses,
    };
    use crate::core::input_event::CaptureFrame;
    use crate::core::sequencer::test_support::{rect, spans};
    use crate::core::sequencer::{PassStep, SequencerCommand, SequencerEdit, UndoStep, top_step};
    use crate::core::time::Meter;
    use crate::core::view_state::ViewState;
    use crate::models::event::{Event, EventType};

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// A quarter-note phrase played into the capture buffer from `start`.
    fn play_phrase(h: &mut Harness, start: i32) {
        for i in 0..4 {
            let t = start + i * 960;
            h.sequencer.handle_midi_input(&[0x90, 60, 100], t);
            h.sequencer.handle_midi_input(&[0x80, 60, 0], t + 480);
        }
    }

    /// Running over a loop from 0 to `loop_end`.
    fn run_looping(h: &mut Harness, loop_end: i32) {
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(loop_end, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
    }

    /// A lead clip at `start` (half a bar, one note) under the cursor, with
    /// `view` focused. Returns its event count.
    fn with_a_lead_clip(h: &mut Harness, start: i32, view: ViewState) -> usize {
        h.cursor_tick.store(start, Ordering::Relaxed);
        add_clip(&mut h.sequencer, start);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(view);
        h.sequencer.lead_clip().unwrap().events().len()
    }

    /// `/` with `frame`, recorded on a fresh record, which it returns.
    fn slash(h: &mut Harness, frame: Option<CaptureFrame>) -> Record<UndoStep> {
        let mut record = Record::new();
        run_command(h, &mut record, SequencerCommand::Commit { frame });
        record
    }

    /// A stopped `/` with a marquee lands exactly over it, says so in the
    /// footer, and an undo lifts the clip and leaves the marquee, which the
    /// commit never touched (`470-marquee-capture.md` decisions 6–7).
    #[test]
    fn a_marquee_commit_names_its_span_and_undoes_to_the_same_marquee() {
        let mut h = harness();
        let marquee = rect(BAR * 2, BAR * 4, 0, 0);
        h.sequencer.set_marquee(Some(marquee));
        play_phrase(&mut h, 300);

        let mut record = slash(&mut h, Some(CaptureFrame::Marquee(marquee)));

        assert_eq!(spans(&h.sequencer, 0), vec![(BAR * 2, BAR * 4)]);
        assert_eq!(record.len(), 1);
        assert!(statuses(&h).contains(&"Captured bars 3–4 (marquee)".to_owned()));
        assert_eq!(h.sequencer.marquee(), Some(marquee));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(spans(&h.sequencer, 0).is_empty());
        assert_eq!(h.sequencer.marquee(), Some(marquee));
    }

    /// Running and looping, a marquee outside the loop is refused in the
    /// footer, and nothing is recorded.
    #[test]
    fn a_marquee_outside_the_loop_is_refused_in_the_footer() {
        let mut h = harness();
        run_looping(&mut h, BAR * 2);
        play_phrase(&mut h, 300);

        let record = slash(
            &mut h,
            Some(CaptureFrame::Marquee(rect(BAR, BAR * 3, 0, 0))),
        );

        assert!(record.is_empty());
        assert!(spans(&h.sequencer, 0).is_empty());
        assert_eq!(
            statuses(&h),
            vec!["While looping, the marquee (bars 2–3) must be inside the loop (bars 1–2)"]
        );
    }

    /// A track-only marquee (no tick range) frames nothing: the commit is
    /// today's, at the cursor, with no marquee line.
    #[test]
    fn a_track_only_marquee_commits_as_without_one() {
        let mut h = harness();
        play_phrase(&mut h, 300);

        let record = slash(&mut h, Some(CaptureFrame::Marquee(rect(BAR, BAR, 0, 1))));

        assert_eq!(record.len(), 1);
        assert_eq!(spans(&h.sequencer, 0)[0].0, 0, "at the cursor");
        assert!(statuses(&h).iter().all(|line| !line.contains("marquee")));
    }

    /// Running into the lead clip, the arranger's marquee and the clip view's
    /// span each punch in: only the note played inside, cut to the clip,
    /// lands; the footer names the span, in the clip's bars for the clip
    /// view; an undo lifts the note and leaves the marquee
    /// (`470-marquee-capture.md` decisions 9 and 11).
    #[test]
    fn a_running_marquee_or_clip_span_into_the_lead_clip_punches_in() {
        let marquee = rect(BAR + 960, BAR * 3, 0, 0);
        // The arranger counts bars from 0, the clip view from the clip.
        for (view, frame, label) in [
            (
                ViewState::Arranger,
                CaptureFrame::Marquee(marquee),
                (BAR + 960, BAR + 1920),
            ),
            (
                ViewState::Clip,
                CaptureFrame::ClipSpan((960, 1920)),
                (960, 1920),
            ),
        ] {
            let mut h = harness();
            run_looping(&mut h, BAR * 2);
            let before = with_a_lead_clip(&mut h, BAR, view);
            h.sequencer.set_marquee(Some(marquee));
            // Notes at the clip's event ticks 300 and 1260 (and two past it).
            play_phrase(&mut h, BAR + 300);

            let mut record = slash(&mut h, Some(frame));

            assert_eq!(record.len(), 1);
            let lead = h.sequencer.lead_clip().unwrap();
            assert_eq!(lead.events().len(), before + 2, "only the note at 1260");
            let label = span_label(label.0, label.1, Meter::FOUR_FOUR);
            let expected = format!("Inserted {label} (marquee)");
            assert!(statuses(&h).contains(&expected), "{view:?}");

            run_command(&mut h, &mut record, SequencerCommand::Undo);
            assert_eq!(h.sequencer.lead_clip().unwrap().events().len(), before);
            assert_eq!(h.sequencer.marquee(), Some(marquee));
        }
    }

    /// Running into the lead clip, a marquee that misses it is refused.
    #[test]
    fn a_marquee_outside_the_lead_clip_is_refused_in_the_footer() {
        let mut h = harness();
        run_looping(&mut h, BAR);
        with_a_lead_clip(&mut h, 0, ViewState::Arranger);
        play_phrase(&mut h, 300);

        let marquee = rect(BAR * 2, BAR * 3, 0, 0);
        let record = slash(&mut h, Some(CaptureFrame::Marquee(marquee)));

        assert!(record.is_empty());
        assert_eq!(statuses(&h), vec!["The marquee is outside the clip"]);
    }

    /// Stopped into the lead clip, the marquee is ignored: the take session
    /// opens as without one, even for a marquee that misses the clip.
    #[test]
    fn a_stopped_marquee_into_the_lead_clip_opens_the_take_session() {
        let mut h = harness();
        with_a_lead_clip(&mut h, 0, ViewState::Arranger);
        play_phrase(&mut h, 300);

        slash(
            &mut h,
            Some(CaptureFrame::Marquee(rect(BAR * 2, BAR * 3, 0, 0))),
        );

        assert!(h.sequencer.is_framing_take());
        assert!(statuses(&h).iter().all(|line| !line.contains("marquee")));
    }

    // --- `,`/`.` (`540-earlier-pass.md`) ---

    /// A `pitch` note from `on` to `off`, played live.
    fn note(h: &mut Harness, pitch: u8, on: i32, off: i32) {
        h.sequencer.handle_midi_input(&[0x90, pitch, 100], on);
        h.sequencer.handle_midi_input(&[0x80, pitch, 0], off);
    }

    /// Running over a one-bar loop: one note a pass, pitches 60, 62, 64,
    /// then `/`. Returns the record.
    fn three_passes(h: &mut Harness) -> Record<UndoStep> {
        run_looping(h, BAR);
        for (pass, pitch) in [(0, 60), (1, 62), (2, 64)] {
            note(h, pitch, pass * BAR + 100, pass * BAR + 400);
        }
        let mut record = Record::new();
        run_command(h, &mut record, SequencerCommand::Commit { frame: None });
        statuses(h);
        record
    }

    fn earlier(h: &mut Harness, record: &mut Record<UndoStep>) {
        run_command(h, record, SequencerCommand::StepPass(PassStep::Earlier));
    }

    fn later(h: &mut Harness, record: &mut Record<UndoStep>) {
        run_command(h, record, SequencerCommand::StepPass(PassStep::Later));
    }

    /// The pitches of the note-ons on track 0, clip by clip.
    fn pitches(h: &Harness) -> Vec<u8> {
        h.sequencer.tracks()[0]
            .clips()
            .iter()
            .flat_map(|clip| clip.events().iter())
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .filter_map(Event::note_number)
            .collect()
    }

    /// The walkthrough: each step changes the clip in place and says which
    /// pass; the record stays one step, so one ⌘Z removes the take and a
    /// redo puts back the pass it showed (decisions 3, 4 and 8).
    #[test]
    fn a_step_amends_the_commit_and_one_undo_removes_the_take() {
        let mut h = harness();
        let mut record = three_passes(&mut h);
        let id = h.sequencer.tracks()[0].clips()[0].id();
        let cursor = h.sequencer.cursor_tick();
        assert_eq!(pitches(&h), vec![64]);
        // Not an undo step, so it leaves the steps alone; nor is it the
        // view the commit's redo puts back.
        h.cursor_tick.store(BAR * 2, Ordering::Relaxed);

        earlier(&mut h, &mut record);
        assert_eq!(pitches(&h), vec![62]);
        assert_eq!(statuses(&h), vec!["Pass 1 back of 3"]);
        earlier(&mut h, &mut record);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["Pass 2 back of 3", "No earlier pass"]);
        later(&mut h, &mut record);
        assert_eq!(pitches(&h), vec![62]);
        assert_eq!(statuses(&h), vec!["Pass 1 back of 3"]);

        assert_eq!(record.len(), 1, "the steps amend the commit");
        assert_eq!(h.sequencer.tracks()[0].clips()[0].id(), id, "in place");
        assert_eq!(h.sequencer.cursor_tick(), BAR * 2, "the cursor stays");

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(pitches(&h).is_empty(), "one undo removes the take");
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No running take to step through"]);

        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(pitches(&h), vec![62], "the pass it showed when undone");
        assert_eq!(
            h.sequencer.cursor_tick(),
            cursor,
            "the commit's own view after: a step records none"
        );
        later(&mut h, &mut record);
        assert_eq!(pitches(&h), vec![64], "and the steps go on from there");
        assert_eq!(statuses(&h), vec!["Last pass"]);
        later(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["Already the last pass"]);
    }

    /// The keys act only while the running commit is the newest step with
    /// nothing to redo (decision 5); any other step on top ends it, and the
    /// commit's kept buffer goes with it (decision 6).
    #[test]
    fn another_step_on_top_ends_the_steps_and_drops_the_buffer() {
        let keeps_buffer = |record: &Record<UndoStep>| match record
            .get_entry(0)
            .map(|entry| entry.as_ref().sequencer_edit())
        {
            Some(SequencerEdit::CommitClip(commit)) => commit.keeps_buffer(),
            _ => panic!("the commit is first"),
        };
        let mut h = harness();
        let mut record = three_passes(&mut h);
        assert!(keeps_buffer(&record));

        // An empty clip further on: an edit on top.
        h.cursor_tick.store(BAR * 4, Ordering::Relaxed);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertEmptyClip { time_bounds: None },
        );
        assert_eq!(record.len(), 2);
        assert!(!keeps_buffer(&record), "dropped once a step is on top");
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No running take to step through"]);

        // Undone, the commit is on top again but has something to redo.
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No running take to step through"]);
        assert_eq!(pitches(&h), vec![64]);
    }

    /// A linear running take has no passes, and a stopped commit on top is
    /// no running commit.
    #[test]
    fn steps_are_refused_on_a_linear_take_and_a_stopped_commit() {
        // Playback past the loop's end never wraps.
        let mut h = harness();
        run_looping(&mut h, BAR);
        h.playback_tick.store(BAR * 8, Ordering::Relaxed);
        h.cursor_tick.store(BAR * 8, Ordering::Relaxed);
        note(&mut h, 60, BAR * 8 + 100, BAR * 8 + 400);
        let mut record = slash(&mut h, None);
        statuses(&h);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["The take isn't looping"]);

        let mut h = harness();
        play_phrase(&mut h, 300);
        let mut record = slash(&mut h, None);
        statuses(&h);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No running take to step through"]);
    }

    /// Whether the record's newest step is a running insert keeping its
    /// buffer.
    fn insert_keeps_buffer(record: &Record<UndoStep>) -> bool {
        match top_step(record).map(UndoStep::sequencer_edit) {
            Some(SequencerEdit::InsertCapture(insert)) => insert.keeps_buffer(),
            _ => false,
        }
    }

    /// Phase 2: a running insert into the lead clip steps like a new clip,
    /// each pass re-landed over the clip as it was before the insert, so
    /// passes never stack; one undo puts the clip back, a redo the pass it
    /// showed, and an edit on top drops the buffer.
    #[test]
    fn an_insert_steps_through_its_passes_over_the_clip_from_before() {
        let mut h = harness();
        run_looping(&mut h, BAR);
        with_a_lead_clip(&mut h, 0, ViewState::Arranger);
        let before = pitches(&h);
        for (pass, pitch) in [(0, 62), (1, 64), (2, 67)] {
            note(&mut h, pitch, pass * BAR + 100, pass * BAR + 400);
        }
        let mut record = slash(&mut h, None);
        statuses(&h);
        assert_eq!(pitches(&h), [before.clone(), vec![67]].concat());

        earlier(&mut h, &mut record);
        assert_eq!(pitches(&h), [before.clone(), vec![64]].concat());
        assert_eq!(statuses(&h), vec!["Pass 1 back of 3"]);
        earlier(&mut h, &mut record);
        assert_eq!(pitches(&h), [before.clone(), vec![62]].concat());
        later(&mut h, &mut record);
        assert_eq!(pitches(&h), [before.clone(), vec![64]].concat());
        assert_eq!(record.len(), 1, "the steps amend the insert");
        statuses(&h);

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(pitches(&h), before, "one undo puts the clip back");
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(pitches(&h), [before.clone(), vec![64]].concat());
        later(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["Last pass"]);
        assert_eq!(pitches(&h), [before.clone(), vec![67]].concat());

        assert!(insert_keeps_buffer(&record));
        h.cursor_tick.store(BAR * 4, Ordering::Relaxed);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::InsertEmptyClip { time_bounds: None },
        );
        assert_eq!(record.len(), 2);
        statuses(&h);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No running take to step through"]);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(
            !insert_keeps_buffer(&record),
            "dropped once a step was on top"
        );
    }

    /// An insert's steps start from its own buffer: the passes the new
    /// clip's `/` consumed before it are not among them.
    #[test]
    fn an_insert_after_a_commit_steps_only_its_own_passes() {
        let mut h = harness();
        let mut record = three_passes(&mut h);
        with_a_lead_clip(&mut h, 0, ViewState::Arranger);
        note(&mut h, 67, BAR * 3 + 100, BAR * 3 + 400);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit { frame: None },
        );
        assert_eq!(record.len(), 2, "an insert into the lead clip");
        statuses(&h);
        earlier(&mut h, &mut record);
        assert_eq!(statuses(&h), vec!["No earlier pass"]);
    }

    /// A step while running releases the old pass's sounding note at once
    /// (`muted-note-release`: through `run_command`, whose result handler
    /// re-seeks).
    #[test]
    fn a_step_while_running_releases_the_old_pass_sounding_note() {
        let mut h = harness();
        run_looping(&mut h, BAR);
        note(&mut h, 60, 100, BAR - 200);
        note(&mut h, 62, BAR + 100, BAR * 2 - 200);
        let mut record = slash(&mut h, None);
        h.sequencer.reset_to_tick(0);
        h.playback_tick.store(0, Ordering::Relaxed);
        assert_eq!(play(&mut h, 200), vec![(100, 62, true)]);

        earlier(&mut h, &mut record);

        let edges = play(&mut h, BAR - 200);
        assert_eq!(edges[0], (200, 62, false), "released at once: {edges:?}");
        assert!(
            edges.iter().all(|&(_, pitch, on)| !(pitch == 62 && on)),
            "{edges:?}"
        );
    }
}
