//! Level-meter ballistics for the arranger's track meters
//! (`310-audio-meters.md`): peak, RMS and peak hold for one channel, advanced
//! once per audio block by the instrument mixer's summing pass, on the audio
//! thread, so no block's peak is missed between UI frames. Pure arithmetic,
//! no allocation. What they publish is a [`ChannelLevel`].

use crate::core::audio::meter::ChannelLevel;

/// How fast the peak and the released hold fall, in dB per second.
pub(crate) const PEAK_FALL_DB_PER_S: f32 = 20.0;

/// Time constant of the RMS average, seconds.
const RMS_TAU_S: f32 = 0.3;

/// How long the hold stays at a peak before it starts to fall, seconds.
const PEAK_HOLD_S: f32 = 1.5;

/// Below this amplitude (−120 dB) a level snaps to silence, so a falling
/// level reaches `0.0` instead of decaying through subnormals forever.
const SILENCE: f32 = 1e-6;

/// The per-block factors every channel's [`MeterBallistics::step`] shares,
/// computed once per block (one `powf`, one `exp`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockDecay {
    /// The block's length, seconds.
    dt: f32,
    /// What a falling level is multiplied by over one block.
    fall: f32,
    /// How far the RMS average moves toward this block's mean square.
    rms_coeff: f32,
}

impl BlockDecay {
    /// The factors for a block of `frames` at `sample_rate`.
    pub(crate) fn new(frames: usize, sample_rate: f32) -> Self {
        let dt = frames as f32 / sample_rate;
        BlockDecay {
            dt,
            fall: 10f32.powf(-PEAK_FALL_DB_PER_S * dt / 20.0),
            rms_coeff: 1.0 - (-dt / RMS_TAU_S).exp(),
        }
    }
}

/// One channel's meter state, owned by the audio thread.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MeterBallistics {
    /// The falling peak.
    peak: f32,
    /// The smoothed mean square.
    mean_square: f32,
    /// The held peak.
    hold: f32,
    /// How long the hold has been at its level, seconds.
    hold_age: f32,
}

impl MeterBallistics {
    /// Advances by one block whose largest absolute sample was `block_peak`
    /// and mean square `block_mean_square`.
    pub(crate) fn step(&mut self, block_peak: f32, block_mean_square: f32, decay: &BlockDecay) {
        self.peak = snap(block_peak.max(self.peak * decay.fall));
        self.mean_square =
            snap(self.mean_square + (block_mean_square - self.mean_square) * decay.rms_coeff);
        if block_peak >= self.hold {
            self.hold = block_peak;
            self.hold_age = 0.0;
        } else {
            self.hold_age += decay.dt;
            if self.hold_age > PEAK_HOLD_S {
                // Fall only for the part of the block past the hold: a
                // whole block's fall from then on, a part of one first.
                let over = self.hold_age - PEAK_HOLD_S;
                let fall = if over >= decay.dt {
                    decay.fall
                } else {
                    10f32.powf(-PEAK_FALL_DB_PER_S * over / 20.0)
                };
                self.hold = snap((self.hold * fall).max(self.peak));
            }
        }
    }

    /// The reading to publish.
    pub(crate) fn level(&self) -> ChannelLevel {
        ChannelLevel {
            peak: self.peak,
            rms: self.mean_square.sqrt(),
            hold: self.hold,
        }
    }
}

/// `level`, or `0.0` once it's below [`SILENCE`].
fn snap(level: f32) -> f32 {
    if level < SILENCE { 0.0 } else { level }
}

#[cfg(test)]
mod tests {
    use crate::core::audio::meter::amplitude_db;

    use super::*;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 256;

    /// Steps `meter` through `secs` of blocks of constant `peak` / mean square.
    fn run(meter: &mut MeterBallistics, peak: f32, mean_square: f32, secs: f32) {
        let decay = BlockDecay::new(BLOCK, RATE);
        let blocks = (secs * RATE / BLOCK as f32).round() as usize;
        for _ in 0..blocks {
            meter.step(peak, mean_square, &decay);
        }
    }

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn the_peak_attacks_at_once_and_falls_at_a_steady_rate() {
        let mut meter = MeterBallistics::default();
        meter.step(1.0, 0.0, &BlockDecay::new(BLOCK, RATE));
        assert_eq!(meter.level().peak, 1.0);
        run(&mut meter, 0.0, 0.0, 1.0);
        let db = amplitude_db(meter.level().peak);
        assert!(close(db, -PEAK_FALL_DB_PER_S, 0.2), "{db}");
    }

    #[test]
    fn the_fall_does_not_depend_on_the_block_size() {
        let fall_over_a_second = |frames: usize| {
            let decay = BlockDecay::new(frames, RATE);
            let mut meter = MeterBallistics::default();
            meter.step(1.0, 0.0, &decay);
            for _ in 0..(RATE as usize / frames) {
                meter.step(0.0, 0.0, &decay);
            }
            amplitude_db(meter.level().peak)
        };
        assert!(close(fall_over_a_second(64), fall_over_a_second(960), 0.01));
    }

    #[test]
    fn rms_settles_at_the_signals_rms() {
        // A full-scale sine: peak 1, mean square ½, RMS −3 dB.
        let mut sine = MeterBallistics::default();
        run(&mut sine, 1.0, 0.5, 3.0);
        assert!(close(amplitude_db(sine.level().rms), -3.01, 0.05));
        // A full-scale square: RMS 0 dB.
        let mut square = MeterBallistics::default();
        run(&mut square, 1.0, 1.0, 3.0);
        assert!(close(amplitude_db(square.level().rms), 0.0, 0.05));
    }

    #[test]
    fn the_hold_stays_then_falls_and_never_below_the_peak() {
        let mut meter = MeterBallistics::default();
        meter.step(1.0, 0.0, &BlockDecay::new(BLOCK, RATE));
        run(&mut meter, 0.0, 0.0, PEAK_HOLD_S - 0.1);
        assert_eq!(meter.level().hold, 1.0, "still held");
        run(&mut meter, 0.0, 0.0, 1.1);
        let db = amplitude_db(meter.level().hold);
        assert!(close(db, -PEAK_FALL_DB_PER_S, 0.5), "fell for 1 s: {db}");

        // A steady signal keeps the peak up; the hold settles on it.
        let mut steady = MeterBallistics::default();
        steady.step(1.0, 0.0, &BlockDecay::new(BLOCK, RATE));
        run(&mut steady, 0.5, 0.0, 10.0);
        assert_eq!(steady.level().hold, 0.5);
    }

    #[test]
    fn the_hold_jumps_to_a_higher_peak() {
        let mut meter = MeterBallistics::default();
        let decay = BlockDecay::new(BLOCK, RATE);
        meter.step(0.25, 0.0, &decay);
        meter.step(0.75, 0.0, &decay);
        assert_eq!(meter.level().hold, 0.75);
        // Over 0 dBFS is kept as it is: that's what the clip line shows.
        meter.step(1.5, 0.0, &decay);
        assert_eq!(meter.level().hold, 1.5);
    }

    #[test]
    fn silence_ends_at_zero() {
        let mut meter = MeterBallistics::default();
        meter.step(1.0, 1.0, &BlockDecay::new(BLOCK, RATE));
        run(&mut meter, 0.0, 0.0, 30.0);
        assert_eq!(meter.level(), ChannelLevel::default());
    }
}
