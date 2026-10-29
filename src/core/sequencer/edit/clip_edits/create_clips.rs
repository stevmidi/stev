//! The agent bridge's `create_clips` (`360-mcp-server.md` § Writing): new
//! clips with notes on one or more tracks, as one undo step. A
//! [`PasteClipsEdit`] of ready-built clips that leaves the selection alone
//! (`PasteLead::KeepSelection`, decision 15), plus an optional move of the
//! loop region's end (`extend_loop`), undone with the clips.

use crate::models::clip::Clip;

use super::super::super::Sequencer;
use super::super::{EditResult, PasteLead};
use super::paste::PasteClipsEdit;

/// New clips from the agent bridge: a paste of `targets` that carves what
/// they land on (the caller has refused a collision unless
/// `replace_existing`), then, with `extend_loop`, the loop region's end moved
/// out to the last clip's end. The one other edit, beside
/// `DuplicateTimeEdit`, that moves the loop region: the tool's argument is
/// the user's explicit intent (`050` § Invariants).
pub(crate) struct CreateClipsEdit {
    /// The paste of the new clips.
    paste: PasteClipsEdit,
    /// The loop region `(start, end)` before and after, when it moves.
    region: Option<((i32, i32), (i32, i32))>,
}

impl CreateClipsEdit {
    /// Puts each `(track index, clip)` on its track as it is (absolute start,
    /// its own id), the selection kept. With `extend_loop`, a loop region
    /// ending before the last clip ends is stretched to it. `None` when no
    /// target is on a track.
    pub(crate) fn new(
        sequencer: &Sequencer,
        targets: Vec<(usize, Clip)>,
        extend_loop: bool,
    ) -> Option<Self> {
        let last_end = targets.iter().map(|(_, clip)| clip.end_tick()).max();
        let region = (sequencer.region_start(), sequencer.region_end());
        let region = last_end
            .filter(|&end| extend_loop && region.1 > region.0 && end > region.1)
            .map(|end| (region, (region.0, end)));
        Some(Self {
            paste: PasteClipsEdit::from_targets(sequencer, targets, PasteLead::KeepSelection)?,
            region,
        })
    }

    /// The loop region this edit leaves, when it moves it.
    pub(crate) fn new_region(&self) -> Option<(i32, i32)> {
        self.region.map(|(_, after)| after)
    }

    /// Pastes the clips, then moves the region. Returns the paste's
    /// [`EditResult::ClipsPasted`].
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        let result = self.paste.edit(sequencer);
        if let Some((_, (start, end))) = self.region {
            sequencer.set_global_region(start, end);
        }
        result
    }

    /// Puts the region back, then un-pastes the clips. Returns the paste's
    /// [`EditResult::ClipsUnpasted`].
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        if let Some(((start, end), _)) = self.region {
            sequencer.set_global_region(start, end);
        }
        self.paste.undo(sequencer)
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::sequencer::SequencerEdit;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::core::time::Meter;

    use super::*;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    fn spans(sequencer: &Sequencer, track_idx: usize) -> Vec<(i32, i32)> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|c| (c.start_tick(), c.end_tick()))
            .collect()
    }

    #[test]
    fn several_clips_are_one_undo_step() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        let targets = vec![(0, clip_at(BAR * 4, BAR)), (1, clip_at(BAR * 4, BAR * 2))];
        let edit = CreateClipsEdit::new(&sequencer, targets, false).unwrap();
        record.edit(&mut sequencer, edit.into());
        assert_eq!(spans(&sequencer, 0), vec![(BAR * 4, BAR * 5)]);
        assert_eq!(spans(&sequencer, 1), vec![(BAR * 4, BAR * 6)]);

        record.undo(&mut sequencer);
        assert!(spans(&sequencer, 0).is_empty());
        assert!(spans(&sequencer, 1).is_empty());

        record.redo(&mut sequencer);
        assert_eq!(spans(&sequencer, 1), vec![(BAR * 4, BAR * 6)]);
    }

    #[test]
    fn writes_never_merge_into_one_step() {
        let mut sequencer = test_sequencer();
        let mut record: Record<SequencerEdit> = Record::new();
        for bar in [0, 1] {
            let targets = vec![(0, clip_at(BAR * bar, BAR))];
            let edit = CreateClipsEdit::new(&sequencer, targets, false).unwrap();
            record.edit(&mut sequencer, edit.into());
        }
        assert_eq!(record.len(), 2);
    }

    #[test]
    fn extend_loop_moves_the_region_end_and_undo_puts_it_back() {
        let mut sequencer = test_sequencer();
        sequencer.set_global_region(0, BAR * 2);
        let targets = vec![(0, clip_at(BAR * 2, BAR * 2))];
        let mut edit = CreateClipsEdit::new(&sequencer, targets, true).unwrap();
        assert_eq!(edit.new_region(), Some((0, BAR * 4)));

        edit.edit(&mut sequencer);
        assert_eq!(
            (sequencer.region_start(), sequencer.region_end()),
            (0, BAR * 4)
        );
        edit.undo(&mut sequencer);
        assert_eq!(
            (sequencer.region_start(), sequencer.region_end()),
            (0, BAR * 2)
        );
    }

    #[test]
    fn the_region_stays_without_extend_loop_or_when_it_already_covers() {
        let mut sequencer = test_sequencer();
        sequencer.set_global_region(0, BAR * 2);
        let late = || vec![(0, clip_at(BAR * 2, BAR))];
        assert_eq!(
            CreateClipsEdit::new(&sequencer, late(), false)
                .unwrap()
                .new_region(),
            None
        );
        let inside = vec![(0, clip_at(0, BAR))];
        assert_eq!(
            CreateClipsEdit::new(&sequencer, inside, true)
                .unwrap()
                .new_region(),
            None
        );
        // No loop region at all: nothing to extend.
        sequencer.set_global_region(0, 0);
        assert_eq!(
            CreateClipsEdit::new(&sequencer, late(), true)
                .unwrap()
                .new_region(),
            None
        );
    }
}
