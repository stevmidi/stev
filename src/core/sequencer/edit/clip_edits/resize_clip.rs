//! Undoable clip-edge edit — every change to where one clip sits and which
//! part of its events plays: the arranger edge drag and `[`/`]` (arrangement
//! trims) and the clip view's `[`/`]` (start/end markers). Only
//! [`ClipBounds`] change; events never do, so material outside the window
//! (`220-capture-without-pending-view.md`) is revealed or hidden, never lost.

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::models::clip::{ClipBounds, ClipEdge};

use super::super::super::Sequencer;
use super::super::EditResult;

// ---------------------------------------------------------------------------
// ResizeClip
// ---------------------------------------------------------------------------

/// One clip's share of a [`ResizeClipEdit`].
struct ClipResize {
    /// Track the clip is on.
    track_idx: usize,
    /// The clip — neither added nor removed here, so the id stays valid.
    clip_id: Uuid,
    /// Bounds before the (first) edit, restored by `undo()`.
    before: ClipBounds,
    /// Bounds after the (latest merged) edit, applied by `edit()`.
    after: ClipBounds,
}

/// Moves one clip, or several clips together, from their `before` to their
/// `after` bounds. The caller computes `after` (the `Sequencer::selected_clip_*`
/// helpers and `tied_edges_trimmed_to` in `sequencer/region.rs` own the
/// clamping against neighbours); this edit only applies and reverses it.
///
/// A mouse edge drag sends one edit per pointer move, all carrying the drag's
/// id, and they merge into one undo step: the merged edit keeps the first
/// `before` and takes each later `after` (`SequencerEdit::merge`). Keyboard
/// edits carry no id and never merge. A drag over a marquee moves every clip
/// whose edge is level with the grabbed one as one edit (`for_tied_edges`).
pub(crate) struct ResizeClipEdit {
    /// The clips moved, in a fixed order the merge relies on.
    clips: Vec<ClipResize>,
    /// The mouse drag this step belongs to; `None` for a keyboard edit.
    drag_id: Option<u64>,
    /// Whether this is a drag over a marquee (`for_tied_edges`), reported as
    /// [`EditResult::ClipsResized`] even for one clip.
    grouped: bool,
    /// The marquee after the (latest merged) edit when its edge was on the
    /// dragged edge and follows it; `None` leaves the marquee alone. Undo
    /// restores the marquee through the step's view snapshot, not from here.
    marquee: Option<TimeSelectionRect>,
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
            clips: vec![ClipResize {
                track_idx: sequencer.selected_track_index()?,
                clip_id,
                before,
                after,
            }],
            drag_id,
            grouped: false,
            marquee: None,
        })
    }

    /// Drags the `edge` of every `tied` clip (`(track, clip)` pairs) to
    /// `target_tick` together, as one edit. `None` if none of them moves.
    pub(crate) fn for_tied_edges(
        sequencer: &Sequencer,
        tied: &[(usize, Uuid)],
        edge: ClipEdge,
        target_tick: i32,
        drag_id: u64,
    ) -> Option<Self> {
        let clips: Vec<ClipResize> = sequencer
            .tied_edges_trimmed_to(tied, edge, target_tick)
            .into_iter()
            .filter_map(|(track_idx, clip_id, after)| {
                let before = sequencer.clip_on(track_idx, clip_id)?.bounds();
                Some(ClipResize {
                    track_idx,
                    clip_id,
                    before,
                    after,
                })
            })
            .collect();
        let marquee = clips
            .first()
            .and_then(|lead| followed_marquee(sequencer.marquee()?, edge, lead));
        (!clips.is_empty()).then_some(Self {
            clips,
            drag_id: Some(drag_id),
            grouped: true,
            marquee,
        })
    }

    /// Two steps merge iff they come from the same mouse drag on the same
    /// clips.
    pub(in crate::core::sequencer::edit) fn same_gesture(&self, other: &Self) -> bool {
        self.drag_id.is_some()
            && self.drag_id == other.drag_id
            && self.clips.len() == other.clips.len()
            && self
                .clips
                .iter()
                .zip(&other.clips)
                .all(|(a, b)| a.track_idx == b.track_idx && a.clip_id == b.clip_id)
    }

    /// Folds a later step of the same drag in: keeps this `before`, takes its
    /// `after`.
    pub(in crate::core::sequencer::edit) fn absorb(&mut self, other: &Self) {
        for (clip, later) in self.clips.iter_mut().zip(&other.clips) {
            clip.after = later.after;
        }
        self.marquee = other.marquee;
    }

    /// Applies `after`. Returns [`EditResult::ClipResized`] (one clip, not
    /// over a marquee) or [`EditResult::ClipsResized`], or `NoOp` if the
    /// clips are gone.
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.apply(sequencer, |clip| clip.after, self.marquee)
    }

    /// Restores `before`. Same result shape as `edit()`.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.apply(sequencer, |clip| clip.before, None)
    }

    /// Moves each clip to the bounds `pick` chooses. While playing, a clip is
    /// heard in its new window from the playhead at once
    /// (`Sequencer::set_clip_bounds`).
    fn apply(
        &self,
        sequencer: &mut Sequencer,
        pick: impl Fn(&ClipResize) -> ClipBounds,
        marquee: Option<TimeSelectionRect>,
    ) -> EditResult {
        let mut resized: Vec<_> = self
            .clips
            .iter()
            .filter_map(|clip| sequencer.set_clip_bounds(clip.track_idx, clip.clip_id, pick(clip)))
            .collect();
        match resized.len() {
            0 => EditResult::NoOp,
            1 if !self.grouped => EditResult::ClipResized {
                clip: resized.remove(0),
                retime: None,
            },
            _ => EditResult::ClipsResized {
                clips: resized,
                marquee,
            },
        }
    }
}

/// `marquee` with the boundary on `lead`'s dragged `edge` moved to where the
/// edge went, when that boundary was exactly on it — a marquee drawn over
/// the clips follows their edge, one that reaches past it stays. `None` when
/// it stays, or when following would leave it without a tick range.
fn followed_marquee(
    marquee: TimeSelectionRect,
    edge: ClipEdge,
    lead: &ClipResize,
) -> Option<TimeSelectionRect> {
    let edge_tick = |bounds: ClipBounds| match edge {
        ClipEdge::Start => bounds.start_tick,
        ClipEdge::End => bounds.start_tick + bounds.region_end - bounds.region_start,
    };
    let (from, to) = (edge_tick(lead.before), edge_tick(lead.after));
    let mut followed = marquee;
    match edge {
        ClipEdge::Start if marquee.start == from => followed.start = to,
        ClipEdge::End if marquee.end == from => followed.end = to,
        _ => return None,
    }
    (marquee.has_tick_range() && followed.has_tick_range()).then_some(followed)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::sequencer::SequencerEdit;

    use crate::core::time::{self, Meter};
    use crate::models::clip::{Clip, ClipEdge};
    use crate::models::event::Event;

    use crate::core::sequencer::test_support::{clip_at, sequencer_with};

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

    /// Three clips ending at bar 2 on tracks 0-2, a neighbour at bar 3 on
    /// track 2 only: the ids of the three, tied.
    fn tied_ends(sequencer: &mut Sequencer) -> Vec<(usize, Uuid)> {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut tied = Vec::new();
        for track_idx in 0..3 {
            let clip = clip_at(0, bar * 2);
            tied.push((track_idx, clip.id()));
            sequencer.tracks_mut()[track_idx].add_clip(&clip);
        }
        sequencer.tracks_mut()[2].add_clip(&clip_at(bar * 3, bar));
        tied
    }

    fn end_of(sequencer: &Sequencer, (track_idx, id): (usize, Uuid)) -> i32 {
        sequencer.clip_on(track_idx, id).unwrap().end_tick()
    }

    /// A drag over a marquee moves every tied end by the same delta, held to
    /// the tightest clip's limit (track 2's neighbour), as one undo step
    /// however many pointer moves it takes.
    #[test]
    fn tied_ends_move_together_as_one_undo_step() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();
        let tied = tied_ends(&mut sequencer);
        let mut record = Record::new();

        for target in [bar * 2 + 100, bar * 5] {
            let edit = ResizeClipEdit::for_tied_edges(&sequencer, &tied, ClipEdge::End, target, 7)
                .unwrap();
            record.edit(&mut sequencer, SequencerEdit::ResizeClip(edit));
        }

        for &clip in &tied {
            assert_eq!(
                end_of(&sequencer, clip),
                bar * 3,
                "stopped at the neighbour"
            );
        }
        assert_eq!(record.len(), 1);

        record.undo(&mut sequencer);
        for &clip in &tied {
            assert_eq!(end_of(&sequencer, clip), bar * 2);
        }
        record.redo(&mut sequencer);
        for &clip in &tied {
            assert_eq!(end_of(&sequencer, clip), bar * 3);
        }
    }

    /// Dragging the starts back reveals each clip's hidden material by the
    /// same delta, held to the clip with the least room.
    #[test]
    fn tied_starts_move_by_the_same_delta() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();
        let mut tied = Vec::new();
        for (track_idx, hidden) in [(0, bar), (1, bar / 2)] {
            let mut clip = clip_at(bar * 4, bar);
            clip.region_mut()
                .set_region(Some(hidden), Some(hidden + bar));
            tied.push((track_idx, clip.id()));
            sequencer.tracks_mut()[track_idx].add_clip(&clip);
        }

        let edit =
            ResizeClipEdit::for_tied_edges(&sequencer, &tied, ClipEdge::Start, 0, 1).unwrap();
        let mut record = Record::new();
        record.edit(&mut sequencer, SequencerEdit::ResizeClip(edit));

        for &(track_idx, id) in &tied {
            let clip = sequencer.clip_on(track_idx, id).unwrap();
            assert_eq!(clip.start_tick(), bar * 4 - bar / 2, "the least room wins");
            assert_eq!(clip.end_tick(), bar * 5, "the ends stay");
        }
    }

    /// A tied clip that is gone is skipped; with nothing left to move there is
    /// no edit.
    #[test]
    fn tied_edges_with_nothing_to_move_make_no_edit() {
        let mut sequencer = test_sequencer();
        let tied = tied_ends(&mut sequencer);
        let at_end = end_of(&sequencer, tied[0]);
        assert!(
            ResizeClipEdit::for_tied_edges(&sequencer, &tied, ClipEdge::End, at_end, 1).is_none()
        );
        assert!(
            ResizeClipEdit::for_tied_edges(
                &sequencer,
                &[(0, Uuid::new_v4())],
                ClipEdge::End,
                at_end + 480,
                1
            )
            .is_none()
        );
    }

    fn marquee_over(tracks: (usize, usize), ticks: (i32, i32)) -> TimeSelectionRect {
        TimeSelectionRect {
            start: ticks.0,
            end: ticks.1,
            track_start: tracks.0,
            track_end: tracks.1,
        }
    }

    /// The result of the first step of a tied end drag to `target`.
    fn first_step(sequencer: &mut Sequencer, tied: &[(usize, Uuid)], target: i32) -> EditResult {
        let mut edit =
            ResizeClipEdit::for_tied_edges(sequencer, tied, ClipEdge::End, target, 1).unwrap();
        edit.edit(sequencer)
    }

    /// A marquee whose end is on the dragged end follows it, so it still
    /// holds the clips afterwards; undo leaves putting it back to the step's
    /// view snapshot.
    #[test]
    fn a_marquee_on_the_dragged_end_follows_it() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();
        let tied = tied_ends(&mut sequencer);
        sequencer.set_marquee(Some(marquee_over((0, 2), (0, bar * 2))));

        let mut edit =
            ResizeClipEdit::for_tied_edges(&sequencer, &tied, ClipEdge::End, bar * 3, 1).unwrap();
        let EditResult::ClipsResized { marquee, .. } = edit.edit(&mut sequencer) else {
            panic!("expected ClipsResized");
        };
        assert_eq!(marquee, Some(marquee_over((0, 2), (0, bar * 3))));

        let EditResult::ClipsResized { marquee, .. } = edit.undo(&mut sequencer) else {
            panic!("expected ClipsResized");
        };
        assert_eq!(marquee, None);
    }

    /// A marquee reaching past the dragged end stays as it is.
    #[test]
    fn a_marquee_past_the_dragged_end_stays() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();
        let tied = tied_ends(&mut sequencer);
        sequencer.set_marquee(Some(marquee_over((0, 2), (0, bar * 4))));

        let EditResult::ClipsResized { marquee, .. } = first_step(&mut sequencer, &tied, bar * 3)
        else {
            panic!("expected ClipsResized");
        };
        assert_eq!(marquee, None);
    }
}
