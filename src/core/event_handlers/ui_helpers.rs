//! One-line `EventHandlers` helpers that each fan out one kind of
//! [`UiEvent`], so the workflow files read as intent. The clip senders take
//! a slice, since an edit reports its clips in buckets; a single clip is
//! `slice::from_ref`.

use crate::metadata::clip_view::ClipView;

use super::*;

impl EventHandlers {
    /// Fans out [`UiEvent::TrackSelected`].
    pub(super) fn send_track_selected_ui_event(&self, track_idx: usize) {
        self.ui_event_tx
            .send(UiEvent::TrackSelected { track_idx })
            .ok();
    }

    /// Fans out [`UiEvent::ClipUpdated`] for the selected clip.
    pub(super) fn send_selected_clip_updated_ui_event(&self, sequencer: &Sequencer) {
        if let Some(clip) = sequencer.selected_clip_metadata() {
            self.ui_event_tx.send(UiEvent::ClipUpdated { clip }).ok();
        }
    }

    /// Fans out [`UiEvent::ClipUpdated`] for each of `clips`.
    pub(super) fn send_clips_updated_ui_event(&self, clips: &[ClipMetadata]) {
        for clip in clips {
            self.ui_event_tx
                .send(UiEvent::ClipUpdated { clip: clip.clone() })
                .ok();
        }
    }

    /// Fans out [`UiEvent::ClipAdded`] for each of `clips`.
    pub(super) fn send_clips_added_ui_event(&self, clips: &[ClipMetadata]) {
        for clip in clips {
            self.ui_event_tx
                .send(UiEvent::ClipAdded { clip: clip.clone() })
                .ok();
        }
    }

    /// Sends a passing footer message ([`UiEvent::Status`]).
    pub(super) fn send_status_ui_event(&self, message: String) {
        self.ui_event_tx.send(UiEvent::Status { message }).ok();
    }

    /// Fans out [`UiEvent::ClipRemoved`] for each of `clips`.
    pub(super) fn send_clips_removed_ui_event(&self, clips: &[ClipMetadata]) {
        for clip in clips {
            self.ui_event_tx
                .send(UiEvent::ClipRemoved { clip: clip.clone() })
                .ok();
        }
    }

    /// Fans out [`UiEvent::ClipEntered`].
    pub(super) fn send_clip_entered_ui_event(&self, clip_view: Option<ClipView>) {
        self.ui_event_tx
            .send(UiEvent::ClipEntered { clip_view })
            .ok();
    }

    /// Fans out [`UiEvent::ClipExited`].
    pub(super) fn send_clip_exited_ui_event(&self) {
        self.ui_event_tx.send(UiEvent::ClipExited).ok();
    }

    /// Fans out [`UiEvent::LeadClipChanged`].
    pub(super) fn send_lead_clip_changed_ui_event(&self, clip_view: Option<ClipView>) {
        self.ui_event_tx
            .send(UiEvent::LeadClipChanged { clip_view })
            .ok();
    }

    /// Fans out [`UiEvent::EventsUpdated`].
    pub(super) fn send_events_updated_ui_event(&self, clip_view: ClipView) {
        self.ui_event_tx
            .send(UiEvent::EventsUpdated { clip_view })
            .ok();
    }

    /// Fans out [`UiEvent::EventSelected`] for each of `event_ids`.
    pub(super) fn send_events_selected_ui_event(&self, event_ids: impl IntoIterator<Item = Uuid>) {
        for event_id in event_ids {
            self.ui_event_tx
                .send(UiEvent::EventSelected { event_id })
                .ok();
        }
    }

    /// Fans out [`UiEvent::EventDeselected`] for each of `event_ids`.
    pub(super) fn send_events_deselected_ui_event(
        &self,
        event_ids: impl IntoIterator<Item = Uuid>,
    ) {
        for event_id in event_ids {
            self.ui_event_tx
                .send(UiEvent::EventDeselected { event_id })
                .ok();
        }
    }

    /// Fans out [`UiEvent::TimeSelectionSet`]: the marquee over `[start, end)`,
    /// on `track_span` (inclusive) or on every track.
    pub(super) fn send_time_selection_set_ui_event(
        &self,
        start: i32,
        end: i32,
        track_span: Option<(usize, usize)>,
    ) {
        self.ui_event_tx
            .send(UiEvent::TimeSelectionSet {
                start,
                end,
                track_span,
            })
            .ok();
    }

    /// Fans out [`UiEvent::PerformanceLaneSelected`].
    pub(super) fn send_performance_lane_selected_ui_event(&self) {
        self.ui_event_tx.send(UiEvent::PerformanceLaneSelected).ok();
    }
}
