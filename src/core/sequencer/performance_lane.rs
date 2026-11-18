//! `Sequencer`-side accessors for the arranger performance lane: its armed
//! state, its run and its pads.
//!
//! The lane model itself is [`PerformanceLane`] and [`PerformancePads`]; the
//! trigger routing lives in `event_handlers/performance_lane_handler.rs`. The
//! armed flag is shared with `MidiInputForwarder` (a different thread) so it
//! can suppress MIDI thru for trigger keys, and with `Transport`, which
//! doesn't wrap at the loop end while it is set — see
//! `110-performance-lane.md` and `520-performance-capture.md`.

use std::sync::atomic::Ordering;

use crate::models::{
    clip::Clip,
    performance_lane::{PAD_COUNT, PerformanceLane, PerformancePads},
};

use super::Sequencer;

impl Sequencer {
    /// Mutable access to the lane's run.
    pub(crate) fn performance_lane_mut(&mut self) -> &mut PerformanceLane {
        &mut self.performance_lane
    }

    /// Whether the lane is armed (the keyboard jumps the transport instead of
    /// playing notes).
    pub(crate) fn is_performance_lane_armed(&self) -> bool {
        self.performance_lane_armed.load(Ordering::Relaxed)
    }

    /// Arms / disarms the lane. Shared with `MidiInputForwarder` and
    /// `Transport`. Disarming ends the run: the presses before it can't be
    /// kept once the lane is left.
    pub(crate) fn set_performance_lane_armed(&mut self, armed: bool) {
        self.performance_lane_armed.store(armed, Ordering::Relaxed);
        if !armed {
            self.performance_lane.clear();
        }
    }

    /// The lane's pads.
    pub(crate) fn performance_pads(&self) -> &PerformancePads {
        &self.performance_pads
    }

    /// Puts `clip` on pad `pad` (empties it for `None`):
    /// `SetPerformancePadEdit`'s one write.
    pub(crate) fn set_performance_pad(&mut self, pad: usize, clip: Option<Clip>) {
        self.performance_pads.set(pad, clip);
    }

    /// Mutable access to the lane's pads, for a note edit on a performance
    /// (`Sequencer::clip_on_mut` with `ClipHome::Lane`).
    pub(crate) fn performance_pads_mut(&mut self) -> &mut PerformancePads {
        &mut self.performance_pads
    }

    /// The performance the clip view shows and edits while the lane is
    /// selected: the selected pad's (`520-performance-capture.md`
    /// decisions 2 and 6). `None` unarmed, or on an empty pad.
    pub(crate) fn shown_performance(&self) -> Option<&Clip> {
        if !self.is_performance_lane_armed() {
            return None;
        }
        self.performance_pads.get(self.selected_pad)
    }

    /// Mutable [`shown_performance`](Self::shown_performance).
    pub(crate) fn shown_performance_mut(&mut self) -> Option<&mut Clip> {
        if !self.is_performance_lane_armed() {
            return None;
        }
        self.performance_pads.get_mut(self.selected_pad)
    }

    /// The selected pad (`520-performance-capture.md` decision 6).
    pub(crate) fn selected_pad(&self) -> usize {
        self.selected_pad
    }

    /// Selects pad `pad`; nothing past the last.
    pub(crate) fn select_pad(&mut self, pad: usize) {
        if pad < PAD_COUNT {
            self.selected_pad = pad;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::ClipHome;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::models::performance_lane::performance_clip;

    use super::*;

    /// A sequencer with a performance on pad `pad` (C3 then E3, a beat each);
    /// returns it and the performance's id.
    fn with_performance(pad: usize) -> (Sequencer, uuid::Uuid) {
        let mut sequencer = test_sequencer();
        let clip = performance_clip(&[(0, 60, 100), (960, 64, 100)], 1920).unwrap();
        let id = clip.id();
        sequencer.set_performance_pad(pad, Some(clip));
        (sequencer, id)
    }

    #[test]
    fn a_lane_clip_is_found_by_its_id_on_whichever_pad_holds_it() {
        let (mut sequencer, id) = with_performance(3);
        assert_eq!(
            sequencer.clip_on(ClipHome::Lane, id).map(Clip::id),
            Some(id)
        );
        assert!(sequencer.clip_on_mut(ClipHome::Lane, id).is_some());
        assert!(sequencer.clip_on(0, id).is_none(), "not on a track");
        assert!(
            sequencer
                .clip_on(ClipHome::Lane, uuid::Uuid::new_v4())
                .is_none()
        );
    }

    #[test]
    fn the_open_clip_is_the_selected_pads_performance_while_the_lane_is_armed() {
        let (mut sequencer, id) = with_performance(2);
        let clip = clip_at(0, 1920);
        let track_clip = clip.id();
        let track_id = sequencer.tracks()[0].id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(Some(track_clip));
        assert_eq!(sequencer.selected_clip().map(Clip::id), Some(track_clip));

        sequencer.set_performance_lane_armed(true);
        sequencer.select_pad(2);
        assert_eq!(sequencer.selected_clip().map(Clip::id), Some(id));
        assert_eq!(sequencer.selected_clip_home(), Some((ClipHome::Lane, id)));

        sequencer.select_pad(0);
        assert!(sequencer.selected_clip().is_none(), "an empty pad");
        assert_eq!(sequencer.selected_clip_home(), None);
    }

    #[test]
    fn the_arranger_cursor_leaves_a_performances_cursor_alone() {
        let (mut sequencer, _) = with_performance(0);
        sequencer.set_performance_lane_armed(true);
        sequencer.set_selected_clip_cursor_tick(960);
        sequencer.set_cursor_tick(1440);
        sequencer.sync_selected_clip_cursor_with_arranger();
        assert_eq!(sequencer.selected_clip().unwrap().cursor_tick(), 960);
    }
}
