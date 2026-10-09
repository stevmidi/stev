//! The clip's event selection — thin wrappers over the
//! [`Selection`](crate::models::selection::Selection) set plus the cursor /
//! rectangle hit-tests that resolve *which* events a gesture means.
//!
//! `Selection` tracks ids; this file adds
//! the clip-aware queries: pick the note under the cursor, everything, or every
//! `NoteOn` a marquee rectangle overlaps. Only `NoteOn`s are ever selected — an
//! edit carries its paired `NoteOff` along via `pairing.rs`.

use std::collections::HashSet;

use uuid::Uuid;

use crate::models::event::{Event, EventType};

use super::Clip;

impl Clip {
    /// Replaces the selection with `id` (or clears it on `None`). Returns the
    /// previously lead-selected id.
    pub(crate) fn select_event(&mut self, id: Option<Uuid>) -> Option<Uuid> {
        self.event_selection.select(id)
    }

    /// Adds every `NoteOn` in the clip to the selection. Returns the ids that
    /// were not already selected.
    pub(crate) fn select_all_events(&mut self) -> Vec<Uuid> {
        let mut selected: HashSet<Uuid> = self.event_selection.ids().iter().copied().collect();
        let newly_selected: Vec<Uuid> = self
            .events
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| event.id())
            .filter(|&id| selected.insert(id))
            .collect();

        // Only touch the selection when it grows: `set_ids` moves the lead to
        // the last id, which must stay put when everything was already selected.
        if !newly_selected.is_empty() {
            let mut ids = self.event_selection.selected_ids();
            ids.extend_from_slice(&newly_selected);
            self.event_selection.set_ids(ids);
        }

        newly_selected
    }

    /// Clears the selection, returning the ids that were selected (so the
    /// caller can emit deselection UI events).
    pub(crate) fn clear_event_selection(&mut self) -> Vec<Uuid> {
        self.event_selection.clear_selection()
    }

    /// Replaces the selection with exactly `selected_ids` — undo restore.
    pub(crate) fn restore_event_selection(&mut self, selected_ids: &[Uuid]) {
        self.event_selection.set_ids(selected_ids.to_vec());
    }

    /// The lead-selected event id, if any.
    pub(crate) fn selected_event_id(&self) -> Option<Uuid> {
        self.event_selection.selected_id()
    }

    /// All selected event ids.
    pub(crate) fn selected_event_ids(&self) -> Vec<Uuid> {
        self.event_selection.selected_ids()
    }

    /// Marquee (rubber-band) select: replaces the selection with every
    /// `NoteOn` event whose pitch falls in `[note_min, note_max]` and whose
    /// span overlaps `[tick_min, tick_max)` at all — partial overlap is
    /// enough, matching Ableton/Bitwig/Logic (not full containment).
    /// Recomputes the whole set on every call rather than incrementally
    /// adding/removing, so a shrinking rectangle naturally deselects events
    /// that fall back outside it. Returns `(newly_deselected, newly_selected)`
    /// so callers can emit UI events without re-querying the full set.
    pub(crate) fn select_events_in_rect(
        &mut self,
        tick_min: i32,
        tick_max: i32,
        note_min: u8,
        note_max: u8,
    ) -> (Vec<Uuid>, Vec<Uuid>) {
        let new_ids: Vec<Uuid> = self
            .events
            .iter()
            .filter(|e| {
                e.event_type() == Some(EventType::NoteOn)
                    && e.note_number()
                        .is_some_and(|nn| nn >= note_min && nn <= note_max)
                    && e.tick() < tick_max
                    && e.end_tick().max(e.tick() + 1) > tick_min
            })
            .map(|e| e.id())
            .collect();

        // Set lookups keep this linear: it runs on every marquee-drag frame.
        let old_ids = self.event_selection.ids();
        let old_set: HashSet<Uuid> = old_ids.iter().copied().collect();
        let new_set: HashSet<Uuid> = new_ids.iter().copied().collect();
        let deselected: Vec<Uuid> = old_ids
            .iter()
            .copied()
            .filter(|id| !new_set.contains(id))
            .collect();
        let selected: Vec<Uuid> = new_ids
            .iter()
            .copied()
            .filter(|id| !old_set.contains(id))
            .collect();

        self.event_selection.set_ids(new_ids);

        (deselected, selected)
    }

    /// The `NoteOn`s among `ids` worth auditioning when they join a marquee
    /// selection: earliest first (lowest pitch on a tie), one per pitch, at
    /// most `cap` — so a sweep that swallows a dense passage in one rect
    /// update sounds a handful of notes, not all of them.
    pub(crate) fn audition_note_ons(&self, ids: &[Uuid], cap: usize) -> Vec<&Event> {
        let mut note_ons: Vec<&Event> = self
            .note_on_indices(ids)
            .into_iter()
            .map(|idx| &self.events[idx])
            .collect();
        note_ons.sort_by_key(|e| (e.tick(), e.note_number()));
        let mut pitches = HashSet::new();
        note_ons
            .into_iter()
            .filter(|e| e.note_number().is_some_and(|nn| pitches.insert(nn)))
            .take(cap)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{
        clip::Clip,
        event::{Event, EventType},
    };

    fn clip_with_region(region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    #[test]
    fn select_all_events_selects_only_note_on_events() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));

        let newly_selected = clip.select_all_events();

        assert_eq!(newly_selected.len(), 2);
        assert_eq!(clip.selected_event_ids().len(), 2);
        assert!(clip.selected_event_ids().iter().all(|id| {
            clip.events()
                .iter()
                .any(|event| event.id() == *id && event.event_type() == Some(EventType::NoteOn))
        }));
    }

    #[test]
    fn select_all_events_adds_to_the_selection_and_leads_with_the_last_new_note() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));
        let first = clip.events()[0].id();
        let last = clip.events()[2].id();
        clip.select_event(Some(first));

        let newly_selected = clip.select_all_events();

        assert_eq!(newly_selected, vec![last]);
        assert_eq!(clip.selected_event_ids(), vec![first, last]);
        assert_eq!(clip.selected_event_id(), Some(last));
    }

    #[test]
    fn select_all_events_keeps_the_lead_when_everything_is_already_selected() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));
        let first = clip.events()[0].id();
        let last = clip.events()[2].id();
        clip.restore_event_selection(&[last, first]);

        let newly_selected = clip.select_all_events();

        assert!(newly_selected.is_empty());
        assert_eq!(clip.selected_event_ids(), vec![last, first]);
        assert_eq!(clip.selected_event_id(), Some(first));
    }

    #[test]
    fn select_events_in_rect_selects_note_on_events_overlapping_by_pitch_and_tick() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));
        clip.add_event(on(300, 64));
        clip.add_event(off(340, 64));
        clip.calculate_note_lengths();

        let ids: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.id())
            .collect();

        // Rect spans ticks [50, 200) and pitches [60, 62]: the first note
        // (0..80) only partially overlaps the tick range and the second
        // (120..180) is fully inside it — both should be selected on overlap
        // alone. The third note is out of pitch range.
        let (deselected, selected) = clip.select_events_in_rect(50, 200, 60, 62);

        assert!(deselected.is_empty());
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&ids[0]));
        assert!(selected.contains(&ids[1]));
        assert_eq!(clip.selected_event_ids().len(), 2);
    }

    #[test]
    fn select_events_in_rect_deselects_events_that_fall_outside_a_shrinking_rect() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));
        clip.calculate_note_lengths();

        let ids: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.id())
            .collect();

        clip.select_events_in_rect(0, 200, 60, 62);
        assert_eq!(clip.selected_event_ids().len(), 2);

        // Shrink the rect so only the first note is still inside — the
        // second must drop out of the selection, not just stop growing.
        let (deselected, selected) = clip.select_events_in_rect(0, 100, 60, 62);

        assert_eq!(deselected, vec![ids[1]]);
        assert!(selected.is_empty());
        assert_eq!(clip.selected_event_ids(), vec![ids[0]]);
    }

    #[test]
    fn select_events_in_rect_with_no_matches_clears_the_selection() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.calculate_note_lengths();

        let id = clip.events()[0].id();
        clip.select_event(Some(id));
        assert!(clip.selected_event_id().is_some());

        let (deselected, selected) = clip.select_events_in_rect(500, 600, 0, 127);

        assert_eq!(deselected, vec![id]);
        assert!(selected.is_empty());
        assert!(clip.selected_event_id().is_none());
    }

    #[test]
    fn audition_note_ons_plays_earliest_first_one_per_pitch_up_to_the_cap() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(240, 64));
        clip.add_event(off(300, 64));
        clip.add_event(on(0, 67));
        clip.add_event(off(60, 67));
        clip.add_event(on(0, 60));
        clip.add_event(off(60, 60));
        // A second 60 later on: the pitch already sounds, so it is skipped.
        clip.add_event(on(120, 60));
        clip.add_event(off(180, 60));
        clip.add_event(on(480, 72));
        clip.add_event(off(540, 72));
        let ids: Vec<_> = clip.events().iter().map(|e| e.id()).collect();

        let pitches = |cap| -> Vec<u8> {
            clip.audition_note_ons(&ids, cap)
                .iter()
                .filter_map(|e| e.note_number())
                .collect()
        };

        assert_eq!(pitches(10), vec![60, 67, 64, 72]);
        assert_eq!(pitches(2), vec![60, 67]);
    }

    #[test]
    fn audition_note_ons_ignores_events_outside_the_ids() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(80, 60));
        clip.add_event(on(120, 62));
        clip.add_event(off(180, 62));
        clip.calculate_note_lengths();

        let (_, selected) = clip.select_events_in_rect(0, 100, 60, 62);
        let auditioned = clip.audition_note_ons(&selected, 4);

        assert_eq!(auditioned.len(), 1);
        assert_eq!(auditioned[0].note_number(), Some(60));
    }
}
