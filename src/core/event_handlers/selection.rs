//! `EventHandlers` workflows for track / clip / event selection.
//!
//! Selection is model state (not undoable), but changing it fans out a wave of
//! `UiEvent`s and, for tracks, re-arms live input. `publish_event_selection` mirrors
//! whether any event is selected into `SharedAtomics::has_event_selection`
//! for the UI thread's key routing. The marquee is the view's, but the
//! sequencer moves it too (`set_marquee_workflow`), keeping its mirror current.

use crate::core::input_event::TimeSelectionRect;
use crate::core::sequencer::ViewSnapshot;

use super::*;

impl EventHandlers {
    // --- Selection ---

    /// Selects the track at `new_idx`: disarms the performance lane, clears
    /// the clip selection, selects the track, re-selects whatever clip is
    /// under the cursor, and tells the UI.
    pub(crate) fn select_track_workflow(&self, sequencer: &mut Sequencer, new_idx: usize) {
        if self.switch_track_workflow(sequencer, new_idx) {
            let clip_id = sequencer.find_selected_track_clip_id_at_cursor();
            self.select_clip_workflow(sequencer, clip_id.map(|id| (new_idx, id)));
        }
    }

    /// `select_track_workflow` up to the lead clip, which the caller then
    /// picks itself (a band press its pressed clip, an undo the lead from
    /// before its edit), so the clip under the cursor is never selected and
    /// swapped straight out. Returns whether anything changed: `false` for an
    /// unknown index or the track already selected with the lane unarmed.
    pub(super) fn switch_track_workflow(&self, sequencer: &mut Sequencer, new_idx: usize) -> bool {
        let Some(id) = sequencer.track_id_by_index(new_idx) else {
            return false;
        };
        let new_id = Some(id);

        let performance_lane_was_armed = sequencer.is_performance_lane_armed();

        if new_id == sequencer.selected_track_id() && !performance_lane_was_armed {
            return false;
        }

        sequencer.set_performance_lane_armed(false);

        self.clear_clip_selection_workflow(sequencer);

        sequencer.select_track(new_id);
        self.send_track_selected_ui_event(new_idx);
        true
    }

    /// Cycle track selection by one, wrapping at both ends. `direction > 0`
    /// selects the next track, `direction <= 0` the previous. With no track
    /// selected (the performance lane is) the next track is the first and
    /// the previous the last. No-op when there are no tracks.
    pub(super) fn cycle_selected_track_workflow(&self, sequencer: &mut Sequencer, direction: i32) {
        let track_count = sequencer.tracks().len();
        if track_count == 0 {
            return;
        }

        // With no selection, start one step before the end entered at.
        let forward = direction > 0;
        let entry_idx = if forward { track_count - 1 } else { 0 };
        let current_idx = sequencer.selected_track_index().unwrap_or(entry_idx);
        let step = if forward { 1 } else { track_count - 1 };
        let new_idx = (current_idx + step) % track_count;
        self.select_track_workflow(sequencer, new_idx);
    }

    /// Selects the performance lane, the arranger's other kind of selected
    /// row (`450-performance-lane-selection.md` decision 1): the lead clip,
    /// the track selection and the marquee go, and the armed lane maps the
    /// physical MIDI keyboard to bar-jump triggers instead of note capture.
    pub(super) fn select_performance_lane_workflow(&self, sequencer: &mut Sequencer) {
        if sequencer.is_performance_lane_armed() {
            return;
        }
        self.clear_clip_selection_workflow(sequencer);
        sequencer.select_track(None);
        sequencer.set_performance_lane_armed(true);
        self.send_performance_lane_selected_ui_event();
        self.clear_marquee_workflow(sequencer);
    }

    /// Makes `lead`, a clip and the track it is on, the lead clip (`None`
    /// clears it). The lead clip lives on the selected track — the clip
    /// view looks it up there — so a clip on another track selects that
    /// track first (`switch_track_workflow`, which disarms the performance
    /// lane as any track selection does), and no caller can make a lead in
    /// name only. The lead clip is what `Shift+Tab` opens, `/` inserts into
    /// and the clip view shows; the arranger deliberately draws no "selected
    /// clip" (see `030-ui-design.md` § `draw_clip_body`). A change of lead
    /// goes through [`Self::change_lead_clip`].
    pub(super) fn select_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        lead: Option<(usize, Uuid)>,
    ) {
        if let Some((track_idx, _)) = lead
            && sequencer.selected_track_index() != Some(track_idx)
        {
            self.switch_track_workflow(sequencer, track_idx);
        }
        let new_id = lead.map(|(_, clip_id)| clip_id);
        self.change_lead_clip(sequencer, new_id, |sequencer| {
            sequencer.select_clip(new_id);
        });
    }

    /// Clears the lead clip selection (every selected clip, not just the
    /// lead), as a change of lead to none.
    pub(super) fn clear_clip_selection_workflow(&self, sequencer: &mut Sequencer) {
        self.change_lead_clip(sequencer, None, |sequencer| {
            sequencer.clear_clip_selection();
        });
    }

    /// Runs `select`, which makes `new_lead` the lead clip, and — when that is
    /// a different clip than before — first drops the old lead's event
    /// selection (while it is still the selected clip, so the mirror is
    /// republished empty rather than going stale), then mirrors the transport
    /// cursor into the new lead's cursor and tells the view
    /// (`UiEvent::LeadClipChanged`). See `archive/210-docked-clip-panel.md`.
    fn change_lead_clip(
        &self,
        sequencer: &mut Sequencer,
        new_lead: Option<Uuid>,
        select: impl FnOnce(&mut Sequencer),
    ) {
        let previous_lead = sequencer.selected_clip_id();
        let changed = previous_lead != new_lead;
        if changed {
            self.clear_event_selection_workflow(sequencer);
        }
        select(sequencer);
        if changed {
            sequencer.sync_selected_clip_cursor_with_arranger();
            self.send_lead_clip_changed_ui_event(sequencer.current_clip_view());
        }
    }

    /// Click-to-select: always fully deselects first (so a click away from a
    /// multi-selection built by `Shift+L/R` correctly deselects every member,
    /// not just the previous lead), then selects the clicked event, if any.
    /// `event_id: None` (an empty-grid click) leaves the selection cleared —
    /// this is also how "deselect all" is expressed.
    pub(super) fn select_clip_event_workflow(
        &self,
        sequencer: &mut Sequencer,
        event_id: Option<Uuid>,
    ) {
        self.clear_event_selection_workflow(sequencer);
        if let Some(id) = event_id {
            sequencer.select_event(Some(id));
            self.send_events_selected_ui_event([id]);
        }
        self.publish_event_selection(sequencer);
    }

    /// Marquee (rubber-band) select: recomputes the whole selection from the
    /// drag rectangle on every call (see `Clip::select_events_in_rect`) and
    /// emits UI events only for the diff. Auditions only the notes that just
    /// joined — earliest first, one per pitch, capped
    /// (`Sequencer::marquee_audition_note_ons`) — so sweeping across a melody
    /// plays it note by note, an unchanged rect stays silent, and a sweep that
    /// swallows a dense passage in one update sounds a handful, not all.
    pub(super) fn select_events_in_rect_workflow(
        &self,
        sequencer: &mut Sequencer,
        tick_min: i32,
        tick_max: i32,
        note_min: u8,
        note_max: u8,
    ) {
        let Some((deselected, selected)) =
            sequencer.select_events_in_rect(tick_min, tick_max, note_min, note_max)
        else {
            return;
        };
        let note_ons = sequencer.marquee_audition_note_ons(&selected);
        sequencer.preview_notes(&note_ons);
        self.send_events_deselected_ui_event(deselected);
        self.send_events_selected_ui_event(selected);
        self.publish_event_selection(sequencer);
    }

    /// Selects every `NoteOn` in the open clip and fans out the UI events.
    pub(super) fn select_all_events_workflow(&self, sequencer: &mut Sequencer) {
        if let Some(selected_ids) = sequencer.select_all_events() {
            self.send_events_selected_ui_event(selected_ids);
        }
        self.publish_event_selection(sequencer);
    }

    /// Mirrors whether the open clip has any selected events into
    /// `SharedAtomics::has_event_selection`, where the UI thread's key
    /// routing reads it (`ClipContext`, `Display::forward_input_event`): the
    /// selection itself lives here on the `"sequencer"` thread. Called from
    /// exactly the places where selection membership can change:
    /// `select_clip_event_workflow` / `select_all_events_workflow` /
    /// `select_events_in_rect_workflow` above, `clear_event_selection_workflow`
    /// below (ESC, a click on empty grid, and leaving the clip all come
    /// through it), and the `EventsModified` arm of `handle_edit_result` (the
    /// only `EditResult` variant that carries a selection, produced by
    /// delete/duplicate and their undo/redo), and `restore_view_workflow`,
    /// since a lead clip put back by an undo or redo carries its own. Not a
    /// blanket call after every command. It replaced `sync_clip_edit_view_state`, which flipped a
    /// `Clip` ⇄ `ClipEdit` view for the same bit; unlike it, this runs in
    /// every view, so the mirror is right when a clip is next opened.
    pub(super) fn publish_event_selection(&self, sequencer: &Sequencer) {
        self.has_event_selection
            .store(sequencer.selected_event_id().is_some(), Ordering::Relaxed);
    }

    /// Clears the open clip's event selection, fans out deselect events and
    /// publishes the now-empty selection.
    pub(super) fn clear_event_selection_workflow(&self, sequencer: &mut Sequencer) {
        if let Some(event_ids) = sequencer.clear_event_selection() {
            self.send_events_deselected_ui_event(event_ids);
        }
        self.publish_event_selection(sequencer);
    }

    // --- Cursor ---

    /// Moves the selected clip's cursor to `tick`. The clip cursor is its
    /// own: the arranger cursor stays put, so `Space` replays from it while you edit in the clip view
    /// (`archive/210-docked-clip-panel.md`).
    pub(super) fn set_clip_cursor_workflow(&self, sequencer: &mut Sequencer, tick: i32) {
        sequencer.set_selected_clip_cursor_tick(tick);
    }

    /// Re-syncs clip selection to whatever clip (if any) sits under the
    /// transport cursor's current position. Shared by keyboard cursor moves
    /// and click-to-place-cursor in the arranger. Also mirrors the moved
    /// cursor into the lead clip's own cursor when the lead stays the same
    /// (a change of lead does that itself): an arranger cursor move resets
    /// the clip cursor, but not the other way round
    /// (`archive/210-docked-clip-panel.md`).
    ///
    /// An open take session is cancelled first: the lead clip may change
    /// under it.
    pub(super) fn sync_clip_selection_to_cursor_workflow(&self, sequencer: &mut Sequencer) {
        self.cancel_take_framing_workflow(sequencer);
        let clip_id = sequencer.find_selected_track_clip_id_at_cursor();
        let lead = sequencer.selected_track_index().zip(clip_id);
        self.select_clip_workflow(sequencer, lead);
        sequencer.sync_selected_clip_cursor_with_arranger();
    }

    /// The arranger clip band press: puts the cursor on the clip's start,
    /// selects its track and the clip, and marquees exactly its span — the
    /// one-press equivalent of dragging a time selection edge to edge over
    /// it, so the marquee ops (`⌘/Ctrl+D`, `Delete`, `⌘C`/`⌘X`, `M`) get the
    /// whole clip without a hand-dragged range. Also re-run after a clip
    /// move so the clip lands in exactly this state wherever it now sits
    /// (its undo puts back the view from before, `restore_view_workflow`). See `020-views-and-state.md` § "Clip Band Press & Move
    /// Drag".
    ///
    /// Done here rather than in the view because the marquee is
    /// cursor-anchored: the view's `sync_time_selection_to_cursor` collapses
    /// it the frame the cursor or selected track changes, and this press
    /// changes both. Setting everything sequencer-side and sending the
    /// selection *after* the cursor/track (same FIFO channel) lets the
    /// `TimeSelectionSet` handler re-latch those watchers race-free. The
    /// cursor goes to the clip *start* — the marquee's anchor edge must sit
    /// on the cursor (`nudge_time_selection_edge`), and it's Ableton's
    /// "selection start is the insert marker".
    pub(super) fn select_clip_span_workflow(
        &self,
        sequencer: &mut Sequencer,
        track_idx: usize,
        clip_id: Uuid,
    ) {
        let Some((start, end)) = sequencer
            .clip_on(track_idx, clip_id)
            .map(|clip| (clip.start_tick(), clip.end_tick()))
        else {
            return;
        };

        sequencer.set_cursor_tick(start);
        self.switch_track_workflow(sequencer, track_idx);
        self.select_clip_workflow(sequencer, Some((track_idx, clip_id)));
        sequencer.sync_selected_clip_cursor_with_arranger();

        let rect = TimeSelectionRect::spanning(start, end, track_idx, track_idx);
        self.set_marquee_workflow(sequencer, rect);
    }

    // --- Undo and redo ---

    /// Puts back the view an undo step recorded (`UndoStep`): before the
    /// edit after an undo, after it after a redo, whatever the user did in
    /// between. The cursor first, then the track and the lead clip (the
    /// lead if it is still on that track, else none; a step with no track,
    /// or one whose track is gone, leaves the selection as it is —
    /// `reselect_surviving_clip`), the lead clip's cursor
    /// following the arranger cursor only if the lead or the cursor
    /// changed, then the marquee, sent last so the view re-latches the
    /// cursor and track it is anchored to (`020-views-and-state.md`). The
    /// focus, the layout, the loop and playback are untouched
    /// (`440-undo-view-state.md` decision 6); the performance lane is left
    /// by a track put back and never armed
    /// (`450-performance-lane-selection.md` decision 4).
    pub(super) fn restore_view_workflow(&self, sequencer: &mut Sequencer, view: ViewSnapshot) {
        // The cursor first, so a lead clip put back takes it as its own
        // (`change_lead_clip`). A lead that stays keeps its clip cursor
        // unless the arranger cursor moves, as everywhere else: an undo in
        // the clip view leaves the clip cursor where the edit's result put
        // it (`EventsModified`'s duplicate span).
        let cursor_moved = sequencer.cursor_tick() != view.cursor_tick;
        sequencer.set_cursor_tick(view.cursor_tick);
        if let Some(track_idx) = view.track.and_then(|id| sequencer.track_index_by_id(id)) {
            self.reselect_surviving_clip(sequencer, Some(track_idx), view.lead_clip);
        }
        if cursor_moved {
            sequencer.sync_selected_clip_cursor_with_arranger();
        }
        match view.marquee {
            Some(rect) => self.set_marquee_workflow(sequencer, rect),
            None => self.clear_marquee_workflow(sequencer),
        }
        // A lead clip put back carries its own event selection.
        self.publish_event_selection(sequencer);
    }

    // --- Marquee ---

    /// Puts the arranger marquee on `rect` (`UiEvent::TimeSelectionSet`),
    /// its track span always named. The sequencer's mirror of the marquee
    /// (`Sequencer::marquee`) is set in the same step, so an undo step's
    /// `after` sees it at once rather than a frame later, when the view
    /// would echo it back (`440-undo-view-state.md` decision 4). Sent after
    /// any cursor or track move of the same workflow: the view re-latches
    /// the cursor and track the marquee is anchored to when it arrives
    /// (`020-views-and-state.md` § Arranger Time Selection).
    pub(super) fn set_marquee_workflow(&self, sequencer: &mut Sequencer, rect: TimeSelectionRect) {
        sequencer.set_marquee(Some(rect));
        self.ui_event_tx
            .send(UiEvent::TimeSelectionSet { rect })
            .ok();
    }

    /// Drops the arranger marquee (`UiEvent::TimeSelectionCleared`), the
    /// mirror with it, as [`set_marquee_workflow`](Self::set_marquee_workflow).
    pub(super) fn clear_marquee_workflow(&self, sequencer: &mut Sequencer) {
        sequencer.set_marquee(None);
        self.ui_event_tx.send(UiEvent::TimeSelectionCleared).ok();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crossbeam_channel::Receiver;
    use uuid::Uuid;

    use crate::core::event_handlers::test_harness::{add_clip, harness};
    use crate::core::view_state::{Pane, ViewState};
    use crate::view::display::UiEvent;

    /// The lead clip each `LeadClipChanged` sent so far announced.
    fn lead_changes(ui_events: &Receiver<UiEvent>) -> Vec<Option<Uuid>> {
        ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::LeadClipChanged { clip_view } => {
                    Some(clip_view.map(|clip_view| clip_view.clip_id))
                }
                _ => None,
            })
            .collect()
    }

    /// Moving the cursor onto another clip makes it the lead clip, tells the
    /// view, and drops the old lead's event selection — republishing the
    /// mirror, which used to go stale and route the arrows to note nudging in
    /// a clip showing nothing selected.
    #[test]
    fn a_new_lead_clip_is_announced_and_drops_the_old_event_selection() {
        let mut h = harness();
        let (first, note) = add_clip(&mut h.sequencer, 0);
        let (second, _) = add_clip(&mut h.sequencer, 1920);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.sequencer.select_event(Some(note));
        h.handlers.publish_event_selection(&h.sequencer);
        assert!(h.has_event_selection.load(Ordering::Relaxed));
        assert_eq!(lead_changes(&h.ui_events), vec![Some(first)]);

        h.cursor_tick.store(2000, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);

        assert_eq!(h.sequencer.selected_clip_id(), Some(second));
        assert_eq!(lead_changes(&h.ui_events), vec![Some(second)]);
        assert!(!h.has_event_selection.load(Ordering::Relaxed));
        let old = h.sequencer.tracks()[0].get_clip_by_id(first).unwrap();
        assert!(old.selected_event_ids().is_empty());
    }

    /// A cursor move within the lead clip announces nothing, keeps its event
    /// selection, and mirrors the cursor into the clip's own cursor (in the
    /// clip's event ticks) — the clip view has no cursor of its own.
    #[test]
    fn a_move_within_the_lead_clip_mirrors_the_cursor_and_keeps_the_selection() {
        let mut h = harness();
        let (clip, note) = add_clip(&mut h.sequencer, 960);
        h.cursor_tick.store(960, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.sequencer.select_event(Some(note));
        h.handlers.publish_event_selection(&h.sequencer);
        assert_eq!(lead_changes(&h.ui_events), vec![Some(clip)]);

        h.cursor_tick.store(1440, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);

        assert!(lead_changes(&h.ui_events).is_empty());
        assert!(h.has_event_selection.load(Ordering::Relaxed));
        assert_eq!(h.sequencer.selected_clip_cursor_tick(), Some(480));
    }

    /// A clip-view cursor move stays in the clip: the arranger cursor is
    /// left where it is, so `Space` keeps replaying from it while the clip
    /// view edits (`archive/210-docked-clip-panel.md`).
    #[test]
    fn a_clip_view_cursor_move_leaves_the_arranger_cursor_alone() {
        let mut h = harness();
        add_clip(&mut h.sequencer, 1920);
        h.cursor_tick.store(1920, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Clip);
        while h.transport_commands.try_recv().is_ok() {}

        h.handlers.set_clip_cursor_workflow(&mut h.sequencer, 480);

        assert_eq!(h.sequencer.selected_clip_cursor_tick(), Some(480));
        assert!(h.transport_commands.try_recv().is_err());
    }

    /// Clicking back into the docked clip pane keeps the clip cursor where
    /// the clip view last put it — focusing used to re-sync it from the
    /// arranger cursor.
    #[test]
    fn focusing_the_clip_pane_keeps_the_clip_cursor() {
        let mut h = harness();
        add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Clip);
        h.handlers.set_clip_cursor_workflow(&mut h.sequencer, 960);
        h.handlers
            .focus_pane_workflow(&mut h.sequencer, Pane::Arranger);

        h.handlers.focus_pane_workflow(&mut h.sequencer, Pane::Clip);

        assert_eq!(h.sequencer.selected_clip_cursor_tick(), Some(960));
    }

    /// An arranger cursor move still resets the clip cursor (the mirror runs
    /// one way), including the clip header press that lands the cursor on
    /// the lead clip's own start.
    #[test]
    fn a_clip_span_press_mirrors_the_cursor_into_the_lead_clip() {
        let mut h = harness();
        let (clip, _) = add_clip(&mut h.sequencer, 1920);
        h.cursor_tick.store(1920, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Clip);
        h.handlers.set_clip_cursor_workflow(&mut h.sequencer, 960);
        h.handlers.set_view_state(ViewState::Arranger);

        h.handlers
            .select_clip_span_workflow(&mut h.sequencer, 0, clip);

        assert_eq!(h.sequencer.selected_clip_cursor_tick(), Some(0));
    }

    /// Cycling the track selection wraps at both ends.
    #[test]
    fn track_cycling_wraps_at_both_ends() {
        let mut h = harness();
        let last = h.sequencer.tracks().len() - 1;

        h.handlers
            .cycle_selected_track_workflow(&mut h.sequencer, -1);
        assert_eq!(h.sequencer.selected_track_index(), Some(last));

        h.handlers
            .cycle_selected_track_workflow(&mut h.sequencer, 1);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));

        h.handlers
            .cycle_selected_track_workflow(&mut h.sequencer, 1);
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
    }

    /// From the performance lane, which has no place in the cycle, the next
    /// track is the first and the previous the last.
    #[test]
    fn track_cycling_from_the_performance_lane_enters_at_either_end() {
        let mut h = harness();
        let last = h.sequencer.tracks().len() - 1;

        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        h.handlers
            .cycle_selected_track_workflow(&mut h.sequencer, 1);
        assert_eq!(h.sequencer.selected_track_index(), Some(0));
        assert!(!h.sequencer.is_performance_lane_armed());

        h.handlers
            .select_performance_lane_workflow(&mut h.sequencer);
        h.handlers
            .cycle_selected_track_workflow(&mut h.sequencer, -1);
        assert_eq!(h.sequencer.selected_track_index(), Some(last));
    }

    /// Moving off every clip announces "no lead clip".
    #[test]
    fn moving_off_every_clip_announces_no_lead_clip() {
        let mut h = harness();
        let (clip, _) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        assert_eq!(lead_changes(&h.ui_events), vec![Some(clip)]);

        h.cursor_tick.store(5000, Ordering::Relaxed);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);

        assert_eq!(h.sequencer.selected_clip_id(), None);
        assert_eq!(lead_changes(&h.ui_events), vec![None]);
    }
}
