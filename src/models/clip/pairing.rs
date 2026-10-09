//! Matching `NoteOn`s to their `NoteOff`s.
//!
//! Most clip operations that touch notes need to know which `NoteOff` belongs
//! to which `NoteOn` — to move a note as a unit, compute its length, close a
//! hanging note at a boundary, or chase notes that are sounding at a seek
//! target. [`pair_note_events`](Clip::pair_note_events) does that once with a
//! per-note-number stack (so overlapping same-pitch notes nest correctly) and
//! returns a [`NotePairing`]; the helpers here are built on it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use uuid::Uuid;

use crate::models::event::{Event, EventType};

use super::Clip;

/// The result of walking a clip's event list and matching note edges.
/// Indices are into `Clip::events` as it stood at the call.
pub(super) struct NotePairing {
    /// `NoteOn` index → its matched `NoteOff` index.
    pub(super) on_to_off: HashMap<usize, usize>,
    /// `NoteOn` indices with no matching `NoteOff` (still sounding at clip end).
    pub(super) open_note_ons: Vec<usize>,
    /// `NoteOff` indices with no preceding `NoteOn` — stray offs to prune.
    pub(super) orphan_note_offs: Vec<usize>,
}

impl NotePairing {
    /// Where the note starting at `on_idx` ends: its paired `NoteOff`'s tick,
    /// else (an open note) the `NoteOn`'s own
    /// [`end_tick`](Event::end_tick). `events` is the list this pairing was
    /// built from.
    pub(super) fn note_end_tick(&self, events: &[Event], on_idx: usize) -> i32 {
        self.on_to_off.get(&on_idx).map_or_else(
            || events[on_idx].end_tick(),
            |&off_idx| events[off_idx].tick(),
        )
    }
}

impl Clip {
    /// Walks the event list once, matching each `NoteOff` to the most recent
    /// unmatched `NoteOn` of the same note number (a per-pitch stack, so
    /// overlapping same-pitch notes nest). See [`NotePairing`].
    pub(super) fn pair_note_events(&self) -> NotePairing {
        let mut note_on_stacks: HashMap<u8, Vec<usize>> = HashMap::new();
        let mut on_to_off = HashMap::new();
        let mut orphan_note_offs = Vec::new();

        for (idx, event) in self.events.iter().enumerate() {
            let Some(note) = event.note_number() else {
                continue;
            };

            match event.event_type() {
                Some(EventType::NoteOn) => {
                    note_on_stacks.entry(note).or_default().push(idx);
                }
                Some(EventType::NoteOff) => {
                    match note_on_stacks.get_mut(&note).and_then(Vec::pop) {
                        Some(on_idx) => {
                            on_to_off.insert(on_idx, idx);
                        }
                        None => orphan_note_offs.push(idx),
                    }
                }
                None => {}
            }
        }

        let open_note_ons = note_on_stacks.into_values().flatten().collect();

        NotePairing {
            on_to_off,
            open_note_ons,
            orphan_note_offs,
        }
    }

    /// Index of the `NoteOn` carrying each id in `ids`, in `ids` order; an id
    /// with no `NoteOn` is skipped. One pass over the events, not one per id.
    pub(super) fn note_on_indices(&self, ids: &[Uuid]) -> Vec<usize> {
        let wanted: HashSet<Uuid> = ids.iter().copied().collect();
        let mut index_of: HashMap<Uuid, usize> = HashMap::with_capacity(wanted.len());
        for (idx, event) in self.events.iter().enumerate() {
            if event.event_type() == Some(EventType::NoteOn) && wanted.contains(&event.id()) {
                index_of.entry(event.id()).or_insert(idx);
            }
        }
        ids.iter()
            .filter_map(|id| index_of.get(id).copied())
            .collect()
    }

    /// Recomputes every `NoteOn`'s `length` field from the tick distance to its
    /// paired `NoteOff` (0 for an unpaired note). Call after any edit that
    /// moves note edges.
    pub(crate) fn calculate_note_lengths(&mut self) {
        for event in &mut self.events {
            if event.event_type() == Some(EventType::NoteOn) {
                event.set_length(0);
            }
        }

        let pairing = self.pair_note_events();

        for (on_idx, off_idx) in pairing.on_to_off {
            let length = self.events[off_idx].tick() - self.events[on_idx].tick();
            self.events[on_idx].set_length(length);
        }
    }

    /// Clones what a capture insert takes from `[start_tick, end_tick)`:
    /// every whole note pair whose `NoteOn` falls in it — the `NoteOff` is
    /// included even if it lies outside the range, so a copied slice never
    /// loses a note's tail — and every non-note event (a wheel move) in it.
    /// In list order.
    pub(crate) fn cloned_events_in_range(&self, start_tick: i32, end_tick: i32) -> Vec<Event> {
        let pairing = self.pair_note_events();
        let mut included_indices = BTreeSet::new();

        for (idx, event) in self.events.iter().enumerate() {
            if !(start_tick..end_tick).contains(&event.tick()) {
                continue;
            }

            match event.event_type() {
                Some(EventType::NoteOn) => {
                    included_indices.insert(idx);
                    if let Some(off_idx) = pairing.on_to_off.get(&idx).copied() {
                        included_indices.insert(off_idx);
                    }
                }
                // Offs travel with their on.
                Some(EventType::NoteOff) => {}
                None => {
                    included_indices.insert(idx);
                }
            }
        }

        included_indices
            .into_iter()
            .map(|idx| self.events[idx].clone())
            .collect()
    }

    /// Keeps the events whose ticks fall in `[window_start, window_end)` — a
    /// span of exactly one loop — and folds them onto the loop's phase circle:
    /// `tick' = (tick - phase_origin).rem_euclid(loop_len)`. Note pairs travel
    /// together: a paired `NoteOff` is placed by its `NoteOn`'s folded tick plus
    /// the note length, and a note that would run past the loop end (held
    /// across the wrap, or still held) is closed at `loop_len - 1`, as
    /// [`crop`](Self::crop) does. Orphan offs are dropped. Leaves `region` as
    /// `[0, loop_len)`; re-sorted (a later tick can fold to an earlier phase)
    /// and lengths recomputed. Running capture uses this to turn the last
    /// pass of a take into a clip whose out-of-region material the edge drags
    /// can reveal (`100-running-capture.md`).
    pub(crate) fn fold_into_loop(
        &mut self,
        window_start: i32,
        window_end: i32,
        phase_origin: i32,
        loop_len: i32,
    ) {
        if loop_len <= 0 {
            return;
        }

        let fold = |tick: i32| (tick - phase_origin).rem_euclid(loop_len);
        self.events = self.windowed_events(window_start..window_end, fold, loop_len - 1);
        self.region_mut().set_region(Some(0), Some(loop_len));
        self.sort_events_by_tick();
        self.calculate_note_lengths();
    }

    /// The events in `window`, retimed by `retime`, note pairs kept together:
    /// a paired `NoteOff` lands at its `NoteOn`'s retimed tick plus the note
    /// length, stopped at `last_tick`; a still-open note closes at
    /// `last_tick` (its synthetic off muted like its `NoteOn`); orphan offs
    /// are dropped. Clones keep their ids; the result is unsorted. The one
    /// rule [`fold_into_loop`](Self::fold_into_loop) and Merge Clips' bake
    /// (`merge.rs`) carry notes out of a window by.
    pub(super) fn windowed_events(
        &self,
        window: Range<i32>,
        retime: impl Fn(i32) -> i32,
        last_tick: i32,
    ) -> Vec<Event> {
        let pairing = self.pair_note_events();
        let mut kept = Vec::with_capacity(self.events.len());

        for (idx, event) in self.events.iter().enumerate() {
            let tick = event.tick();
            if !window.contains(&tick) {
                continue;
            }

            let mut retimed = event.clone();
            retimed.set_tick(retime(tick));
            match event.event_type() {
                Some(EventType::NoteOn) => {
                    let on_tick = retimed.tick();
                    kept.push(retimed);

                    let note_off = match pairing.on_to_off.get(&idx) {
                        Some(&off_idx) => {
                            let length = self.events[off_idx].tick() - tick;
                            let mut note_off = self.events[off_idx].clone();
                            note_off.set_tick((on_tick + length).min(last_tick));
                            note_off
                        }
                        None => {
                            let mut note_off = Self::note_off_for(event, last_tick);
                            note_off.set_muted(event.is_muted());
                            note_off
                        }
                    };
                    kept.push(note_off);
                }
                // Paired offs travel with their on; orphan offs are dropped.
                Some(EventType::NoteOff) => {}
                None => kept.push(retimed),
            }
        }

        kept
    }

    /// A synthetic `NoteOff` at `tick` (same note and channel) for `note_on`.
    pub(super) fn note_off_for(note_on: &Event, tick: i32) -> Event {
        let channel = note_on.midi_channel().unwrap_or(0);
        let note = note_on.note_number().unwrap_or(0);
        Event::new(tick, 0, vec![0x80 | channel, note, 0])
    }

    /// Clones the `NoteOff`s for notes that have started but not ended at the
    /// current playback index — the offs a track must emit to silence this clip
    /// cleanly when it is muted or removed mid-playback.
    pub(crate) fn pending_note_offs(&self) -> Vec<Event> {
        // Nothing walked yet, so nothing started: skip the pairing (this runs
        // on the sequencer thread at every clip exit).
        if self.current_event_idx == 0 {
            return Vec::new();
        }
        let pairing = self.pair_note_events();
        pairing
            .on_to_off
            .iter()
            .filter(|&(&on_idx, &off_idx)| {
                on_idx < self.current_event_idx && off_idx >= self.current_event_idx
            })
            .filter_map(|(_, &off_idx)| self.events.get(off_idx).cloned())
            .collect()
    }

    /// Appends a synthetic `NoteOff` at `tick` (same note and channel) for
    /// every still-open `NoteOn`, then re-sorts. Used when cropping a clip to
    /// guarantee every note closes inside the playable window.
    pub(super) fn close_open_notes_at(&mut self, tick: i32) {
        let pairing = self.pair_note_events();

        if pairing.open_note_ons.is_empty() {
            return;
        }

        for on_idx in pairing.open_note_ons {
            let note_off = Self::note_off_for(&self.events[on_idx], tick);
            self.events.push(note_off);

            dprintln!("Closing open note idx {} at tick {}", on_idx, tick);
        }

        self.sort_events_by_tick();
    }

    /// Drops every `NoteOff` with no preceding `NoteOn` — cleans up after a
    /// crop / trim that removed the note's start.
    pub(super) fn remove_orphan_note_offs(&mut self) {
        let pairing = self.pair_note_events();
        if pairing.orphan_note_offs.is_empty() {
            return;
        }

        let mut idx = 0usize;
        self.events.retain(|_| {
            let keep = pairing.orphan_note_offs.binary_search(&idx).is_err();
            idx += 1;
            keep
        });
    }

    /// Shifts a `NoteOn` and its paired `NoteOff` by `nudge` ticks together, so
    /// the note keeps its length. Does not re-sort — call
    /// [`sort_events_by_tick`](Self::sort_events_by_tick) after a batch.
    pub(super) fn nudge_note_pair(
        &mut self,
        note_on_idx: usize,
        nudge: i32,
        pairing: &NotePairing,
    ) {
        if nudge == 0 {
            return;
        }

        let note_on_tick = self.events[note_on_idx].tick();
        self.events[note_on_idx].set_tick(note_on_tick + nudge);

        if let Some(note_off_idx) = pairing.on_to_off.get(&note_on_idx).copied() {
            let note_off_tick = self.events[note_off_idx].tick();
            self.events[note_off_idx].set_tick(note_off_tick + nudge);
        }
    }

    /// Ends the note at `on_idx` at event tick `end`: sets the `NoteOn`'s
    /// length and moves its `NoteOff` (`off_idx`, if it has one). Does not
    /// re-sort.
    pub(super) fn set_note_end(&mut self, on_idx: usize, off_idx: Option<usize>, end: i32) {
        let start = self.events[on_idx].tick();
        self.events[on_idx].set_length(end - start);
        if let Some(off_idx) = off_idx {
            self.events[off_idx].set_tick(end);
        }
    }

    /// Transposes the note at `on_idx` and its paired `NoteOff` by
    /// `semitones`, clamped to MIDI 0–127.
    pub(super) fn transpose_note_pair(
        &mut self,
        on_idx: usize,
        semitones: i32,
        pairing: &NotePairing,
    ) {
        let Some(pitch) = self.events[on_idx].note_number() else {
            return;
        };
        let pitch = (i32::from(pitch) + semitones).clamp(0, 127) as u8;
        self.events[on_idx].set_note_number(pitch);
        if let Some(off_idx) = pairing.on_to_off.get(&on_idx).copied() {
            self.events[off_idx].set_note_number(pitch);
        }
    }

    /// Returns cloned NoteOn events that are "in flight" at `playback_tick` —
    /// i.e. NoteOn happened before this tick but the paired NoteOff has not yet fired.
    pub(crate) fn chased_note_ons_at(&self, playback_tick: i32) -> Vec<Event> {
        let target = self.event_tick_from_arrangement_tick(playback_tick);
        // No event before the target, so no note in flight: skip the pairing
        // (this runs on the sequencer thread at every clip-start arrival).
        if self
            .events
            .first()
            .is_none_or(|event| event.tick() >= target)
        {
            return Vec::new();
        }
        let pairing = self.pair_note_events();

        pairing
            .on_to_off
            .iter()
            .filter(|&(&on_idx, &off_idx)| {
                let on_tick = self.events[on_idx].tick();
                let off_tick = self.events[off_idx].tick();
                on_tick < target && off_tick >= target
            })
            .filter_map(|(&on_idx, _)| self.events.get(on_idx).cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::models::{
        clip::Clip,
        event::{Event, EventType},
    };

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    fn clip_with(events: Vec<Event>) -> Clip {
        let mut clip = Clip::new();
        for e in events {
            clip.add_event(e);
        }
        clip
    }

    #[test]
    fn simple_on_off_pair_is_matched() {
        let clip = clip_with(vec![on(0, 60), off(100, 60)]);
        let p = clip.pair_note_events();
        assert_eq!(p.on_to_off.len(), 1);
        assert!(p.open_note_ons.is_empty());
        assert!(p.orphan_note_offs.is_empty());
    }

    #[test]
    fn unpaired_note_on_is_open() {
        let clip = clip_with(vec![on(0, 60)]);
        let p = clip.pair_note_events();
        assert_eq!(p.open_note_ons.len(), 1);
        assert!(p.on_to_off.is_empty());
    }

    #[test]
    fn orphan_note_off_with_no_preceding_on() {
        let clip = clip_with(vec![off(100, 60)]);
        let p = clip.pair_note_events();
        assert_eq!(p.orphan_note_offs.len(), 1);
        assert!(p.on_to_off.is_empty());
    }

    #[test]
    fn overlapping_same_note_uses_stack_most_recent_on_matches_off() {
        // on@0, on@50, off@100: stack matches second on (idx 1) with the off (idx 2).
        let clip = clip_with(vec![on(0, 60), on(50, 60), off(100, 60)]);
        let p = clip.pair_note_events();
        assert_eq!(p.on_to_off.len(), 1);
        assert_eq!(p.open_note_ons.len(), 1); // first on is still unpaired
        assert!(p.on_to_off.contains_key(&1));
        assert_eq!(p.on_to_off[&1], 2);
    }

    #[test]
    fn different_notes_paired_independently() {
        let clip = clip_with(vec![on(0, 60), on(0, 62), off(100, 60), off(100, 62)]);
        let p = clip.pair_note_events();
        assert_eq!(p.on_to_off.len(), 2);
        assert!(p.open_note_ons.is_empty());
    }

    #[test]
    fn calculate_note_lengths_sets_length_from_off_minus_on() {
        let mut clip = clip_with(vec![on(0, 60), off(240, 60)]);
        clip.calculate_note_lengths();
        let note_on = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap();
        assert_eq!(note_on.end_tick(), 240); // ticks=0, length=240
    }

    #[test]
    fn cloned_events_in_range_keeps_only_surviving_note_pairs() {
        let clip = clip_with(vec![on(0, 60), off(100, 60), on(120, 62), off(220, 62)]);

        let kept = clip.cloned_events_in_range(110, 240);
        let kept_ticks: Vec<_> = kept.iter().map(|event| event.tick()).collect();

        assert_eq!(kept_ticks, vec![120, 220]);
    }

    #[test]
    fn cloned_events_in_range_keeps_open_note_on_without_off() {
        let clip = clip_with(vec![on(0, 60), on(120, 62)]);

        let kept = clip.cloned_events_in_range(100, 240);
        let kept_ticks: Vec<_> = kept.iter().map(|event| event.tick()).collect();

        assert_eq!(kept_ticks, vec![120]);
    }

    #[test]
    fn cloned_events_in_range_carries_the_wheel_moves_inside_it() {
        let bend = |tick| Event::new(tick, 0, vec![0xE0, 0x00, 0x50]);
        let clip = clip_with(vec![
            bend(50),
            on(120, 62),
            bend(150),
            off(260, 62),
            bend(250),
        ]);

        let kept = clip.cloned_events_in_range(100, 240);
        let kept_ticks: Vec<_> = kept.iter().map(|event| event.tick()).collect();

        // The bend before the range and the one after it stay behind; the
        // note's off comes along though it lies past the range.
        assert_eq!(kept_ticks, vec![120, 150, 260]);
    }

    #[test]
    fn close_open_notes_adds_synthetic_note_off_at_given_tick() {
        let mut clip = clip_with(vec![on(0, 60)]);
        clip.close_open_notes_at(480);
        let note_offs: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOff))
            .collect();
        assert_eq!(note_offs.len(), 1);
        assert_eq!(note_offs[0].tick(), 480);
        assert_eq!(note_offs[0].note_number(), Some(60));
    }

    #[test]
    fn close_open_notes_preserves_midi_channel() {
        let mut clip = clip_with(vec![Event::new(0, 0, vec![0x91, 60, 100])]); // ch 1
        clip.close_open_notes_at(480);
        let note_offs: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOff))
            .collect();
        assert_eq!(note_offs[0].midi_channel(), Some(1));
    }

    #[test]
    fn close_open_notes_noop_when_all_notes_are_paired() {
        let mut clip = clip_with(vec![on(0, 60), off(100, 60)]);
        clip.close_open_notes_at(480);
        let note_offs: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOff))
            .collect();
        // Only the original note-off at 100, no synthetic one at 480
        assert_eq!(note_offs.len(), 1);
        assert_eq!(note_offs[0].tick(), 100);
    }

    // --- fold_into_loop ---

    /// `(tick, note, is_on)` triples in list order.
    fn note_edges(clip: &Clip) -> Vec<(i32, u8, bool)> {
        clip.events()
            .iter()
            .map(|e| {
                (
                    e.tick(),
                    e.note_number().unwrap(),
                    e.event_type() == Some(EventType::NoteOn),
                )
            })
            .collect()
    }

    /// Only onsets inside `[window_start, window_end)` survive — the same
    /// phase one loop older, sitting exactly at `window_start - 1`, is out.
    #[test]
    fn fold_into_loop_keeps_only_onsets_inside_the_window() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![
            on(100, 60),
            off(200, 60), // one loop older than the last note: excluded
            on(500, 61),
            off(600, 61),
            on(1100, 62),
            off(1200, 62), // the last note-on
        ]);

        clip.fold_into_loop(1100 - loop_len + 1, 1100 + 1, 0, loop_len);

        assert_eq!(
            note_edges(&clip),
            vec![
                (100, 62, true),
                (200, 62, false),
                (500, 61, true),
                (600, 61, false)
            ]
        );
        assert_eq!(clip.region().start(), 0);
        assert_eq!(clip.region().end(), loop_len);
    }

    /// Ticks fold by phase relative to `phase_origin`, and the result is
    /// sorted even though a later tick folds to an earlier phase.
    #[test]
    fn fold_into_loop_folds_by_phase_origin_and_sorts() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![
            on(2900, 60),
            off(2950, 60),
            on(3100, 61),
            off(3150, 61),
        ]);

        // Origin 2000: 2900 → 900, 3100 → 100.
        clip.fold_into_loop(2101, 3101, 2000, loop_len);

        assert_eq!(
            note_edges(&clip),
            vec![
                (100, 61, true),
                (150, 61, false),
                (900, 60, true),
                (950, 60, false)
            ]
        );
    }

    /// A note held across the loop end is closed at `loop_len - 1`, never
    /// left with its off before its on.
    #[test]
    fn fold_into_loop_closes_a_note_straddling_the_loop_end() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![on(900, 60), off(1300, 60)]);

        clip.fold_into_loop(0, 1000, 0, loop_len);

        assert_eq!(note_edges(&clip), vec![(900, 60, true), (999, 60, false)]);
        assert_eq!(clip.events()[0].end_tick(), 999);
    }

    /// A note still held (no off at all) is closed at `loop_len - 1`.
    #[test]
    fn fold_into_loop_closes_a_held_note_at_the_loop_end() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![Event::new(300, 0, vec![0x91, 60, 100])]); // ch 1

        clip.fold_into_loop(0, 1000, 0, loop_len);

        assert_eq!(note_edges(&clip), vec![(300, 60, true), (999, 60, false)]);
        assert_eq!(clip.events()[1].midi_channel(), Some(1));
    }

    /// An off whose on fell outside the window is an orphan and is dropped.
    #[test]
    fn fold_into_loop_drops_orphan_offs() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![on(50, 60), off(1050, 60), on(1200, 61), off(1250, 61)]);

        clip.fold_into_loop(1000, 2000, 0, loop_len);

        assert_eq!(note_edges(&clip), vec![(200, 61, true), (250, 61, false)]);
    }

    /// Non-note events inside the window fold like any tick.
    #[test]
    fn fold_into_loop_folds_non_note_events() {
        let loop_len = 1000;
        let mut clip = clip_with(vec![Event::new(1300, 0, vec![0xB0, 1, 64])]);

        clip.fold_into_loop(1000, 2000, 0, loop_len);

        assert_eq!(clip.events().len(), 1);
        assert_eq!(clip.events()[0].tick(), 300);
    }

    #[test]
    fn note_on_indices_follow_the_id_order_and_skip_unknown_ids() {
        let clip = clip_with(vec![on(0, 60), off(100, 60), on(200, 62), off(300, 62)]);
        let first = clip.events()[0].id();
        let off_id = clip.events()[1].id();
        let second = clip.events()[2].id();

        let indices = clip.note_on_indices(&[second, off_id, Uuid::new_v4(), first]);

        assert_eq!(indices, vec![2, 0]);
    }

    #[test]
    fn chased_note_ons_at_the_first_event_finds_nothing() {
        let mut clip = clip_with(vec![on(0, 60), off(100, 60)]);
        clip.region_mut().set_region(Some(0), Some(960));

        assert!(clip.chased_note_ons_at(clip.start_tick()).is_empty());
    }
}
