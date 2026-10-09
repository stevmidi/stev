//! [`SetTempoEdit`]: the project tempo as one undo step — typed into the
//! header's BPM field, dragged on its chip, or tapped (`T`). A drag or a tap
//! burst sends a step per change; the steps of one gesture merge into a single
//! undo step (`SequencerEdit::merge`).

use crate::core::time::clamp_tempo_us;

use super::{EditResult, Sequencer};

/// Which gesture a tempo step belongs to: consecutive steps of the same
/// gesture merge into one undo step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TempoGesture {
    /// A drag on the header's BPM chip, by the view's drag id.
    Drag(u64),
    /// A run of `T` taps, by its burst number (`TapTempo`).
    Taps(u64),
}

/// Sets the project tempo.
pub(crate) struct SetTempoEdit {
    /// The tempo before, µs per quarter.
    before: i32,
    /// The tempo after, µs per quarter.
    after: i32,
    /// The gesture this step belongs to; `None` (a typed value) never merges.
    gesture: Option<TempoGesture>,
}

impl SetTempoEdit {
    /// Sets the tempo to `tempo_us`, clamped to the app's range. `None` when
    /// that is the tempo already — no undo step for a change that changes
    /// nothing.
    pub(crate) fn new(
        sequencer: &Sequencer,
        tempo_us: i32,
        gesture: Option<TempoGesture>,
    ) -> Option<Self> {
        let before = sequencer.tempo_us();
        let after = clamp_tempo_us(tempo_us);
        (before != after).then_some(SetTempoEdit {
            before,
            after,
            gesture,
        })
    }

    /// Sets the new tempo.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_tempo(self.after);
        EditResult::TempoChanged
    }

    /// Puts the old one back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_tempo(self.before);
        EditResult::TempoChanged
    }

    /// Whether `other` is a later step of this edit's gesture.
    pub(crate) fn same_gesture(&self, other: &Self) -> bool {
        self.gesture.is_some() && self.gesture == other.gesture
    }

    /// Takes in `later`, a step of the same gesture. Steps are absolute, so
    /// the merged edit ends on `later`'s tempo.
    pub(crate) fn absorb(&mut self, later: &Self) {
        self.after = later.after;
    }

    /// Whether `later` puts the tempo back where this edit found it — the
    /// gesture then leaves no undo step.
    pub(crate) fn is_reverted_by(&self, later: &Self) -> bool {
        later.after == self.before
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::test_support::test_sequencer;
    use crate::core::sequencer::{Sequencer, SequencerEdit};
    use crate::core::time::{TEMPO_BPM_MAX, bpm_to_tempo_us};

    use super::{SetTempoEdit, TempoGesture};

    /// Records a tempo step to `tempo_us` in `gesture`, if it changes anything.
    fn set(
        record: &mut Record<SequencerEdit>,
        sequencer: &mut Sequencer,
        tempo_us: i32,
        gesture: Option<TempoGesture>,
    ) {
        if let Some(edit) = SetTempoEdit::new(sequencer, tempo_us, gesture) {
            record.edit(sequencer, edit.into());
        }
    }

    #[test]
    fn typed_tempo_is_one_undo_step() {
        let mut sequencer = test_sequencer();
        let start = sequencer.tempo_us();
        let mut record = Record::new();

        set(&mut record, &mut sequencer, 400_000, None);
        assert_eq!(sequencer.tempo_us(), 400_000);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.tempo_us(), start);
        record.redo(&mut sequencer);
        assert_eq!(sequencer.tempo_us(), 400_000);
    }

    #[test]
    fn unchanged_tempo_makes_no_step() {
        let sequencer = test_sequencer();
        assert!(SetTempoEdit::new(&sequencer, sequencer.tempo_us(), None).is_none());
    }

    #[test]
    fn tempo_is_clamped_to_the_app_range() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();
        set(&mut record, &mut sequencer, 1, None);
        assert_eq!(sequencer.tempo_us(), bpm_to_tempo_us(TEMPO_BPM_MAX));
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut sequencer = test_sequencer();
        let start = sequencer.tempo_us();
        let mut record = Record::new();

        for tempo_us in [490_000, 480_000, 470_000] {
            set(
                &mut record,
                &mut sequencer,
                tempo_us,
                Some(TempoGesture::Drag(3)),
            );
        }
        assert_eq!(record.len(), 1);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.tempo_us(), start);
        record.redo(&mut sequencer);
        assert_eq!(sequencer.tempo_us(), 470_000);
    }

    /// A drag back to where it started (or Esc) leaves no undo step.
    #[test]
    fn a_drag_back_to_its_start_leaves_no_step() {
        let mut sequencer = test_sequencer();
        let start = sequencer.tempo_us();
        let mut record = Record::new();

        set(
            &mut record,
            &mut sequencer,
            490_000,
            Some(TempoGesture::Drag(1)),
        );
        set(
            &mut record,
            &mut sequencer,
            start,
            Some(TempoGesture::Drag(1)),
        );
        assert_eq!(record.len(), 0);
        assert_eq!(sequencer.tempo_us(), start);
    }

    #[test]
    fn separate_gestures_and_typed_values_do_not_merge() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();

        set(
            &mut record,
            &mut sequencer,
            490_000,
            Some(TempoGesture::Drag(1)),
        );
        set(
            &mut record,
            &mut sequencer,
            480_000,
            Some(TempoGesture::Drag(2)),
        );
        set(
            &mut record,
            &mut sequencer,
            470_000,
            Some(TempoGesture::Taps(1)),
        );
        set(
            &mut record,
            &mut sequencer,
            460_000,
            Some(TempoGesture::Taps(2)),
        );
        set(&mut record, &mut sequencer, 450_000, None);
        set(&mut record, &mut sequencer, 440_000, None);
        assert_eq!(record.len(), 6);
    }

    /// A drag id and a tap burst number never mistake each other.
    #[test]
    fn a_drag_and_a_tap_burst_with_the_same_number_do_not_merge() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();

        set(
            &mut record,
            &mut sequencer,
            490_000,
            Some(TempoGesture::Drag(0)),
        );
        set(
            &mut record,
            &mut sequencer,
            480_000,
            Some(TempoGesture::Taps(0)),
        );
        assert_eq!(record.len(), 2);
    }
}
