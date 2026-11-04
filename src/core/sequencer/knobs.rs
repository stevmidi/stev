//! The knob panel's sequencer side (`400-parameter-knobs.md`): what each
//! track's knobs drive, set by a project load, and the hand turning them.
//! Turning a knob is not undoable, like volume and pan; assigning one is
//! (`SetKnobParamEdit`). The parameter itself is set by the view, straight on
//! the plugin; here a turn counts for the unsaved prompt, holds off playback
//! and goes into the capture buffer, where `/` keeps it.

use crate::models::clip::Clip;
use crate::models::event::{Event, ParamMove};
#[cfg(test)]
use crate::models::knobs::KnobParams;
use crate::models::knobs::{UNASSIGNED_KNOBS, knob_holding};

use super::Sequencer;

impl Sequencer {
    /// Gives the track at `track_idx` `knobs`. Out-of-range indices are
    /// ignored.
    #[cfg(test)]
    pub(crate) fn set_track_knobs(&mut self, track_idx: usize, knobs: KnobParams) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.set_knobs(knobs);
        }
    }

    /// The hand took hold of knob `knob` of the track at `track_idx`, which
    /// stood at `start` (normalized, if known): a drag in the knob panel, or
    /// a turn beginning in the plugin's editor. Playback lets go of its
    /// parameter until [`release_knob`](Self::release_knob) (decision 6),
    /// and the turn's start value goes ahead of its first move, stamped
    /// `tick`, for a landing to hold the stretch before the turn at
    /// (decision 10). Out-of-range indices are ignored.
    pub(crate) fn hold_knob(
        &mut self,
        track_idx: usize,
        knob: usize,
        start: Option<f64>,
        tick: i32,
    ) {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return;
        };
        track.hold_knob(knob);
        if let Some(start) = start {
            self.capture_knob_move(track_idx, knob, start, tick);
        }
    }

    /// The hand let go of knob `knob` of the track at `track_idx`.
    /// Out-of-range indices are ignored.
    pub(crate) fn release_knob(&mut self, track_idx: usize, knob: usize) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.release_knob(knob);
        }
    }

    /// The hand turned knob `knob` of the track at `track_idx` to `value`
    /// (normalized, clamped to `0.0..=1.0`) at `tick`, its `clock_tick` — a
    /// drag in the knob panel, or the parameter's control turned in the
    /// plugin's editor. Counts toward the unsaved prompt
    /// (`400-parameter-knobs.md` decision 9), and a held knob's turn goes
    /// into the capture buffer (decision 10): a move outside a turn is the
    /// plugin's own doing, a preset it loaded. Out-of-range indices are
    /// ignored.
    pub(crate) fn turn_knob(&mut self, track_idx: usize, knob: usize, value: f64, tick: i32) {
        let value = value.clamp(0.0, 1.0);
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return;
        };
        track.set_knob_by_hand(knob, value);
        if track.is_knob_held(knob) {
            self.capture_knob_move(track_idx, knob, value, tick);
        }
    }

    /// The plugin on the track at `track_idx` loaded a preset. Out-of-range
    /// indices are ignored.
    pub(crate) fn count_plugin_change(&mut self, track_idx: usize) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.count_plugin_change();
        }
    }

    /// Puts knob `knob`'s parameter at `value` in the capture buffer, at
    /// `tick`. The buffer is the selected track's, as the live keyboard is:
    /// another track's turn is left out, its parameter meaningless to the
    /// clip a commit lands in.
    fn capture_knob_move(&mut self, track_idx: usize, knob: usize, value: f64, tick: i32) {
        if self.selected_track_index() != Some(track_idx) {
            return;
        }
        let Some(&Some(param)) = self.tracks[track_idx].knobs().get(knob) else {
            return;
        };
        self.mark_capture_clock(tick);
        let param_move = ParamMove {
            param,
            value: value.clamp(0.0, 1.0),
        };
        // A turn is stamped when it happened but reaches the buffer a frame
        // later, maybe behind a note played since.
        self.capture_clip
            .add_late_event(Event::from_param_move(tick, param_move));
    }

    /// The capture buffer as a running take reads it: a copy, holding only
    /// what [`keep_knob_moves`](Self::keep_knob_moves) keeps.
    pub(super) fn kept_capture(&self) -> Clip {
        let mut capture = self.capture_clip.clone();
        self.keep_knob_moves(&mut capture);
        capture
    }

    /// Drops from `capture` — a copy of the capture buffer on its way to a
    /// commit — the parameter moves no knob of the selected track holds now:
    /// turned on a knob reassigned since, or before the track changed. They
    /// could only sit in the clip unheard.
    pub(super) fn keep_knob_moves(&self, capture: &mut Clip) {
        let knobs = self
            .selected_track()
            .map_or(UNASSIGNED_KNOBS, |track| *track.knobs());
        capture.rewrite_param_moves(|param| knob_holding(&knobs, param).is_none(), None);
    }
}
