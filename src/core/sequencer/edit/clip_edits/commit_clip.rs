//! Undoable "a take became a new clip on a track" — the capture commit (`/`,
//! running or stopped). Undo lifts the clip off its track by id; redo puts
//! it back as it was when undone. An empty clip (`⇧⌘M`) is a paste instead
//! (`insert_empty_clips.rs`), since it can cover several tracks.

use std::sync::Arc;

use uuid::Uuid;

use crate::core::sequencer::capture::PassGrid;
use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;
use crate::models::event::Event;

use super::super::super::Sequencer;
use super::super::EditResult;
use super::{detach, detached_clone};

// ---------------------------------------------------------------------------
// CommitClip
// ---------------------------------------------------------------------------

/// Puts one frozen clip, committed from the capture, on one track (see
/// `050-undo-redo.md`): the capture commits build the clip here via
/// [`Sequencer::build_committed_capture`] (running) or
/// [`Sequencer::build_stopped_capture_clip`] (stopped) and let `edit()` place
/// it.
///
/// The fresh commit and every redo share one `edit()` body with no "applied"
/// flag. Only the capture buffer needs a one-shot: a capture commit clears it
/// on the first `edit()` and never again, so a redo can't wipe a take the user
/// has started since.
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
///
/// A looping running commit keeps the buffer it consumed and the
/// grid it was framed on, so `,`/`.` can step the clip back and forward
/// through the passes the buffer held ([`step_pass`](Self::step_pass),
/// `540-earlier-pass.md`). A step amends this edit rather than becoming an
/// undo step of its own ([`absorb`](Self::absorb)), so one ⌘Z removes the
/// take whatever pass it shows. The buffer goes once another step is
/// recorded on top ([`drop_passes`](Self::drop_passes)), since the keys can
/// never reach it again.
pub(crate) struct CommitClipEdit {
    /// Track the clip lives on.
    track_idx: usize,
    /// Region-detached copy of the clip as last seen off the track: the
    /// committed clip until the first `undo()`, then whatever `undo()` lifted.
    /// Its id is what undo lifts and what redo re-adds, so the UI shape keyed
    /// on it stays valid.
    clip: Clip,
    /// `true` until the first `edit()` has cleared `capture_clip`.
    consume_capture: bool,
    /// What `,`/`.` can step the clip through.
    passes: Passes,
}

/// The passes of a commit `,`/`.` step through (`540-earlier-pass.md`).
enum Passes {
    /// None: a stopped commit, or one whose buffer was dropped.
    None,
    /// A linear running take, which has no passes.
    Linear,
    /// A looping running take not yet placed: the first `edit()` takes the
    /// buffer.
    Pending(PassGrid),
    /// A looping running take, with the buffer it consumed.
    Kept(KeptPasses),
}

/// The buffer a looping running commit consumed, and where in it the clip
/// is.
struct KeptPasses {
    /// The grid the commit was framed on.
    grid: PassGrid,
    /// The capture buffer as the commit read it, knob moves no knob holds
    /// dropped (`Sequencer::kept_capture`).
    buffer: Clip,
    /// The passes holding a note-on, up to the one `/` took, in order.
    passes: Vec<i32>,
    /// The index in `passes` of the one the clip shows.
    shown: usize,
}

impl KeptPasses {
    /// The footer line for the pass at `index` (decision 8).
    fn label(&self, index: usize) -> String {
        let back = self.passes.len() - 1 - index;
        if back == 0 {
            "Last pass".to_owned()
        } else {
            format!("Pass {back} back of {}", self.passes.len())
        }
    }
}

/// Which way `,`/`.` step a running take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassStep {
    /// `,`: the pass before.
    Earlier,
    /// `.`: the pass after.
    Later,
}

/// Why `,`/`.` did nothing (`540-earlier-pass.md` decision 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassRefusal {
    /// The newest undo step isn't a running new-clip commit (decision 5).
    NoRunningTake,
    /// A linear running take has no passes (decision 9).
    NotLooping,
    /// The clip shows the first pass.
    NoEarlier,
    /// The clip shows the pass `/` took.
    AlreadyLast,
}

impl PassRefusal {
    /// The footer line saying why.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NoRunningTake => "No running take to step through",
            Self::NotLooping => "The take isn't looping",
            Self::NoEarlier => "No earlier pass",
            Self::AlreadyLast => "Already the last pass",
        }
    }
}

impl CommitClipEdit {
    /// The running-capture commit: builds the clip the capture would freeze
    /// into (`None` on an empty buffer or when it wouldn't fit before the next
    /// clip, so nothing enters the record) and arms the buffer clear.
    /// `marquee` frames it when the arranger has one
    /// (`470-marquee-capture.md`).
    pub(crate) fn from_running_capture(
        sequencer: &Sequencer,
        marquee: Option<(i32, i32)>,
    ) -> Option<Self> {
        let (track_idx, clip, grid) = sequencer.build_committed_capture(marquee)?;
        let passes = grid.map_or(Passes::Linear, Passes::Pending);
        Some(Self {
            track_idx,
            clip,
            consume_capture: true,
            passes,
        })
    }

    /// The stopped-transport commit (`/` with no lead clip): the clip phrase
    /// detection frames from the capture
    /// ([`Sequencer::build_stopped_capture_clip`]), with the buffer clear
    /// armed like the running commit. `None` on an empty buffer or no room at
    /// the cursor. `marquee` frames it as for the running commit.
    pub(crate) fn from_stopped_capture(
        sequencer: &Sequencer,
        marquee: Option<(i32, i32)>,
    ) -> Option<Self> {
        let (track_idx, clip) = sequencer.build_stopped_capture_clip(marquee)?;
        Some(Self {
            track_idx,
            clip,
            consume_capture: true,
            passes: Passes::None,
        })
    }

    /// Puts the frozen clip on its track (a fresh commit, or a redo) and,
    /// once, clears the capture buffer the running commit consumed. Returns
    /// [`EditResult::ClipCommitted`], or `NoOp` if the add was refused
    /// (something else occupies the span now).
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        let Some(track) = sequencer.tracks_mut().get_mut(self.track_idx) else {
            return EditResult::NoOp;
        };
        if !track.add_clip(&detached_clone(&self.clip)) {
            return EditResult::NoOp;
        }

        if self.consume_capture {
            if let Passes::Pending(grid) = self.passes {
                let mut buffer = std::mem::replace(&mut sequencer.capture_clip, Clip::new());
                sequencer.keep_knob_moves(&mut buffer);
                let last = grid.last_pass(&buffer);
                let passes = grid.passes(&buffer, last);
                self.passes = Passes::Kept(KeptPasses {
                    grid,
                    shown: passes.len().saturating_sub(1),
                    buffer,
                    passes,
                });
            }
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

    /// The step `,` or `.` makes to the clip, with its footer line, or why
    /// there is none: the pass before or after the one it shows, framed as
    /// `/` framed the one it took (`540-earlier-pass.md` decisions 3 and 7).
    /// The caller has checked this is the newest undo step (decision 5).
    pub(crate) fn step_pass(
        &self,
        sequencer: &Sequencer,
        step: PassStep,
    ) -> Result<(PassStepEdit, String), PassRefusal> {
        let kept = match &self.passes {
            Passes::Kept(kept) => kept,
            Passes::Linear => return Err(PassRefusal::NotLooping),
            Passes::None | Passes::Pending(_) => return Err(PassRefusal::NoRunningTake),
        };
        let index = match step {
            PassStep::Earlier => kept.shown.checked_sub(1).ok_or(PassRefusal::NoEarlier)?,
            PassStep::Later => Some(kept.shown + 1)
                .filter(|&later| later < kept.passes.len())
                .ok_or(PassRefusal::AlreadyLast)?,
        };
        let clip = sequencer
            .clip_on(self.track_idx, self.clip.id())
            .ok_or(PassRefusal::NoRunningTake)?;
        let mut framed = kept.grid.frame(kept.buffer.clone(), kept.passes[index]);
        framed.detect_and_store_swing();
        let edit = PassStepEdit {
            track_idx: self.track_idx,
            clip_id: self.clip.id(),
            pass: ClipContent::of(&framed),
            before: ClipContent::of(clip),
            shown: index,
        };
        Ok((edit, kept.label(index)))
    }

    /// Takes in a pass step made to its clip, so the step amends this edit
    /// (decision 4): the clip it keeps shows the new pass, and the next step
    /// goes from there. `false` for a step to another clip, or when there
    /// are no passes to step through.
    pub(in crate::core::sequencer::edit) fn absorb(&mut self, step: &PassStepEdit) -> bool {
        let Passes::Kept(kept) = &mut self.passes else {
            return false;
        };
        if step.clip_id != self.clip.id() {
            return false;
        }
        kept.shown = step.shown;
        step.pass.put_in(&mut self.clip);
        true
    }

    /// Drops the kept buffer: another step went on top, so `,`/`.` can't
    /// reach this commit again (decision 6).
    pub(in crate::core::sequencer::edit) fn drop_passes(&mut self) {
        if matches!(self.passes, Passes::Kept(_) | Passes::Pending(_)) {
            self.passes = Passes::None;
        }
    }

    /// Whether the commit keeps a buffer to step through.
    #[cfg(test)]
    pub(crate) fn keeps_buffer(&self) -> bool {
        matches!(self.passes, Passes::Kept(_))
    }
}

/// What a pass step swaps in a clip: its events and the swing detected in
/// them. The rest of the clip (id, start, region, length) stays.
struct ClipContent {
    /// The events, shared with the clip.
    events: Arc<Vec<Event>>,
    /// The clip's swing.
    swing_pct: u8,
}

impl ClipContent {
    /// `clip`'s events and swing.
    fn of(clip: &Clip) -> Self {
        Self {
            events: Arc::clone(clip.shared_events()),
            swing_pct: clip.swing_pct(),
        }
    }

    /// Puts these events and swing in `clip`.
    fn put_in(&self, clip: &mut Clip) {
        clip.restore_shared_events(Arc::clone(&self.events));
        clip.set_swing_pct(self.swing_pct);
    }
}

/// `,`/`.`: the clip a running `/` made shows another pass of the take
/// (`540-earlier-pass.md`). Only its events and swing change, through
/// [`Sequencer::edit_clip_events`], so the old pass's sounding notes are
/// released. Built by [`CommitClipEdit::step_pass`], it merges into the
/// commit ([`CommitClipEdit::absorb`]); its own undo, for a step that stays
/// on its own, puts back the clip's content from before.
pub(crate) struct PassStepEdit {
    /// Track the clip is on.
    track_idx: usize,
    /// The clip the `/` made.
    clip_id: Uuid,
    /// The pass, framed as the clip's.
    pass: ClipContent,
    /// The clip's content before the step.
    before: ClipContent,
    /// The pass's index among the commit's passes.
    shown: usize,
}

impl PassStepEdit {
    /// Puts the pass in the clip.
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.swap_in(sequencer, &self.pass)
    }

    /// Puts back the clip's content from before.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.swap_in(sequencer, &self.before)
    }

    /// Puts `content` in the clip: `EventsModified` with nothing selected
    /// (the events have ids of their own), or `NoOp` with the clip gone.
    fn swap_in(&self, sequencer: &mut Sequencer, content: &ClipContent) -> EditResult {
        if sequencer
            .edit_clip_events(self.track_idx, self.clip_id, |clip| content.put_in(clip))
            .is_none()
        {
            return EditResult::NoOp;
        }
        EditResult::EventsModified {
            track_idx: self.track_idx,
            clip_id: self.clip_id,
            selected_event_ids: Vec::new(),
            inserted_take: None,
            duplicate_span: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use rtrb::Consumer;
    use uuid::Uuid;

    use crate::core::sequencer::ClipInstrumentEvent;
    use crate::core::time::{self, Meter};

    use crate::core::sequencer::test_support::{
        cc_at, clip_at, instrument_track_0, note_off, note_on, sequencer_with,
    };
    use crate::models::event::EventType;

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
        let mut edit =
            CommitClipEdit::from_running_capture(sequencer, None).expect("a take to commit");
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
        assert!(CommitClipEdit::from_running_capture(&sequencer, None).is_none());
    }

    #[test]
    fn from_running_capture_is_none_when_the_gap_is_under_the_minimum_clip_length() {
        let mut sequencer = test_sequencer();
        let min_len = time::min_clip_length_ticks();
        // The next clip starts half a minimum length after the cursor: the
        // clip is floored to the full minimum, so it cannot fit.
        sequencer.tracks_mut()[0].add_clip(&clip_at(min_len / 2, min_len * 4));
        capture_note(&mut sequencer, 10, 20);

        assert!(CommitClipEdit::from_running_capture(&sequencer, None).is_none());
        assert_eq!(
            sequencer.capture_clip.events().len(),
            2,
            "a refused commit leaves the take in the buffer"
        );
    }

    /// Regression: a non-undoable change to the live clip after the commit
    /// (here a region trim, standing in for the `Shift+-` tempo rescale that
    /// halves a clip) must survive an undo/redo round trip — redo restores
    /// the clip as it was when undone, not as it was when committed.
    #[test]
    fn redo_restores_the_clip_as_it_was_when_undone_not_as_committed() {
        let mut sequencer = test_sequencer();
        capture_note(&mut sequencer, 0, 480);
        let (mut edit, clip) = commit(&mut sequencer);
        let id = clip.clip_id;

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
        capture_note(&mut sequencer, 0, 480);
        let (mut edit, clip) = commit(&mut sequencer);
        let id = clip.clip_id;
        let committed_end = edit.clip.region().end();
        assert_ne!(committed_end, 480, "the trim below must change the clip");

        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(id)
            .unwrap()
            .region_mut()
            .set_region(None, Some(480));
        assert_eq!(
            edit.clip.region().end(),
            committed_end,
            "live trim does not leak in"
        );

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
                .filter_map(|e| e.message.midi())
                .collect()
        };
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        edit.undo(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }

    // --- `,`/`.`: earlier passes (`540-earlier-pass.md`) ---

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// Looping over `[0, loop_len)`, playback at its start, the cursor at
    /// `cursor`, track 0 selected.
    fn looping(loop_len: i32, cursor: i32) -> Sequencer {
        let mut sequencer = sequencer_with(true).0;
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.set_global_region(0, loop_len);
        sequencer.playback_tick.store(0, Ordering::Relaxed);
        sequencer.cursor_tick.store(cursor, Ordering::Relaxed);
        sequencer
    }

    /// A `pitch` note from `on` to `off` into the capture buffer.
    fn play(sequencer: &mut Sequencer, pitch: u8, on: i32, off: i32) {
        let capture = &mut sequencer.capture_clip;
        capture.add_event(Event::new(on, 0, vec![0x90, pitch, 100]));
        capture.add_event(Event::new(off, 0, vec![0x80, pitch, 0]));
    }

    /// What a step may change in the committed clip: its events (tick,
    /// message) and swing, with its region and start, which it may not.
    type Shape = (Vec<(i32, Vec<u8>)>, u8, (i32, i32), i32);

    fn shape(sequencer: &Sequencer, id: Uuid) -> Shape {
        let clip = sequencer.tracks()[0].get_clip_by_id(id).unwrap();
        let events = clip
            .events()
            .iter()
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect();
        let region = (clip.region().start(), clip.region().end());
        (events, clip.swing_pct(), region, clip.start_tick())
    }

    /// The pitches of the committed clip's note-ons, in order.
    fn pitches(sequencer: &Sequencer, id: Uuid) -> Vec<u8> {
        let clip = sequencer.tracks()[0].get_clip_by_id(id).unwrap();
        clip.events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .filter_map(Event::note_number)
            .collect()
    }

    /// One `,` or `.` as the handler makes it: the step, merged into the
    /// commit. Returns its footer line.
    fn step(
        sequencer: &mut Sequencer,
        commit: &mut CommitClipEdit,
        step: PassStep,
    ) -> Result<String, PassRefusal> {
        let (mut edit, label) = commit.step_pass(sequencer, step)?;
        assert!(matches!(
            edit.edit(sequencer),
            EditResult::EventsModified { .. }
        ));
        assert!(commit.absorb(&edit), "the step amends the commit");
        Ok(label)
    }

    /// Stepping back to the first pass and forward again gives back exactly
    /// the clip `/` made, events and region, and each step lands on the
    /// commit's grid: the start, region and length never move.
    fn assert_round_trips(mut sequencer: Sequencer, passes: usize, case: &str) {
        let (mut commit, clip) = commit(&mut sequencer);
        let made = shape(&sequencer, clip.clip_id);
        for _ in 1..passes {
            step(&mut sequencer, &mut commit, PassStep::Earlier).expect(case);
            let stepped = shape(&sequencer, clip.clip_id);
            assert_eq!(stepped.2, made.2, "{case}: the region stays");
            assert_eq!(stepped.3, made.3, "{case}: the start stays");
        }
        assert_eq!(
            step(&mut sequencer, &mut commit, PassStep::Earlier),
            Err(PassRefusal::NoEarlier),
            "{case}"
        );
        for _ in 1..passes {
            step(&mut sequencer, &mut commit, PassStep::Later).expect(case);
        }
        assert_eq!(
            step(&mut sequencer, &mut commit, PassStep::Later),
            Err(PassRefusal::AlreadyLast),
            "{case}"
        );
        assert_eq!(shape(&sequencer, clip.clip_id), made, "{case}");
    }

    #[test]
    fn stepping_back_and_forward_gives_back_the_clip_slash_made() {
        // A start at phase 0: one note a pass.
        let mut s = looping(BAR, 0);
        for (pass, pitch) in [(0, 60), (1, 62), (2, 64)] {
            play(&mut s, pitch, pass * BAR + 100, pass * BAR + 400);
        }
        assert_round_trips(s, 3, "a start at phase 0");

        // A start mid-loop: the first pass is its second half.
        let mut s = looping(BAR * 2, 0);
        play(&mut s, 60, BAR + 300, BAR + 600);
        play(&mut s, 62, BAR * 2 + 100, BAR * 2 + 400);
        play(&mut s, 64, BAR * 3 + 700, BAR * 3 + 900);
        assert_round_trips(s, 2, "a partial first pass");

        // A mid-pass `/`: the last pass only its first beat.
        let mut s = looping(BAR * 2, 0);
        play(&mut s, 60, 100, 400);
        play(&mut s, 62, BAR + 100, BAR + 400);
        play(&mut s, 64, BAR * 2 + 100, BAR * 2 + 200);
        assert_round_trips(s, 2, "a partial last pass");

        // The cursor mid-loop: the region is a slice from its phase.
        let mut s = looping(BAR * 2, BAR);
        play(&mut s, 60, 100, BAR + 400);
        play(&mut s, 62, BAR * 2 + BAR + 100, BAR * 3 + 400);
        assert_round_trips(s, 2, "the cursor mid-loop");

        // An early chord before the wrap starts the last pass.
        let mut s = looping(BAR * 2, 0);
        play(&mut s, 50, 600, 900);
        for pitch in [60, 64, 67] {
            play(&mut s, pitch, BAR * 2 - 40, BAR * 2 + 400);
        }
        assert_round_trips(s, 2, "an early chord");

        // A held pedal, carried onto the pass stepped to.
        let mut s = looping(BAR, 0);
        s.capture_clip.add_event(cc_at(50, 64, 127));
        for pass in 0..3 {
            play(&mut s, 60, pass * BAR + 100, pass * BAR + 400);
        }
        assert_round_trips(s, 3, "a held pedal");
    }

    /// Each step shows that pass, and the footer says where it is.
    #[test]
    fn each_step_shows_its_pass_and_names_it() {
        let mut sequencer = looping(BAR, 0);
        for (pass, pitch) in [(0, 60), (1, 62), (2, 64)] {
            play(&mut sequencer, pitch, pass * BAR + 100, pass * BAR + 400);
        }
        let (mut commit, clip) = commit(&mut sequencer);
        let id = clip.clip_id;
        assert_eq!(pitches(&sequencer, id), vec![64]);

        let mut back = || step(&mut sequencer, &mut commit, PassStep::Earlier);
        assert_eq!(back().as_deref(), Ok("Pass 1 back of 3"));
        assert_eq!(back().as_deref(), Ok("Pass 2 back of 3"));
        assert_eq!(pitches(&sequencer, id), vec![60]);
        assert_eq!(
            step(&mut sequencer, &mut commit, PassStep::Later).as_deref(),
            Ok("Pass 1 back of 3")
        );
        assert_eq!(pitches(&sequencer, id), vec![62]);
        assert_eq!(
            step(&mut sequencer, &mut commit, PassStep::Later).as_deref(),
            Ok("Last pass")
        );
        assert_eq!(pitches(&sequencer, id), vec![64]);
    }

    /// A pass spent listening isn't counted: one step back from the third
    /// pass is the first.
    #[test]
    fn a_silent_pass_is_skipped() {
        let mut sequencer = looping(BAR, 0);
        play(&mut sequencer, 60, 100, 400);
        play(&mut sequencer, 64, BAR * 2 + 100, BAR * 2 + 400);
        let (mut commit, clip) = commit(&mut sequencer);

        let label = step(&mut sequencer, &mut commit, PassStep::Earlier);
        assert_eq!(label.as_deref(), Ok("Pass 1 back of 2"));
        assert_eq!(pitches(&sequencer, clip.clip_id), vec![60]);
    }

    /// The early chord that starts the last pass (the case of
    /// `commit_clip_to_track_early_chord_starts_the_next_pass`) stays out of
    /// the pass before it, stepped back to: it is the next pass's.
    #[test]
    fn stepping_back_past_an_early_chord_leaves_it_out() {
        let mut sequencer = looping(BAR * 2, 0);
        play(&mut sequencer, 50, 600, 900);
        play(&mut sequencer, 52, BAR + 600, BAR + 900);
        for (pitch, on) in [(60, BAR * 2 - 40), (64, BAR * 2 - 30), (67, BAR * 2 - 20)] {
            play(&mut sequencer, pitch, on, BAR * 2 + 400);
        }
        let (mut commit, clip) = commit(&mut sequencer);
        assert_eq!(pitches(&sequencer, clip.clip_id), vec![60, 64, 67]);

        step(&mut sequencer, &mut commit, PassStep::Earlier).unwrap();

        assert_eq!(pitches(&sequencer, clip.clip_id), vec![50, 52]);
    }

    /// A pedal pressed in the first pass and held is carried onto the start
    /// of the second when stepped back to it, as `/` would carry it.
    #[test]
    fn a_pedal_held_from_an_earlier_pass_is_carried_onto_the_pass_stepped_to() {
        let mut sequencer = looping(BAR, 0);
        sequencer.capture_clip.add_event(cc_at(50, 64, 127));
        for pass in 0..3 {
            play(&mut sequencer, 60, pass * BAR + 100, pass * BAR + 400);
        }
        let (mut commit, clip) = commit(&mut sequencer);

        step(&mut sequencer, &mut commit, PassStep::Earlier).unwrap();

        let clip = sequencer.tracks()[0].get_clip_by_id(clip.clip_id).unwrap();
        let pedal: Vec<(i32, Vec<u8>)> = clip
            .events()
            .iter()
            .filter(|e| !e.is_note_edge())
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect();
        assert_eq!(pedal, vec![(0, vec![0xB0, 64, 127])]);
    }

    /// A linear running take has no passes; a stopped commit has none to
    /// step through.
    #[test]
    fn only_a_looping_running_commit_steps() {
        let mut linear = test_sequencer();
        play(&mut linear, 60, 100, 400);
        let (commit_linear, _) = commit(&mut linear);
        assert_eq!(
            commit_linear.step_pass(&linear, PassStep::Earlier).err(),
            Some(PassRefusal::NotLooping)
        );

        let mut stopped = looping(BAR, 0);
        play(&mut stopped, 60, 100, 400);
        let mut commit_stopped = CommitClipEdit::from_stopped_capture(&stopped, None).unwrap();
        commit_stopped.edit(&mut stopped);
        assert_eq!(
            commit_stopped.step_pass(&stopped, PassStep::Earlier).err(),
            Some(PassRefusal::NoRunningTake)
        );
    }

    /// A marquee over the loop (⌘L leaves it there) frames the `/`, and the
    /// clip steps through the passes all the same, its span staying the
    /// marquee's: the marquee anchors the same pass grid.
    #[test]
    fn a_marquee_framed_commit_steps_its_span_through_the_passes() {
        let mut sequencer = looping(BAR * 2, 0);
        play(&mut sequencer, 60, BAR + 100, BAR + 400);
        play(&mut sequencer, 62, BAR * 3 + 100, BAR * 3 + 400);
        play(&mut sequencer, 64, BAR * 5 + 100, BAR * 5 + 400);
        let mut commit = CommitClipEdit::from_running_capture(&sequencer, Some((BAR, BAR * 2)))
            .expect("a take to commit");
        let EditResult::ClipCommitted { clip } = commit.edit(&mut sequencer) else {
            panic!("expected ClipCommitted");
        };
        let made = shape(&sequencer, clip.clip_id);
        assert_eq!(made.3, BAR, "at the marquee's start");
        assert_eq!(pitches(&sequencer, clip.clip_id), vec![64]);

        let label = step(&mut sequencer, &mut commit, PassStep::Earlier);
        assert_eq!(label.as_deref(), Ok("Pass 1 back of 3"));
        assert_eq!(pitches(&sequencer, clip.clip_id), vec![62]);
        let stepped = shape(&sequencer, clip.clip_id);
        assert_eq!((stepped.2, stepped.3), (made.2, made.3), "the span stays");

        step(&mut sequencer, &mut commit, PassStep::Later).unwrap();
        assert_eq!(shape(&sequencer, clip.clip_id), made);
    }

    /// The commit keeps the buffer it consumed, and the knob moves no knob
    /// holds go from it as from the clip.
    #[test]
    fn the_commit_keeps_the_buffer_it_consumed() {
        let mut sequencer = looping(BAR, 0);
        play(&mut sequencer, 60, 100, 400);
        play(&mut sequencer, 62, BAR + 100, BAR + 400);
        let (commit, _) = commit(&mut sequencer);
        assert!(commit.keeps_buffer());
        assert!(sequencer.capture_clip.events().is_empty());
    }
}
