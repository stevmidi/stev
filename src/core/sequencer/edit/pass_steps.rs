//! `,`/`.`: stepping the take a running `/` landed back and forward through
//! the passes its capture buffer held (`540-earlier-pass.md`). What the two
//! running commits share: the buffer they keep ([`Passes`]), the step's
//! direction and refusals, and the step itself ([`PassStepEdit`]), which
//! amends the commit it steps. The new clip's commit is `CommitClipEdit`,
//! the insert into the lead clip `InsertCaptureEdit`; each frames a pass
//! through its own grid ([`PassFraming`]).

use std::sync::Arc;

use uuid::Uuid;

use crate::core::sequencer::capture::PassFraming;
use crate::models::clip::Clip;
use crate::models::event::Event;

use super::super::Sequencer;
use super::EditResult;

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
    /// The newest undo step isn't a running commit (decision 5).
    NoRunningTake,
    /// A linear running take has no passes (decision 9).
    NotLooping,
    /// The take shows the first pass.
    NoEarlier,
    /// The take shows the pass `/` took.
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

/// The passes of a running commit `,`/`.` step through, read through the
/// grid `G` it was framed on.
pub(super) enum Passes<G> {
    /// None: a stopped commit, or one whose buffer was dropped.
    None,
    /// A linear running take, which has no passes.
    Linear,
    /// A looping running take not yet landed: its first `edit()` takes the
    /// buffer ([`keep`](Self::keep)).
    Pending(G),
    /// A looping running take, with the buffer it consumed.
    Kept(KeptPasses<G>),
}

/// The buffer a looping running commit consumed, and which of its passes
/// the take shows.
pub(super) struct KeptPasses<G> {
    /// The grid the commit was framed on.
    pub(super) grid: G,
    /// The capture buffer as the commit read it, knob moves no knob holds
    /// dropped (`Sequencer::kept_capture`).
    pub(super) buffer: Clip,
    /// The passes to step through, up to the one `/` took, in order.
    passes: Vec<i32>,
    /// The index in `passes` of the one the take shows.
    shown: usize,
}

impl<G> KeptPasses<G> {
    /// The pass at `index`, as the grid numbers it.
    pub(super) fn pass(&self, index: usize) -> i32 {
        self.passes[index]
    }

    /// The pass `/` took.
    pub(super) fn last(&self) -> i32 {
        self.passes[self.passes.len() - 1]
    }

    /// The footer line for the pass at `index` (decision 8).
    pub(super) fn label(&self, index: usize) -> String {
        let back = self.passes.len() - 1 - index;
        if back == 0 {
            "Last pass".to_owned()
        } else {
            format!("Pass {back} back of {}", self.passes.len())
        }
    }
}

impl<G: PassFraming + Copy> Passes<G> {
    /// The passes of a running take framed on `grid`, `None` when it didn't
    /// loop.
    pub(super) fn running(grid: Option<G>) -> Self {
        grid.map_or(Self::Linear, Self::Pending)
    }

    /// At the commit's first `edit()`, before the buffer is cleared: takes
    /// the buffer a looping take consumed (`std::mem::take`, decision 6) and
    /// shows the pass `/` took.
    pub(super) fn keep(&mut self, sequencer: &mut Sequencer) {
        let Self::Pending(grid) = *self else {
            return;
        };
        let mut buffer = std::mem::replace(&mut sequencer.capture_clip, Clip::new());
        sequencer.keep_knob_moves(&mut buffer);
        let last = grid.last_pass(&buffer);
        let passes = grid.passes(&buffer, last);
        *self = Self::Kept(KeptPasses {
            grid,
            shown: passes.len() - 1,
            buffer,
            passes,
        });
    }

    /// The kept passes and the index `step` goes to, or why there is none.
    pub(super) fn step(&self, step: PassStep) -> Result<(&KeptPasses<G>, usize), PassRefusal> {
        let kept = match self {
            Self::Kept(kept) => kept,
            Self::Linear => return Err(PassRefusal::NotLooping),
            Self::None | Self::Pending(_) => return Err(PassRefusal::NoRunningTake),
        };
        let index = match step {
            PassStep::Earlier => kept.shown.checked_sub(1).ok_or(PassRefusal::NoEarlier)?,
            PassStep::Later => Some(kept.shown + 1)
                .filter(|&later| later < kept.passes.len())
                .ok_or(PassRefusal::AlreadyLast)?,
        };
        Ok((kept, index))
    }

    /// The edit recorded on top of the commit whose take is in clip
    /// `clip_id`: `step` when it is a pass step. A step of this take moves
    /// the pass shown (decision 4) and returns it, for the commit to take
    /// in; anything else drops the kept buffer, since `,`/`.` can't reach
    /// the commit again (decision 6), and returns `None`.
    pub(super) fn absorb<'a>(
        &mut self,
        step: Option<&'a PassStepEdit>,
        clip_id: Uuid,
    ) -> Option<&'a PassStepEdit> {
        match (self, step) {
            (Self::Kept(kept), Some(step)) if step.clip_id == clip_id => {
                kept.shown = step.shown;
                Some(step)
            }
            (passes @ (Self::Kept(_) | Self::Pending(_)), _) => {
                *passes = Self::None;
                None
            }
            _ => None,
        }
    }

    /// Whether a buffer is kept to step through.
    #[cfg(test)]
    pub(super) fn keeps_buffer(&self) -> bool {
        matches!(self, Self::Kept(_))
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

/// `,`/`.`: the take a running `/` landed shows another pass
/// (`540-earlier-pass.md`). Only the clip's events and swing change, through
/// [`Sequencer::edit_clip_events`], so the old pass's sounding notes are
/// released. Built by the commit it steps, it merges into it
/// (`SequencerEdit::merge`); its own undo, for a step that stays on its own,
/// puts back the clip's content from before.
pub(crate) struct PassStepEdit {
    /// Track the clip is on.
    track_idx: usize,
    /// The clip the take landed in.
    pub(super) clip_id: Uuid,
    /// The clip with the pass in it.
    pass: ClipContent,
    /// The clip's content before the step.
    before: ClipContent,
    /// The clip's event selection, reported both ways.
    selected_event_ids: Vec<Uuid>,
    /// The pass's index among the commit's passes.
    shown: usize,
    /// The events an insert's step landed, for the insert's redo; empty for
    /// a new clip's.
    pub(super) take: Vec<Event>,
}

impl PassStepEdit {
    /// The step from `clip` (on track `track_idx`) to `stepped`, the same
    /// clip showing the pass at `shown`, with `take` the events an insert
    /// landed in it.
    pub(super) fn new(
        track_idx: usize,
        clip: &Clip,
        stepped: &Clip,
        shown: usize,
        take: Vec<Event>,
    ) -> Self {
        Self {
            track_idx,
            clip_id: clip.id(),
            pass: ClipContent::of(stepped),
            before: ClipContent::of(clip),
            selected_event_ids: stepped.selected_event_ids(),
            shown,
            take,
        }
    }

    /// Puts the pass in `clip`, a copy of the one stepped (the commit's
    /// held clip, absorbing the step).
    pub(super) fn put_in(&self, clip: &mut Clip) {
        self.pass.put_in(clip);
    }

    /// Puts the pass in the clip.
    pub(super) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        self.swap_in(sequencer, &self.pass)
    }

    /// Puts back the clip's content from before.
    pub(super) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        self.swap_in(sequencer, &self.before)
    }

    /// Puts `content` in the clip: `EventsModified` with the clip's
    /// selection, or `NoOp` with the clip gone.
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
            selected_event_ids: self.selected_event_ids.clone(),
            inserted_take: None,
            duplicate_span: None,
        }
    }
}
