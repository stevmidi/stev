//! `EventHandlers` workflows for a clip appearing on / leaving a track through
//! `CommitClipEdit` — `/`'s `Commit` binding, and the undo/redo of it. See
//! `050-undo-redo.md`, `100-running-capture.md`.

use std::slice;

use undo::Record;

use crate::core::agent::span_label;
use crate::core::input_event::TimeSelectionRect;
use crate::core::sequencer::{CommitClipEdit, InsertCaptureEdit, SequencerEdit, UndoStep};

use super::*;

impl EventHandlers {
    // --- Clip operations ---

    /// `/` (`SequencerCommand::Commit`): into the lead clip if there is one,
    /// otherwise a new clip — the last pass while running, the detected
    /// phrase while stopped. Stopped into the lead clip opens a take session
    /// instead, which inserts on Enter.
    ///
    /// A `marquee` with a tick range frames the capture
    /// (`470-marquee-capture.md`): a new clip instead of the loop, and a
    /// running insert into the lead clip as a punch-in, landing only what was
    /// played inside it. It is refused with a footer line when it can't
    /// (outside the loop, over a clip, outside the lead clip), otherwise the
    /// footer names the span it captured or punched in. The clip view's
    /// `clip_span` (the piano roll's marquee, in the lead clip's event ticks)
    /// punches in a running insert the same way, its footer counting the
    /// clip's bars as that view's ruler does. A stopped `/` into the lead
    /// clip ignores both: the take session frames with its own band. Neither
    /// is ever cleared: the view keeps it.
    pub(super) fn commit_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        marquee: Option<TimeSelectionRect>,
        clip_span: Option<(i32, i32)>,
    ) {
        let into_lead_clip = sequencer.selected_clip_id().is_some();
        let running = sequencer.is_running();
        let clip_frame = clip_span
            .filter(|_| running && into_lead_clip)
            .and_then(|span| sequencer.clip_span_in_arrangement(span));
        let frame = marquee
            .filter(|rect| rect.has_tick_range() && (running || !into_lead_clip))
            .map(|rect| (rect.start, rect.end))
            .or(clip_frame);
        if let Some(refusal) = frame.and_then(|frame| sequencer.marquee_capture_refusal(frame)) {
            self.send_status_ui_event(refusal.message().to_owned());
            return;
        }
        // Into the lead clip, the span that lands: the marquee cut to the clip.
        let punch_in = frame
            .filter(|_| into_lead_clip)
            .and_then(|frame| sequencer.punch_in_span(frame));
        let edit = match (into_lead_clip, running) {
            (true, true) => InsertCaptureEdit::from_running_capture(sequencer, punch_in)
                .map(SequencerEdit::from),
            (true, false) => {
                if self.view_state() == ViewState::Arranger {
                    sequencer.sync_selected_clip_cursor_with_arranger();
                }
                self.open_take_framing_workflow(sequencer);
                None
            }
            (false, true) => {
                CommitClipEdit::from_running_capture(sequencer, frame).map(SequencerEdit::from)
            }
            (false, false) => {
                #[cfg(debug_assertions)]
                sequencer.write_stopped_capture_fixture();
                CommitClipEdit::from_stopped_capture(sequencer, frame).map(SequencerEdit::from)
            }
        };
        if !self.record_edit(sequencer, undo_record, edit) {
            return;
        }
        if let Some((start, end)) = punch_in {
            // The clip view's ruler counts from the clip, the arranger's from 0.
            let origin = clip_frame
                .and_then(|_| sequencer.lead_clip_ruler_origin())
                .unwrap_or(0);
            let span = span_label(start - origin, end - origin, sequencer.meter());
            self.send_status_ui_event(format!("Inserted {span} (marquee)"));
        } else if let Some((start, end)) = frame {
            let span = span_label(start, end, sequencer.meter());
            self.send_status_ui_event(format!("Captured {span} (marquee)"));
        }
        if !into_lead_clip && !running {
            // The project's first clip: loop over it.
            self.loop_sole_clip_workflow(sequencer, true);
        }
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
        Harness, add_clip, harness, run_command, statuses,
    };
    use crate::core::sequencer::test_support::{rect, spans};
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::view_state::ViewState;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// A quarter-note phrase played into the capture buffer from `start`.
    fn play_phrase(h: &mut Harness, start: i32) {
        for i in 0..4 {
            let t = start + i * 960;
            h.sequencer.handle_midi_input(&[0x90, 60, 100], t);
            h.sequencer.handle_midi_input(&[0x80, 60, 0], t + 480);
        }
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
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(marquee),
                clip_span: None,
            },
        );

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
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(BAR * 2, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
        play_phrase(&mut h, 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(rect(BAR, BAR * 3, 0, 0)),
                clip_span: None,
            },
        );

        assert!(record.is_empty());
        assert!(spans(&h.sequencer, 0).is_empty());
        assert_eq!(
            statuses(&h),
            vec!["While looping, the marquee must be inside the loop"]
        );
    }

    /// A track-only marquee (no tick range) frames nothing: the commit is
    /// today's, at the cursor, with no marquee line.
    #[test]
    fn a_track_only_marquee_commits_as_without_one() {
        let mut h = harness();
        play_phrase(&mut h, 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(rect(BAR, BAR, 0, 1)),
                clip_span: None,
            },
        );

        assert_eq!(record.len(), 1);
        assert_eq!(spans(&h.sequencer, 0)[0].0, 0, "at the cursor");
        assert!(statuses(&h).iter().all(|line| !line.contains("marquee")));
    }

    /// A lead clip at 0 (half a bar, one note) under the arranger cursor.
    fn with_a_lead_clip(h: &mut Harness) -> usize {
        add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Arranger);
        h.sequencer.lead_clip().unwrap().events().len()
    }

    /// Running into the lead clip, the marquee punches in: only the note
    /// played inside it, cut to the clip, lands; the footer names that span,
    /// and an undo lifts the note and leaves the marquee
    /// (`470-marquee-capture.md` decision 9).
    #[test]
    fn a_running_marquee_into_the_lead_clip_punches_in() {
        let mut h = harness();
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(BAR, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
        let before = with_a_lead_clip(&mut h);
        let marquee = rect(960, BAR * 2, 0, 0);
        h.sequencer.set_marquee(Some(marquee));
        play_phrase(&mut h, 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(marquee),
                clip_span: None,
            },
        );

        assert_eq!(record.len(), 1);
        let lead = h.sequencer.lead_clip().unwrap();
        assert_eq!(lead.events().len(), before + 2, "one note-on and its off");
        let label = span_label(960, 1920, Meter::FOUR_FOUR);
        assert!(statuses(&h).contains(&format!("Inserted {label} (marquee)")));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert_eq!(h.sequencer.lead_clip().unwrap().events().len(), before);
        assert_eq!(h.sequencer.marquee(), Some(marquee));
    }

    /// Running into the lead clip, a marquee that misses it is refused.
    #[test]
    fn a_marquee_outside_the_lead_clip_is_refused_in_the_footer() {
        let mut h = harness();
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(BAR, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
        with_a_lead_clip(&mut h);
        play_phrase(&mut h, 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(rect(BAR * 2, BAR * 3, 0, 0)),
                clip_span: None,
            },
        );

        assert!(record.is_empty());
        assert_eq!(statuses(&h), vec!["The marquee is outside the clip"]);
    }

    /// Stopped into the lead clip, the marquee is ignored: the take session
    /// opens as without one, even for a marquee that misses the clip.
    #[test]
    fn a_stopped_marquee_into_the_lead_clip_opens_the_take_session() {
        let mut h = harness();
        with_a_lead_clip(&mut h);
        play_phrase(&mut h, 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: Some(rect(BAR * 2, BAR * 3, 0, 0)),
                clip_span: None,
            },
        );

        assert!(h.sequencer.is_framing_take());
        assert!(statuses(&h).iter().all(|line| !line.contains("marquee")));
    }

    /// In the clip view, the piano roll's span punches in the same way: only
    /// the note played inside it lands, and the footer counts the clip's
    /// bars, as that view's ruler does.
    #[test]
    fn a_running_clip_view_span_punches_in() {
        let mut h = harness();
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(BAR * 2, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
        h.cursor_tick.store(BAR, Ordering::Relaxed);
        add_clip(&mut h.sequencer, BAR);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Clip);
        let before = h.sequencer.lead_clip().unwrap().events().len();
        // Notes at the clip's event ticks 300 and 1260 (and two past it).
        play_phrase(&mut h, BAR + 300);
        let mut record: Record<UndoStep> = Record::new();

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit {
                marquee: None,
                clip_span: Some((960, 1920)),
            },
        );

        assert_eq!(record.len(), 1);
        let lead = h.sequencer.lead_clip().unwrap();
        assert_eq!(lead.events().len(), before + 2, "only the note at 1260");
        let label = span_label(960, 1920, Meter::FOUR_FOUR);
        assert!(statuses(&h).contains(&format!("Inserted {label} (marquee)")));
    }
}
