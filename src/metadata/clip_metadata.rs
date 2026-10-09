//! The render-side snapshot of a clip.
//!
//! `Display` never reads the sequencer's live [`Clip`]s — it works from
//! `ClipMetadata`, a flat, `PartialEq` value carrying just what the arranger
//! draws (bounds, mute, a precomputed thumbnail). The sequencer builds one with
//! [`ClipMetadata::from_clip`] and ships it in a `UiEvent` whenever the clip
//! changes. See `020-views-and-state.md`.

use uuid::Uuid;

use crate::models::{clip::Clip, event::EventType};

/// A flat, comparable snapshot of a clip for the render thread. See the module
/// docs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClipMetadata {
    /// Track the clip is on.
    pub(crate) track_idx: usize,
    /// The clip's id.
    pub(crate) clip_id: Uuid,
    /// Clip start on the arrangement timeline.
    pub(crate) start_tick: i32,
    /// Clip end on the arrangement timeline.
    pub(crate) end_tick: i32,
    /// Normalized (x_start_frac, x_end_frac, pitch_frac) triples for NoteOn events, used to draw
    /// piano-roll thumbnails on arranger clips.  All components are in 0.0..=1.0.
    pub(crate) note_thumbnails: Vec<(f32, f32, f32)>,
    /// Whether the clip is muted (drawn dimmed).
    pub(crate) muted: bool,
}

impl ClipMetadata {
    /// Builds the snapshot, precomputing the thumbnail triples.
    pub(crate) fn from_clip(track_idx: usize, clip: &Clip) -> Self {
        let note_thumbnails = build_note_thumbnails(clip);

        Self {
            track_idx,
            clip_id: clip.id(),
            start_tick: clip.start_tick(),
            end_tick: clip.end_tick(),
            note_thumbnails,
            muted: clip.is_muted(),
        }
    }
}

/// Extracts `(on_tick, end_tick, note_number)` for every `NoteOn` and hands
/// off to [`note_thumbnails_from_spans`] for the actual overlap-filter/clamp
/// math — see that function's doc for why.
pub(crate) fn build_note_thumbnails(clip: &Clip) -> Vec<(f32, f32, f32)> {
    let note_ons = clip
        .events()
        .iter()
        .filter(|e| e.event_type() == Some(EventType::NoteOn) && !e.is_muted())
        .filter_map(|e| e.note_number().map(|n| (e.tick(), e.end_tick(), n)));

    note_thumbnails_from_spans(note_ons, clip.region().start(), clip.region().end())
}

/// Compute (x_start_frac, x_end_frac, pitch_frac) thumbnail triples for every
/// `(on_tick, end_tick, note_number)` note span overlapping
/// `[region_start, region_end)`. Both x fracs are the note's phase position
/// within the region (0.0 = start, 1.0 = end); `pitch_frac` is the note number
/// normalised to the clip's own pitch range.
///
/// Shared by [`build_note_thumbnails`] (reading straight from a live [`Clip`])
/// and the view's `EventsUpdated` refresh (`thumbnails_from_event_metadata` in
/// `view/display/state/ui_events.rs`, reading from the flat `EventMetadata`
/// snapshot the piano roll already has) — the two used to each hand-roll this
/// independently, and only one of them got the overlap fix below when it was
/// first needed, which is exactly how a stale-thumbnail regression like that
/// slips back in. Route any future caller through here too.
///
/// Notes are filtered by whether their `[on_tick, end_tick)` *span* overlaps
/// the region, rather than run through [`Clip::phase_from_event_tick`]'s
/// modulo wrap or tested by onset alone: a non-destructive split/duplicate/
/// delete (`050-undo-redo.md`) shrinks a clip's `region` while leaving its
/// *full* original event list untouched (`clip_edits/split.rs`), so an
/// unfiltered pass would wrap the sibling half's now-out-of-region events
/// straight back into this clip's visible phase range, while an onset-only
/// test would miss two overlap cases whose onset falls *outside* the region:
/// a note that starts inside and sustains past `region_end` (drawn clamped to
/// this clip's own end), and — the one onset-only filtering used to drop — a
/// note that starts *before* `region_start` and sustains into it, the visual
/// counterpart of the `Track::pending_note_ons` chase-back-in at playback
/// time (also `050-undo-redo.md`): its onset belongs to the sibling half, but
/// the sound continues into this one, so the thumbnail draws it starting at
/// this clip's own left edge (phase 0).
pub(crate) fn note_thumbnails_from_spans(
    notes: impl Iterator<Item = (i32, i32, u8)>,
    region_start: i32,
    region_end: i32,
) -> Vec<(f32, f32, f32)> {
    let region_len = region_end - region_start;
    if region_len <= 0 {
        return Vec::new();
    }
    let rlen = region_len as f32;

    notes
        .filter(|&(tick, end_tick, _)| tick < region_end && end_tick > region_start)
        .map(|(tick, end_tick, note)| {
            let start_phase = (tick - region_start).clamp(0, region_len);
            let end_phase = (end_tick - region_start).clamp(0, region_len);
            let x_start = (start_phase as f32 / rlen).clamp(0.0, 1.0);
            let x_end = (end_phase as f32 / rlen).clamp(0.0, 1.0);
            (x_start, x_end, pitch_frac(note))
        })
        .collect()
}

/// Lowest note of the thumbnail pitch window (C2).
const THUMBNAIL_PITCH_MIN: u8 = 36;
/// Highest note of the thumbnail pitch window (C6).
const THUMBNAIL_PITCH_MAX: u8 = 96;

/// `note`'s vertical position in a clip thumbnail, `0.0..=1.0`. The window is
/// a fixed absolute one (C2–C6, MIDI 36–96) shared across all clips, so
/// y-position is consistent and comparable between clips — a high note in
/// one clip always sits higher on screen than a lower note in another. Notes
/// outside the window clamp to its edges.
pub(crate) fn pitch_frac(note: u8) -> f32 {
    let range = (THUMBNAIL_PITCH_MAX - THUMBNAIL_PITCH_MIN) as f32;
    (note.saturating_sub(THUMBNAIL_PITCH_MIN) as f32 / range).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use crate::models::clip::Clip;
    use crate::models::event::Event;

    use super::*;

    fn clip_with_region(region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    /// Regression: a non-destructive split/duplicate/delete-in-range shrinks a
    /// clip's `region` but leaves the full original event list untouched
    /// (`clip_edits/split.rs`), so a note belonging to the sibling half must not
    /// wrap back into this clip's thumbnail via `phase_from_event_tick`'s modulo.
    #[test]
    fn build_note_thumbnails_excludes_events_after_current_region_end() {
        let mut clip = clip_with_region(0, 480);
        clip.add_event(Event::new(0, 100, vec![0x90, 60, 100])); // this half
        clip.add_event(Event::new(600, 100, vec![0x90, 72, 100])); // sibling half

        let thumbnails = build_note_thumbnails(&clip);

        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].0, 0.0);
    }

    /// A muted note never sounds, so it has no business in the preview either.
    #[test]
    fn build_note_thumbnails_excludes_muted_notes() {
        let mut clip = clip_with_region(0, 480);
        clip.add_event(Event::new(0, 100, vec![0x90, 60, 100]));
        let mut muted_on = Event::new(200, 100, vec![0x90, 72, 100]);
        muted_on.set_muted(true);
        clip.add_event(muted_on);

        let thumbnails = build_note_thumbnails(&clip);

        assert_eq!(thumbnails.len(), 1);
    }

    /// Same bleed, other direction: an event before the clip's current region
    /// start belongs to the sibling half produced on the other side of a
    /// split, and — unlike the chased-in case further down — its sustain
    /// doesn't reach into this region at all, so it's excluded outright.
    #[test]
    fn build_note_thumbnails_excludes_events_before_current_region_start() {
        let mut clip = clip_with_region(480, 960);
        clip.add_event(Event::new(200, 100, vec![0x90, 60, 100])); // sibling half, ends at 300 — no overlap
        clip.add_event(Event::new(600, 100, vec![0x90, 72, 100])); // this half

        let thumbnails = build_note_thumbnails(&clip);

        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].0, (600 - 480) as f32 / 480.0);
    }

    /// A note that starts inside the region but sustains past it (its note-off
    /// still lives in the full, non-destructively-shared event list) draws
    /// clamped to this clip's own end rather than wrapping to a small phase.
    #[test]
    fn build_note_thumbnails_clamps_sustain_crossing_region_end() {
        let mut clip = clip_with_region(0, 480);
        clip.add_event(Event::new(400, 200, vec![0x90, 60, 100]));

        let thumbnails = build_note_thumbnails(&clip);

        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].1, 1.0);
    }

    /// The mirror image of the sustain-crossing-end case above, and the visual
    /// counterpart of `Track::pending_note_ons` chasing a note back in at
    /// playback time (`050-undo-redo.md`): a note whose onset belongs to the
    /// sibling half (before this region's start) but whose sustain reaches
    /// into this one must still draw here — starting at this clip's own left
    /// edge (phase 0), not excluded just because its onset is elsewhere.
    #[test]
    fn build_note_thumbnails_draws_a_note_chased_in_from_before_region_start() {
        let mut clip = clip_with_region(480, 960);
        // Sibling half's note-on (tick 200) sustains to tick 700 — past this
        // region's start (480), so its tail sounds here too.
        clip.add_event(Event::new(200, 500, vec![0x90, 60, 100]));

        let thumbnails = build_note_thumbnails(&clip);

        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].0, 0.0); // clamped to this clip's own start
        assert_eq!(thumbnails[0].1, (700 - 480) as f32 / 480.0);
    }
}
