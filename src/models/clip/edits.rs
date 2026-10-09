//! In-place edits of the selected events: nudge time / length, transpose,
//! nudge velocity, mute, delete, duplicate.
//!
//! Every method resolves the target from `self.event_selection` (the one
//! exception, [`nudge_events_velocity`](Clip::nudge_events_velocity), takes an
//! explicit id set for the ⌘/Ctrl+drag gesture, which can target an unselected
//! hovered note). Each moves a `NoteOn` and its paired `NoteOff` together via
//! `pairing.rs`, and ends by re-sorting so the list invariants hold. The
//! undoable wrappers live in `core/sequencer/edit/event_edits.rs`.

use uuid::Uuid;

use super::Clip;

impl Clip {
    /// Shifts every selected note (`NoteOn` + its paired `NoteOff`) by `nudge`
    /// ticks. No-op with an empty selection.
    pub(crate) fn nudge_selected_events(&mut self, nudge: i32) {
        if self.event_selection.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();

        for on_idx in self.note_on_indices(self.event_selection.ids()) {
            self.nudge_note_pair(on_idx, nudge, &pairing);
        }
        self.sort_events_by_tick();
    }

    /// Grows or shrinks each selected note by moving only its `NoteOff` by
    /// `nudge` ticks, clamped so a note stays at least 1 tick long.
    pub(crate) fn nudge_selected_events_length(&mut self, nudge: i32) {
        if self.event_selection.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();

        for on_idx in self.note_on_indices(self.event_selection.ids()) {
            let start_tick = self.events[on_idx].tick();
            let current_end_tick = pairing.note_end_tick(&self.events, on_idx);
            let new_end_tick = (current_end_tick + nudge).max(start_tick + 1);
            self.set_note_end(
                on_idx,
                pairing.on_to_off.get(&on_idx).copied(),
                new_end_tick,
            );
        }

        self.sort_events_by_tick();
    }

    /// Transposes each selected note by `semitones` (clamped to MIDI 0–127),
    /// updating both the `NoteOn` and its paired `NoteOff` note number.
    pub(crate) fn transpose_selected_events(&mut self, semitones: i32) {
        if self.event_selection.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();

        for on_idx in self.note_on_indices(self.event_selection.ids()) {
            self.transpose_note_pair(on_idx, semitones, &pairing);
        }
    }

    /// Nudges the velocity of the notes in `event_ids` (not
    /// `self.event_selection`) by `nudge`, clamped to the MIDI range while
    /// preserving their relative differences — used by the ⌘/Ctrl+drag
    /// velocity gesture, which can target a single hovered note that isn't
    /// part of the current selection.
    pub(crate) fn nudge_events_velocity(&mut self, event_ids: &[Uuid], nudge: i32) {
        // Collect matching indices first so we can do two passes (compute
        // group-limited nudge, then apply) without borrowing issues.
        let selected_indices = self.note_on_indices(event_ids);

        if selected_indices.is_empty() {
            return;
        }

        // Cap the nudge so that no event crosses the [1, 127] boundary.
        // This preserves relative velocity differences between events —
        // the group moves as a unit and stops when the loudest/quietest
        // member would overflow, matching standard DAW behaviour.
        let effective_nudge = if nudge > 0 {
            let max_vel = selected_indices
                .iter()
                .filter_map(|&idx| self.events[idx].velocity())
                .max()
                .unwrap_or(64) as i32;
            nudge.min(127 - max_vel)
        } else {
            let min_vel = selected_indices
                .iter()
                .filter_map(|&idx| self.events[idx].velocity())
                .min()
                .unwrap_or(64) as i32;
            nudge.max(1 - min_vel)
        };

        for idx in selected_indices {
            let current = self.events[idx].velocity().unwrap_or(64) as i32;
            self.events[idx].set_velocity((current + effective_nudge) as u8);
        }
    }

    /// Removes each selected note and its paired `NoteOff`, then clears the
    /// selection and re-sorts.
    pub(crate) fn delete_selected_events(&mut self) {
        if self.event_selection.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();
        let mut remove_indices = Vec::new();

        for on_idx in self.note_on_indices(self.event_selection.ids()) {
            remove_indices.push(on_idx);

            if let Some(off_idx) = pairing.on_to_off.get(&on_idx).copied() {
                remove_indices.push(off_idx);
            }
        }

        remove_indices.sort_unstable();
        remove_indices.dedup();

        for idx in remove_indices.into_iter().rev() {
            self.events.remove(idx);
        }

        self.event_selection.clear_selection();
        self.sort_events_by_tick();
    }

    /// Toggles mute on every selected note (its `NoteOn` and paired
    /// `NoteOff` together): if any selected note is unmuted, mute all;
    /// otherwise unmute all — same uniform-toggle rule as
    /// `Clip::set_muted`'s whole-clip counterpart. No-op with an empty
    /// selection.
    pub(crate) fn toggle_muted_for_selected_events(&mut self) {
        let selected_on_indices = self.note_on_indices(self.event_selection.ids());

        if selected_on_indices.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();
        let target_muted = selected_on_indices
            .iter()
            .any(|&idx| !self.events[idx].is_muted());

        for &on_idx in &selected_on_indices {
            self.events[on_idx].set_muted(target_muted);
            if let Some(off_idx) = pairing.on_to_off.get(&on_idx).copied() {
                self.events[off_idx].set_muted(target_muted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{
        clip::Clip,
        event::{Event, EventType},
    };

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn on_vel(tick: i32, note: u8, velocity: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, velocity])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    fn add_selected_note_pair(clip: &mut Clip, note_on_tick: i32, note_off_tick: i32, note: u8) {
        clip.add_event(on(note_on_tick, note));
        clip.add_event(off(note_off_tick, note));

        let selected_note_on_id = clip
            .events()
            .iter()
            .find(|event| {
                event.event_type() == Some(EventType::NoteOn) && event.tick() == note_on_tick
            })
            .map(|event| event.id())
            .unwrap();
        clip.select_event(Some(selected_note_on_id));
    }

    fn clip_with_region(region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    #[test]
    fn nudge_selected_event_length_moves_note_off() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 180, 60);

        clip.nudge_selected_events_length(20);

        let note_off_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOff))
            .unwrap()
            .tick();
        assert_eq!(note_off_tick, 200);
    }

    /// Regression: a note nudged onto the end of a same-pitch note used to
    /// sort its `NoteOn` ahead of that note's `NoteOff`, so pairing matched
    /// the off to the moved note and both lengths came out wrong.
    #[test]
    fn nudge_onto_a_same_pitch_note_end_keeps_both_notes_paired() {
        let mut clip = clip_with_region(0, 1920);
        add_selected_note_pair(&mut clip, 0, 240, 60);
        clip.add_event(on(480, 60));
        clip.add_event(off(720, 60));

        clip.nudge_selected_events(720);
        let pairing = clip.pair_note_events();
        assert!(pairing.orphan_note_offs.is_empty(), "orphan NoteOff");
        assert!(pairing.open_note_ons.is_empty(), "open NoteOn");
        clip.calculate_note_lengths();

        let notes: Vec<(i32, i32)> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.end_tick()))
            .collect();
        assert_eq!(notes, vec![(480, 720), (720, 960)]);
    }

    #[test]
    fn transpose_selected_events_updates_note_on_and_note_off_note_numbers() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 180, 60);

        clip.transpose_selected_events(3);

        let note_numbers: Vec<u8> = clip
            .events()
            .iter()
            .filter_map(|event| event.note_number())
            .collect();
        assert!(note_numbers.contains(&63));
        assert_eq!(note_numbers.iter().filter(|note| **note == 63).count(), 2);
    }

    #[test]
    fn nudge_velocity_increases_note_on_velocity() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 180, 60);

        let selected_ids = clip.selected_event_ids();
        clip.nudge_events_velocity(&selected_ids, 5);

        let velocity = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .and_then(|e| e.velocity())
            .unwrap();
        assert_eq!(velocity, 105); // 100 + 5
    }

    #[test]
    fn nudge_velocity_clamped_at_127() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 180, 60);

        let selected_ids = clip.selected_event_ids();
        clip.nudge_events_velocity(&selected_ids, 100);

        let velocity = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .and_then(|e| e.velocity())
            .unwrap();
        assert_eq!(velocity, 127);
    }

    #[test]
    fn nudge_velocity_clamped_at_1() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 180, 60);

        let selected_ids = clip.selected_event_ids();
        clip.nudge_events_velocity(&selected_ids, -200);

        let velocity = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .and_then(|e| e.velocity())
            .unwrap();
        assert_eq!(velocity, 1);
    }

    // Two selected events: nudge is capped by the loudest event so
    // the relative velocity gap between them is preserved.
    #[test]
    fn nudge_velocity_group_limited_at_ceiling_preserves_relative_difference() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on_vel(0, 60, 94));
        clip.add_event(off(100, 60));
        clip.add_event(on_vel(0, 64, 48)); // chord note, lower velocity
        clip.add_event(off(100, 64));
        clip.select_all_events(); // selects both NoteOn events

        // Max headroom before hitting 127 is (127-94)=33. A nudge of 100 should
        // be capped to 33, keeping the 46-unit gap intact.
        let selected_ids = clip.selected_event_ids();
        clip.nudge_events_velocity(&selected_ids, 100);

        let velocities: std::collections::HashMap<u8, u8> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.note_number().unwrap(), e.velocity().unwrap()))
            .collect();
        assert_eq!(velocities[&60], 127); // 94 + 33
        assert_eq!(velocities[&64], 81); // 48 + 33 — gap of 46 preserved
    }

    #[test]
    fn nudge_velocity_group_limited_at_floor_preserves_relative_difference() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on_vel(0, 60, 94));
        clip.add_event(off(100, 60));
        clip.add_event(on_vel(0, 64, 48));
        clip.add_event(off(100, 64));
        clip.select_all_events();

        // Min headroom before hitting 1 is (48-1)=47. A nudge of -100 should
        // be capped to -47, keeping the 46-unit gap intact.
        let selected_ids = clip.selected_event_ids();
        clip.nudge_events_velocity(&selected_ids, -100);

        let velocities: std::collections::HashMap<u8, u8> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.note_number().unwrap(), e.velocity().unwrap()))
            .collect();
        assert_eq!(velocities[&64], 1); // 48 - 47
        assert_eq!(velocities[&60], 47); // 94 - 47 — gap of 46 preserved
    }

    // Explicit-ids variant used by the ⌘/Ctrl+drag velocity gesture: it must
    // affect only the named event, regardless of what's actually selected.
    #[test]
    fn nudge_events_velocity_targets_explicit_ids_not_selection() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on_vel(0, 60, 60));
        clip.add_event(off(100, 60));
        clip.add_event(on_vel(0, 64, 40));
        clip.add_event(off(100, 64));
        // Note 60 is selected, but the drag targets note 64 (hovered, unselected).
        let note_60_id = clip
            .events()
            .iter()
            .find(|e| e.note_number() == Some(60) && e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.id())
            .unwrap();
        let note_64_id = clip
            .events()
            .iter()
            .find(|e| e.note_number() == Some(64) && e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.id())
            .unwrap();
        clip.select_event(Some(note_60_id));

        clip.nudge_events_velocity(&[note_64_id], 5);

        let velocities: std::collections::HashMap<u8, u8> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.note_number().unwrap(), e.velocity().unwrap()))
            .collect();
        assert_eq!(velocities[&60], 60); // untouched — not a drag target
        assert_eq!(velocities[&64], 45); // 40 + 5 — the actual target
    }

    // --- toggle_muted_for_selected_events ---

    #[test]
    fn toggle_muted_mutes_the_selected_note_and_its_paired_note_off() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 300, 60);

        clip.toggle_muted_for_selected_events();

        assert!(clip.events().iter().all(|e| e.is_muted()));
    }

    #[test]
    fn toggle_muted_unmutes_when_every_selected_note_is_already_muted() {
        let mut clip = clip_with_region(0, 960);
        add_selected_note_pair(&mut clip, 120, 300, 60);
        clip.toggle_muted_for_selected_events();
        assert!(clip.events().iter().all(|e| e.is_muted()));

        clip.toggle_muted_for_selected_events();

        assert!(clip.events().iter().all(|e| !e.is_muted()));
    }

    #[test]
    fn toggle_muted_mutes_all_selected_when_any_one_is_unmuted() {
        let mut clip = clip_with_region(0, 1920);
        clip.add_event(on(0, 60));
        clip.add_event(off(240, 60));
        clip.add_event(on(480, 64));
        clip.add_event(off(960, 64));

        // Pre-mute just note 60 by selecting only it first.
        let note_60_id = clip
            .events()
            .iter()
            .find(|e| e.note_number() == Some(60) && e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.id())
            .unwrap();
        clip.restore_event_selection(&[note_60_id]);
        clip.toggle_muted_for_selected_events();

        // Now select both notes — one muted, one not — and toggle again.
        clip.select_all_events();
        clip.toggle_muted_for_selected_events();

        assert!(clip.events().iter().all(|e| e.is_muted()));
    }

    #[test]
    fn toggle_muted_leaves_unselected_notes_untouched() {
        let mut clip = clip_with_region(0, 1920);
        add_selected_note_pair(&mut clip, 0, 240, 60); // selected
        clip.add_event(on(480, 64));
        clip.add_event(off(960, 64)); // not selected

        clip.toggle_muted_for_selected_events();

        let muted_notes: Vec<u8> = clip
            .events()
            .iter()
            .filter(|e| e.is_muted())
            .filter_map(|e| e.note_number())
            .collect();
        assert_eq!(muted_notes, vec![60, 60]);
    }

    #[test]
    fn toggle_muted_noop_when_nothing_selected() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60));
        clip.add_event(off(240, 60));

        clip.toggle_muted_for_selected_events();

        assert!(clip.events().iter().all(|e| !e.is_muted()));
    }
}
