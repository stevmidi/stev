//! [`UndoStep`]: one entry of the undo record — a [`SequencerEdit`] with what
//! the user was looking at just before it and just after it
//! ([`ViewSnapshot`]), so an undo puts back the view before the edit and a
//! redo the view after it, whatever the user did in between. See
//! `050-undo-redo.md` § Undo and the view, `440-undo-view-state.md`.

use std::cell::Cell;

use undo::{Edit, Merged, Record};
use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;

use super::super::Sequencer;
use super::{EditResult, SequencerEdit};

/// The view an undo or redo puts back: the selected track, the lead clip,
/// the cursor and the arranger marquee. The track is kept by id, so nothing
/// depends on indices staying put; the marquee comes from the sequencer's
/// mirror of the view's (`Sequencer::marquee`), track span included. The
/// clip view's own cursor and note selection are not in it: a note edit's
/// undo restores its event selection itself (`EditResult::EventsModified`),
/// and the clip cursor follows the arranger cursor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewSnapshot {
    /// The selected track.
    pub(crate) track: Option<Uuid>,
    /// The lead clip.
    pub(crate) lead_clip: Option<Uuid>,
    /// The arranger cursor, which the marquee is anchored to.
    pub(crate) cursor_tick: i32,
    /// The arranger marquee, `None` for none.
    pub(crate) marquee: Option<TimeSelectionRect>,
}

impl ViewSnapshot {
    /// The view as the sequencer has it now.
    pub(crate) fn of(sequencer: &Sequencer) -> Self {
        Self {
            track: sequencer.selected_track_id(),
            lead_clip: sequencer.selected_clip_id(),
            cursor_tick: sequencer.cursor_tick(),
            marquee: sequencer.marquee(),
        }
    }
}

/// One undo step: the edit, the view just before it ran (what an undo puts
/// back) and the view once its result was applied (what a redo puts back).
/// `after` is a `Cell` because the `undo` crate only lends a recorded entry
/// immutably, and it is filled once the edit's result has run
/// (`EventHandlers::record_edit`). A merge keeps the first step's `before`,
/// so one drag undoes to where it started, and `record_edit` writes the
/// newest `after` over it, so the drag redoes to where it ended.
pub(crate) struct UndoStep {
    /// The edit itself.
    edit: SequencerEdit,
    /// The view just before the edit.
    before: ViewSnapshot,
    /// The view after the edit's result was applied; `None` until then.
    after: Cell<Option<ViewSnapshot>>,
}

impl UndoStep {
    /// `edit`, recorded from the view `before`.
    pub(crate) fn new(edit: SequencerEdit, before: ViewSnapshot) -> Self {
        Self {
            edit,
            before,
            after: Cell::new(None),
        }
    }

    /// The edit.
    pub(crate) fn sequencer_edit(&self) -> &SequencerEdit {
        &self.edit
    }

    /// The view an undo of this step puts back, `None` for an edit that
    /// leaves the view alone ([`SequencerEdit::restores_view`]).
    fn view_before(&self) -> Option<ViewSnapshot> {
        self.edit.restores_view().then_some(self.before)
    }

    /// The view a redo of this step puts back, `None` for an edit that
    /// leaves the view alone, or before `after` was filled.
    fn view_after(&self) -> Option<ViewSnapshot> {
        self.after.get().filter(|_| self.edit.restores_view())
    }

    /// Records the view once the edit's result was applied.
    pub(crate) fn set_after(&self, after: ViewSnapshot) {
        self.after.set(Some(after));
    }
}

/// The step on top of `record`: the one last recorded or redone, which a
/// merge goes into and the next undo takes back.
pub(crate) fn top_step(record: &Record<UndoStep>) -> Option<&UndoStep> {
    let top = record.head().checked_sub(1)?;
    record.get_entry(top).map(|entry| entry.as_ref())
}

/// What a step's edit did, with the view an undo or redo of it puts back
/// (`None` on the edit itself, and for an edit that leaves the view alone).
pub(crate) type StepOutput = (EditResult, Option<ViewSnapshot>);

impl Edit for UndoStep {
    type Target = Sequencer;
    type Output = StepOutput;

    fn edit(&mut self, target: &mut Sequencer) -> StepOutput {
        (self.edit.edit(target), None)
    }

    fn undo(&mut self, target: &mut Sequencer) -> StepOutput {
        (self.edit.undo(target), self.view_before())
    }

    fn redo(&mut self, target: &mut Sequencer) -> StepOutput {
        (self.edit.redo(target), self.view_after())
    }

    /// Merges the edits (`SequencerEdit::merge`), keeping this step's
    /// `before`; the caller fills `after` afresh.
    fn merge(&mut self, other: Self) -> Merged<Self> {
        let Self {
            edit,
            before,
            after,
        } = other;
        match self.edit.merge(edit) {
            Merged::Yes => Merged::Yes,
            Merged::Annul => Merged::Annul,
            Merged::No(edit) => Merged::No(Self {
                edit,
                before,
                after,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::test_support::test_sequencer;

    use super::super::{SetTempoEdit, TempoGesture};
    use super::*;

    /// A view with the cursor at `cursor_tick` and nothing else.
    fn view_at(cursor_tick: i32) -> ViewSnapshot {
        ViewSnapshot {
            track: None,
            lead_clip: None,
            cursor_tick,
            marquee: None,
        }
    }

    /// A tempo step towards `bpm`, part of drag `drag_id`.
    fn tempo_step(sequencer: &Sequencer, bpm: i32, drag_id: u64) -> SequencerEdit {
        let gesture = Some(TempoGesture::Drag(drag_id));
        SetTempoEdit::new(sequencer, 60_000_000 / bpm, gesture)
            .unwrap()
            .into()
    }

    /// A merge keeps the first step's `before` and drops the merged one's,
    /// so a drag undoes to where it started; `after` is left for the
    /// caller to write.
    #[test]
    fn a_merge_keeps_the_first_steps_before() {
        let mut sequencer = test_sequencer();
        let mut record: Record<UndoStep> = Record::new();
        let first = tempo_step(&sequencer, 100, 1);
        record.edit(&mut sequencer, UndoStep::new(first, view_at(10)));
        record.get_entry(0).unwrap().as_ref().set_after(view_at(20));
        let second = tempo_step(&sequencer, 110, 1);
        record.edit(&mut sequencer, UndoStep::new(second, view_at(30)));

        assert_eq!(record.len(), 1);
        let step = record.get_entry(0).unwrap().as_ref();
        assert_eq!(step.before, view_at(10));
        assert_eq!(step.after.get(), Some(view_at(20)));
    }

    /// A step whose edit leaves the view alone (a tempo change) puts back
    /// no view on undo or redo, whatever it recorded.
    #[test]
    fn a_tempo_step_puts_back_no_view() {
        let sequencer = test_sequencer();
        let step = UndoStep::new(tempo_step(&sequencer, 100, 1), view_at(10));
        step.set_after(view_at(20));

        assert!(!step.sequencer_edit().restores_view());
        assert_eq!(step.view_before(), None);
        assert_eq!(step.view_after(), None);
    }
}
