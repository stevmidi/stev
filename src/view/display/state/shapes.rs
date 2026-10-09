//! `Display`'s clip / event shape lists — the render models reconciled against
//! `UiEvent`s (add / update / remove; select for events). The shapes are `ClipShape` /
//! `EventShape` from [`crate::shapes`].

use super::*;

impl Display {
    // --- State mutators/loaders ---
    /// Adds (or replaces) the clip's shape.
    pub(super) fn add_clip_shape(&mut self, clip: ClipMetadata) {
        self.remove_clip_shape(clip.track_idx, clip.clip_id);
        self.render.clip_shapes.push(ClipShape::from_metadata(clip));
    }

    /// Removes the shape for `(track_idx, clip_id)`, if present.
    pub(super) fn remove_clip_shape(&mut self, track_idx: usize, clip_id: Uuid) {
        if let Some(shape_idx) = self
            .render
            .clip_shapes
            .iter()
            .position(|shape| shape.matches(track_idx, clip_id))
        {
            self.render.clip_shapes.remove(shape_idx);
        }
    }

    /// Updates the clip's shape bounds / mute / thumbnail in place.
    pub(super) fn update_clip_shape(&mut self, clip: ClipMetadata) {
        if let Some(shape) = self
            .render
            .clip_shapes
            .iter_mut()
            .find(|shape| shape.matches(clip.track_idx, clip.clip_id))
        {
            shape.update_ticks(clip.start_tick, clip.end_tick);
            shape.set_muted(clip.muted);
            shape.update_note_thumbnails(clip.note_thumbnails);
        }
    }

    /// Replaces the whole event-shape list from a fresh event snapshot of the
    /// lead clip, reusing the list's allocation.
    pub(super) fn rebuild_event_shapes(&mut self, events: Vec<EventMetadata>) {
        self.render.event_shapes.clear();
        self.render
            .event_shapes
            .extend(events.into_iter().map(EventShape::from_metadata));
    }

    /// Makes `clip_view` the lead clip the clip view shows (`ClipEntered`,
    /// `LeadClipChanged`): its timing, its event shapes (none with no clip —
    /// the pane then reads "No clip at the cursor") and its home framing.
    pub(super) fn show_lead_clip(&mut self, clip_view: Option<ClipView>) {
        self.lead_clip_time = clip_view.as_ref().map(ClipTimeAtomics::of);
        match clip_view {
            Some(clip_view) => self.rebuild_event_shapes(clip_view.events),
            None => self.render.event_shapes.clear(),
        }
        // Each clip starts at its home framing.
        self.reset_clip_zoom();
        self.in_pane(Pane::Clip, Self::frame_clip_home);
    }

    /// Sets the selected flag on the event's shape, if present.
    pub(super) fn set_event_shape_selected(&mut self, event_id: Uuid, selected: bool) {
        if let Some(shape) = self
            .render
            .event_shapes
            .iter_mut()
            .find(|shape| shape.matches(event_id))
        {
            shape.set_selected(selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::{metadata::clip_metadata::ClipMetadata, shapes::clip_shape::ClipShape};

    fn metadata(muted: bool) -> ClipMetadata {
        ClipMetadata {
            track_idx: 0,
            clip_id: Uuid::new_v4(),
            start_tick: 0,
            end_tick: 960,
            note_thumbnails: Vec::new(),
            muted,
        }
    }

    #[test]
    fn shape_built_from_loaded_metadata_carries_the_muted_flag() {
        // Regression: a project reload rebuilds every clip shape via
        // `add_clip_shape`, which used to always start `is_muted: false`
        // regardless of the persisted value, so muted clips lost their
        // dimmed appearance until something else touched the shape.
        assert!(ClipShape::from_metadata(metadata(true)).is_muted());
        assert!(!ClipShape::from_metadata(metadata(false)).is_muted());
    }
}
