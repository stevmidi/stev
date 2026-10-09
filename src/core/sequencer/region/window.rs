//! Bar-aligned capture / phrase window maths.
//!
//! Pure functions — no `&self` — that derive a `(start, end)` tick window from
//! a clip's events and the transport loop. Two consumers:
//!
//! - **Running capture** ([`calculate_running_region`](Sequencer::calculate_running_region))
//!   pins a fixed-length window at the anchor's phase inside the transport
//!   pass holding the clip's most recently *inserted* note, so repeated loops
//!   of the same phrase yield the same loop-relative crop and the cursor
//!   position never shifts the events. See `100-running-capture.md`.
//! - **Stopped capture** (`calculate_phrase_window_*`, `phrase_end_*`,
//!   `clamp_*`) sizes the window of a just-recorded phrase from its last note
//!   plus a tail, floored at one bar. See `040-phrase-detection.md`.
//!
//! `stopped_capture.rs` and `capture.rs` are the callers; the region/cursor
//! mutators that consume the results live in `mod.rs`.

use crate::core::time::{self, Meter};
use crate::models::clip::Clip;
use crate::models::event::{Event, EventType};

use super::Sequencer;

impl Sequencer {
    /// Computes the `(start, end)` capture window for a running-capture clip relative to the
    /// transport loop.
    ///
    /// Running capture records against a continuously advancing clock, while playback loops over
    /// a finite transport region. `anchor_tick` is where the committed material begins in that
    /// clock space (the cursor, from the arranger), and `transport_loop` is the loop playback is
    /// wrapping inside — `Some(len)` when it actually wraps, `None` for a linear take (loop off,
    /// or a start after the region end that never reaches a wrap).
    ///
    /// The window is **pinned** at the anchor's phase: for a looping take it starts at
    /// `anchor + k · len` where `k` is the pass holding the latest note-on, for a linear take it
    /// starts at `anchor`. It never slides toward the last note — the clip is placed at the
    /// anchor, so any slide would move every event off the region phase it was played at.
    ///
    /// The window satisfies the following invariants:
    ///
    /// 1. The window length is at least the minimum clip length — a beat, the
    ///    shortest a clip can be (enforced via `max(min_clip_length_ticks())`).
    /// 2. The window start is the anchor's phase — `(start - anchor) % len == 0` when looping,
    ///    `start == anchor` when linear.
    /// 3. Successive passes with identical loop-relative phrase timing produce windows that are
    ///    offset by exactly one `len`, so the loop-relative placement never drifts.
    /// 4. Only the pass holding the latest note-on is windowed: a multi-pass take commits its
    ///    last cycle, and a note at a phase before the anchor (played after the wrap) selects
    ///    the pass it followed rather than pulling the window forward.
    ///
    /// A window longer than the loop (an existing clip longer than the arranger loop) simply
    /// extends past that pass's end; nothing later than the latest *inserted* note-on exists
    /// there to be swept in.
    pub(in crate::core::sequencer) fn calculate_running_region(
        clip: &Clip,
        anchor_tick: i32,
        region_length: i32,
        transport_loop: Option<i32>,
    ) -> (i32, i32) {
        let min_len = time::min_clip_length_ticks();
        let region_len = region_length.max(min_len);

        // The window starts at the anchor's phase in the pass holding the
        // latest note-on (the anchor itself for a linear take) and never
        // slides: the anchor is where the committed material begins, so any
        // slide is a phase error.
        let region_start_tick = transport_loop.map_or(anchor_tick, |loop_len| {
            let loop_len = loop_len.max(min_len);
            let last_note_on_tick =
                Self::last_inserted_event_tick(clip.events(), EventType::NoteOn);
            let loop_idx = (last_note_on_tick - anchor_tick).div_euclid(loop_len);
            anchor_tick + loop_idx * loop_len
        });

        (region_start_tick, region_start_tick + region_len)
    }

    /// A `(start, end)` phrase window of `region_length` ending at the clip's
    /// last event of `event_type`.
    pub(in crate::core::sequencer) fn calculate_phrase_window_from_last_note(
        clip: &Clip,
        event_type: EventType,
        region_length: i32,
    ) -> (i32, i32) {
        let last_note_tick = Self::max_event_tick(clip.events(), event_type);

        let start = (last_note_tick - region_length).max(0);
        let mut end = start + region_length;

        if end <= region_length {
            end = last_note_tick;
        }

        (start, end)
    }

    /// Phrase window end at the last `NoteOff` plus a two-beat tail (floored
    /// at `region_start + 1`), or `None` if the clip has no `NoteOff`.
    pub(in crate::core::sequencer) fn phrase_end_from_last_note_off_with_tail(
        clip: &Clip,
        region_start: i32,
    ) -> Option<i32> {
        let last_note_off_tick = Self::max_event_tick(clip.events(), EventType::NoteOff);

        (last_note_off_tick > 0).then_some(
            (last_note_off_tick + Self::phrase_default_tail_ticks()).max(region_start + 1),
        )
    }

    /// Widens `region_end` so the phrase window is at least one bar of `meter`.
    pub(in crate::core::sequencer) fn clamp_phrase_end_to_min_window(
        region_start: i32,
        region_end: i32,
        meter: Meter,
    ) -> i32 {
        region_end.max(region_start + meter.bar_ticks())
    }

    /// Default tail added past the last `NoteOff` for a phrase window — two
    /// beats.
    fn phrase_default_tail_ticks() -> i32 {
        time::beats_to_ticks(2.0)
    }

    /// Highest tick among a slice's events of `event_type` (`0` if none).
    fn max_event_tick(events: &[Event], event_type: EventType) -> i32 {
        events
            .iter()
            .filter(|e| e.event_type() == Some(event_type))
            .map(|e| e.tick())
            .max()
            .unwrap_or(0)
    }

    /// Returns the tick of the first *inserted* event matching `event_type`
    /// (`0` if none) — the earliest note of a take by insertion order, the
    /// counterpart of [`Self::last_inserted_event_tick`].
    pub(in crate::core::sequencer) fn first_inserted_event_tick(
        events: &[Event],
        event_type: EventType,
    ) -> i32 {
        events
            .iter()
            .find(|e| e.event_type() == Some(event_type))
            .map_or(0, |e| e.tick())
    }

    /// Returns the tick of the most recently *inserted* event matching `event_type`,
    /// searching from the end of the slice (newest-first).
    ///
    /// Unlike [`Self::max_event_tick`], this reflects insertion order rather than tick
    /// magnitude.  Use it when you want to anchor a capture window to the note the
    /// user *just* played, ignoring stale high-tick events that accumulated in the
    /// buffer from a previous clock context (e.g. after `clock_tick` was moved
    /// onto playback's phase by a `ClockCommand::AlignToPlayback` correction on a
    /// seek or a region restore — a plain loop wrap only nudges the phase and
    /// never moves the clock out of the loop it has free-run to).
    pub(in crate::core::sequencer) fn last_inserted_event_tick(
        events: &[Event],
        event_type: EventType,
    ) -> i32 {
        events
            .iter()
            .rev()
            .find(|e| e.event_type() == Some(event_type))
            .map_or(0, |e| e.tick())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequencer::test_support::note_on;

    fn clip_with_note_on(last_note_on_tick: i32) -> Clip {
        let mut clip = Clip::new();
        clip.add_event(note_on(last_note_on_tick));
        clip
    }

    #[test]
    fn phrase_end_from_last_note_off_with_tail_adds_two_beats() {
        let mut clip = Clip::new();
        clip.add_event(note_on(1200));
        clip.add_event(Event::new(1800, 0, vec![0x80, 60, 0]));

        let end = Sequencer::phrase_end_from_last_note_off_with_tail(&clip, 0);

        assert_eq!(end, Some(3720));
    }

    #[test]
    fn phrase_end_from_last_note_off_with_tail_respects_region_start() {
        let mut clip = Clip::new();
        clip.add_event(note_on(100));
        clip.add_event(Event::new(120, 0, vec![0x80, 60, 0]));

        let end = Sequencer::phrase_end_from_last_note_off_with_tail(&clip, 900);

        assert_eq!(end, Some(2040));
    }

    #[test]
    fn phrase_end_from_last_note_off_with_tail_returns_none_without_note_off() {
        let mut clip = Clip::new();
        clip.add_event(note_on(100));

        let end = Sequencer::phrase_end_from_last_note_off_with_tail(&clip, 0);

        assert_eq!(end, None);
    }

    #[test]
    fn clamp_phrase_end_to_min_window_extends_short_window() {
        let end = Sequencer::clamp_phrase_end_to_min_window(11_000, 11_960, Meter::FOUR_FOUR);

        assert_eq!(end, 14_840);
    }

    #[test]
    fn clamp_phrase_end_to_min_window_is_a_bar_of_the_meter() {
        let seven_eight = Meter::new(7, 8).unwrap();
        let end = Sequencer::clamp_phrase_end_to_min_window(1000, 1100, seven_eight);
        assert_eq!(end, 1000 + seven_eight.bar_ticks());
    }

    #[test]
    fn clamp_phrase_end_to_min_window_preserves_longer_window() {
        let end = Sequencer::clamp_phrase_end_to_min_window(11_000, 15_400, Meter::FOUR_FOUR);

        assert_eq!(end, 15_400);
    }

    #[test]
    fn calculate_running_region_first_bar_note_keeps_two_bar_window_at_loop_start() {
        let bar = time::bars_to_ticks(1);
        let clip = clip_with_note_on(600);

        let (start, end) = Sequencer::calculate_running_region(&clip, 0, bar * 2, Some(bar * 3));

        assert_eq!(start, 0);
        assert_eq!(end, bar * 2);
    }

    /// The window is pinned at the anchor's phase: a note past the window's
    /// end (bar 3 of a 3-bar loop, 2-bar window) does not pull it forward —
    /// the material is placed at the anchor, so a slide would shift it.
    #[test]
    fn calculate_running_region_window_stays_pinned_at_anchor_when_last_note_is_past_it() {
        let bar = time::bars_to_ticks(1);
        let clip = clip_with_note_on(bar * 2 + 600);

        let (start, end) = Sequencer::calculate_running_region(&clip, 0, bar * 2, Some(bar * 3));

        assert_eq!(start, 0);
        assert_eq!(end, bar * 2);
    }

    /// A window longer than the loop starts at the pass holding the last
    /// note and extends past that pass's end — there is nothing later than
    /// the last inserted note-on to sweep in.
    #[test]
    fn calculate_running_region_loop_shorter_than_window_starts_at_the_pass_holding_the_last_note()
    {
        let bar = time::bars_to_ticks(1);
        let clip = clip_with_note_on(bar * 2 + bar + 500); // pass 1 of a 2-bar loop

        let (start, end) = Sequencer::calculate_running_region(&clip, 0, bar * 3, Some(bar * 2));

        assert_eq!(start, bar * 2);
        assert_eq!(end, bar * 5);
    }

    /// Mid-loop anchor (a bar into a 2-bar loop), last note played in bar 0
    /// of the *next* pass — a phase before the anchor. The window is the
    /// remainder that note followed, not slid forward onto it.
    #[test]
    fn calculate_running_region_note_before_anchor_phase_keeps_the_previous_remainder() {
        let bar = time::bars_to_ticks(1);
        let clip = clip_with_note_on(bar * 2 + 100);

        let (start, end) = Sequencer::calculate_running_region(&clip, bar, bar, Some(bar * 2));

        assert_eq!(start, bar);
        assert_eq!(end, bar * 2);
    }

    /// A linear take (no wrap) always windows from the anchor, however far
    /// the last note is past the window.
    #[test]
    fn calculate_running_region_linear_take_starts_at_the_anchor() {
        let bar = time::bars_to_ticks(1);
        let clip = clip_with_note_on(bar * 7 + 600);

        let (start, end) = Sequencer::calculate_running_region(&clip, bar, bar * 2, None);

        assert_eq!(start, bar);
        assert_eq!(end, bar * 3);
    }

    #[test]
    fn calculate_running_region_same_phrase_on_successive_loops_keeps_relative_window_phase() {
        let bar = time::bars_to_ticks(1);
        let loop_len = bar * 3;
        let region_len = bar * 2;

        // Same loop-relative phrase timing, captured one full loop later.
        let clip_a = clip_with_note_on(bar + 600);
        let clip_b = clip_with_note_on(bar + 600 + loop_len);

        let (start_a, end_a) =
            Sequencer::calculate_running_region(&clip_a, 0, region_len, Some(loop_len));
        let (start_b, end_b) =
            Sequencer::calculate_running_region(&clip_b, 0, region_len, Some(loop_len));

        // Absolute window moves by one loop.
        assert_eq!(start_b - start_a, loop_len);
        assert_eq!(end_b - end_a, loop_len);

        // But loop-relative placement remains identical.
        assert_eq!(start_a.rem_euclid(loop_len), start_b.rem_euclid(loop_len));
        assert_eq!(end_a.rem_euclid(loop_len), end_b.rem_euclid(loop_len));
    }

    /// Regression: stale high-tick events from a previous clock context (e.g. clip
    /// view with multiple loop wraps) must not displace the window away from the
    /// most recently played note.
    ///
    /// Scenario: user was in clip view where clock_tick accumulated to 3 × loop_len
    /// (three wraps), leaving an old NoteOn at tick `3 * loop_len`.  After restoring
    /// the arranger region the clock is synced back to ~0, so the user's new bar-2
    /// note lands at tick `bar * 1 + 600` (well below the stale event).
    /// `calculate_running_region` must anchor to the *last inserted* note (the new
    /// one), not the max-tick stale one, so the window stays in loop 0.
    #[test]
    fn calculate_running_region_stale_high_tick_event_does_not_displace_window() {
        let bar = time::bars_to_ticks(1);
        let loop_len = bar * 2;
        let region_len = bar * 2;

        // Stale note from a prior clock context at a very high absolute tick.
        let stale_tick = loop_len * 3; // 3 full loops ahead
        // Recent note played in bar 2 of the current (newly restored) loop.
        let recent_bar2_tick = bar + 600;

        let mut clip = Clip::new();
        clip.add_event(note_on(stale_tick)); // inserted first — old, high tick
        clip.add_event(note_on(recent_bar2_tick)); // inserted last — recent, low tick

        // With the fix, the window anchors to `recent_bar2_tick` (loop 0).
        let (start, end) =
            Sequencer::calculate_running_region(&clip, 0, region_len, Some(loop_len));

        assert_eq!(start, 0, "window must be in loop 0 (start)");
        assert_eq!(end, loop_len, "window must be in loop 0 (end)");
        // The recent bar-2 note must fall inside the window.
        assert!(
            recent_bar2_tick >= start && recent_bar2_tick < end,
            "recent bar-2 note at {recent_bar2_tick} must be inside window [{start}, {end})"
        );
    }
}
