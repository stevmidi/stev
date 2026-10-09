//! [`SetMeterEdit`]: the project's time signature as one undo step, typed
//! into the header's meter field. A plain value swap: notes, clips and the
//! loop region keep their ticks (`archive/270-time-signature.md`).

use crate::core::time::Meter;

use super::{EditResult, Sequencer};

/// Sets the project's meter.
pub(crate) struct SetMeterEdit {
    /// The meter before.
    before: Meter,
    /// The meter after.
    after: Meter,
}

impl SetMeterEdit {
    /// Sets the meter to `meter`. `None` when that is the meter already — no
    /// undo step for a change that changes nothing.
    pub(crate) fn new(sequencer: &Sequencer, meter: Meter) -> Option<Self> {
        let before = sequencer.meter();
        (before != meter).then_some(SetMeterEdit {
            before,
            after: meter,
        })
    }

    /// Sets the new meter.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_meter(self.after);
        EditResult::MeterChanged
    }

    /// Puts the old one back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_meter(self.before);
        EditResult::MeterChanged
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::SequencerEdit;
    use crate::core::sequencer::test_support::test_sequencer;
    use crate::core::time::Meter;
    use crate::models::clip::Clip;

    use super::SetMeterEdit;

    #[test]
    fn a_typed_meter_is_one_undo_step() {
        let mut sequencer = test_sequencer();
        let seven_eight = Meter::new(7, 8).unwrap();
        let mut record: Record<SequencerEdit> = Record::new();

        let edit = SetMeterEdit::new(&sequencer, seven_eight).unwrap();
        record.edit(&mut sequencer, edit.into());
        assert_eq!(sequencer.meter(), seven_eight);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.meter(), Meter::FOUR_FOUR);
        record.redo(&mut sequencer);
        assert_eq!(sequencer.meter(), seven_eight);
    }

    #[test]
    fn unchanged_meter_makes_no_step() {
        let sequencer = test_sequencer();
        assert!(SetMeterEdit::new(&sequencer, Meter::FOUR_FOUR).is_none());
    }

    /// A meter change moves bar lines, never content: clips and the loop
    /// region keep their ticks, off the new bar lines or not.
    #[test]
    fn a_meter_change_keeps_clips_and_the_loop_region() {
        let mut sequencer = test_sequencer();
        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(3840), Some(7680));
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.set_global_region(0, 7680);
        let mut record: Record<SequencerEdit> = Record::new();

        let edit = SetMeterEdit::new(&sequencer, Meter::new(3, 4).unwrap()).unwrap();
        record.edit(&mut sequencer, edit.into());
        let clip = sequencer.clip_on(0, clip.id()).unwrap();
        assert_eq!((clip.region().start(), clip.region().end()), (3840, 7680));
        assert_eq!(
            (sequencer.region_start(), sequencer.region_end()),
            (0, 7680)
        );
    }
}
