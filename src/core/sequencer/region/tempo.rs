//! First-clip tempo detection and clip-relative tempo rescale.
//!
//! Stev has no tempo set up front — the first phrase recorded *is* the
//! tempo reference. Enter's `RetimeClipEdit` adopts the tempo that fits
//! that clip to a whole bar count, via
//! [`fit_first_clip_tempo`](Sequencer::fit_first_clip_tempo), which then
//! pulls an implausible result (too slow / too fast) back by an octave.
//! [`rescale_clip_tempo`](Sequencer::rescale_clip_tempo) is the manual
//! `⌥=`/`⌥-` adjustment (the same edit), which retimes the clip's content to
//! a new bar count while keeping its wall-clock duration. Both go through
//! `Clip::adjust_to_tempo`. See `040-phrase-detection.md`.

use crate::core::config;
use crate::core::time::{self, Meter};
use crate::models::clip::{Clip, EventSpaceRetime};

use super::Sequencer;

impl Sequencer {
    /// Fits `clip` — the project's first, on a track or about to be — to the
    /// bar count *closest to what was actually played* and returns the tempo
    /// that implies, for the caller to adopt. The target is not the loop
    /// region, which needn't match the phrase, so the tempo lands near the
    /// player's natural speed regardless of loop size. `adjust_to_tempo`
    /// retimes the clip's events and bar-snaps its region, then
    /// [`Self::octave_correct_clip_tempo`] pulls an implausible result back an
    /// octave. Returns the tempo and how the clip's event ticks moved; `None`
    /// when the tempo doesn't change (the clip is untouched).
    pub(in crate::core::sequencer) fn fit_first_clip_tempo(
        clip: &mut Clip,
        current_tempo: i32,
        meter: Meter,
    ) -> Option<(i32, EventSpaceRetime)> {
        let bar = meter.bar_ticks();
        let played = clip.region_length();
        let target_length = time::snap_to_grid(played, bar).max(bar);

        let next_tempo = time::scaled_tempo_us(current_tempo, played, target_length)?;
        if next_tempo == current_tempo {
            return None;
        }

        let retime = clip.adjust_to_tempo(next_tempo, current_tempo, meter);

        Some(
            match Self::octave_correct_clip_tempo(clip, next_tempo, meter) {
                Some((octaved, octave_retime)) => (octaved, retime.then(octave_retime)),
                None => (next_tempo, retime),
            },
        )
    }

    /// Reinterprets an implausibly slow/fast just-detected first-clip `tempo`
    /// as an octave error and corrects `clip` for it: at or below
    /// `FIRST_CLIP_TEMPO_US_SLOW` (~50 BPM) → tempo up an octave and the clip's
    /// bar count doubled; at or above `FIRST_CLIP_TEMPO_US_FAST` (~140 BPM) →
    /// tempo down an octave and the bar count halved. The automatic form of
    /// the manual rescale — `adjust_to_tempo` keeps the clip's wall-clock
    /// timing, only the metric reading changes. A single step: anything still
    /// out of band after it is left for the user to rescale by hand. Returns
    /// the corrected tempo and the clip's retime, `None` when `tempo` is in
    /// band.
    fn octave_correct_clip_tempo(
        clip: &mut Clip,
        tempo: i32,
        meter: Meter,
    ) -> Option<(i32, EventSpaceRetime)> {
        let octaved = if tempo >= config::FIRST_CLIP_TEMPO_US_SLOW {
            tempo / 2
        } else if tempo <= config::FIRST_CLIP_TEMPO_US_FAST {
            tempo * 2
        } else {
            return None;
        };

        Some((octaved, clip.adjust_to_tempo(octaved, tempo, meter)))
    }

    /// The length `⌥=`/`⌥-` stretch a clip of `length` ticks to: `direction`
    /// bars longer/shorter, never below one bar. `None` when that changes
    /// nothing (a one-bar clip can't shrink) or the clip is empty.
    pub(in crate::core::sequencer) fn rescaled_length(
        length: i32,
        direction: i32,
        meter: Meter,
    ) -> Option<i32> {
        let bar = meter.bar_ticks();
        let target = (length + direction * bar).max(bar);
        (length > 0 && target != length).then_some(target)
    }

    /// Stretches `clip` to `target_length` ticks
    /// ([`rescaled_length`](Self::rescaled_length)) and retimes its content to
    /// fill it, keeping its wall-clock duration — `⌥=`/`⌥-`
    /// through `RetimeClipEdit`, which also puts the window back on a bar
    /// line. Returns the tempo that implies (the project's, when `clip` is
    /// its only clip) and how the clip's event ticks moved; `None` when
    /// nothing changes (the clip is untouched).
    pub(in crate::core::sequencer) fn rescale_clip_tempo(
        clip: &mut Clip,
        current_tempo: i32,
        target_length: i32,
        meter: Meter,
    ) -> Option<(i32, EventSpaceRetime)> {
        let current_length = clip.region_length();
        let next_tempo = time::scaled_tempo_us(current_tempo, current_length, target_length)?;

        Some((
            next_tempo,
            clip.adjust_to_tempo(next_tempo, current_tempo, meter),
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::test_support::{clip_at, note_off, note_on};
    use crate::core::time::{self, Meter};
    use crate::models::clip::Clip;

    use super::Sequencer;

    /// A just-fitted first clip of `length` ticks holding one note.
    fn first_clip(length: i32) -> Clip {
        let mut clip = clip_at(0, length);
        clip.add_event(note_on(100));
        clip.add_event(note_off(length - 100));
        clip
    }

    /// `⌥=`/`⌥-` step and floor in bars of the project's meter.
    #[test]
    fn rescaled_length_steps_in_bars_of_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let bar = three_four.bar_ticks();
        assert_eq!(
            Sequencer::rescaled_length(bar * 2, 1, three_four),
            Some(bar * 3)
        );
        assert_eq!(
            Sequencer::rescaled_length(bar * 2 + 100, -1, three_four),
            Some(bar + 100)
        );
        assert_eq!(Sequencer::rescaled_length(bar, -1, three_four), None);
    }

    /// A fitted tempo at/below ~50 BPM is octaved up: tempo ×2, clip length
    /// ×2 (the automatic `⌥=`).
    #[test]
    fn octave_correct_doubles_a_too_slow_first_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = first_clip(bar);

        let tempo = Sequencer::octave_correct_clip_tempo(
            &mut clip,
            time::bpm_to_tempo_us(40),
            Meter::FOUR_FOUR,
        )
        .unwrap()
        .0;

        let bpm = time::tempo_us_to_bpm(tempo);
        assert!((bpm - 80.0).abs() < 0.5, "40 -> 80 BPM, got {bpm}");
        assert_eq!(clip.region_length(), bar * 2);
    }

    /// A fitted tempo at/above ~140 BPM is octaved down.
    #[test]
    fn octave_correct_halves_a_too_fast_first_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = first_clip(bar * 2);

        let tempo = Sequencer::octave_correct_clip_tempo(
            &mut clip,
            time::bpm_to_tempo_us(160),
            Meter::FOUR_FOUR,
        )
        .unwrap()
        .0;

        let bpm = time::tempo_us_to_bpm(tempo);
        assert!((bpm - 80.0).abs() < 0.5, "160 -> 80 BPM, got {bpm}");
        assert_eq!(clip.region_length(), bar);
    }

    /// An in-band tempo is left untouched.
    #[test]
    fn octave_correct_leaves_an_in_band_first_clip_alone() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut clip = first_clip(bar);

        assert_eq!(
            Sequencer::octave_correct_clip_tempo(
                &mut clip,
                time::bpm_to_tempo_us(100),
                Meter::FOUR_FOUR
            ),
            None
        );
        assert_eq!(clip.region_length(), bar);
    }

    /// Single step only — 30 BPM is octaved once to 60, not iterated into band.
    #[test]
    fn octave_correct_is_a_single_step() {
        let mut clip = first_clip(Meter::FOUR_FOUR.bar_ticks());

        let tempo = Sequencer::octave_correct_clip_tempo(
            &mut clip,
            time::bpm_to_tempo_us(30),
            Meter::FOUR_FOUR,
        )
        .unwrap()
        .0;

        let bpm = time::tempo_us_to_bpm(tempo);
        assert!((bpm - 60.0).abs() < 0.5, "one step: 30 -> 60, got {bpm}");
    }
}
