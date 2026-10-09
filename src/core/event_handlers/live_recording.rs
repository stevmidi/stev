//! `EventHandlers` workflows for the dedicated live-record path — arm / start /
//! end a take and fan out the `RecordingStarted`/`Completed`/`Canceled` UI
//! events. See `090-live-recording.md`.

use undo::Record;

use crate::core::sequencer::{CommitClipEdit, LiveRecResult, SequencerEdit};

use super::*;

impl EventHandlers {
    /// Arms a live-record take at the current odometer value and fans out
    /// `RecordingStarted`.
    pub(super) fn start_live_recording_workflow(&self, sequencer: &mut Sequencer) {
        // The odometer is frozen while stopped, so arming here and pressing play
        // later still starts the take at play. No run-state branch needed.
        let elapsed_tick = sequencer.elapsed_tick();

        if let Some(metadata) = sequencer.start_live_recording(elapsed_tick) {
            self.ui_event_tx
                .send(UiEvent::RecordingStarted { clip: metadata })
                .ok();
        }
    }

    /// Ends the take: commits or discards it, selects the new clip if the
    /// cursor is on it, and fans out the completed / canceled UI events. A
    /// completed take is recorded on `undo_record` as a `CommitClipEdit` so
    /// ⌘Z removes the clip — see `090-live-recording.md`.
    pub(crate) fn end_live_recording_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
    ) {
        let Some(result) = sequencer.end_live_recording() else {
            return;
        };

        match result {
            LiveRecResult::Completed { clip } => {
                if sequencer.cursor_tick() >= clip.start_tick
                    && sequencer.cursor_tick() < clip.end_tick
                {
                    self.select_clip_workflow(sequencer, Some(clip.clip_id));
                }
                self.ui_event_tx.send(UiEvent::RecordingCompleted).ok();

                // The clip is already on the track and its shape already in
                // the UI (from `RecordingStarted`), so the edit's forward
                // result is deliberately not handled — the first `edit()`
                // only finds the clip present. Undo/redo go through
                // `handle_edit_result` as usual.
                if let Some(edit) =
                    CommitClipEdit::from_placed_clip(sequencer, clip.track_idx, clip.clip_id)
                {
                    undo_record.edit(sequencer, SequencerEdit::CommitClip(edit));
                }

                self.ui_event_tx.send(UiEvent::ClipUpdated { clip }).ok();
            }
            LiveRecResult::Canceled {
                track_idx,
                rec_clip_id,
            } => {
                self.ui_event_tx
                    .send(UiEvent::RecordingCanceled {
                        track_idx,
                        clip_id: rec_clip_id,
                    })
                    .ok();
            }
        }

        // No-ops when nothing is selected (cursor on empty space).
        self.send_selected_clip_updated_ui_event(sequencer);
    }
}
