//! Selected-clip region and cursor operations on the `Sequencer`.
//!
//! This is the sequencer-side of what `models/clip/cursor_region.rs` does for
//! one clip: resolve "the selected clip", apply the edit, keep the transport in
//! step.
//!
//! ## Module split
//!
//! - `mod.rs` — the region/cursor accessors and mutators: clip edge targets
//!   and trims, the clip cursor.
//! - `tempo.rs` — first-clip tempo detection and clip-relative tempo rescale.
//! - `window.rs` — the pure `calculate_*` helpers that derive bar-aligned
//!   capture / phrase windows (`100-running-capture.md`, `040-phrase-detection.md`).
//! - `phrase_tokens.rs` — phrase-token detection: where the last phrase starts.

mod phrase_tokens;
mod tempo;
mod window;

use uuid::Uuid;

use crate::{
    core::time,
    models::clip::{Clip, ClipBounds, ClipEdge},
};

use super::Sequencer;

/// What `[`/`]` would do: move `clip_id` (selected track) to `bounds` —
/// [`Sequencer::clip_edge_target`].
pub(crate) struct EdgeTarget {
    /// The clip the edge edit acts on.
    pub(crate) clip_id: Uuid,
    /// Its bounds after the edit.
    pub(crate) bounds: ClipBounds,
}

impl Sequencer {
    // --- Clip/region accessors ---
    /// The selected clip's cursor, in event ticks.
    pub(crate) fn selected_clip_cursor_tick(&self) -> Option<i32> {
        self.selected_clip().map(Clip::cursor_tick)
    }

    /// `⌥Space` in the clip view: where playback from the lead clip's cursor
    /// starts in the arrangement — the clip's start when the cursor sits on
    /// material outside the window (`Clip::play_from_arrangement_tick`).
    pub(crate) fn selected_clip_play_from_tick(&self) -> Option<i32> {
        self.selected_clip()
            .map(|clip| clip.play_from_arrangement_tick(clip.cursor_tick()))
    }

    /// Target tick for a grid-snapped cursor step (Left/Right inside a clip):
    /// see [`clip_cursor_grid_step_target`](Self::clip_cursor_grid_step_target).
    /// `step_ticks` is resolved by the view in `Clip` — the zoom-adaptive
    /// snap, the same resolution as `Display::cursor_grid_ticks()`'s mouse
    /// snap. `None` when no clip is selected. The caller routes the result
    /// through `set_clip_cursor_workflow` so the transport cursor stays in
    /// sync.
    pub(crate) fn grid_snapped_selected_clip_cursor_tick(&self, step_ticks: i32) -> Option<i32> {
        self.selected_clip_cursor_tick()
            .map(|current| Self::clip_cursor_grid_step_target(current, step_ticks))
    }

    /// Pure core of a grid-snapped clip-cursor step: `step_ticks > 0` jumps
    /// forward to the next multiple of `|step_ticks|`, otherwise back to the
    /// previous one (a full step back when already sitting exactly on one).
    /// A zero step is treated as a one-tick grid, never a division by zero.
    fn clip_cursor_grid_step_target(current: i32, step_ticks: i32) -> i32 {
        time::step_to_grid(current, step_ticks.abs().max(1), step_ticks.signum())
    }

    // --- Clip/region mutators ---
    /// The clip `[`/`]` trim in the arranger: the one under the cursor on the
    /// selected track, otherwise the nearest one on the side the edge faces —
    /// for [`ClipEdge::Start`] the next clip starting after the cursor, for
    /// [`ClipEdge::End`] the last one ending at or before it — so the keys can
    /// grow a clip toward the cursor as well as shrink it.
    pub(crate) fn arranger_edge_clip_id(&self, edge: ClipEdge) -> Option<Uuid> {
        let cursor = self.cursor_tick();
        let track = self.selected_track()?;
        if let Some(id) = track.find_clip_id_at(cursor) {
            return Some(id);
        }

        let clips = track.clips().iter();
        match edge {
            ClipEdge::Start => clips
                .filter(|clip| clip.start_tick() > cursor)
                .min_by_key(|clip| clip.start_tick()),
            ClipEdge::End => clips
                .filter(|clip| clip.end_tick() <= cursor)
                .max_by_key(|clip| clip.end_tick()),
        }
        .map(Clip::id)
    }

    /// The arrangement span of the project's only clip, on whichever track —
    /// `None` with none or more than one. While a project has one clip, the
    /// loop region is kept on it (`EventHandlers::loop_sole_clip_workflow`,
    /// `220-capture-without-pending-view.md`).
    pub(crate) fn sole_clip_span(&self) -> Option<(i32, i32)> {
        if self.number_of_clips() != 1 {
            return None;
        }
        let clip = self.tracks.iter().find_map(|track| track.clips().first())?;
        Some((clip.start_tick(), clip.end_tick()))
    }

    /// Which clip `[`/`]` act on and the bounds they'd give it
    /// (`EventHandlers::clip_edge_to_cursor_workflow`). Always at the cursor
    /// the user placed, playing or not: in the arranger (`in_clip_view` off)
    /// an arrangement trim at the arranger cursor on
    /// [`Self::arranger_edge_clip_id`]'s clip; in the clip view a start/end
    /// marker on the lead clip at its cursor. While the project has one clip
    /// the end is exact in the clip view too — that length is what Enter fits
    /// the tempo to (`RetimeClipEdit`). `None` when there is nothing to act
    /// on or nothing would change.
    pub(crate) fn clip_edge_target(
        &self,
        edge: ClipEdge,
        in_clip_view: bool,
    ) -> Option<EdgeTarget> {
        let (clip_id, bounds) = if in_clip_view {
            let clip = self.selected_clip()?;
            let tick = clip.cursor_tick();
            // While the project has one clip, its length is the tempo
            // reference that Enter fits: never rounded.
            let whole_bars = self.number_of_clips() != 1;
            let bounds = match edge {
                ClipEdge::Start => self.selected_clip_start_marker_at(tick),
                ClipEdge::End => self.selected_clip_end_marker_at(tick, whole_bars),
            };
            (clip.id(), bounds?)
        } else {
            let clip_id = self.arranger_edge_clip_id(edge)?;
            let tick = self.cursor_tick();
            let bounds = match edge {
                ClipEdge::Start => self.clip_start_trimmed_to(clip_id, tick),
                ClipEdge::End => self.clip_end_trimmed_to(clip_id, tick),
            };
            (clip_id, bounds?)
        };

        Some(EdgeTarget { clip_id, bounds })
    }

    /// Where the right edge of `clip_id` (on the selected track) would go for
    /// an arrangement-time
    /// trim to `target_end_tick` (the mouse edge drag, `]` in the arranger):
    /// the end moves, the start and the content stay where they are in time.
    /// Clamped to `[start + minimum clip length, next clip's start]` (a beat —
    /// `time::min_clip_length_ticks`). `None` on a no-op. Applied through
    /// `ResizeClipEdit`.
    pub(crate) fn clip_end_trimmed_to(
        &self,
        clip_id: Uuid,
        target_end_tick: i32,
    ) -> Option<ClipBounds> {
        let min_length = time::min_clip_length_ticks();
        let track = self.selected_track()?;
        let clip = track.get_clip_by_id(clip_id)?;
        let start_tick = clip.start_tick();

        let max_end = track.next_clip_start_after(start_tick).unwrap_or(i32::MAX);
        let clamped_end = target_end_tick.clamp(start_tick + min_length, max_end);
        let new_length = clamped_end - start_tick;

        let mut bounds = clip.bounds();
        bounds.region_end = bounds.region_start + new_length;
        (bounds != clip.bounds()).then_some(bounds)
    }

    /// Where the left edge of `clip_id` (on the selected track) would go for
    /// an arrangement-time trim to `target_start_tick` (the mouse edge drag, `[` in the arranger).
    /// Keeps `end_tick` fixed by moving `start_tick` and `region().start()`
    /// together by the same delta, so the loop's playback phase doesn't jump.
    /// Clamped between the previous clip's end (or 0) and however far
    /// `region().start()` can retreat before hitting 0 — a clip has no more
    /// original material to reveal past that point — and never past
    /// `end - minimum clip length` (a beat — `time::min_clip_length_ticks`).
    /// `None` on a no-op.
    pub(crate) fn clip_start_trimmed_to(
        &self,
        clip_id: Uuid,
        target_start_tick: i32,
    ) -> Option<ClipBounds> {
        let min_length = time::min_clip_length_ticks();
        let track = self.selected_track()?;
        let clip = track.get_clip_by_id(clip_id)?;
        let cur_start_tick = clip.start_tick();
        let end_tick = clip.end_tick();
        let region_start = clip.region().start();

        let min_start_from_prev_clip = track.prev_clip_end_before(cur_start_tick).unwrap_or(0);
        let min_start_from_region = cur_start_tick - region_start;
        let min_start = min_start_from_prev_clip.max(min_start_from_region);
        let max_start = end_tick - min_length;

        if min_start >= max_start {
            return None;
        }

        let clamped_start = target_start_tick.clamp(min_start, max_start);
        let delta = clamped_start - cur_start_tick;
        if delta == 0 {
            return None;
        }

        let mut bounds = clip.bounds();
        bounds.start_tick = clamped_start;
        bounds.region_start = region_start + delta;
        Some(bounds)
    }

    /// `[` in the clip view: the window starts at `event_tick` and its end
    /// stays put — a start marker, not a trim. The clip keeps its arrangement
    /// start, so the new 1.1.1 lands where the old one was, and its length
    /// becomes end − start (its arrangement end moves); the end only ever
    /// changes deliberately, with `]` (decided with the user 2026-09-26 — it
    /// used to keep the length and slide the end along). Clamped to at least
    /// the minimum clip length before the end and to the room before the next
    /// clip. `None` on a no-op.
    pub(crate) fn selected_clip_start_marker_at(&self, event_tick: i32) -> Option<ClipBounds> {
        let clip = self.selected_clip()?;
        let region_end = clip.region().end();
        let latest = region_end - time::min_clip_length_ticks();
        let earliest = self
            .selected_track()?
            .next_clip_start_after(clip.start_tick())
            .map_or(0, |next_start| {
                region_end - (next_start - clip.start_tick())
            })
            .max(0);
        if earliest > latest {
            return None;
        }

        let mut bounds = clip.bounds();
        bounds.region_start = event_tick.clamp(earliest, latest);
        (bounds != clip.bounds()).then_some(bounds)
    }

    /// `length` fitted into the room a clip starting at `start_tick` on the
    /// selected track has before the next clip: unchanged when it fits,
    /// otherwise floored to the whole bars that fit there, or the bare gap
    /// when not even one bar does. Shared by the stopped capture commit and
    /// the clip view's `]`.
    pub(crate) fn fit_length_before_next_clip(&self, start_tick: i32, length: i32) -> i32 {
        let bar = time::bars_to_ticks(1);
        let Some(available) = self
            .selected_track()
            .and_then(|track| track.next_clip_start_after(start_tick))
            .map(|next_start| next_start - start_tick)
        else {
            return length;
        };
        if length <= available {
            length
        } else if available >= bar {
            available / bar * bar
        } else {
            available
        }
    }

    /// `]` in the clip view: the window ends on the bar line at or after
    /// `event_tick`, measured from the window start — the bar the cursor is in
    /// becomes the last one, so everything left of the cursor stays in (at
    /// least one bar; rounded *up* since 2026-09-26, it was nearest, which
    /// often left the end where it was). Floored to the bars that fit before
    /// the next clip, or the bare gap when not even one bar does. Whole bars
    /// keep a clip looped against the metronome in time (`150`). With
    /// `whole_bars` off (the project's only clip, whose length is what Enter
    /// fits the tempo to) it ends exactly at `event_tick`, at least the
    /// minimum clip length. `None` on a no-op.
    pub(crate) fn selected_clip_end_marker_at(
        &self,
        event_tick: i32,
        whole_bars: bool,
    ) -> Option<ClipBounds> {
        let bar = time::bars_to_ticks(1);
        let clip = self.selected_clip()?;
        let region_start = clip.region().start();

        let raw = event_tick - region_start;
        let length = if whole_bars {
            time::next_bar_boundary_after(raw - 1).max(bar)
        } else {
            raw.max(time::min_clip_length_ticks())
        };
        let length = self.fit_length_before_next_clip(clip.start_tick(), length);

        let mut bounds = clip.bounds();
        bounds.region_end = region_start + length;
        (bounds != clip.bounds()).then_some(bounds)
    }

    /// Maps the arranger cursor into the selected clip's event-tick space and
    /// parks the clip cursor there — done on entering the clip view.
    pub(crate) fn sync_selected_clip_cursor_with_arranger(&mut self) {
        let absolute_tick = self.cursor_tick();
        if let Some(clip) = self.selected_clip_mut() {
            clip.sync_clip_cursor_with_absolute_tick(absolute_tick);
        }
    }

    /// Moves the selected clip's cursor to `target_tick` (clamped by the clip).
    /// Returns the signed distance actually moved.
    pub(crate) fn set_selected_clip_cursor_tick(&mut self, target_tick: i32) -> i32 {
        if let Some(clip) = self.selected_clip_mut() {
            let before = clip.cursor_tick();
            let delta = target_tick - before;
            clip.nudge_cursor_by_ticks(delta);
            let after = clip.cursor_tick();
            after - before
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_cursor_grid_step_follows_the_given_grid() {
        let sixteenth = time::sixteenth_straight_ticks();
        let eighth = 2 * sixteenth;
        // On a 16th grid from a 16th that isn't an 8th...
        assert_eq!(
            Sequencer::clip_cursor_grid_step_target(sixteenth, sixteenth),
            eighth
        );
        // ...an 8th grid (zoomed out) lands on the next 8th, not a 16th away...
        assert_eq!(
            Sequencer::clip_cursor_grid_step_target(sixteenth, eighth),
            eighth
        );
        assert_eq!(
            Sequencer::clip_cursor_grid_step_target(3 * sixteenth, -eighth),
            eighth
        );
        // ...and from a boundary, back one full step.
        assert_eq!(Sequencer::clip_cursor_grid_step_target(eighth, -eighth), 0);
    }

    #[test]
    fn clip_cursor_grid_step_survives_a_zero_step() {
        assert_eq!(Sequencer::clip_cursor_grid_step_target(100, 0), 99);
    }
}
