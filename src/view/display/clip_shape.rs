//! The arranger's render model for one clip rectangle.
//!
//! `Display` keeps a `Vec<ClipShape>` (`view/display/state/shapes.rs`) it
//! reconciles against incoming [`ClipMetadata`]:
//! bounds, the muted flag and the precomputed thumbnail live here, so the
//! per-frame paint reads a plain struct rather than recomputing geometry. The
//! fill colour is the track's, which `Display` looks up from the active theme
//! at paint time, so a theme switch needs no pass over the shapes.
//! `EventShape` is the piano-roll equivalent.

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;

/// One clip rectangle as the arranger draws it. See the module docs.
#[derive(Debug)]
pub(crate) struct ClipShape {
    /// Track lane the clip sits in.
    track_idx: usize,
    /// The clip's id.
    clip_id: Uuid,
    /// Left edge, in arrangement ticks.
    start_tick: i32,
    /// Right edge, in arrangement ticks.
    end_tick: i32,
    /// Whether the clip is muted (drawn dimmed).
    is_muted: bool,
    /// (x_start_frac, x_end_frac, pitch_frac) triples for NoteOn events; all in 0.0..=1.0.
    note_thumbnails: Vec<(f32, f32, f32)>,
}

impl ClipShape {
    /// The shape of `clip`, in sync with it (bounds, mute, thumbnail).
    pub(crate) fn from_metadata(clip: ClipMetadata) -> Self {
        ClipShape {
            track_idx: clip.track_idx,
            clip_id: clip.clip_id,
            start_tick: clip.start_tick,
            end_tick: clip.end_tick,
            is_muted: clip.muted,
            note_thumbnails: clip.note_thumbnails,
        }
    }

    /// Sets the muted flag.
    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.is_muted = muted;
    }

    /// Whether this shape represents the clip `clip_id` on track `track_idx` —
    /// the reconcile key.
    pub(crate) fn matches(&self, track_idx: usize, clip_id: Uuid) -> bool {
        self.track_idx == track_idx && self.clip_id == clip_id
    }

    /// Updates the horizontal bounds.
    pub(crate) fn update_ticks(&mut self, start_tick: i32, end_tick: i32) {
        self.start_tick = start_tick;
        self.end_tick = end_tick;
    }

    /// Swaps in a fresh thumbnail triple set.
    pub(crate) fn update_note_thumbnails(&mut self, thumbnails: Vec<(f32, f32, f32)>) {
        self.note_thumbnails = thumbnails;
    }

    /// Track lane.
    pub(crate) fn track_idx(&self) -> usize {
        self.track_idx
    }

    /// Moves the shape to the track now at `track_idx` — its own track,
    /// renumbered by a track add / remove above it.
    pub(crate) fn set_track_idx(&mut self, track_idx: usize) {
        self.track_idx = track_idx;
    }

    /// The clip's id.
    pub(crate) fn clip_id(&self) -> Uuid {
        self.clip_id
    }

    /// Left edge, in arrangement ticks.
    pub(crate) fn start_tick(&self) -> i32 {
        self.start_tick
    }

    /// Right edge, in arrangement ticks.
    pub(crate) fn end_tick(&self) -> i32 {
        self.end_tick
    }

    /// Whether the shape is marked muted.
    pub(crate) fn is_muted(&self) -> bool {
        self.is_muted
    }

    /// The thumbnail triples — see the field.
    pub(crate) fn note_thumbnails(&self) -> &[(f32, f32, f32)] {
        &self.note_thumbnails
    }
}
