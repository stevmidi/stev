//! Undoable clip-edge edit — every change to where one clip sits and which
//! part of its events plays: the arranger edge drag and `[`/`]` (arrangement
//! trims) and the clip view's `[`/`]` (start/end markers). Only
//! [`ClipBounds`] change; events never do, so material outside the window
//! (`220-capture-without-pending-view.md`) is revealed or hidden, never lost.

use uuid::Uuid;

use crate::models::clip::ClipBounds;

use super::super::super::Sequencer;
use super::super::EditResult;

// ---------------------------------------------------------------------------
// ResizeClip
// ---------------------------------------------------------------------------

/// Moves one clip from `before` to `after` bounds. The caller computes
/// `after` (the `Sequencer::selected_clip_*` helpers in `region/mod.rs` own
/// the clamping against neighbours); this edit only applies and reverses it.
///
/// A mouse edge drag sends one edit per pointer move, all carrying the drag's
/// id, and they merge into one undo step: the merged edit keeps the first
/// `before` and takes each later `after` (`SequencerEdit::merge`). Keyboard
/// edits carry no id and never merge.
pub(crate) struct ResizeClipEdit {
    /// Track the clip is on.
    track_idx: usize,
    /// The clip — neither added nor removed here, so the id stays valid.
    clip_id: Uuid,
    /// Bounds before the (first) edit, restored by `undo()`.
    before: ClipBounds,
    /// Bounds after the (latest merged) edit, applied by `edit()`.
    after: ClipBounds,
    /// The mouse drag this step belongs to; `None` for a keyboard edit.
    drag_id: Option<u64>,
}

impl ResizeClipEdit {
    /// Moves `clip_id` on the selected track to `after`. `None` if there is
    /// no such clip or `after` is where it already is.
    pub(crate) fn for_clip(
        sequencer: &Sequencer,
        clip_id: Uuid,
        after: ClipBounds,
        drag_id: Option<u64>,
    ) -> Option<Self> {
        let before = sequencer
            .selected_track()?
            .get_clip_by_id(clip_id)?
            .bounds();
        if before == after {
            return None;
        }

        Some(Self {
            track_idx: sequencer.selected_track_index()?,
            clip_id,
            before,
            after,
            drag_id,
        })
    }

    /// Two steps merge iff they come from the same mouse drag on the same
    /// clip.
    pub(in crate::core::sequencer::edit) fn same_gesture(&self, other: &Self) -> bool {
        self.drag_id.is_some()
            && self.drag_id == other.drag_id
            && self.track_idx == other.track_idx
            && self.clip_id == other.clip_id
    }

    /// Folds a later step of the same drag in: keeps this `before`, takes its
    /// `after`.
    pub(in crate::core::sequencer::edit) fn absorb(&mut self, other: &Self) {
        self.after = other.after;
    }

    /// Applies `after`. Returns [`EditResult::ClipResized`], or `NoOp` if the
    /// clip is gone.
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.apply(sequencer, self.after)
    }

    /// Restores `before`. Same result shape as `edit()`.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.apply(sequencer, self.before)
    }

    /// Moves the clip to `bounds`. While playing, the clip is heard in its
    /// new window from the playhead at once (`Sequencer::set_clip_bounds`).
    fn apply(&self, sequencer: &mut Sequencer, bounds: ClipBounds) -> EditResult {
        match sequencer.set_clip_bounds(self.track_idx, self.clip_id, bounds) {
            Some(clip) => EditResult::ClipResized { clip, retime: None },
            None => EditResult::NoOp,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::sequencer::SequencerEdit;

    use crate::core::time::{self, Meter};
    use crate::models::clip::{Clip, ClipEdge};
    use crate::models::event::Event;

    use crate::core::sequencer::test_support::sequencer_with;

    use super::*;

    fn test_sequencer() -> Sequencer {
        let mut sequencer = sequencer_with(false).0;
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer
    }

    /// A selected 2-bar clip at bar 4 whose window starts a bar into its
    /// events (a bar of material hidden before it), plus a clip at bar 10.
    fn sequencer_with_clip() -> Sequencer {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();

        let mut clip = Clip::new();
        clip.set_start_tick(bar * 4);
        clip.region_mut().set_region(Some(bar), Some(bar * 3));
        clip.add_event(Event::new(100, 0, vec![0x90, 50, 100]));
        clip.add_event(Event::new(400, 0, vec![0x80, 50, 0]));
        clip.add_event(Event::new(bar + 100, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(bar + 400, 0, vec![0x80, 60, 0]));
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);

        let mut neighbour = Clip::new();
        neighbour.set_start_tick(bar * 10);
        neighbour.region_mut().set_region(Some(0), Some(bar));
        sequencer.tracks_mut()[0].add_clip(&neighbour);

        sequencer.select_clip(Some(clip_id));
        sequencer
    }

    fn clip_id(sequencer: &Sequencer) -> Uuid {
        sequencer.selected_clip_id().unwrap()
    }

    fn bounds(sequencer: &Sequencer) -> ClipBounds {
        sequencer.selected_clip().unwrap().bounds()
    }

    /// `[` in the clip view at `event_tick`: the lead clip's bounds after.
    fn start_marker(sequencer: &Sequencer, event_tick: i32) -> Option<ClipBounds> {
        sequencer
            .selected_clip_marker_at(ClipEdge::Start, event_tick)
            .map(|target| target.bounds)
    }

    fn record_resize(
        sequencer: &mut Sequencer,
        record: &mut Record<SequencerEdit>,
        after: Option<ClipBounds>,
        drag_id: Option<u64>,
    ) {
        let edit = ResizeClipEdit::for_clip(sequencer, clip_id(sequencer), after.unwrap(), drag_id)
            .unwrap();
        record.edit(sequencer, SequencerEdit::ResizeClip(edit));
    }

    /// Pulling the left edge back in the arranger reveals the hidden bar: the
    /// clip grows left, its content stays put in time, and undo restores it.
    #[test]
    fn arranger_start_trim_reveals_hidden_material_and_undoes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = sequencer_with_clip();
        let original = bounds(&sequencer);
        let mut record = Record::new();

        let after = sequencer.clip_start_trimmed_to(clip_id(&sequencer), bar * 3);
        record_resize(&mut sequencer, &mut record, after, None);

        assert_eq!(
            bounds(&sequencer),
            ClipBounds {
                start_tick: bar * 3,
                region_start: 0,
                region_end: bar * 3,
            }
        );

        record.undo(&mut sequencer);
        assert_eq!(bounds(&sequencer), original);
        record.redo(&mut sequencer);
        assert_eq!(bounds(&sequencer).region_start, 0);
    }

    /// The clip view's `[` moves only the start: the end stays, the clip
    /// keeps its arrangement start, and the length follows.
    #[test]
    fn start_marker_moves_only_the_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let sequencer = sequencer_with_clip(); // window [bar, 3 bars) at bar 4

        let after = start_marker(&sequencer, bar / 2).unwrap();
        assert_eq!(
            after,
            ClipBounds {
                start_tick: bar * 4,
                region_start: bar / 2,
                region_end: bar * 3,
            }
        );

        // Never past the end: at least the minimum length is left.
        let after = start_marker(&sequencer, bar * 5).unwrap();
        assert_eq!(
            after.region_end - after.region_start,
            time::min_clip_length_ticks()
        );
    }

    /// `]` ends the window exactly at the tick, with any number of clips
    /// (the fixture has two), at least the minimum length after the start
    /// and no further than the next clip — given a reach past it, here a
    /// note kept 8 bars on.
    #[test]
    fn end_marker_is_exact_and_stops_at_the_next_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = sequencer_with_clip();
        let window_start = sequencer.selected_clip().unwrap().region().start();
        let clip = sequencer.selected_clip_mut().unwrap();
        clip.add_event(Event::new(window_start + bar * 8, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(
            window_start + bar * 8 + 100,
            0,
            vec![0x80, 60, 0],
        ));
        clip.sort_events_by_tick();
        let sequencer = sequencer;
        let length_at = |from_start: i32| {
            sequencer
                .selected_clip_marker_at(ClipEdge::End, window_start + from_start)
                .map(|after| after.bounds.region_end - after.bounds.region_start)
        };

        assert_eq!(length_at(bar * 6 / 10), Some(bar * 6 / 10));
        assert_eq!(length_at(bar * 24 / 10), Some(bar * 24 / 10));
        assert_eq!(length_at(1), Some(time::min_clip_length_ticks()));

        // 9 bars asked for, 6 bars of room before the clip at bar 10.
        assert_eq!(length_at(bar * 9), Some(bar * 6));
    }

    /// In the arranger `[`/`]` act on the clip under the cursor, else the
    /// nearest one on the side the edge faces — so they can grow a clip.
    #[test]
    fn arranger_edge_clip_is_under_the_cursor_or_the_nearest_on_that_side() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let sequencer = sequencer_with_clip(); // clips at bars 4–6 and 10–11
        let first = clip_id(&sequencer);
        let at = |tick: i32, edge: ClipEdge| {
            sequencer.cursor_tick.store(tick, Ordering::Relaxed);
            sequencer.arranger_edge_clip_id(edge)
        };

        assert_eq!(
            at(bar * 5, ClipEdge::Start),
            Some(first),
            "under the cursor"
        );
        assert_eq!(at(bar * 2, ClipEdge::Start), Some(first), "next clip after");
        assert_eq!(at(bar * 2, ClipEdge::End), None, "nothing ends before");
        assert_eq!(at(bar * 8, ClipEdge::End), Some(first), "last clip before");
        assert_ne!(
            at(bar * 8, ClipEdge::Start),
            Some(first),
            "the one at bar 10"
        );
    }

    /// One mouse drag is one undo step, however many moves it sent.
    #[test]
    fn one_drag_merges_into_one_undo_step() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = sequencer_with_clip();
        let original = bounds(&sequencer);
        let mut record = Record::new();

        for end in [bar * 7, bar * 8, bar * 9] {
            let after = sequencer.clip_end_trimmed_to(clip_id(&sequencer), end);
            record_resize(&mut sequencer, &mut record, after, Some(7));
        }

        assert_eq!(record.len(), 1, "one drag, one step");
        assert_eq!(bounds(&sequencer).region_end, bar * 6);

        record.undo(&mut sequencer);
        assert_eq!(bounds(&sequencer), original);
    }

    /// Keyboard edits and separate drags stay separate steps.
    #[test]
    fn keyboard_edits_and_separate_drags_do_not_merge() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = sequencer_with_clip();
        let mut record = Record::new();

        let after = sequencer.clip_end_trimmed_to(clip_id(&sequencer), bar * 7);
        record_resize(&mut sequencer, &mut record, after, None);
        let after = sequencer.clip_end_trimmed_to(clip_id(&sequencer), bar * 8);
        record_resize(&mut sequencer, &mut record, after, None);
        let after = sequencer.clip_end_trimmed_to(clip_id(&sequencer), bar * 9);
        record_resize(&mut sequencer, &mut record, after, Some(1));
        let after = sequencer.clip_end_trimmed_to(clip_id(&sequencer), bar * 10);
        record_resize(&mut sequencer, &mut record, after, Some(2));

        assert_eq!(record.len(), 4);
    }
}
