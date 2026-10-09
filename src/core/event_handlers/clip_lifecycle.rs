//! `EventHandlers` workflows for a clip appearing on / leaving a track through
//! `CommitClipEdit` — `/`'s `Commit` binding, and the undo/redo of it and of
//! a live take. See
//! `050-undo-redo.md`, `090-live-recording.md`, `100-running-capture.md`.

use std::slice;

use super::*;

impl EventHandlers {
    // --- Clip operations ---

    /// A committed clip is on its track (`EditResult::ClipCommitted` — the
    /// running-capture commit, or a redo of any commit): adds its shape,
    /// selects it, and re-seeks the sequencer.
    pub(super) fn commit_clip_workflow(&self, sequencer: &mut Sequencer, clip: &ClipMetadata) {
        self.send_clips_added_ui_event(slice::from_ref(clip));
        self.select_clip_workflow(sequencer, Some(clip.clip_id));
        sequencer.reset();
    }

    /// Undo of a clip commit (`EditResult::ClipUncommitted`): the clip has
    /// already been lifted off its track. If it is the clip open in the clip
    /// view — the user opened the clip after committing it — leave to the
    /// Arranger through `exit_clip_workflow` (the lifted clip has no view to
    /// refresh). Then drop it from the selection (telling the view the lead
    /// clip is gone) and, last, remove its shape.
    pub(super) fn uncommit_clip_workflow(&self, sequencer: &mut Sequencer, clip: &ClipMetadata) {
        sequencer.reset();

        let was_selected = sequencer.selected_clip_id() == Some(clip.clip_id);

        if was_selected {
            if self.view_state() == ViewState::Clip {
                self.exit_clip_workflow(sequencer);
            }
            self.select_clip_workflow(sequencer, None);
            self.send_lead_clip_changed_ui_event(None);
        }

        self.send_clips_removed_ui_event(slice::from_ref(clip));
    }
}
