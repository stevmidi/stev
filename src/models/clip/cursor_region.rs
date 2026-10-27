//! The clip cursor, region-edge nudges, and the arrangement-tick ⇄ event-tick
//! mapping.
//!
//! The three conversions here — [`event_tick_from_arrangement_tick`](Clip::event_tick_from_arrangement_tick),
//! [`arrangement_tick_from_event_tick`](Clip::arrangement_tick_from_event_tick),
//! [`phase_from_event_tick`](Clip::phase_from_event_tick) — are the *only*
//! sanctioned way to cross between a clip's two timelines (`080-conventions.md`).
//! All three treat a zero-length region and a non-zero `region.start` as
//! ordinary cases.

use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use log::debug;

use crate::core::time::{self, Meter};

use super::Clip;

/// Where a clip sits and which part of its events plays: the arrangement
/// start plus the event-space window. Everything a clip-edge edit changes,
/// and nothing else — see `ResizeClipEdit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipBounds {
    /// Arrangement-timeline start ([`Clip::start_tick`]).
    pub(crate) start_tick: i32,
    /// Window start, in event ticks.
    pub(crate) region_start: i32,
    /// Window end, in event ticks.
    pub(crate) region_end: i32,
}

/// How a retime moved a clip's event-tick space: every tick `t` went to
/// `t * scale + offset`. [`Clip::adjust_to_tempo`] returns its scale and
/// [`Clip::align_window_start_to_bar`] its shift; a tempo fit chains them
/// ([`then`](Self::then)). The clip view maps its framing through it so the
/// notes stay where they were on screen (`220-capture-without-pending-view.md`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EventSpaceRetime {
    /// How much longer every span became.
    pub(crate) scale: f64,
    /// The shift after scaling, in event ticks.
    pub(crate) offset: f64,
}

impl EventSpaceRetime {
    /// Nothing moved.
    pub(crate) const IDENTITY: Self = Self {
        scale: 1.0,
        offset: 0.0,
    };

    /// `self`, then `next`.
    pub(crate) fn then(self, next: Self) -> Self {
        Self {
            scale: self.scale * next.scale,
            offset: self.offset * next.scale + next.offset,
        }
    }

    /// The retime that undoes `self`.
    pub(crate) fn inverse(self) -> Self {
        Self {
            scale: 1.0 / self.scale,
            offset: -self.offset / self.scale,
        }
    }

    /// Where `tick` lands after the retime.
    pub(crate) fn map_tick(&self, tick: i32) -> i32 {
        (f64::from(tick) * self.scale + self.offset).round() as i32
    }
}

/// Room past a clip's window end that its reach always includes, in bars:
/// the clip cursor can step into it and `]` can then lengthen the clip, even
/// when nothing lies after the end — a set end is never a wall
/// (`220-capture-without-pending-view.md`). The clip view scrolls over the
/// same span.
const CLIP_END_HEADROOM_BARS: i32 = 1;

/// The one definition of a clip's *reach*, the event-tick span its cursor
/// and its view can get to: `window` plus [`CLIP_END_HEADROOM_BARS`] after
/// it, widened to any `(start, end)` span further out, and its start pulled
/// back to the bar line at or before it. [`Clip::reach`] takes it over the
/// clip's events; the clip view's scroll range over its note shapes.
///
/// The bar line is the start's counterpart of the end's headroom: a set
/// start is never a wall either. Without it a start moved past the gap
/// before the first note couldn't come back — the reach shrank to the first
/// note, and the bar line the clip began on was out of reach of the cursor
/// and the window band (found 2026-10-12).
pub(crate) fn reach_over(
    meter: Meter,
    (window_start, window_end): (i32, i32),
    spans: impl Iterator<Item = (i32, i32)>,
) -> (i32, i32) {
    let end_with_headroom = window_end + meter.bars_to_ticks(CLIP_END_HEADROOM_BARS);
    let (lo, hi) = spans.fold(
        (window_start, end_with_headroom),
        |(lo, hi), (start, end)| (lo.min(start), hi.max(end)),
    );
    (meter.bar_line_at_or_before(lo), hi)
}

/// The shortest window [`window_with_edge_at`] keeps: a clip's minimum
/// length (`time::min_clip_length_ticks`, a beat), except for the take of an
/// open take session (`framing_take`, `290-framing-a-take.md`), whose window
/// only picks the notes to insert and never sets a clip's length — a tick, so
/// a phrase shorter than a beat can be framed by hand as well as by detection
/// (found 2026-10-27: the start stopped a beat before the end).
pub(crate) fn min_window_length(framing_take: bool) -> i32 {
    if framing_take {
        1
    } else {
        time::min_clip_length_ticks()
    }
}

/// The clip view's start/end marker rule, shared by `[`/`]` and the window
/// band (`Sequencer::selected_clip_marker_at`, and
/// the band's drag preview, which must land where the release will):
/// `window` with `edge` moved to the event tick `tick`. `room` is the
/// arrangement room the clip has before the next clip on its track, `None`
/// with none after it; `min_length` is [`min_window_length`].
///
/// - **Start**: moves alone, the end stays (the clip keeps its arrangement
///   start, so its length follows) — at least `min_length` before the end,
///   and no earlier than the room allows, since an earlier start lengthens
///   the clip; `None` when not even that fits. Never below tick 0.
/// - **End**: moves alone — at least `min_length` after the start, and no
///   further than the room.
pub(crate) fn window_with_edge_at(
    edge: ClipEdge,
    tick: i32,
    (start, end): (i32, i32),
    room: Option<i32>,
    min_length: i32,
) -> Option<(i32, i32)> {
    match edge {
        ClipEdge::Start => {
            let latest = end - min_length;
            let earliest = room.map_or(0, |room| end - room).max(0);
            (earliest <= latest).then(|| (tick.clamp(earliest, latest), end))
        }
        ClipEdge::End => {
            let length = (tick - start).max(min_length).min(room.unwrap_or(i32::MAX));
            Some((start, start + length))
        }
    }
}

/// Which edge of a clip an edge edit moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipEdge {
    /// The left edge (`[`).
    Start,
    /// The right edge (`]`).
    End,
}

impl Clip {
    /// The clip's current [`ClipBounds`].
    pub(crate) fn bounds(&self) -> ClipBounds {
        ClipBounds {
            start_tick: self.start_tick(),
            region_start: self.region.start(),
            region_end: self.region.end(),
        }
    }

    /// Moves the clip to `bounds`, writing through the shared region atomics
    /// (the transport and the clip view see the change without a re-send).
    /// Events are untouched.
    pub(crate) fn set_bounds(&mut self, bounds: ClipBounds) {
        self.set_start_tick(bounds.start_tick);
        self.region
            .set_region(Some(bounds.region_start), Some(bounds.region_end));
    }

    /// The edit cursor's position, in event-tick space.
    pub(crate) fn cursor_tick(&self) -> i32 {
        self.cursor_tick.load(Ordering::Relaxed)
    }

    /// A clone of the cursor atomic — handed to the transport / UI.
    pub(crate) fn cursor_tick_atomic(&self) -> Arc<AtomicI32> {
        Arc::clone(&self.cursor_tick)
    }

    /// Moves the cursor by a signed tick amount, clamped to the clip's
    /// [`reach`](Self::reach) — the window plus any material kept outside it,
    /// so the cursor can get to notes a trim or a stopped capture commit left
    /// outside the window (`220-capture-without-pending-view.md`).
    pub(crate) fn nudge_cursor_by_ticks(&mut self, nudge: i32, meter: Meter) {
        let (reach_start, reach_end) = self.reach(meter);
        let new_cursor = (self.cursor_tick() + nudge).clamp(reach_start, reach_end);

        self.cursor_tick.store(new_cursor, Ordering::Relaxed);
    }

    /// The event-tick span the clip cursor can reach ([`reach_over`] its
    /// note edges): the window plus [`CLIP_END_HEADROOM_BARS`] after it,
    /// widened to the first note and the last note's end when notes lie
    /// further out. A wheel move never widens it — the clip view draws only
    /// notes, and its scroll range is the same [`reach_over`] of them.
    pub(crate) fn reach(&self, meter: Meter) -> (i32, i32) {
        reach_over(
            meter,
            (self.region.start(), self.region.end()),
            self.events
                .iter()
                .filter(|event| event.is_note_edge())
                .map(|event| (event.tick(), event.end_tick())),
        )
    }

    /// Where playback starting at the event-space `cursor` begins in the
    /// arrangement: the tick the cursor maps to when it is inside the window,
    /// otherwise the clip's start. Material outside the window never plays,
    /// so there is no arrangement tick for it (the phase mapping would wrap
    /// it to an unrelated point of the loop).
    pub(crate) fn play_from_arrangement_tick(&self, cursor: i32) -> i32 {
        if self.is_in_window(cursor) {
            self.arrangement_tick_from_event_tick(cursor)
        } else {
            self.start_tick()
        }
    }

    /// Whether `event_tick` is inside the window, the part of the clip that
    /// plays.
    pub(crate) fn is_in_window(&self, event_tick: i32) -> bool {
        (self.region.start()..self.region.end()).contains(&event_tick)
    }

    /// Puts the cursor at `tick`, unclamped — for restoring a saved position
    /// (an undo), where the tick is one the clip already had.
    pub(crate) fn set_cursor_tick(&mut self, tick: i32) {
        self.cursor_tick.store(tick, Ordering::Relaxed);
    }

    /// Jumps the cursor to `region.start`.
    pub(crate) fn nudge_cursor_to_region_start(&mut self) {
        self.set_cursor_tick(self.region.start());
    }

    /// Places the cursor at the event-tick position that `absolute_tick`
    /// (arrangement space) maps to — used for click-to-place in the arranger.
    pub(crate) fn sync_clip_cursor_with_absolute_tick(&mut self, absolute_tick: i32) {
        self.set_cursor_tick(self.event_tick_from_arrangement_tick(absolute_tick));
    }

    /// Phase (0..region_length) for an event-space tick within this clip.
    pub(crate) fn phase_from_event_tick(&self, event_tick: i32) -> i32 {
        let len = self.region_length();
        if len <= 0 {
            return 0;
        }
        (event_tick - self.region.start()).rem_euclid(len)
    }

    /// Convert arrangement/global playback ticks -> event-space ticks.
    pub(crate) fn event_tick_from_arrangement_tick(&self, playback_tick: i32) -> i32 {
        let len = self.region_length();
        if len <= 0 {
            return self.region.start();
        }

        let phase = (playback_tick - self.start_tick()).rem_euclid(len);
        self.region.start() + phase
    }

    /// Convert event-space ticks -> arrangement/global playback ticks.
    pub(crate) fn arrangement_tick_from_event_tick(&self, event_tick: i32) -> i32 {
        self.start_tick() + self.phase_from_event_tick(event_tick)
    }

    /// Rescales the clip's region and every event tick from `clip_tempo` to
    /// `current_tempo` so it keeps its wall-clock timing when the project tempo
    /// differs from the one it was recorded at, then bar-snaps the region.
    /// Re-sorts and re-pairs note lengths afterwards. Returns the scale it
    /// applied to the event ticks.
    pub(crate) fn adjust_to_tempo(
        &mut self,
        clip_tempo: i32,
        current_tempo: i32,
        meter: Meter,
    ) -> EventSpaceRetime {
        let region_start =
            Self::adjust_tick_to_tempo(self.region.start(), clip_tempo, current_tempo);
        let region_end = Self::adjust_tick_to_tempo(self.region.end(), clip_tempo, current_tempo);

        self.region.set_region(Some(region_start), Some(region_end));

        // Snap to the nearest whole bar of `meter`, never below one
        let bar = meter.bar_ticks();
        let target_length = time::snap_to_grid(self.region_length(), bar).max(bar);
        self.region
            .set_region(None, Some(self.region.start() + target_length));

        debug!(
            "Adjusted clip region to tempo: {} -> {}, snapped to {} ticks",
            self.region.start(),
            self.region.end(),
            self.region_length()
        );

        for event in &mut self.events {
            event.set_tick(Self::adjust_tick_to_tempo(
                event.tick(),
                clip_tempo,
                current_tempo,
            ));
        }
        // The cursor lives in the same event space, so it moves with the
        // notes: it stays on the musical point it was on (after a first-clip
        // fit, exactly on the new end the user set).
        self.set_cursor_tick(Self::adjust_tick_to_tempo(
            self.cursor_tick(),
            clip_tempo,
            current_tempo,
        ));

        self.sort_events_by_tick();
        self.calculate_note_lengths();
        // The scale `adjust_tick_to_tempo` actually applies: whole µs per tick.
        EventSpaceRetime {
            scale: f64::from(time::ticks_to_microseconds(current_tempo, 1))
                / f64::from(time::ticks_to_microseconds(clip_tempo, 1).max(1)),
            offset: 0.0,
        }
    }

    /// Maps one tick from the `clip_tempo` grid to the `current_tempo` grid,
    /// preserving the instant it represents. `0` if `clip_tempo` is invalid.
    fn adjust_tick_to_tempo(tick: i32, clip_tempo: i32, current_tempo: i32) -> i32 {
        if clip_tempo <= 0 {
            return 0;
        }

        let clip_time_us = time::ticks_to_microseconds(clip_tempo, tick);
        let current_time_us = time::ticks_to_microseconds(current_tempo, tick);
        let time_diff = current_time_us - clip_time_us;
        let ticks_diff =
            (time_diff as f64 / time::ticks_to_microseconds(clip_tempo, 1) as f64).round() as i32;

        tick + ticks_diff
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{
        CLIP_END_HEADROOM_BARS, ClipEdge, EventSpaceRetime, min_window_length, reach_over,
        window_with_edge_at,
    };

    use crate::core::time::{self, Meter};
    use crate::models::{
        clip::{Clip, ClipBounds},
        event::Event,
    };

    /// Create a clip positioned at `start_tick` on the arrangement timeline
    /// with its internal region set to [`region_start`, `region_end`].
    fn make_clip(start_tick: i32, region_start: i32, region_end: i32) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut()
            .set_region(Some(region_start), Some(region_end));
        clip
    }

    #[test]
    fn set_bounds_round_trips_and_shares_the_region_atomics() {
        let mut clip = make_clip(100, 0, 960);
        let region_end = clip.region().end_atomic();
        let bounds = ClipBounds {
            start_tick: 40,
            region_start: 480,
            region_end: 2400,
        };

        clip.set_bounds(bounds);

        assert_eq!(clip.bounds(), bounds);
        assert_eq!(clip.end_tick(), 40 + 1920);
        assert_eq!(region_end.load(Ordering::Relaxed), 2400, "same atomics");
    }

    #[test]
    fn phase_from_event_tick_non_zero_region_start() {
        // region [480, 1440), len=960; event at 720 → phase = 720 - 480 = 240
        let clip = make_clip(0, 480, 1440);
        assert_eq!(clip.phase_from_event_tick(720), 240);
    }

    #[test]
    fn phase_from_event_tick_zero_length_region_returns_zero() {
        let clip = make_clip(0, 100, 100);
        assert_eq!(clip.phase_from_event_tick(100), 0);
    }

    #[test]
    fn phase_from_event_tick_wraps_at_region_length() {
        // event exactly region_len past region_start → phase 0
        let clip = make_clip(0, 0, 960);
        assert_eq!(clip.phase_from_event_tick(960), 0);
    }

    // --- event_tick_from_arrangement_tick ---

    #[test]
    fn event_tick_from_arrangement_first_loop_identity() {
        // clip at 0, region [0, 960); arrangement 240 → event 240
        let clip = make_clip(0, 0, 960);
        assert_eq!(clip.event_tick_from_arrangement_tick(240), 240);
    }

    #[test]
    fn event_tick_from_arrangement_wraps_on_second_loop() {
        // arrangement 960 → wraps → event 0; arrangement 1200 → event 240
        let clip = make_clip(0, 0, 960);
        assert_eq!(clip.event_tick_from_arrangement_tick(960), 0);
        assert_eq!(clip.event_tick_from_arrangement_tick(1200), 240);
    }

    #[test]
    fn event_tick_from_arrangement_preserves_non_zero_region_start() {
        // clip at 0, region [480, 1440); arrangement 0 → phase 0 → event 480
        let clip = make_clip(0, 480, 1440);
        assert_eq!(clip.event_tick_from_arrangement_tick(0), 480);
    }

    #[test]
    fn event_tick_from_arrangement_accounts_for_clip_start_offset() {
        // clip starts at 1000, region [0, 960); arrangement 1240 → phase (1240-1000)%960=240 → event 240
        let clip = make_clip(1000, 0, 960);
        assert_eq!(clip.event_tick_from_arrangement_tick(1240), 240);
    }

    #[test]
    fn event_tick_from_arrangement_zero_length_returns_region_start() {
        let clip = make_clip(0, 100, 100);
        assert_eq!(clip.event_tick_from_arrangement_tick(0), 100);
    }

    // --- arrangement_tick_from_event_tick ---

    #[test]
    fn arrangement_from_event_tick_basic() {
        // clip at 0, region [0, 960); event 240 → arrangement 240
        let clip = make_clip(0, 0, 960);
        assert_eq!(clip.arrangement_tick_from_event_tick(240), 240);
    }

    #[test]
    fn arrangement_from_event_tick_with_clip_start_offset() {
        // clip at 1000, region [0, 960); event 240 → arrangement 1000+240=1240
        let clip = make_clip(1000, 0, 960);
        assert_eq!(clip.arrangement_tick_from_event_tick(240), 1240);
    }

    #[test]
    fn arrangement_from_event_tick_non_zero_region_start() {
        // clip at 0, region [480, 1440); event 720 → phase 240 → arrangement 0+240=240
        let clip = make_clip(0, 480, 1440);
        assert_eq!(clip.arrangement_tick_from_event_tick(720), 240);
    }

    // --- nudge_cursor_by_ticks ---

    #[test]
    fn nudge_cursor_by_ticks_clamps_at_the_bar_line_before_region_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = make_clip(0, bar + 100, bar + 500);
        clip.set_cursor_tick(bar + 100);
        clip.nudge_cursor_by_ticks(-9999, Meter::FOUR_FOUR);
        assert_eq!(clip.cursor_tick(), bar);
    }

    #[test]
    fn nudge_cursor_by_ticks_clamps_a_bar_past_the_region_end() {
        let mut clip = make_clip(0, 0, 500);
        clip.nudge_cursor_by_ticks(99_999, Meter::FOUR_FOUR);
        assert_eq!(
            clip.cursor_tick(),
            500 + Meter::FOUR_FOUR.bars_to_ticks(CLIP_END_HEADROOM_BARS)
        );
    }

    #[test]
    fn nudge_cursor_by_ticks_normal_movement() {
        let mut clip = make_clip(0, 0, 960);
        clip.nudge_cursor_by_ticks(240, Meter::FOUR_FOUR);
        assert_eq!(clip.cursor_tick(), 240);
    }

    #[test]
    fn nudge_cursor_by_ticks_reaches_notes_outside_the_window() {
        // Window [960, 1920); a note before it at 100..400 and one after it
        // ending at 2500 — inside the bar of headroom after the window, which
        // is what the reach ends at.
        let mut clip = make_clip(0, 960, 1920);
        clip.add_event(Event::new(100, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(400, 0, vec![0x80, 60, 0]));
        clip.add_event(Event::new(2000, 0, vec![0x90, 62, 100]));
        clip.add_event(Event::new(2500, 0, vec![0x80, 62, 0]));
        clip.nudge_cursor_to_region_start();

        clip.nudge_cursor_by_ticks(-9999, Meter::FOUR_FOUR);
        assert_eq!(
            clip.cursor_tick(),
            0,
            "down to the first kept note's bar line"
        );
        clip.nudge_cursor_by_ticks(9999, Meter::FOUR_FOUR);
        let headroom_end = 1920 + Meter::FOUR_FOUR.bars_to_ticks(CLIP_END_HEADROOM_BARS);
        assert_eq!(clip.cursor_tick(), headroom_end, "a bar past the end");
        assert_eq!(clip.reach(Meter::FOUR_FOUR), (0, headroom_end));
        assert!(!clip.is_in_window(100));
        assert!(clip.is_in_window(960));
        assert!(!clip.is_in_window(1920), "the window is half-open");
    }

    #[test]
    fn reach_over_widens_the_window_and_its_headroom_to_spans_outside() {
        let window = (
            Meter::FOUR_FOUR.bars_to_ticks(4),
            Meter::FOUR_FOUR.bars_to_ticks(6),
        );
        assert_eq!(
            reach_over(Meter::FOUR_FOUR, window, std::iter::empty()),
            (
                Meter::FOUR_FOUR.bars_to_ticks(4),
                Meter::FOUR_FOUR.bars_to_ticks(7)
            ),
            "a bar of headroom after the end"
        );
        assert_eq!(
            reach_over(
                Meter::FOUR_FOUR,
                window,
                [
                    (100, 400),
                    (
                        Meter::FOUR_FOUR.bars_to_ticks(5),
                        Meter::FOUR_FOUR.bars_to_ticks(9)
                    )
                ]
                .into_iter(),
            ),
            (0, Meter::FOUR_FOUR.bars_to_ticks(9)),
            "spans further out widen it, the start to its bar line"
        );
    }

    /// A start moved past the gap before the first note can come back to
    /// the bar line the clip began on: the reach starts on the bar line at
    /// or before the window start and the first note, in the meter's bars.
    #[test]
    fn the_reach_starts_on_the_bar_line_before_the_window_and_the_first_note() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        // Window moved from 0 to beat 3; the first note just after beat 1.
        let mut clip = make_clip(0, bar / 2, bar * 2);
        clip.add_event(Event::new(30, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(400, 0, vec![0x80, 60, 0]));
        assert_eq!(clip.reach(Meter::FOUR_FOUR).0, 0);

        let empty = |window_start| {
            reach_over(
                Meter::FOUR_FOUR,
                (window_start, bar * 9),
                std::iter::empty(),
            )
        };
        assert_eq!(
            empty(bar * 5 + 960).0,
            bar * 5,
            "a window start off the bar line"
        );
        assert_eq!(empty(bar * 5).0, bar * 5, "one on it stays");

        let three_four = Meter::new(3, 4).unwrap();
        let three_four_bar = three_four.bar_ticks();
        assert_eq!(
            reach_over(
                three_four,
                (three_four_bar + 960, three_four_bar * 4),
                std::iter::empty()
            )
            .0,
            three_four_bar
        );
    }

    /// The headroom after the end is one bar of the project's meter.
    #[test]
    fn the_reach_headroom_is_a_bar_of_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let bar = three_four.bar_ticks();
        assert_eq!(
            reach_over(three_four, (0, bar * 2), std::iter::empty()),
            (0, bar * 3)
        );
    }

    #[test]
    fn the_reach_reaches_past_a_late_note_and_a_bar_past_an_empty_end() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = make_clip(0, 0, bar);
        assert_eq!(
            clip.reach(Meter::FOUR_FOUR),
            (0, bar * 2),
            "a bar of room after the end"
        );
        clip.add_event(Event::new(bar * 3, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(bar * 3 + 100, 0, vec![0x80, 60, 0]));
        assert_eq!(
            clip.reach(Meter::FOUR_FOUR),
            (0, bar * 3 + 100),
            "a later note reaches further"
        );
        clip.add_event(Event::new(bar * 5, 0, vec![0xE0, 0x00, 0x40]));
        clip.sort_events_by_tick();
        assert_eq!(
            clip.reach(Meter::FOUR_FOUR),
            (0, bar * 3 + 100),
            "a wheel move doesn't, as the view can't show it"
        );
    }

    #[test]
    fn a_tempo_rescale_moves_the_cursor_with_the_notes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = make_clip(0, 0, bar * 2);
        clip.add_event(Event::new(bar, 0, vec![0x90, 60, 100]));
        clip.nudge_cursor_by_ticks(bar, Meter::FOUR_FOUR);
        // 120 → 60 BPM: every tick position halves.
        clip.adjust_to_tempo(1_000_000, 500_000, Meter::FOUR_FOUR);
        assert_eq!(
            clip.events()[0].tick(),
            clip.cursor_tick(),
            "still on the note"
        );
    }

    /// A tempo retime snaps the window to whole bars of the project's
    /// meter: 3.4 bars of 3/4 land on 3 bars of 3/4, not on 4/4 bars.
    #[test]
    fn a_tempo_retime_snaps_to_bars_of_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let bar = three_four.bar_ticks();
        let mut clip = make_clip(0, 0, bar * 34 / 10);
        clip.adjust_to_tempo(500_000, 500_000, three_four);
        assert_eq!(clip.region_length(), bar * 3);
    }

    #[test]
    fn play_from_outside_the_window_starts_at_the_clip_start() {
        let clip = make_clip(3840, 960, 1920);
        assert_eq!(clip.play_from_arrangement_tick(1200), 3840 + 240);
        assert_eq!(
            clip.play_from_arrangement_tick(100),
            3840,
            "before the window"
        );
        assert_eq!(
            clip.play_from_arrangement_tick(2400),
            3840,
            "after the window"
        );
    }

    #[test]
    fn nudge_cursor_to_region_start_sets_cursor_to_region_start() {
        let mut clip = make_clip(0, 480, 960);
        clip.nudge_cursor_by_ticks(100, Meter::FOUR_FOUR); // move to 100 (clamped to 480)
        clip.nudge_cursor_to_region_start();
        assert_eq!(clip.cursor_tick(), 480);
    }

    /// The retimes `adjust_to_tempo` and `align_window_start_to_bar` return,
    /// chained, are the map the events went through: every old tick lands
    /// on its new one (to rounding), and the inverse takes it back.
    #[test]
    fn the_returned_retimes_map_every_tick() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = make_clip(0, bar * 4 + 300, bar * 6 + 300);
        let old_ticks = [12, bar * 4 + 300, bar * 5, bar * 7 + 17];
        for &tick in &old_ticks {
            clip.add_event(Event::new(tick, 0, vec![0x90, 60, 100]));
        }

        let retime = clip
            .adjust_to_tempo(450_000, 500_000, Meter::FOUR_FOUR)
            .then(clip.align_window_start_to_bar(Meter::FOUR_FOUR));

        let new_ticks: Vec<i32> = clip.events().iter().map(Event::tick).collect();
        for (&old, &new) in old_ticks.iter().zip(&new_ticks) {
            assert!((retime.map_tick(old) - new).abs() <= 1, "{old}: {new}");
            assert!(
                (retime.inverse().map_tick(new) - old).abs() <= 1,
                "{new}: {old}"
            );
        }
        assert_eq!(EventSpaceRetime::IDENTITY.then(retime), retime);
    }

    /// The marker rule `[`/`]` and the window band's preview share: the
    /// start moves alone, kept a minimum length before the end, not before
    /// tick 0 nor earlier than the room lets the clip grow; the end moves
    /// alone, kept a minimum length after the start and inside the room.
    #[test]
    fn window_with_edge_at_moves_one_edge_inside_the_room() {
        let min = min_window_length(false);
        assert_eq!(min, time::min_clip_length_ticks());
        let window = (960, 3840);
        let at = |edge, tick, room| window_with_edge_at(edge, tick, window, room, min);

        assert_eq!(at(ClipEdge::Start, 480, None), Some((480, 3840)));
        assert_eq!(
            at(ClipEdge::Start, -500, None),
            Some((0, 3840)),
            "not below 0"
        );
        assert_eq!(at(ClipEdge::Start, 9999, None), Some((3840 - min, 3840)));
        assert_eq!(
            at(ClipEdge::Start, 0, Some(3000)),
            Some((840, 3840)),
            "an earlier start lengthens the clip: held to the room"
        );
        assert_eq!(
            at(ClipEdge::Start, 0, Some(min - 1)),
            None,
            "no room at all"
        );

        assert_eq!(at(ClipEdge::End, 2000, None), Some((960, 2000)));
        assert_eq!(at(ClipEdge::End, 0, None), Some((960, 960 + min)));
        assert_eq!(at(ClipEdge::End, 9999, Some(4000)), Some((960, 4960)));
    }

    /// A take's window is held only a tick apart, not a beat: a phrase
    /// shorter than a beat (two quick last notes) can be framed by moving
    /// the start up to its first note (found 2026-10-27).
    #[test]
    fn a_take_window_can_be_shorter_than_a_beat() {
        let min = min_window_length(true);
        assert_eq!(min, 1);
        let window = (960, 3840);
        let at = |edge, tick| window_with_edge_at(edge, tick, window, None, min);

        let under_a_beat = 3840 - time::min_clip_length_ticks() / 2;
        assert_eq!(
            at(ClipEdge::Start, under_a_beat),
            Some((under_a_beat, 3840))
        );
        assert_eq!(at(ClipEdge::Start, 9999), Some((3839, 3840)));
        assert_eq!(at(ClipEdge::End, 1000), Some((960, 1000)));
    }
}
