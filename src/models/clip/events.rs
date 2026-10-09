//! The clip's event list: playback stepping, and the structural edits that add,
//! restore, crop or trim events.
//!
//! [`tick`](Clip::tick) and [`seek`](Clip::seek) are the playback side — the
//! sequencer steps the list one tick at a time using each event's cached
//! `delta_ticks` — with [`wheels_at`](Clip::wheels_at), the wheel values a
//! seek chases. The rest reshape the list around the region: [`crop`](Clip::crop),
//! [`trim_before_tick`](Clip::trim_before_tick),
//! [`relocate_late_notes_to_region_start`](Clip::relocate_late_notes_to_region_start).
//! All of it keeps two invariants: the list stays sorted by tick, and
//! `delta_ticks` stays consistent — most operations end with
//! [`sort_events_by_tick`](Clip::sort_events_by_tick) /
//! [`update_delta_ticks`](Clip::update_delta_ticks).
//!
//! Selected-event editing is in `edits.rs`; quantize and swing detection in
//! `quantize.rs`.

use crate::core::time::Meter;
use crate::models::{
    event::{Event, EventType},
    wheels::{NEUTRAL_WHEELS, WheelValues, apply_wheel_move},
};

use super::{Clip, EventSpaceRetime};

impl Clip {
    /// Appends an event, setting its `delta_ticks` from the last event already
    /// in the list. Assumes the caller adds in tick order (the capture path);
    /// out-of-order callers must [`sort_events_by_tick`](Self::sort_events_by_tick)
    /// afterwards.
    pub(crate) fn add_event(&mut self, mut event: Event) {
        let prev_tick = self
            .events
            .last()
            .map(|existing| existing.tick())
            .unwrap_or(0);

        event.set_delta_ticks(prev_tick);
        self.events.push(event);
    }

    /// Advances the clip by one tick. Returns the event at
    /// [`current_event_idx`](Self::current_event_idx) once
    /// [`elapsed_delta_ticks`](Self::elapsed_delta_ticks) has caught up to its
    /// `delta_ticks`, then steps the index. `None` on a tick where nothing is
    /// due.
    pub(crate) fn tick(&mut self) -> Option<Event> {
        if let Some(event) = self.events.get_mut(self.current_event_idx) {
            if self.elapsed_delta_ticks < event.delta_ticks() {
                self.elapsed_delta_ticks += 1;
            } else if self.elapsed_delta_ticks == event.delta_ticks() {
                let out = event.clone();
                self.current_event_idx += 1;
                self.elapsed_delta_ticks = 0;
                return Some(out);
            } else {
                self.current_event_idx += 1;
                self.elapsed_delta_ticks = 0;
            }
        }

        None
    }

    /// Repositions playback: maps the arrangement `playback_tick` into
    /// event-tick space, points [`current_event_idx`](Self::current_event_idx)
    /// at the first event at or after it (lower-bound, so chords starting at
    /// one tick are not split), and primes
    /// [`elapsed_delta_ticks`](Self::elapsed_delta_ticks) with how far into the
    /// next gap the seek landed.
    pub(crate) fn seek(&mut self, playback_tick: i32) {
        if self.region_length() <= 0 || self.events.is_empty() {
            self.current_event_idx = 0;
            self.elapsed_delta_ticks = 0;
            return;
        }

        let target_tick = self.event_tick_from_arrangement_tick(playback_tick);

        // Use lower-bound lookup so duplicate timestamps (e.g. chord note-ons at tick 0)
        // always start from the first event at `target_tick`.
        let idx = self
            .events
            .partition_point(|event| event.tick() < target_tick);

        self.current_event_idx = idx;

        if idx >= self.events.len() {
            self.elapsed_delta_ticks = 0;
            return;
        }

        let prev_tick = if idx == 0 {
            0
        } else {
            self.events[idx - 1].tick()
        };
        let event_delta = self.events[idx].delta_ticks();
        let elapsed_into_delta = target_tick - prev_tick;

        self.elapsed_delta_ticks = elapsed_into_delta.clamp(0, event_delta);
    }

    /// The wheel values in force at arrangement tick `playback_tick`: per
    /// wheel, the last unmuted pitch bend / mod-wheel event before it in
    /// event space — hidden material before the window included, as for
    /// [`chased_note_ons_at`](Self::chased_note_ons_at) — or neutral when
    /// there is none. An event exactly at the target is left to the walk,
    /// which plays it next.
    pub(crate) fn wheels_at(&self, playback_tick: i32) -> WheelValues {
        let target = self.event_tick_from_arrangement_tick(playback_tick);
        let mut values = NEUTRAL_WHEELS;
        for event in self.events.iter().take_while(|event| event.tick() < target) {
            apply_wheel_move(&mut values, event);
        }
        values
    }

    /// Replaces the whole event list (undo restore), re-sorting and rebuilding
    /// `delta_ticks`.
    pub(crate) fn restore_events(&mut self, events: Vec<Event>) {
        self.events = events;
        self.sort_events_by_tick();
    }

    /// Trims the clip to its region: drops events outside `[region.start,
    /// region.end)`, rebases the survivors to a zero-based region, bar-snaps to
    /// `length`, prunes orphan offs, closes any note left open at the boundary,
    /// and recomputes lengths / deltas. See `100-running-capture.md`.
    pub(crate) fn crop(&mut self, length: i32) {
        let region_start = self.region.start();
        let clip_length = self.region_length();

        let window = region_start..self.region.end();
        self.events.retain(|event| window.contains(&event.tick()));

        for event in &mut self.events {
            event.set_tick(event.tick() - region_start);
        }

        self.region_mut().set_region(Some(0), Some(clip_length));
        self.region_mut().snap_to_grid(length);

        self.remove_orphan_note_offs();
        // Clip playback is half-open [0, clip_length), so close notes at the
        // last playable tick to guarantee note-off dispatch.
        self.close_open_notes_at(clip_length.saturating_sub(1));
        self.calculate_note_lengths();
        self.update_delta_ticks();
    }

    /// Moves every event, the region and the cursor `delta` ticks later in
    /// event space — the same music, relabelled. The clip's arrangement
    /// placement (`start_tick`) and what plays there are unchanged. Unlike
    /// [`crop`](Self::crop) nothing outside the region is dropped. `delta` must
    /// not push the region below 0 (`Region::set_region` clamps negatives).
    pub(crate) fn shift_event_space(&mut self, delta: i32) {
        for event in &mut self.events {
            event.set_tick(event.tick() + delta);
        }

        let region_start = self.region.start();
        let region_end = self.region.end();
        self.region
            .set_region(Some(region_start + delta), Some(region_end + delta));
        self.set_cursor_tick(self.cursor_tick() + delta);
        self.update_delta_ticks();
    }

    /// Shifts the event space (see [`shift_event_space`](Self::shift_event_space))
    /// so the window starts on the next bar line of `meter` at or after where
    /// it starts now — the grid a crop's rebase to 0 used to give quantize, swing and
    /// the clip view, kept without dropping what lies outside the window.
    /// Rounding up keeps every tick `>= 0`. Returns the shift as a retime.
    pub(crate) fn align_window_start_to_bar(&mut self, meter: Meter) -> EventSpaceRetime {
        let region_start = self.region.start();
        let shift = meter.next_bar_boundary_after(region_start - 1) - region_start;
        self.shift_event_space(shift);
        EventSpaceRetime {
            scale: 1.0,
            offset: f64::from(shift),
        }
    }

    /// Discards events before `cutoff_tick` **without** rebasing the survivors —
    /// their ticks keep their absolute values. Prunes the offs thereby
    /// orphaned. Used to drop stale history from a running capture.
    pub(crate) fn trim_before_tick(&mut self, cutoff_tick: i32) {
        self.events.retain(|event| event.tick() >= cutoff_tick);
        self.remove_orphan_note_offs();
        self.calculate_note_lengths();
        self.update_delta_ticks();
    }

    /// Pulls notes played a hair on either side of the region edge back inside
    /// it, as a group so their relative timing survives: a note within
    /// `tolerance` of `region_end` (played early, would be cropped) *and* a
    /// note within `tolerance` before `region_start` (played late) are shifted
    /// to `region_start`. See [`LATE_NOTE_TOLERANCE_TICKS`](crate::core::config::LATE_NOTE_TOLERANCE_TICKS)
    /// and `100-running-capture.md`.
    pub(crate) fn relocate_late_notes_to_region_start(
        &mut self,
        region_start: i32,
        region_end: i32,
        tolerance: i32,
    ) {
        if region_end <= region_start || tolerance <= 0 {
            return;
        }

        let late_window = (region_end - tolerance).max(region_start)..region_end;
        let early_window = (region_start - tolerance)..region_start;
        let pairing = self.pair_note_events();
        let mut late_group: Vec<(usize, usize, i32)> = Vec::new();
        let mut early_group: Vec<(usize, usize, i32)> = Vec::new();

        for (&on_idx, &off_idx) in &pairing.on_to_off {
            let note_on_tick = self.events[on_idx].tick();
            if late_window.contains(&note_on_tick) {
                late_group.push((on_idx, off_idx, note_on_tick));
            } else if early_window.contains(&note_on_tick) {
                early_group.push((on_idx, off_idx, note_on_tick));
            }
        }

        if late_group.is_empty() && early_group.is_empty() {
            return;
        }

        let relocate_group = |events: &mut [Event], group: &[(usize, usize, i32)]| {
            let anchor_tick = group
                .iter()
                .map(|(_, _, on_tick)| *on_tick)
                .min()
                .unwrap_or(region_start);
            let group_shift = region_start - anchor_tick;

            for &(on_idx, off_idx, _) in group {
                events[on_idx].set_tick(events[on_idx].tick() + group_shift);
                events[off_idx].set_tick(events[off_idx].tick() + group_shift);
            }
        };

        relocate_group(&mut self.events, &late_group);
        relocate_group(&mut self.events, &early_group);

        self.sort_events_by_tick();
        self.calculate_note_lengths();
    }

    /// Sorts the event list by tick and refreshes every `delta_ticks`. The
    /// tail of nearly every editing operation.
    ///
    /// On a shared tick `NoteOff`s go first (stable otherwise), so a note
    /// ending where a same-pitch note starts releases before the new one
    /// sounds and [`pair_note_events`](Self::pair_note_events)' per-pitch
    /// stack matches each off to its own note. The one exception is a
    /// zero-length note — its off has no open note to end, so it stays
    /// behind its own same-tick `NoteOn`.
    pub(crate) fn sort_events_by_tick(&mut self) {
        self.events
            .sort_by_key(|event| (event.tick(), event.event_type() != Some(EventType::NoteOff)));
        self.keep_zero_length_offs_after_their_ons();
        self.update_delta_ticks();
    }

    /// The zero-length-note pass of
    /// [`sort_events_by_tick`](Self::sort_events_by_tick): a `NoteOff` with
    /// no open note of its pitch moves behind the first same-pitch `NoteOn`
    /// on its tick, if there is one (otherwise it is a genuine orphan and
    /// stays put). Counts open notes per pitch as `pair_note_events` does.
    fn keep_zero_length_offs_after_their_ons(&mut self) {
        let mut open_notes = [0_u32; 256];
        let mut idx = 0;
        while idx < self.events.len() {
            let event = &self.events[idx];
            let Some(note) = event.note_number() else {
                idx += 1;
                continue;
            };
            let open = &mut open_notes[usize::from(note)];
            match event.event_type() {
                Some(EventType::NoteOn) => *open += 1,
                Some(EventType::NoteOff) if *open > 0 => *open -= 1,
                Some(EventType::NoteOff) => {
                    let tick = event.tick();
                    let own_on = self.events[idx + 1..]
                        .iter()
                        .take_while(|later| later.tick() == tick)
                        .position(|later| {
                            later.event_type() == Some(EventType::NoteOn)
                                && later.note_number() == Some(note)
                        });
                    if let Some(offset) = own_on {
                        // The off lands just behind its `NoteOn`; re-examine
                        // whatever slid into `idx`.
                        self.events[idx..=idx + 1 + offset].rotate_left(1);
                        continue;
                    }
                }
                None => {}
            }
            idx += 1;
        }
    }

    /// Recomputes each event's `delta_ticks` as the gap from the previous event
    /// in the (already sorted) list.
    pub(super) fn update_delta_ticks(&mut self) {
        let mut prev_tick = 0;

        for event in &mut self.events {
            event.set_delta_ticks(prev_tick);
            prev_tick = event.tick();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        core::time::Meter,
        models::{
            clip::Clip,
            event::{Event, EventType},
        },
    };

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    fn clip_with_region(region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    // --- shift_event_space ---

    #[test]
    fn shift_event_space_moves_events_region_and_cursor_together() {
        let mut clip = clip_with_region(100, 500);
        clip.add_event(on(50, 60)); // outside the region — kept
        clip.add_event(off(80, 60));
        clip.add_event(on(200, 61));
        clip.add_event(off(400, 61));
        clip.nudge_cursor_to_region_start();

        clip.shift_event_space(1000);

        let ticks: Vec<i32> = clip.events().iter().map(|e| e.tick()).collect();
        assert_eq!(ticks, vec![1050, 1080, 1200, 1400]);
        assert_eq!(clip.region().start(), 1100);
        assert_eq!(clip.region().end(), 1500);
        assert_eq!(clip.cursor_tick(), 1100);
        // Same phase inside the region, so the same thing plays.
        assert_eq!(clip.phase_from_event_tick(1200), 100);
    }

    #[test]
    fn align_window_start_to_bar_rounds_up_to_the_next_bar_line() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = clip_with_region(bar + 100, bar * 3);
        clip.add_event(on(50, 60));
        clip.align_window_start_to_bar(Meter::FOUR_FOUR);
        assert_eq!(clip.region().start(), bar * 2);
        assert_eq!(clip.region_length(), bar * 2 - 100, "length unchanged");
        assert_eq!(clip.events()[0].tick(), 50 + bar - 100);

        clip.align_window_start_to_bar(Meter::FOUR_FOUR);
        assert_eq!(clip.region().start(), bar * 2, "already aligned: no-op");
    }

    #[test]
    fn align_window_start_to_bar_uses_the_meters_bar_lines() {
        let three_four = Meter::new(3, 4).unwrap();
        let bar = three_four.bar_ticks();
        let mut clip = clip_with_region(bar + 100, bar * 3);
        clip.align_window_start_to_bar(three_four);
        assert_eq!(clip.region().start(), bar * 2);
    }

    // --- crop ---

    #[test]
    fn crop_retains_events_within_half_open_region() {
        let mut clip = clip_with_region(100, 500);
        clip.add_event(on(50, 60)); // before region — excluded
        clip.add_event(on(100, 61)); // at region_start — included
        clip.add_event(on(300, 62)); // in region — included
        clip.add_event(on(500, 63)); // at region_end — excluded (half-open)
        clip.crop(Meter::FOUR_FOUR.bar_ticks());

        let note_numbers: Vec<u8> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.note_number().unwrap())
            .collect();
        assert!(note_numbers.contains(&61));
        assert!(note_numbers.contains(&62));
        assert!(!note_numbers.contains(&60));
        assert!(!note_numbers.contains(&63));
    }

    #[test]
    fn crop_normalizes_event_ticks_to_zero_base() {
        let mut clip = clip_with_region(100, 500);
        clip.add_event(on(200, 60));
        clip.add_event(off(400, 60));
        clip.crop(Meter::FOUR_FOUR.bar_ticks());

        let note_on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(note_on_tick, 100); // 200 - region_start(100) = 100
    }

    #[test]
    fn crop_closes_open_notes_at_clip_boundary() {
        let mut clip = clip_with_region(0, 960);
        clip.add_event(on(0, 60)); // no matching note-off
        clip.crop(Meter::FOUR_FOUR.bar_ticks());

        let note_offs: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOff))
            .collect();
        assert!(
            !note_offs.is_empty(),
            "open note must receive a synthetic note-off"
        );
    }

    #[test]
    fn crop_empty_clip_does_not_panic() {
        let mut clip = clip_with_region(0, 960);
        clip.crop(Meter::FOUR_FOUR.bar_ticks());
    }

    // --- trim_before_tick ---

    #[test]
    fn trim_before_tick_discards_older_events_without_rebasing_ticks() {
        let mut clip = Clip::new();
        clip.add_event(on(100, 60));
        clip.add_event(off(200, 60));
        clip.add_event(on(1_000, 61));
        clip.add_event(off(1_200, 61));
        clip.calculate_note_lengths();

        clip.trim_before_tick(900);

        let ticks: Vec<i32> = clip.events().iter().map(|event| event.tick()).collect();
        assert_eq!(ticks, vec![1_000, 1_200]);
    }

    #[test]
    fn trim_before_tick_removes_orphan_note_offs() {
        let mut clip = Clip::new();
        clip.add_event(on(100, 60));
        clip.add_event(off(1_000, 60));
        clip.add_event(on(1_100, 61));
        clip.add_event(off(1_200, 61));
        clip.calculate_note_lengths();

        clip.trim_before_tick(900);

        let note_numbers: Vec<u8> = clip
            .events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| event.note_number().unwrap())
            .collect();
        assert_eq!(note_numbers, vec![61]);
        assert!(
            clip.events()
                .iter()
                .all(|event| event.note_number() != Some(60))
        );
    }

    // --- relocate_late_notes_to_region_start ---

    #[test]
    fn relocate_late_notes_moves_end_window_notes_to_region_start() {
        // late window = [region_end - tolerance, region_end) = [720, 960)
        let mut clip = Clip::new();
        clip.add_event(on(800, 60));
        clip.add_event(off(900, 60));

        clip.relocate_late_notes_to_region_start(0, 960, 240);

        let note_on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(note_on_tick, 0); // shifted from 800 to 0
    }

    #[test]
    fn relocate_late_notes_leaves_main_window_notes_unchanged() {
        let mut clip = Clip::new();
        clip.add_event(on(400, 60));
        clip.add_event(off(600, 60));

        clip.relocate_late_notes_to_region_start(0, 960, 240);

        let note_on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(note_on_tick, 400);
    }

    #[test]
    fn relocate_late_notes_noop_when_tolerance_is_zero() {
        let mut clip = Clip::new();
        clip.add_event(on(800, 60));
        clip.add_event(off(900, 60));

        clip.relocate_late_notes_to_region_start(0, 960, 0);

        let note_on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();
        assert_eq!(note_on_tick, 800);
    }

    // --- sort_events_by_tick ---

    /// `(tick, is NoteOn)` per event, in list order.
    fn order(clip: &Clip) -> Vec<(i32, bool)> {
        clip.events()
            .iter()
            .map(|e| (e.tick(), e.event_type() == Some(EventType::NoteOn)))
            .collect()
    }

    #[test]
    fn sort_puts_a_note_off_ahead_of_a_same_tick_note_on() {
        let mut clip = Clip::new();
        clip.events = vec![on(480, 60), off(960, 60), on(0, 60), off(480, 60)];
        clip.sort_events_by_tick();

        assert_eq!(
            order(&clip),
            vec![(0, true), (480, false), (480, true), (960, false)]
        );
        let pairing = clip.pair_note_events();
        assert_eq!(pairing.on_to_off.get(&0), Some(&1));
        assert_eq!(pairing.on_to_off.get(&2), Some(&3));
        assert_eq!(
            clip.events()
                .iter()
                .map(|e| e.delta_ticks())
                .collect::<Vec<_>>(),
            vec![0, 480, 0, 480]
        );
    }

    #[test]
    fn sort_keeps_a_zero_length_note_off_behind_its_note_on() {
        let mut clip = Clip::new();
        // A note ending at 480 where a zero-length note of the same pitch sits.
        clip.events = vec![on(480, 60), off(480, 60), on(0, 60), off(480, 60)];
        clip.sort_events_by_tick();

        assert_eq!(
            order(&clip),
            vec![(0, true), (480, false), (480, true), (480, false)]
        );
        let pairing = clip.pair_note_events();
        assert!(pairing.orphan_note_offs.is_empty());
        assert!(pairing.open_note_ons.is_empty());
    }

    #[test]
    fn sort_leaves_a_genuine_orphan_note_off_in_place() {
        let mut clip = Clip::new();
        clip.events = vec![on(480, 62), off(480, 60)];
        clip.sort_events_by_tick();

        assert_eq!(order(&clip), vec![(480, false), (480, true)]);
        assert_eq!(clip.pair_note_events().orphan_note_offs, vec![0]);
    }
}
