//! `EventHandlers` workflows for a clip appearing on / leaving a track through
//! `CommitClipEdit` — `/`'s `Commit` binding, and the undo/redo of it. See
//! `050-undo-redo.md`, `100-running-capture.md`.

use std::slice;

use undo::Record;

use crate::core::input_event::TimeSelectionRect;
use crate::core::sequencer::{
    CommitClipEdit, InsertCaptureEdit, SequencerEdit, UndoStep, span_label,
};

use super::*;

impl EventHandlers {
    // --- Clip operations ---

    /// `/` (`SequencerCommand::Commit`): into the lead clip if there is one,
    /// otherwise a new clip — the last pass while running, the detected
    /// phrase while stopped. Stopped into the lead clip opens a take session
    /// instead, which inserts on Enter.
    ///
    /// A `marquee` with a tick range frames a new clip instead of the loop
    /// (`470-marquee-capture.md`): refused with a footer line when it can't
    /// (outside the loop, over a clip), otherwise the footer names the span
    /// it captured. Into the lead clip it is ignored until the punch-in
    /// overdub (phase 2). It is never cleared: the view keeps it.
    pub(super) fn commit_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        marquee: Option<TimeSelectionRect>,
    ) {
        let into_lead_clip = sequencer.selected_clip_id().is_some();
        let running = sequencer.is_running();
        let frame = marquee
            .filter(|rect| rect.has_tick_range() && !into_lead_clip)
            .map(|rect| (rect.start, rect.end));
        if let Some(refusal) = frame.and_then(|frame| sequencer.marquee_capture_refusal(frame)) {
            self.send_status_ui_event(refusal.message().to_owned());
            return;
        }
        let edit = match (into_lead_clip, running) {
            (true, true) => {
                InsertCaptureEdit::from_running_capture(sequencer).map(SequencerEdit::from)
            }
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
        if let Some((start, end)) = frame {
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

    use crate::core::event_handlers::test_harness::{Harness, harness, run_command, statuses};
    use crate::core::sequencer::test_support::rect;
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// A quarter-note phrase played into the capture buffer from `start`.
    fn play_phrase(h: &mut Harness, start: i32) {
        for i in 0..4 {
            let t = start + i * 960;
            h.sequencer.handle_midi_input(&[0x90, 60, 100], t);
            h.sequencer.handle_midi_input(&[0x80, 60, 0], t + 480);
        }
    }

    /// The `(start, end)` of every clip on track 0.
    fn clip_spans(h: &Harness) -> Vec<(i32, i32)> {
        h.sequencer.tracks()[0]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
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
            },
        );

        assert_eq!(clip_spans(&h), vec![(BAR * 2, BAR * 4)]);
        assert_eq!(record.len(), 1);
        assert!(statuses(&h).contains(&"Captured bars 3–4 (marquee)".to_owned()));
        assert_eq!(h.sequencer.marquee(), Some(marquee));

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(clip_spans(&h).is_empty());
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
            },
        );

        assert!(record.is_empty());
        assert!(clip_spans(&h).is_empty());
        assert_eq!(statuses(&h), vec!["The marquee is outside the loop"]);
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
            },
        );

        assert_eq!(record.len(), 1);
        assert_eq!(clip_spans(&h)[0].0, 0, "at the cursor");
        assert!(statuses(&h).iter().all(|line| !line.contains("marquee")));
    }
}
