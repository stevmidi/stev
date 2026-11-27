//! [`SetMarkerEdit`]: a change to the arrangement's markers as one undo step
//! (`600-markers.md` decision 10). It replaces the whole list, before and
//! after: at nine entries there is nothing to gain from anything finer.

use crate::models::markers::Markers;

use super::{EditResult, Sequencer};

/// Replaces the marker list.
pub(crate) struct SetMarkerEdit {
    /// The markers before.
    before: Markers,
    /// The markers after.
    after: Markers,
}

impl SetMarkerEdit {
    /// Marker `number` set at the arranger cursor (`⌘1`–`⌘9`, decision 3):
    /// moved there if it is set elsewhere, replacing any marker already on
    /// that tick. `None` when that changes nothing (it is already there, or
    /// `number` names no slot): no undo step for it.
    pub(crate) fn at_cursor(sequencer: &Sequencer, number: u8) -> Option<Self> {
        let mut after = sequencer.markers().clone();
        after.set(number, sequencer.cursor_tick());
        Self::to(sequencer, after)
    }

    /// The list replaced by `after`; `None` when it is the list already.
    fn to(sequencer: &Sequencer, after: Markers) -> Option<Self> {
        let before = sequencer.markers().clone();
        (before != after).then_some(SetMarkerEdit { before, after })
    }

    /// Puts the new list in.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_markers(self.after.clone());
        EditResult::MarkersChanged
    }

    /// Puts the old one back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_markers(self.before.clone());
        EditResult::MarkersChanged
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::SequencerEdit;
    use crate::core::sequencer::test_support::test_sequencer;

    use super::*;

    /// The `(number, tick)` of each marker, in order.
    fn placed(sequencer: &Sequencer) -> Vec<(u8, i32)> {
        sequencer
            .markers()
            .as_slice()
            .iter()
            .map(|marker| (marker.number, marker.tick))
            .collect()
    }

    #[test]
    fn a_marker_set_at_the_cursor_is_one_undo_step() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        sequencer.set_cursor_tick(1920);

        let edit = SetMarkerEdit::at_cursor(&sequencer, 2).unwrap();
        record.edit(&mut sequencer, edit.into());
        assert_eq!(placed(&sequencer), vec![(2, 1920)]);
        record.undo(&mut sequencer);
        assert_eq!(placed(&sequencer), vec![]);
        record.redo(&mut sequencer);
        assert_eq!(placed(&sequencer), vec![(2, 1920)]);
    }

    #[test]
    fn moving_a_marker_undoes_back_to_where_it_was() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        for tick in [0, 3840] {
            sequencer.set_cursor_tick(tick);
            let edit = SetMarkerEdit::at_cursor(&sequencer, 1).unwrap();
            record.edit(&mut sequencer, edit.into());
        }
        assert_eq!(placed(&sequencer), vec![(1, 3840)]);
        record.undo(&mut sequencer);
        assert_eq!(placed(&sequencer), vec![(1, 0)]);
    }

    #[test]
    fn a_marker_already_at_the_cursor_makes_no_step() {
        let mut sequencer = test_sequencer();
        sequencer.set_cursor_tick(960);
        let mut edit = SetMarkerEdit::at_cursor(&sequencer, 3).unwrap();
        edit.edit(&mut sequencer);
        assert!(SetMarkerEdit::at_cursor(&sequencer, 3).is_none());
    }
}
