//! The knob panel's sequencer side (`390-knob-panel.md`): a knob's position
//! set by a drag, mirrored into `TrackMixAtomics` for the view, and every
//! track's positions sent to its output once a project is loaded. Moving a
//! knob is not undoable, like volume and pan; reassigning its CC is
//! (`SetKnobCcEdit`). The live CC a drag sends goes out through the view's
//! live-input route, not from here.

use crate::models::knobs::{CC_VALUE_MAX, Knobs};
use crate::models::track::TrackOutput;

use super::Sequencer;

/// A Control Change status byte, on channel 0 until it is sent.
const CONTROL_CHANGE: u8 = 0xB0;

impl Sequencer {
    /// Publishes the knob positions of the track at `track_idx` to its
    /// slot's atomics, for the view. Out-of-range indices are ignored.
    pub(super) fn publish_knobs(&self, track_idx: usize) {
        if let Some(track) = self.tracks.get(track_idx) {
            self.track_mix
                .publish_knobs(track.slot(), Some(track.knobs()));
        }
    }

    /// Knob `knob` of the track at `track_idx` as the view reads it, from
    /// its slot's atomics.
    #[cfg(test)]
    pub(crate) fn published_knob_value(&self, track_idx: usize, knob: usize) -> Option<u8> {
        self.track_mix.knob_value(self.slot_of(track_idx)?, knob)
    }

    /// Gives the track at `track_idx` `knobs` (a project load) and publishes
    /// their positions. Out-of-range indices are ignored.
    pub(crate) fn set_track_knobs(&mut self, track_idx: usize, knobs: Knobs) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.set_knobs(knobs);
            self.publish_knobs(track_idx);
        }
    }

    /// Sets knob `knob` of the track at `track_idx` to `value` (clamped to
    /// `0..=127`) — a drag in the knob panel, whose CC the view has already
    /// sent live. Saved with the project, so it counts toward the unsaved
    /// prompt. Out-of-range indices are ignored.
    pub(crate) fn set_knob_value(&mut self, track_idx: usize, knob: usize, value: u8) {
        let Some(slot) = self
            .tracks
            .get_mut(track_idx)
            .and_then(|t| t.knob_mut(knob))
        else {
            return;
        };
        slot.value = Some(value.min(CC_VALUE_MAX));
        self.publish_knobs(track_idx);
    }

    /// Sends every track's turned knobs to its output, now — after a project
    /// load, once its plugins are in their slots, so each plugin starts where
    /// the project left it. A knob never turned sends nothing: the plugin
    /// keeps what its own state holds.
    pub(crate) fn send_knob_positions(&mut self) {
        for track_idx in 0..self.tracks.len() {
            let track = &self.tracks[track_idx];
            let slot = track.slot();
            let channel = match *track.output() {
                TrackOutput::MidiOut { channel } => Some(channel),
                TrackOutput::Instrument(_) => None,
            };
            let knobs = *track.knobs();
            for knob in knobs {
                let Some(value) = knob.value else {
                    continue;
                };
                let message = [CONTROL_CHANGE, knob.cc, value];
                match channel {
                    Some(channel) => self.send_midi_out_now(message.to_vec(), channel),
                    None => self.push_instrument_now(slot, &message),
                }
            }
        }
    }
}
