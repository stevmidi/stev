//! Small `EventHandlers` helpers that just fan a command out — releasing
//! notes, recording an edit. Named so the workflow files read as intent rather than
//! channel plumbing.

use undo::Record;

use crate::core::sequencer::{SequencerEdit, UndoStep, ViewSnapshot};

use super::*;

impl EventHandlers {
    /// Releases every sounding note on both output paths: the `NoteLogger`'s
    /// MIDI-out notes and the hosted instruments' held notes.
    pub(super) fn release_all_notes(&self, sequencer: &mut Sequencer) {
        self.note_logger_command_tx
            .send(NoteLoggerCommand::ReleaseNotes)
            .ok();
        sequencer.release_instrument_notes();
    }

    /// Records `edit` on `undo_record` and fans out its result — the shape
    /// every undoable command shares, the agent bridge's writes included.
    /// Takes an `Option` like `send_sequencer`, since every edit constructor
    /// returns `None` when there is nothing to do, and any edit type (each
    /// converts `Into` its `SequencerEdit` variant). Returns whether an edit
    /// was recorded.
    ///
    /// The step keeps the view just before the edit and, once its result
    /// has run, just after it (`UndoStep`), for an undo and a redo to put
    /// back. A merge into the step on top keeps that step's `before` and
    /// takes this `after`; a merge that annuls the step on top
    /// (`Merged::Annul`) leaves the record shorter, and nothing is written.
    pub(super) fn record_edit(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<UndoStep>,
        edit: Option<impl Into<SequencerEdit>>,
    ) -> bool {
        let Some(edit) = edit else {
            return false;
        };
        let head = undo_record.head();
        let step = UndoStep::new(edit.into(), ViewSnapshot::of(sequencer));
        let result = undo_record.edit(sequencer, step);
        self.handle_edit_result(sequencer, result);
        if undo_record.head() >= head
            && let Some(entry) = undo_record
                .head()
                .checked_sub(1)
                .and_then(|top| undo_record.get_entry(top))
        {
            entry.as_ref().set_after(ViewSnapshot::of(sequencer));
        }
        true
    }
}
