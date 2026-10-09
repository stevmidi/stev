//! `EventHandlers` workflows for moving one clip's edges through
//! `ResizeClipEdit` — the arranger edge drag and `[`/`]` in both views — and
//! for fitting the tempo to the project's only clip with Enter
//! (`220-capture-without-pending-view.md`, `050-undo-redo.md`). An edit never
//! moves the playhead, and moves the loop region in one case only: while the
//! project has one clip, the loop is that clip (`loop_sole_clip_workflow`).

use undo::Record;

use crate::core::sequencer::{EdgeTarget, ResizeClipEdit, RetimeClipEdit, SequencerEdit};
use crate::models::clip::{ClipBounds, ClipEdge, EventSpaceRetime};

use super::*;

impl EventHandlers {
    /// One step of the arranger edge drag: trims the selected clip's `edge`
    /// to `target_tick`, recorded under `drag_id` so the whole drag merges
    /// into one undo step.
    pub(super) fn drag_clip_edge_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        edge: ClipEdge,
        target_tick: i32,
        drag_id: u64,
    ) {
        let Some(clip_id) = sequencer.selected_clip_id() else {
            return;
        };
        let after = match edge {
            ClipEdge::Start => sequencer.clip_start_trimmed_to(clip_id, target_tick),
            ClipEdge::End => sequencer.clip_end_trimmed_to(clip_id, target_tick),
        };
        self.resize_clip_workflow(sequencer, undo_record, clip_id, after, Some(drag_id));
    }

    /// `[`/`]`: moves `edge` to the cursor, playing or not
    /// (`Sequencer::clip_edge_target`). In the arranger it trims the clip
    /// under the arranger cursor, or the nearest one on that side, and then
    /// re-syncs the lead clip to the cursor, since a `[` that grew a clip
    /// back to the cursor puts the cursor on it. In the clip view it sets the
    /// lead clip's start/end marker at the clip cursor. Anywhere else a no-op.
    pub(super) fn clip_edge_to_cursor_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        edge: ClipEdge,
    ) {
        let in_clip_view = self.view_state().is_clip_view();
        let Some(EdgeTarget {
            clip_id,
            bounds: target,
        }) = sequencer.clip_edge_target(edge, in_clip_view)
        else {
            return;
        };
        self.resize_clip_workflow(sequencer, undo_record, clip_id, Some(target), None);

        if !in_clip_view {
            self.sync_clip_selection_to_cursor_workflow(sequencer);
        }
    }

    /// While the project has exactly one clip, the loop region is that clip:
    /// its end can be judged against the wrap while shaping it, before Enter
    /// sets the tempo — what the pending view did while it was open. The
    /// stopped `/` that makes the first clip also turns looping on
    /// (`turn_loop_on`); an edit to the clip only moves the loop, leaving a
    /// loop the user switched off off. With any other number of clips it does
    /// nothing: the one exception to "edits never move the transport"
    /// (`220-capture-without-pending-view.md`). A playhead an edit leaves past
    /// the new loop end runs on, which is accepted as correct.
    pub(super) fn loop_sole_clip_workflow(&self, sequencer: &Sequencer, turn_loop_on: bool) {
        let Some((start, end)) = sequencer.sole_clip_span() else {
            return;
        };
        let cmd = if turn_loop_on {
            TransportCommand::LoopOver { start, end }
        } else {
            TransportCommand::SetRegion {
                start: Some(start),
                end: Some(end),
            }
        };
        self.send_transport(cmd);
    }

    /// Enter (`SequencerCommand::FitTempo`): fits the tempo to the project's
    /// only clip (`RetimeClipEdit`, undoable), so its length becomes whole
    /// bars. Nothing is stored about it: the edit decides from what's there —
    /// a no-op with more than one clip, or when the clip is already whole
    /// bars from a bar line. Works while playing; the rest of that pass may
    /// play out of phase until the next loop wrap re-seeks it.
    pub(super) fn fit_first_clip_tempo_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
    ) {
        self.record_edit(
            sequencer,
            undo_record,
            RetimeClipEdit::fit_sole_clip(sequencer),
        );
    }

    /// `⌥=`/`⌥-` (`SequencerCommand::RescaleSelectedClipTempo`): stretches the
    /// selected clip `direction` bars longer/shorter, retiming its notes and,
    /// for the project's only clip, the tempo (`RetimeClipEdit`, undoable —
    /// the tempo with it). A no-op on a one-bar clip shrinking.
    pub(super) fn rescale_selected_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        direction: i32,
    ) {
        self.record_edit(
            sequencer,
            undo_record,
            RetimeClipEdit::rescale_selected(sequencer, direction),
        );
    }

    /// Records a `ResizeClipEdit` moving `clip_id` (selected track) to
    /// `after` and fans out its result. A no-op when `after` is `None` or
    /// unchanged.
    fn resize_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        clip_id: Uuid,
        after: Option<ClipBounds>,
        drag_id: Option<u64>,
    ) {
        let edit =
            after.and_then(|after| ResizeClipEdit::for_clip(sequencer, clip_id, after, drag_id));
        self.record_edit(sequencer, undo_record, edit);
    }

    /// A clip's edges moved (`EditResult::ClipResized`, either direction — an
    /// edge drag, `[`/`]`, Enter's tempo fit, `⌥=`/`⌥-`, their undo/redo): updates its
    /// shape and thumbnail and refreshes the open clip view's notes. The
    /// playhead is left alone: the clip's events didn't move, so the current
    /// pass plays out and the next loop wrap's seek picks up the new window,
    /// and `Track::tick` handles notes that cross a moved edge. The project's
    /// only clip keeps the loop on it (`loop_sole_clip_workflow`).
    pub(super) fn clip_resized_workflow(
        &self,
        sequencer: &mut Sequencer,
        clip: ClipMetadata,
        retime: Option<EventSpaceRetime>,
    ) {
        self.loop_sole_clip_workflow(sequencer, false);
        let clip_id = clip.clip_id;
        self.ui_event_tx.send(UiEvent::ClipUpdated { clip }).ok();
        // Refresh the open clip view's notes before sending the retime, so
        // the view maps its framing onto them (`UiEvent::ClipRetimed`).
        self.sync_clip_post_edit_workflow(sequencer);
        if let Some(retime) = retime {
            self.ui_event_tx
                .send(UiEvent::ClipRetimed { clip_id, retime })
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::event_handlers::test_harness::{
        Harness, add_clip, harness, run_command as run,
    };
    use crate::core::sequencer::SequencerCommand;
    use crate::core::time;
    use crate::core::transport::TransportCommand;
    use crate::core::view_state::ViewState;
    use crate::models::clip::ClipEdge;

    /// A running transport looping exactly over a one-bar clip at bar 1,
    /// the playhead three quarters through it, the clip the lead clip, the
    /// clip view focused — the setting where an edit is most tempted to
    /// touch the transport.
    fn playing_a_looped_clip() -> Harness {
        let bar = time::bars_to_ticks(1);
        let mut h = harness();
        add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        let clip_len = window_length(&h);
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(clip_len, Ordering::Relaxed);
        h.running.store(true, Ordering::Relaxed);
        h.playback_tick.store(clip_len * 3 / 4, Ordering::Relaxed);
        h.handlers.set_view_state(ViewState::Clip);
        assert!(clip_len <= bar);
        h
    }

    /// The lead clip's window length.
    fn window_length(h: &Harness) -> i32 {
        h.sequencer.selected_clip().unwrap().region_length()
    }

    /// The loop region each `SetRegion` the handlers sent set, and whether
    /// they sent anything else to the transport.
    fn loop_moves(h: &Harness) -> (Vec<(i32, i32)>, bool) {
        let mut spans = Vec::new();
        let mut other = false;
        for cmd in h.transport_commands.try_iter() {
            match cmd {
                TransportCommand::SetRegion {
                    start: Some(start),
                    end: Some(end),
                } => spans.push((start, end)),
                _ => other = true,
            }
        }
        (spans, other)
    }

    /// While the project has one clip the loop is that clip (`220`): an end
    /// pulled in behind the playhead, an edge drag, and Enter's tempo fit
    /// while playing each move the loop to the clip's new span — and do
    /// nothing else to the transport: the playhead is never moved.
    #[test]
    fn the_only_clip_keeps_the_loop_on_it_and_never_moves_the_playhead() {
        let mut h = playing_a_looped_clip();
        let mut record = Record::new();
        let span = |h: &Harness| {
            let start = h.sequencer.tracks()[0].clips()[0].start_tick();
            (start, h.sequencer.tracks()[0].clips()[0].end_tick())
        };

        // `]` behind the playhead at 1440 (the project's only clip: exact).
        let start = h.sequencer.selected_clip_region_start().unwrap();
        h.sequencer.set_selected_clip_cursor_tick(start + 1200);
        run(
            &mut h,
            &mut record,
            SequencerCommand::SetClipEdgeToCursor(ClipEdge::End),
        );
        assert_eq!(window_length(&h), 1200);
        assert_eq!(loop_moves(&h), (vec![span(&h)], false));

        // An arranger-style edge drag step.
        run(
            &mut h,
            &mut record,
            SequencerCommand::ResizeSelectedClipRegionEnd {
                target_tick: 1100,
                drag_id: 1,
            },
        );
        assert_eq!(loop_moves(&h), (vec![span(&h)], false));

        // Enter: fits the tempo to the only clip, while playing.
        let tempo_before = h.sequencer.tempo_us();
        run(&mut h, &mut record, SequencerCommand::FitTempo);
        assert_ne!(h.sequencer.tempo_us(), tempo_before, "the fit happened");
        assert_eq!(loop_moves(&h), (vec![span(&h)], false));
    }

    /// With more than one clip no edit touches the transport at all: the
    /// loop stays where the user put it.
    #[test]
    fn with_two_clips_an_edge_edit_sends_nothing_to_the_transport() {
        let bar = time::bars_to_ticks(1);
        let mut h = playing_a_looped_clip();
        add_clip(&mut h.sequencer, bar * 8);
        let mut record = Record::new();

        let start = h.sequencer.selected_clip_region_start().unwrap();
        h.sequencer.set_selected_clip_cursor_tick(start + 1200);
        run(
            &mut h,
            &mut record,
            SequencerCommand::SetClipEdgeToCursor(ClipEdge::End),
        );

        assert_eq!(record.len(), 1, "the edit happened");
        assert_eq!(h.transport_commands.try_iter().count(), 0);
    }

    /// Enter only acts on a project's only clip: with a second one it is a
    /// no-op, stored nowhere and recorded nowhere.
    #[test]
    fn enter_does_nothing_with_more_than_one_clip() {
        let bar = time::bars_to_ticks(1);
        let mut h = harness();
        add_clip(&mut h.sequencer, 0);
        add_clip(&mut h.sequencer, bar * 4);
        let mut record = Record::new();
        let tempo_before = h.sequencer.tempo_us();

        run(&mut h, &mut record, SequencerCommand::FitTempo);

        assert_eq!(h.sequencer.tempo_us(), tempo_before);
        assert_eq!(record.len(), 0);
    }
}
