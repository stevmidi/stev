//! Per-track volume, stereo-balance, mute and solo writers. The values live in
//! [`TrackMixAtomics`](crate::core::shared_atomics::TrackMixAtomics) (shared
//! lock-free with the CLAP mixer and the arranger
//! track header); these methods clamp and store them. Not undoable — a mixer
//! surface control, like clip edge drag-resize, region length and metronome
//! mute (see `050-undo-redo.md`).
//!
//! Every method here takes a track's *position* and reads or writes the
//! atomics of its *slot* ([`Track::slot`](crate::models::track::Track::slot)),
//! so callers never see the difference. An out-of-range position reads as
//! neutral and writes nothing.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::audio::mix::{MAX_DB, MIN_DB, track_is_audible};
use crate::core::config::MAX_TRACKS;

use super::Sequencer;

impl Sequencer {
    /// Sets `track_idx`'s volume, clamped to `[MIN_DB, MAX_DB]`. Out-of-range
    /// indices are ignored.
    pub(crate) fn set_track_volume(&mut self, track_idx: usize, volume_db: f32) {
        let Some(slot) = self
            .slot_of(track_idx)
            .map(|s| &self.track_mix.volume_db[s])
        else {
            return;
        };
        slot.store(volume_db.clamp(MIN_DB, MAX_DB).to_bits(), Ordering::Relaxed);
    }

    /// Sets `track_idx`'s stereo balance, clamped to `[-1.0, 1.0]` (`0.0` =
    /// centre). Out-of-range indices are ignored.
    pub(crate) fn set_track_pan(&mut self, track_idx: usize, pan: f32) {
        let Some(slot) = self.slot_of(track_idx).map(|s| &self.track_mix.pan[s]) else {
            return;
        };
        slot.store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// `track_idx`'s volume in dB (`0.0` for an out-of-range index).
    pub(crate) fn track_volume_db(&self, track_idx: usize) -> f32 {
        self.slot_of(track_idx)
            .map_or(0.0, |slot| self.track_mix.volume_db(slot))
    }

    /// `track_idx`'s stereo balance (`0.0` for an out-of-range index).
    pub(crate) fn track_pan(&self, track_idx: usize) -> f32 {
        self.slot_of(track_idx)
            .map_or(0.0, |slot| self.track_mix.pan(slot))
    }

    // --- Mute / solo ---

    /// Whether `track_idx`'s own mute flag is set (ignores solo).
    pub(crate) fn track_muted(&self, track_idx: usize) -> bool {
        self.slot_of(track_idx)
            .is_some_and(|slot| self.track_mix.muted(slot))
    }

    /// Whether `track_idx`'s solo flag is set.
    pub(crate) fn track_soloed(&self, track_idx: usize) -> bool {
        self.slot_of(track_idx)
            .is_some_and(|slot| self.track_mix.soloed(slot))
    }

    /// Whether any track is soloed — when true, non-soloed tracks are silent.
    /// Reads every slot: an idle one is always back at neutral
    /// ([`reset_slot_mix`](Self::reset_slot_mix)), so it never counts.
    pub(crate) fn any_track_soloed(&self) -> bool {
        self.track_mix
            .solo
            .iter()
            .any(|s| s.load(Ordering::Relaxed))
    }

    /// Per-track audibility by position, read in one pass so callers can take
    /// the array before a `self.tracks.iter_mut()` loop without a borrow clash
    /// — the same shape as `InstrumentMixer::target_gains`. Positions past the
    /// last track read as an untouched track would.
    pub(crate) fn track_audibility(&self) -> [bool; MAX_TRACKS] {
        let any_solo = self.any_track_soloed();
        std::array::from_fn(|i| {
            track_is_audible(self.track_muted(i), self.track_soloed(i), any_solo)
        })
    }

    /// Sets `track_idx`'s mute state. Out-of-range indices are ignored. Does
    /// **not** release sounding notes — the caller
    /// ([`toggle_track_mute`](Self::toggle_track_mute) / project load) decides.
    pub(crate) fn set_track_muted(&mut self, track_idx: usize, muted: bool) {
        if let Some(slot) = self.slot_of(track_idx) {
            self.track_mix.mute[slot].store(muted, Ordering::Relaxed);
        }
    }

    /// Sets `track_idx`'s solo flag without releasing anything — for putting
    /// a restored track back as it was. Out-of-range indices are ignored.
    pub(crate) fn set_track_soloed(&mut self, track_idx: usize, soloed: bool) {
        if let Some(slot) = self.slot_of(track_idx) {
            self.track_mix.solo[slot].store(soloed, Ordering::Relaxed);
        }
    }

    /// Flips `track_idx`'s mute, then releases the sounding notes of every track
    /// the change just silenced (this one on a mute; every non-soloed track when
    /// this was the last thing keeping solo inactive — a no-op set stays a
    /// no-op). Out-of-range indices are ignored.
    pub(crate) fn toggle_track_mute(&mut self, track_idx: usize) {
        let Some(slot) = self.slot_of(track_idx) else {
            return;
        };
        if toggle_flag(&self.track_mix.mute, slot) {
            self.release_newly_silenced_tracks();
        }
    }

    /// Flips `track_idx`'s solo, then releases the sounding notes of every track
    /// the change just silenced. Out-of-range indices are ignored.
    pub(crate) fn toggle_track_solo(&mut self, track_idx: usize) {
        let Some(slot) = self.slot_of(track_idx) else {
            return;
        };
        if toggle_flag(&self.track_mix.solo, slot) {
            self.release_newly_silenced_tracks();
        }
    }

    /// Releases the sounding notes of every track that is currently inaudible.
    /// Cheap and idempotent — a track with nothing sounding queues nothing — so
    /// running it over all tracks after any mute/solo change is simpler than
    /// tracking exactly which tracks flipped.
    pub(super) fn release_newly_silenced_tracks(&mut self) {
        let audible = self.track_audibility();
        for (track_idx, audible) in audible.into_iter().take(self.tracks.len()).enumerate() {
            if !audible {
                self.release_track_notes(track_idx);
            }
        }
    }

    /// Restores every slot to unity gain, centre pan, unmuted and unsoloed.
    /// Called from [`new_project`](Self::new_project).
    pub(crate) fn reset_track_mix(&mut self) {
        for slot in 0..MAX_TRACKS {
            self.reset_slot_mix(slot);
        }
    }

    /// Puts engine slot `slot` back at neutral — unity, centre, unmuted,
    /// unsoloed: for every slot on a new project, and for one whose track
    /// has left the arrangement, so an idle slot never counts toward
    /// `any_track_soloed` and the next track to take it starts clean.
    pub(super) fn reset_slot_mix(&mut self, slot: usize) {
        let mix = &self.track_mix;
        mix.volume_db[slot].store(0.0f32.to_bits(), Ordering::Relaxed);
        mix.pan[slot].store(0.0f32.to_bits(), Ordering::Relaxed);
        mix.mute[slot].store(false, Ordering::Relaxed);
        mix.solo[slot].store(false, Ordering::Relaxed);
    }
}

/// Flips `flags[idx]`; `false` (and no change) for an out-of-range index.
fn toggle_flag(flags: &[AtomicBool], idx: usize) -> bool {
    flags
        .get(idx)
        .map(|flag| flag.fetch_xor(true, Ordering::Relaxed))
        .is_some()
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::test_support::test_sequencer;

    #[test]
    fn defaults_are_neutral() {
        let seq = test_sequencer();
        assert_eq!(seq.track_volume_db(0), 0.0);
        assert_eq!(seq.track_pan(3), 0.0);
    }

    #[test]
    fn set_and_read_round_trip() {
        let mut seq = test_sequencer();
        seq.set_track_volume(2, -6.3);
        seq.set_track_pan(2, -0.4);
        assert!((seq.track_volume_db(2) - -6.3).abs() < 1e-4);
        assert!((seq.track_pan(2) - -0.4).abs() < 1e-4);
    }

    #[test]
    fn values_are_clamped() {
        let mut seq = test_sequencer();
        seq.set_track_volume(0, 999.0);
        seq.set_track_pan(0, -999.0);
        assert_eq!(seq.track_volume_db(0), 6.0);
        assert_eq!(seq.track_pan(0), -1.0);
    }

    #[test]
    fn out_of_range_index_is_ignored() {
        let mut seq = test_sequencer();
        seq.set_track_volume(9999, -6.0);
        seq.set_track_pan(9999, 0.5);
    }

    #[test]
    fn reset_restores_neutral() {
        let mut seq = test_sequencer();
        seq.set_track_volume(1, -12.0);
        seq.set_track_pan(1, 0.7);
        seq.set_track_muted(1, true);
        seq.toggle_track_solo(2);
        seq.reset_track_mix();
        assert_eq!(seq.track_volume_db(1), 0.0);
        assert_eq!(seq.track_pan(1), 0.0);
        assert!(!seq.track_muted(1));
        assert!(!seq.track_soloed(2));
    }

    // --- Mute / solo --- (the pure `track_is_audible` rule is tested in
    // `core::audio::mix`; here we exercise the `Sequencer` wiring around it.)

    #[test]
    fn mute_and_solo_defaults_and_round_trip() {
        let mut seq = test_sequencer();
        assert!(!seq.track_muted(0));
        assert!(!seq.track_soloed(0));
        assert!(!seq.any_track_soloed());

        seq.toggle_track_mute(0);
        assert!(seq.track_muted(0));
        seq.toggle_track_mute(0);
        assert!(!seq.track_muted(0));

        seq.toggle_track_solo(1);
        assert!(seq.track_soloed(1));
        assert!(seq.any_track_soloed());
    }

    #[test]
    fn track_audibility_reflects_mute_and_solo() {
        let mut seq = test_sequencer();
        // Nothing touched: everything audible.
        assert!(seq.track_audibility().iter().all(|&a| a));

        seq.toggle_track_mute(0);
        let a = seq.track_audibility();
        assert!(!a[0] && a[1]);

        // Solo track 1: only 1 audible, 0 still muted.
        seq.toggle_track_solo(1);
        let a = seq.track_audibility();
        assert!(!a[0] && a[1] && !a[2]);
    }

    #[test]
    fn mute_solo_out_of_range_index_is_ignored() {
        let mut seq = test_sequencer();
        seq.toggle_track_mute(9999);
        seq.toggle_track_solo(9999);
        seq.set_track_muted(9999, true);
        assert!(!seq.any_track_soloed());
    }
}
