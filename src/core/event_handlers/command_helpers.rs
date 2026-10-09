//! Small `EventHandlers` helpers that just fan a command out — releasing
//! notes, recording an edit. Named so the workflow files read as intent rather than
//! channel plumbing.

use undo::Record;

use crate::core::sequencer::SequencerEdit;

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
    /// every undoable command shares. Takes an `Option` like `send_sequencer`,
    /// since every edit constructor returns `None` when there is nothing to do,
    /// and any edit type (each converts `Into` its `SequencerEdit` variant).
    /// Returns whether an edit was recorded.
    pub(super) fn record_edit(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        edit: Option<impl Into<SequencerEdit>>,
    ) -> bool {
        let Some(edit) = edit else {
            return false;
        };
        let result = undo_record.edit(sequencer, edit.into());
        self.handle_edit_result(sequencer, result);
        true
    }
}
