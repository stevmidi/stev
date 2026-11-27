//! [`SetMarkerEdit`]: a change to the arrangement's markers as one undo step
//! (`600-markers.md` decision 10). It replaces the whole list, before and
//! after: at nine entries there is nothing to gain from anything finer.

use crate::models::markers::{MarkerChange, Markers};

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
        let tick = sequencer.cursor_tick();
        Self::changing(sequencer, |markers| markers.set(number, tick))
    }

    /// `change` to marker `number`, from the marker strip: a drag's drop, a
    /// rename, a `Delete` (decisions 7 and 8). `None` when that changes
    /// nothing (dropped where it was, the same name, no such marker).
    pub(crate) fn changed(
        sequencer: &Sequencer,
        number: u8,
        change: &MarkerChange,
    ) -> Option<Self> {
        Self::changing(sequencer, |markers| markers.change(number, change))
    }

    /// The edit `apply` makes to the markers, `None` when it makes none.
    fn changing(sequencer: &Sequencer, apply: impl FnOnce(&mut Markers)) -> Option<Self> {
        let before = sequencer.markers().clone();
        let mut after = before.clone();
        apply(&mut after);
        (before != after).then_some(SetMarkerEdit { before, after })
    }

    /// Puts the new list in.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        *sequencer.markers_mut() = self.after.clone();
        EditResult::MarkersChanged
    }

    /// Puts the old one back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        *sequencer.markers_mut() = self.before.clone();
        EditResult::MarkersChanged
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::SequencerEdit;
    use crate::core::sequencer::test_support::test_sequencer;

    use super::*;

    #[test]
    fn a_marker_set_at_the_cursor_is_one_undo_step() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        sequencer.set_cursor_tick(1920);

        let edit = SetMarkerEdit::at_cursor(&sequencer, 2).unwrap();
        record.edit(&mut sequencer, edit.into());
        assert_eq!(sequencer.markers().placed(), vec![(2, 1920)]);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.markers().placed(), vec![]);
        record.redo(&mut sequencer);
        assert_eq!(sequencer.markers().placed(), vec![(2, 1920)]);
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
        assert_eq!(sequencer.markers().placed(), vec![(1, 3840)]);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.markers().placed(), vec![(1, 0)]);
    }

    #[test]
    fn a_drag_rename_and_delete_each_undo_back() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        sequencer.markers_mut().set(1, 0);
        sequencer.markers_mut().set(2, 960);
        let start = sequencer.markers().clone();
        for change in [
            MarkerChange::Move(960),
            MarkerChange::Rename(Some("Chorus".to_owned())),
            MarkerChange::Remove,
        ] {
            let edit = SetMarkerEdit::changed(&sequencer, 1, &change).unwrap();
            record.edit(&mut sequencer, edit.into());
            assert_ne!(*sequencer.markers(), start);
            record.undo(&mut sequencer);
            assert_eq!(*sequencer.markers(), start);
        }
    }

    #[test]
    fn a_change_that_changes_nothing_makes_no_step() {
        let mut sequencer = test_sequencer();
        sequencer.markers_mut().set(1, 960);
        assert!(SetMarkerEdit::changed(&sequencer, 1, &MarkerChange::Move(960)).is_none());
        assert!(SetMarkerEdit::changed(&sequencer, 1, &MarkerChange::Rename(None)).is_none());
        assert!(SetMarkerEdit::changed(&sequencer, 4, &MarkerChange::Remove).is_none());
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
