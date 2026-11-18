//! [`SetPerformancePadEdit`]: what one of the performance lane's pads holds
//! (`520-performance-capture.md` decision 6). A capture onto a pad (`/` on the
//! armed lane, replacing what it held) and `Delete` on the selected pad are
//! this edit, one undo step each.

use crate::models::{clip::Clip, performance_lane::PAD_COUNT};

use super::{EditResult, Sequencer};

/// Pad `pad` goes from holding `from` to holding `to` (`None` is empty).
pub(crate) struct SetPerformancePadEdit {
    /// Which pad.
    pad: usize,
    /// What it held before the edit.
    from: Option<Clip>,
    /// What it holds after.
    to: Option<Clip>,
}

impl SetPerformancePadEdit {
    /// Pad `pad` holds `to` instead of what it holds now. `None` when there
    /// is no such pad, or it is empty and stays so: no undo step for a
    /// change that changes nothing.
    pub(crate) fn new(sequencer: &Sequencer, pad: usize, to: Option<Clip>) -> Option<Self> {
        if pad >= PAD_COUNT {
            return None;
        }
        let from = sequencer.performance_pads().get(pad).cloned();
        (from.is_some() || to.is_some()).then_some(SetPerformancePadEdit { pad, from, to })
    }

    /// Puts `to` on the pad.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_performance_pad(self.pad, self.to.clone());
        EditResult::PerformancePadChanged
    }

    /// Puts `from` back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_performance_pad(self.pad, self.from.clone());
        EditResult::PerformancePadChanged
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::test_support::test_sequencer;
    use crate::core::sequencer::{SequencerEdit, UndoStep, ViewSnapshot};
    use crate::models::performance_lane::performance_clip;

    use super::*;

    fn step(sequencer: &Sequencer, edit: SetPerformancePadEdit) -> UndoStep {
        UndoStep::new(SequencerEdit::from(edit), ViewSnapshot::of(sequencer))
    }

    #[test]
    fn a_capture_replaces_what_the_pad_held_and_undo_brings_it_back() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();
        let first = performance_clip(&[(0, 60, 100)], 960).unwrap();
        let second = performance_clip(&[(0, 64, 100)], 1920).unwrap();
        let (first_id, second_id) = (first.id(), second.id());

        for clip in [first, second] {
            let edit = SetPerformancePadEdit::new(&sequencer, 3, Some(clip)).unwrap();
            let step = step(&sequencer, edit);
            record.edit(&mut sequencer, step);
        }
        let held = |sequencer: &Sequencer| sequencer.performance_pads().get(3).map(Clip::id);
        assert_eq!(held(&sequencer), Some(second_id));

        record.undo(&mut sequencer);
        assert_eq!(held(&sequencer), Some(first_id));
        record.undo(&mut sequencer);
        assert_eq!(held(&sequencer), None);
        record.redo(&mut sequencer);
        assert_eq!(held(&sequencer), Some(first_id));
    }

    #[test]
    fn emptying_an_empty_pad_or_a_pad_past_the_last_is_no_edit() {
        let sequencer = test_sequencer();
        assert!(SetPerformancePadEdit::new(&sequencer, 0, None).is_none());
        let clip = performance_clip(&[(0, 60, 100)], 960).unwrap();
        assert!(SetPerformancePadEdit::new(&sequencer, PAD_COUNT, Some(clip)).is_none());
    }
}
