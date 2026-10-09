//! Undoable "a new clip appeared on a track" — the capture commit (`/`,
//! running or stopped), a completed live take and an inserted empty clip
//! (`⇧⌘M`). Undo lifts
//! the clip off its track by id; redo puts it back as it was when undone.

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::{detach, detached_clone};

// ---------------------------------------------------------------------------
// CommitClip
// ---------------------------------------------------------------------------

/// Puts one frozen clip on one track. Every path that creates a brand-new clip
/// reduces to this shape, so one edit backs them all (see
/// `050-undo-redo.md`): [`from_empty_clip`](Self::from_empty_clip) builds an
/// empty one (`⇧⌘M`); the capture commits build the clip here via
/// [`Sequencer::build_committed_capture_clip`] (running) or
/// [`Sequencer::build_stopped_capture_clip`] (stopped) and let `edit()` place
/// it; a live take has *already* placed its clip by the time the record
/// hears about it, so it snapshots it with
/// [`from_placed_clip`](Self::from_placed_clip) and the first `edit()` finds it
/// present and leaves it alone.
///
/// `edit()` is therefore idempotent — "make sure the frozen clip is on its
/// track" — which is what lets the fresh commit, the already-placed first call
/// and every redo share one body with no "applied" flag. Only the capture
/// buffer needs a one-shot: a capture commit clears it on the first `edit()`
/// and never again, so a redo can't wipe a take the user has started since.
///
/// `undo()` keeps the clip it lifted — the *live* one, with every
/// non-undoable change made since the commit (a `Shift+-` tempo rescale, an
/// edge trim) — so redo puts back the clip as it was when undone, not as it
/// was when committed. The construction-time snapshot only ever places the
/// first time; from then on the clip travels through `edit()`/`undo()`, the
/// `MoveClipEdit` pattern.
///
/// Undo leaves the capture buffer cleared (redo is the way back), never
/// reverts the project tempo the first clip's confirm may have detected, and
/// carries no selection context — the handler reads the live selection, since
/// lifting a clip doesn't touch it.
pub(crate) struct CommitClipEdit {
    /// Track the clip lives on.
    track_idx: usize,
    /// Region-detached copy of the clip as last seen off the track: the
    /// committed clip until the first `undo()`, then whatever `undo()` lifted.
    /// Its id is what undo lifts and what redo re-adds, so the UI shape keyed
    /// on it stays valid.
    clip: Clip,
    /// `true` until the first `edit()` of a capture commit has cleared
    /// `capture_clip`; always `false` for an already-placed clip.
    consume_capture: bool,
}

impl CommitClipEdit {
    /// The running-capture commit: builds the clip the capture would freeze
    /// into (`None` on an empty buffer or when it wouldn't fit before the next
    /// clip, so nothing enters the record) and arms the buffer clear.
    pub(crate) fn from_running_capture(sequencer: &Sequencer) -> Option<Self> {
        let (track_idx, clip) = sequencer.build_committed_capture_clip()?;
        Some(Self {
            track_idx,
            clip,
            consume_capture: true,
        })
    }

    /// The stopped-transport commit (`/` with no lead clip): the clip phrase
    /// detection frames from the capture
    /// ([`Sequencer::build_stopped_capture_clip`]), with the buffer clear
    /// armed like the running commit. `None` on an empty buffer or no room at
    /// the cursor.
    pub(crate) fn from_stopped_capture(sequencer: &Sequencer) -> Option<Self> {
        let (track_idx, clip) = sequencer.build_stopped_capture_clip()?;
        Some(Self {
            track_idx,
            clip,
            consume_capture: true,
        })
    }

    /// An empty clip on the selected track — the Arranger's `⇧⌘M`: over the
    /// marquee's tick range, or one bar at the cursor when there is no
    /// marquee or it has no tick width. `None` (nothing enters the record)
    /// with no selected track, or when it would overlap a clip already there.
    pub(crate) fn from_empty_clip(
        sequencer: &Sequencer,
        time_bounds: Option<TimeSelectionRect>,
    ) -> Option<Self> {
        let (start, length) = match time_bounds {
            Some(rect) if rect.has_tick_range() => (rect.start, rect.end - rect.start),
            _ => (sequencer.cursor_tick(), sequencer.meter().bar_ticks()),
        };
        let track_idx = sequencer.selected_track_index()?;
        let mut clip = Clip::new();
        clip.set_start_tick(start);
        clip.region_mut().set_region(None, Some(length));
        sequencer
            .tracks()
            .get(track_idx)?
            .fits(&clip)
            .then_some(Self {
                track_idx,
                clip,
                consume_capture: false,
            })
    }

    /// A clip that is already on `track_idx` (a completed live take): snapshots it so undo can lift it and redo can put
    /// it back. `None` if no such clip is there.
    pub(crate) fn from_placed_clip(
        sequencer: &Sequencer,
        track_idx: usize,
        clip_id: Uuid,
    ) -> Option<Self> {
        let clip = sequencer.clip_on(track_idx, clip_id)?;
        Some(Self {
            track_idx,
            clip: detached_clone(clip),
            consume_capture: false,
        })
    }

    /// Ensures the frozen clip is on its track — adds it if absent (a fresh
    /// commit, or a redo), leaves it alone if already there (the first call
    /// for an already-placed clip) — and, once, clears the capture buffer the
    /// running commit consumed. Returns [`EditResult::ClipCommitted`], or
    /// `NoOp` if the add was refused (something else occupies the span now).
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some(track) = sequencer.tracks_mut().get_mut(self.track_idx) else {
            return EditResult::NoOp;
        };
        if track.get_clip_by_id(self.clip.id()).is_none()
            && !track.add_clip(&detached_clone(&self.clip))
        {
            return EditResult::NoOp;
        }

        if self.consume_capture {
            sequencer.reset_capture();
            self.consume_capture = false;
        }

        EditResult::ClipCommitted {
            clip: ClipMetadata::from_clip(self.track_idx, &self.clip),
        }
    }

    /// Lifts the clip off its track (releasing its sounding notes when the
    /// transport is running, via [`Sequencer::lift_clip`]) and keeps it, so
    /// the next `edit()` re-adds the clip exactly as it was when undone.
    /// Returns [`EditResult::ClipUncommitted`], or `NoOp` if it is no longer
    /// there.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        match sequencer.lift_clip(self.track_idx, self.clip.id()) {
            Some((metadata, clip)) => {
                self.clip = detach(clip);
                EditResult::ClipUncommitted { clip: metadata }
            }
            None => EditResult::NoOp,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use rtrb::Consumer;

    use crate::core::sequencer::ClipInstrumentEvent;
    use crate::core::time::{self, Meter};

    use crate::core::sequencer::test_support::{
        clip_at, instrument_track_0, note_off, note_on, sequencer_with,
    };

    use super::*;

    fn test_sequencer() -> Sequencer {
        let mut sequencer = sequencer_with(false).0;
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer
    }

    /// A one-note take in the capture buffer, `on..off` ticks after the cursor.
    fn capture_note(sequencer: &mut Sequencer, on: i32, off: i32) {
        sequencer.capture_clip.add_event(note_on(on));
        sequencer.capture_clip.add_event(note_off(off));
    }

    fn clip_ids(sequencer: &Sequencer, track_idx: usize) -> Vec<Uuid> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| c.id())
            .collect()
    }

    fn commit(sequencer: &mut Sequencer) -> (CommitClipEdit, ClipMetadata) {
        let mut edit = CommitClipEdit::from_running_capture(sequencer).expect("a take to commit");
        let EditResult::ClipCommitted { clip } = edit.edit(sequencer) else {
            panic!("expected ClipCommitted");
        };
        (edit, clip)
    }

    #[test]
    fn running_commit_places_the_clip_and_clears_the_buffer_undo_lifts_it_redo_restores_it() {
        let mut sequencer = test_sequencer();
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let bystander = clip_at(bar * 4, bar);
        sequencer.tracks_mut()[0].add_clip(&bystander);
        capture_note(&mut sequencer, 600, 900);

        let (mut edit, clip) = commit(&mut sequencer);
        let id = clip.clip_id;
        assert_eq!(clip.start_tick, 0);
        assert_eq!(clip_ids(&sequencer, 0), vec![id, bystander.id()]);
        assert!(
            sequencer.capture_clip.events().is_empty(),
            "commit consumes the take"
        );
        let committed_events: Vec<i32> = sequencer.tracks()[0]
            .get_clip_by_id(id)
            .unwrap()
            .events()
            .iter()
            .map(|e| e.tick())
            .collect();

        let EditResult::ClipUncommitted { clip: lifted } = edit.undo(&mut sequencer) else {
            panic!("expected ClipUncommitted");
        };
        assert_eq!(lifted.clip_id, id);
        assert_eq!(
            clip_ids(&sequencer, 0),
            vec![bystander.id()],
            "exactly the committed clip is gone"
        );
        assert!(
            sequencer.capture_clip.events().is_empty(),
            "undo does not restore the buffer — redo is the way back"
        );

        let EditResult::ClipCommitted { clip: redone } = edit.edit(&mut sequencer) else {
            panic!("expected ClipCommitted");
        };
        assert_eq!(redone.clip_id, id, "redo re-adds the same id");
        assert_eq!(clip_ids(&sequencer, 0), vec![id, bystander.id()]);
        let redone_events: Vec<i32> = sequencer.tracks()[0]
            .get_clip_by_id(id)
            .unwrap()
            .events()
            .iter()
            .map(|e| e.tick())
            .collect();
        assert_eq!(redone_events, committed_events);
    }

    #[test]
    fn redo_does_not_clear_a_buffer_refilled_after_the_undo() {
        let mut sequencer = test_sequencer();
        capture_note(&mut sequencer, 600, 900);
        let (mut edit, _) = commit(&mut sequencer);
        edit.undo(&mut sequencer);

        // The user noodles on after undoing.
        capture_note(&mut sequencer, 5000, 5300);
        edit.edit(&mut sequencer);

        assert_eq!(
            sequencer.capture_clip.events().len(),
            2,
            "the buffer is consumed exactly once, on the original commit"
        );
    }

    #[test]
    fn from_running_capture_is_none_on_an_empty_buffer() {
        let sequencer = test_sequencer();
        assert!(CommitClipEdit::from_running_capture(&sequencer).is_none());
    }

    #[test]
    fn from_running_capture_is_none_when_the_gap_is_under_the_minimum_clip_length() {
        let mut sequencer = test_sequencer();
        let min_len = time::min_clip_length_ticks();
        // The next clip starts half a minimum length after the cursor: the
        // clip is floored to the full minimum, so it cannot fit.
        sequencer.tracks_mut()[0].add_clip(&clip_at(min_len / 2, min_len * 4));
        capture_note(&mut sequencer, 10, 20);

        assert!(CommitClipEdit::from_running_capture(&sequencer).is_none());
        assert_eq!(
            sequencer.capture_clip.events().len(),
            2,
            "a refused commit leaves the take in the buffer"
        );
    }

    fn marquee(start: i32, end: i32) -> Option<TimeSelectionRect> {
        Some(TimeSelectionRect {
            start,
            end,
            track_start: 0,
            track_end: 0,
        })
    }

    fn empty_clip_span(
        sequencer: &Sequencer,
        time_bounds: Option<TimeSelectionRect>,
    ) -> (i32, i32) {
        let edit = CommitClipEdit::from_empty_clip(sequencer, time_bounds).expect("room for it");
        (edit.clip.start_tick(), edit.clip.end_tick())
    }

    #[test]
    fn from_empty_clip_places_an_empty_clip_undo_lifts_it_and_keeps_the_buffer() {
        let mut sequencer = test_sequencer();
        capture_note(&mut sequencer, 600, 900);

        let mut edit = CommitClipEdit::from_empty_clip(&sequencer, marquee(960, 2880)).unwrap();
        let EditResult::ClipCommitted { clip } = edit.edit(&mut sequencer) else {
            panic!("expected ClipCommitted");
        };
        assert_eq!((clip.start_tick, clip.end_tick), (960, 2880));
        let placed = sequencer.tracks()[0].get_clip_by_id(clip.clip_id).unwrap();
        assert!(placed.events().is_empty());
        assert_eq!(
            sequencer.capture_clip.events().len(),
            2,
            "an empty clip never consumes the take"
        );

        edit.undo(&mut sequencer);
        assert!(clip_ids(&sequencer, 0).is_empty());
    }

    /// No marquee, or a zero-width (track-only) one: one bar at the cursor.
    #[test]
    fn from_empty_clip_without_a_tick_range_is_one_bar_at_the_cursor() {
        let sequencer = test_sequencer();
        let bar = Meter::FOUR_FOUR.bar_ticks();
        sequencer.cursor_tick.store(bar, Ordering::Relaxed);

        assert_eq!(empty_clip_span(&sequencer, None), (bar, bar * 2));
        assert_eq!(
            empty_clip_span(&sequencer, marquee(bar * 3, bar * 3)),
            (bar, bar * 2)
        );
    }

    #[test]
    fn from_empty_clip_is_one_bar_of_the_meter() {
        let sequencer = test_sequencer();
        let three_four = Meter::new(3, 4).unwrap();
        sequencer.set_meter(three_four);
        assert_eq!(
            empty_clip_span(&sequencer, None),
            (0, three_four.bar_ticks())
        );
    }

    #[test]
    fn from_empty_clip_is_none_when_it_would_overlap_a_clip() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(1920, 960));

        assert!(CommitClipEdit::from_empty_clip(&sequencer, marquee(960, 2880)).is_none());
        assert!(CommitClipEdit::from_empty_clip(&sequencer, marquee(2400, 3000)).is_none());
        assert!(
            CommitClipEdit::from_empty_clip(&sequencer, None).is_none(),
            "one bar at the cursor (tick 0) reaches the clip"
        );
        assert_eq!(
            empty_clip_span(&sequencer, marquee(960, 1920)),
            (960, 1920),
            "touching the next clip is adjacent, not a collision"
        );
    }

    #[test]
    fn from_empty_clip_is_none_with_no_selected_track() {
        let untracked = sequencer_with(false).0;
        assert!(CommitClipEdit::from_empty_clip(&untracked, None).is_none());
    }

    #[test]
    fn from_placed_clip_first_edit_leaves_the_clip_alone_then_undo_lifts_and_redo_restores() {
        let mut sequencer = test_sequencer();
        let placed = clip_at(960, 960);
        let id = placed.id();
        sequencer.tracks_mut()[0].add_clip(&placed);

        let mut edit = CommitClipEdit::from_placed_clip(&sequencer, 0, id).unwrap();
        let EditResult::ClipCommitted { clip } = edit.edit(&mut sequencer) else {
            panic!("expected ClipCommitted");
        };
        assert_eq!(clip.clip_id, id);
        assert_eq!(clip_ids(&sequencer, 0), vec![id], "no duplicate");

        edit.undo(&mut sequencer);
        assert!(clip_ids(&sequencer, 0).is_empty());

        edit.edit(&mut sequencer);
        assert_eq!(clip_ids(&sequencer, 0), vec![id]);
    }

    #[test]
    fn from_placed_clip_is_none_for_a_missing_clip() {
        let sequencer = test_sequencer();
        assert!(CommitClipEdit::from_placed_clip(&sequencer, 0, Uuid::new_v4()).is_none());
    }

    /// Regression: a non-undoable change to the live clip after the commit
    /// (here a region trim, standing in for the `Shift+-` tempo rescale that
    /// halves a clip) must survive an undo/redo round trip — redo restores
    /// the clip as it was when undone, not as it was when committed.
    #[test]
    fn redo_restores_the_clip_as_it_was_when_undone_not_as_committed() {
        let mut sequencer = test_sequencer();
        let placed = clip_at(0, 960);
        let id = placed.id();
        sequencer.tracks_mut()[0].add_clip(&placed);
        let mut edit = CommitClipEdit::from_placed_clip(&sequencer, 0, id).unwrap();
        edit.edit(&mut sequencer);

        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(id)
            .unwrap()
            .region_mut()
            .set_region(None, Some(480));

        edit.undo(&mut sequencer);
        edit.edit(&mut sequencer);

        let restored = sequencer.tracks()[0].get_clip_by_id(id).unwrap();
        assert_eq!(restored.region().end(), 480);
    }

    /// The held copy never shares its `Region` atomics with the live clip: a
    /// live trim reaches the edit only through `undo()` lifting the clip.
    #[test]
    fn the_held_clip_is_detached_from_the_live_clips_region() {
        let mut sequencer = test_sequencer();
        let placed = clip_at(0, 960);
        let id = placed.id();
        sequencer.tracks_mut()[0].add_clip(&placed);
        let mut edit = CommitClipEdit::from_placed_clip(&sequencer, 0, id).unwrap();
        edit.edit(&mut sequencer);

        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(id)
            .unwrap()
            .region_mut()
            .set_region(None, Some(480));
        assert_eq!(edit.clip.region().end(), 960, "live trim does not leak in");

        edit.undo(&mut sequencer);
        assert_eq!(edit.clip.region().end(), 480, "undo picks up the live clip");

        edit.edit(&mut sequencer);
        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(id)
            .unwrap()
            .region_mut()
            .set_region(None, Some(240));
        assert_eq!(edit.clip.region().end(), 480, "nor after a redo");
    }

    /// Undoing a commit while the committed clip is sounding must release
    /// its open note instead of stranding it — same guard as every other
    /// clip removal.
    #[test]
    fn undoing_a_running_commit_releases_the_playing_clips_stranded_note() {
        let (mut sequencer, mut plugin_rx) = sequencer_with(false);
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        instrument_track_0(&mut sequencer);
        capture_note(&mut sequencer, 0, 480);

        let (mut edit, clip) = commit(&mut sequencer);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(clip.start_tick);

        // One tick sounds the committed clip's note-on.
        sequencer.tick(Instant::now());
        let drain = |rx: &mut Consumer<ClipInstrumentEvent>| -> Vec<[u8; 3]> {
            std::iter::from_fn(|| rx.pop().ok())
                .map(|e| e.message)
                .collect()
        };
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        edit.undo(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }
}
