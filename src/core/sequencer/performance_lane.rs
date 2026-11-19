//! `Sequencer`-side accessors for the arranger performance lane: its armed
//! state, its run, its replay and its pads.
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
    performance_lane::{
        PAD_COUNT, PerformanceLane, PerformancePads, ReplayCue, ReplayPosition,
        key_index_from_note, key_tick,
    },
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
    /// `Transport`. Disarming ends the run, the presses before it can't be
    /// kept once the lane is left, and the replay: playback runs on in the
    /// arrangement.
    pub(crate) fn set_performance_lane_armed(&mut self, armed: bool) {
        self.performance_lane_armed.store(armed, Ordering::Relaxed);
        if !armed {
            self.performance_lane.clear();
            self.publish_performance_replay();
        }
    }

    /// `⌥Space` on the lane: starts replaying the selected pad's
    /// performance from the clip view's cursor in it, its start when the
    /// cursor is outside it (`520-performance-capture.md` decision 4).
    /// `false`, and nothing starts, on an empty pad or with the lane not
    /// selected.
    pub(crate) fn start_performance_replay(&mut self) -> bool {
        if !self.is_performance_lane_armed() {
            return false;
        }
        let pad = self.selected_pad;
        let Some(clip) = self.performance_pads.get(pad) else {
            return false;
        };
        let (start, end) = (clip.region().start(), clip.region().end());
        let cursor = clip.cursor_tick();
        let from = if (start..end).contains(&cursor) {
            cursor - start
        } else {
            0
        };
        self.performance_lane.start_replay(pad, from);
        self.publish_performance_replay();
        true
    }

    /// Ends the replay: playback stopped, or a live press took over.
    pub(crate) fn stop_performance_replay(&mut self) {
        self.performance_lane.stop_replay();
        self.publish_performance_replay();
    }

    /// The replay's cue for the tick about to play from `playback_tick`
    /// (`PerformanceLane::step_replay`, on its pad's performance as it is
    /// now, the keys at the lane's key length). `None` with no replay.
    pub(crate) fn step_performance_replay(&mut self, playback_tick: i32) -> Option<ReplayCue> {
        let pad = self.performance_lane.replay_position()?.pad;
        let target = self.performance_key_target();
        let cue = self.performance_lane.step_replay(
            playback_tick,
            self.performance_pads.get(pad),
            target,
        );
        self.publish_performance_replay();
        cue
    }

    /// Where a performance's note jumps to: its lane key's start at the
    /// lane's key length (`key_tick`), `None` below the keys. What the
    /// replay and the render both play by.
    pub(super) fn performance_key_target(&self) -> impl Fn(u8) -> Option<i32> + use<> {
        let (meter, length) = (self.meter(), self.performance_lane_key_length());
        move |note| key_index_from_note(note).map(|key| key_tick(meter, length, key))
    }

    /// Whether the replay is in a gap: nothing sounds (decision 2), so
    /// `Sequencer::tick` plays every track as muted.
    pub(crate) fn is_performance_replay_silent(&self) -> bool {
        self.performance_lane.is_replay_silent()
    }

    /// Writes where the replay is to `SharedAtomics::performance_replay`.
    fn publish_performance_replay(&self) {
        let bits = ReplayPosition::to_bits(self.performance_lane.replay_position());
        self.performance_replay.store(bits, Ordering::Relaxed);
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
    use uuid::Uuid;

    use crate::core::sequencer::ClipHome;
    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::models::performance_lane::performance_clip;

    use super::*;

    /// A sequencer with a performance on pad `pad` (C3 then E3, a beat each);
    /// returns it and the performance's id.
    fn with_performance(pad: usize) -> (Sequencer, Uuid) {
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
        assert!(sequencer.clip_on(ClipHome::Lane, Uuid::new_v4()).is_none());
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
