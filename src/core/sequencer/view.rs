//! `Sequencer` → render-thread snapshot builders.
//!
//! These turn the live model into the flat values `Display` consumes:
//! [`ClipView`] (a whole piano-roll snapshot) and
//! [`ClipMetadata`] (an arranger clip rectangle). Sent in `UiEvent`s; see
//! `020-views-and-state.md`.
//!
//! [`ClipMetadata`]: crate::metadata::clip_metadata::ClipMetadata

use crate::metadata::{clip_metadata::ClipMetadata, clip_view::ClipView};

use super::Sequencer;

impl Sequencer {
    // --- View/data helpers
    /// A snapshot of the currently open clip, or `None` if none is open.
    pub(crate) fn current_clip_view(&self) -> Option<ClipView> {
        self.selected_clip().map(ClipView::from_clip)
    }

    /// Arranger metadata for the lead-selected clip, or `None`.
    pub(crate) fn selected_clip_metadata(&self) -> Option<ClipMetadata> {
        Some(ClipMetadata::from_clip(
            self.selected_track_index()?,
            self.selected_clip()?,
        ))
    }
}
