//! `EventHandlers` workflows for a clip appearing on / leaving a track through
//! `CommitClipEdit` — `/`'s `Commit` binding, and the undo/redo of it. See
//! `050-undo-redo.md`, `100-running-capture.md`.

use std::slice;

use super::*;

impl EventHandlers {
    // --- Clip operations ---

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
