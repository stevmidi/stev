//! `EventHandlers` workflows for entering and leaving the piano-roll views
//! (`Arranger` ⇄ `Clip`). Neither touches the loop region or either cursor:
//! the clip view is a view of the lead clip, not a transport context. See
//! `020-views-and-state.md` and `archive/210-docked-clip-panel.md`.

use super::*;

impl EventHandlers {
    // --- Clip view ---

    /// `Arranger` → `Clip`: fans out `ClipEntered` for the lead clip. With
    /// none it still shows the panel (empty, "no clip at the cursor") — it
    /// never creates a clip (`⇧⌘M` does).
    /// The loop region and both cursors are left alone — the clip cursor
    /// stays where the clip view last put it, or where the last arranger
    /// cursor move mirrored it.
    pub(super) fn enter_clip_workflow(&self, sequencer: &mut Sequencer) {
        self.set_view_state(ViewState::Clip);

        self.send_clip_entered_ui_event(sequencer.current_clip_view());
    }

    /// `Clip` → `Arranger`: drops the event selection and fans out
    /// `ClipExited`. The arranger cursor is where it was before the clip view
    /// was entered; the clip view never moves it.
    pub(super) fn exit_clip_workflow(&self, sequencer: &mut Sequencer) {
        self.set_view_state(ViewState::Arranger);
        // Leaving to Arranger always drops any event selection (and so
        // publishes an empty one), so a later re-entry starts clean —
        // otherwise a stale selection would route the arrows to note
        // nudging in a clip that shows nothing selected.
        self.clear_event_selection_workflow(sequencer);

        self.sync_clip_post_edit_workflow(sequencer);
        self.send_clip_exited_ui_event();
    }

    /// A click in the unfocused docked pane: moves the keyboard focus to
    /// `pane` without showing or hiding anything. Into the clip view: Grid
    /// cursor mode, as on entry, with the clip cursor left where it is. Back to the
    /// arranger: the event selection is dropped, as on exit — the arranger's
    /// keys must not act on notes. A no-op when `pane` already has the
    /// focus. See
    /// `archive/210-docked-clip-panel.md`.
    pub(super) fn focus_pane_workflow(&self, sequencer: &mut Sequencer, pane: Pane) {
        match (self.view_state(), pane) {
            (ViewState::Arranger, Pane::Clip) => {
                self.set_view_state(ViewState::Clip);
            }
            (ViewState::Clip, Pane::Arranger) => {
                self.set_view_state(ViewState::Arranger);
                self.clear_event_selection_workflow(sequencer);
            }
            _ => {}
        }
    }

    // --- Events ---

    /// Re-renders the open clip after an edit that changed its events.
    pub(super) fn sync_clip_post_edit_workflow(&self, sequencer: &mut Sequencer) {
        if let Some(clip_view) = sequencer.current_clip_view() {
            self.send_events_updated_ui_event(clip_view);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::core::event_handlers::test_harness::{add_clip, harness};
    use crate::core::view_state::ViewState;
    use crate::view::display::UiEvent;

    /// Regression: `Shift+Tab` is show/hide only. Entering the clip view
    /// with no clip under the cursor creates nothing — an empty clip is
    /// `⇧⌘M`'s job — but still fans out an empty `ClipEntered`, so a hidden
    /// panel shows ("No clip at the cursor") instead of the keyboard moving
    /// into an invisible clip view.
    #[test]
    fn entering_with_no_lead_clip_shows_an_empty_panel() {
        let mut h = harness();
        h.cursor_tick.store(960, Ordering::Relaxed);

        h.handlers.enter_clip_workflow(&mut h.sequencer);

        assert_eq!(h.handlers.view_state(), ViewState::Clip);
        assert_eq!(h.sequencer.selected_clip_id(), None);
        assert!(h.sequencer.tracks()[0].clips().is_empty());
        let events: Vec<_> = h.ui_events.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, UiEvent::ClipAdded { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::ClipEntered { clip_view: None }))
        );
    }

    /// Undoing the commit of the clip open in the clip view leaves to the
    /// arranger (`ClipExited`), drops the lead clip and removes its shape.
    #[test]
    fn uncommitting_the_open_clip_exits_to_the_arranger() {
        let mut h = harness();
        let (clip_id, _) = add_clip(&mut h.sequencer, 0);
        h.handlers
            .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
        h.handlers.set_view_state(ViewState::Clip);
        let metadata = h.sequencer.selected_clip_metadata().unwrap();
        h.sequencer.tracks_mut()[0].remove_clip_by_id(clip_id);
        while h.ui_events.try_recv().is_ok() {}

        h.handlers
            .uncommit_clip_workflow(&mut h.sequencer, &metadata);

        assert_eq!(h.handlers.view_state(), ViewState::Arranger);
        assert_eq!(h.sequencer.selected_clip_id(), None);
        let events: Vec<_> = h.ui_events.try_iter().collect();
        assert!(events.iter().any(|e| matches!(e, UiEvent::ClipExited)));
        assert!(matches!(events.last(), Some(UiEvent::ClipRemoved { .. })));
    }
}
