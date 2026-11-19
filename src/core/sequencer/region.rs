//! Selected-clip region and cursor operations on the `Sequencer`.
//!
//! This is the sequencer-side of what `models/clip/cursor_region.rs` does for
//! one clip: resolve "the selected clip", apply the edit, keep the transport in
//! step: the clip edge targets and trims, and the clip cursor.

use uuid::Uuid;

use crate::{
    core::time,
    models::{
        clip::{Clip, ClipBounds, ClipEdge, min_window_length, window_with_edge_at},
        track::Track,
    },
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

/// The ticks `clip`'s arrangement `edge` can be dragged between on `track`:
/// the end from a minimum clip length (`time::min_clip_length_ticks`) after
/// the start to the next clip's start; the start from the previous clip's end
/// (or however far the window can retreat before the events run out) to a
/// minimum length before the end. `None` when the limits cross.
fn edge_limits(track: &Track, clip: &Clip, edge: ClipEdge) -> Option<(i32, i32)> {
    let min_length = time::min_clip_length_ticks();
    let start_tick = clip.start_tick();
    let (lowest, highest) = match edge {
        ClipEdge::End => (
            start_tick + min_length,
            track.next_clip_start_after(start_tick).unwrap_or(i32::MAX),
        ),
        ClipEdge::Start => {
            let from_prev_clip = track.prev_clip_end_before(start_tick).unwrap_or(0);
            let from_region = start_tick - clip.region().start();
            (
                from_prev_clip.max(from_region),
                clip.end_tick() - min_length,
            )
        }
    };
    (lowest <= highest).then_some((lowest, highest))
}

/// `clip`'s bounds with its arrangement `edge` moved to `tick`: the end
/// changes the window's length, the start moves `start_tick` and the window
/// start by the same delta so the content stays put in time.
fn bounds_with_edge_at(clip: &Clip, edge: ClipEdge, tick: i32) -> ClipBounds {
    let mut bounds = clip.bounds();
    match edge {
        ClipEdge::End => bounds.region_end = bounds.region_start + (tick - clip.start_tick()),
        ClipEdge::Start => {
            bounds.region_start += tick - clip.start_tick();
            bounds.start_tick = tick;
        }
    }
    bounds
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
    /// marker on the lead clip at its cursor. `None` when there is nothing to
    /// act on or nothing would change.
    pub(crate) fn clip_edge_target(
        &self,
        edge: ClipEdge,
        in_clip_view: bool,
    ) -> Option<EdgeTarget> {
        if in_clip_view {
            return self.selected_clip_marker_at(edge, self.selected_clip()?.cursor_tick());
        }
        let clip_id = self.arranger_edge_clip_id(edge)?;
        let tick = self.cursor_tick();
        let bounds = match edge {
            ClipEdge::Start => self.clip_start_trimmed_to(clip_id, tick),
            ClipEdge::End => self.clip_end_trimmed_to(clip_id, tick),
        }?;
        Some(EdgeTarget { clip_id, bounds })
    }

    /// A start or end marker on the lead clip — `[`/`]` in the clip view at
    /// the clip cursor, and the window band's handles
    /// (`archive/290-framing-a-take.md`): `edge` of its window to `event_tick`, held
    /// inside the clip's [`reach`](Clip::reach) (the cursor already is), by
    /// the clip view's marker rule (`window_with_edge_at`) against the room
    /// before the next clip on the selected track, and kept
    /// [`min_window_length`] apart (a tick for a take, a beat otherwise).
    ///
    /// `[` moves only the start; the end stays — a marker, not a trim. The
    /// clip keeps its arrangement start, so the new 1.1.1 lands where the
    /// old one was and its length follows (decided with the user 2026-09-26;
    /// it used to keep the length and slide the end along). `]` ends the
    /// window exactly at the tick with any number of clips (2026-10-12; it
    /// used to round up to whole bars with more than one clip, for a
    /// metronome drift `150` has since fixed). `None` when there is no lead
    /// clip or nothing would change.
    pub(crate) fn selected_clip_marker_at(
        &self,
        edge: ClipEdge,
        event_tick: i32,
    ) -> Option<EdgeTarget> {
        let clip = self.selected_clip()?;
        let (reach_start, reach_end) = clip.reach(self.meter());
        let tick = event_tick.clamp(reach_start, reach_end);
        let room = self.room_before_next_clip(clip.start_tick());
        let window = (clip.region().start(), clip.region().end());
        let min_length = min_window_length(self.is_framing_take());
        let (region_start, region_end) = window_with_edge_at(edge, tick, window, room, min_length)?;

        let mut bounds = clip.bounds();
        bounds.region_start = region_start;
        bounds.region_end = region_end;
        (bounds != clip.bounds()).then_some(EdgeTarget {
            clip_id: clip.id(),
            bounds,
        })
    }

    /// The room a clip starting at `start_tick` on the selected track has
    /// before the next clip there; `None` with nothing after it, and in a
    /// take session, whose take isn't on the track (`take_framing.rs`).
    fn room_before_next_clip(&self, start_tick: i32) -> Option<i32> {
        if self.is_framing_take() {
            return None;
        }
        self.selected_track()?
            .next_clip_start_after(start_tick)
            .map(|next_start| next_start - start_tick)
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
        let track = self.selected_track()?;
        let clip = track.get_clip_by_id(clip_id)?;
        let (lowest, highest) = edge_limits(track, clip, ClipEdge::End)?;
        let bounds =
            bounds_with_edge_at(clip, ClipEdge::End, target_end_tick.clamp(lowest, highest));
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
        let track = self.selected_track()?;
        let clip = track.get_clip_by_id(clip_id)?;
        let (lowest, highest) = edge_limits(track, clip, ClipEdge::Start)?;
        let bounds = bounds_with_edge_at(
            clip,
            ClipEdge::Start,
            target_start_tick.clamp(lowest, highest),
        );
        (bounds != clip.bounds()).then_some(bounds)
    }

    /// Where each clip of `tied` (`(track, clip)` pairs, on any track) goes
    /// when their `edge` is dragged to `target_tick` together (the mouse edge
    /// drag over a marquee, `020-views-and-state.md` § Clip Edge Drag-Resize):
    /// every clip moves its edge by the same delta, held to the tightest of
    /// their limits (each clip's own, as `clip_end_trimmed_to` and
    /// `clip_start_trimmed_to` have them) so the edges stay level. Only the
    /// clips that change are listed; a tied clip that is gone is skipped, one
    /// that cannot move at all holds the whole group still.
    pub(crate) fn tied_edges_trimmed_to(
        &self,
        tied: &[(usize, Uuid)],
        edge: ClipEdge,
        target_tick: i32,
    ) -> Vec<(usize, Uuid, ClipBounds)> {
        let clips: Vec<(usize, &Clip)> = tied
            .iter()
            .filter_map(|&(track_idx, clip_id)| {
                Some((track_idx, self.clip_on(track_idx, clip_id)?))
            })
            .collect();
        let mut lowest = i32::MIN;
        let mut highest = i32::MAX;
        for &(track_idx, clip) in &clips {
            let Some(track) = self.tracks().get(track_idx) else {
                return Vec::new();
            };
            let Some((low, high)) = edge_limits(track, clip, edge) else {
                return Vec::new();
            };
            lowest = lowest.max(low);
            highest = highest.min(high);
        }
        if lowest > highest {
            return Vec::new();
        }
        let tick = target_tick.clamp(lowest, highest);
        clips
            .into_iter()
            .filter_map(|(track_idx, clip)| {
                let bounds = bounds_with_edge_at(clip, edge, tick);
                (bounds != clip.bounds()).then_some((track_idx, clip.id(), bounds))
            })
            .collect()
    }

    /// `length` fitted into the room a clip starting at `start_tick` on the
    /// selected track has before the next clip: unchanged when it fits,
    /// otherwise floored to the whole bars that fit there, or the bare gap
    /// when not even one bar does. For the stopped capture commit.
    pub(crate) fn fit_length_before_next_clip(&self, start_tick: i32, length: i32) -> i32 {
        let bar = self.meter().bar_ticks();
        let Some(available) = self.room_before_next_clip(start_tick) else {
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
        let meter = self.meter();
        if let Some(clip) = self.selected_clip_mut() {
            let before = clip.cursor_tick();
            let delta = target_tick - before;
            clip.nudge_cursor_by_ticks(delta, meter);
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
