//! The synthesized metronome click: a single monophonic enveloped sine burst.

use std::f32::consts::TAU;

use super::ClickClass;

// --- Synthesized click voice parameters ---
/// Sine frequency for a weak (off-beat / stopped) click.
const WEAK_FREQ_HZ: f32 = 1_000.0;
/// Sine frequency for a strong (bar-downbeat, running) click.
const STRONG_FREQ_HZ: f32 = 1_500.0;
/// Total burst length.
const CLICK_LEN_SECS: f32 = 0.030;
/// Linear fade-in, long enough to avoid a start click but short enough to stay
/// transient.
const ATTACK_SECS: f32 = 0.001;
/// Exponential amplitude decay time constant.
const DECAY_TAU_SECS: f32 = 0.010;
/// Linear taper over the final stretch of the burst so the voice reaches
/// exactly zero instead of cutting off on the decay tail.
const RELEASE_SECS: f32 = 0.004;
/// Output amplitude scale.
const GAIN: f32 = 0.25;

/// A single monophonic click voice. Retriggering just restarts it — clicks
/// never overlap at musical tempos.
pub(crate) struct ClickVoice {
    /// Output sample rate, Hz.
    sample_rate: f32,
    /// Current sine phase, radians.
    phase: f32,
    /// Phase advance per sample.
    phase_step: f32,
    /// Samples left in the burst (`0` = silent).
    remaining: u32,
    /// Samples since the burst started, for the envelope.
    elapsed: u32,
}

impl ClickVoice {
    /// An idle voice at `sample_rate`.
    pub(crate) fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            phase: 0.0,
            phase_step: 0.0,
            remaining: 0,
            elapsed: 0,
        }
    }

    /// (Re)starts the burst at the frequency for `class`.
    pub(crate) fn trigger(&mut self, class: ClickClass) {
        let freq_hz = match class {
            ClickClass::Weak => WEAK_FREQ_HZ,
            ClickClass::Strong => STRONG_FREQ_HZ,
        };
        self.phase = 0.0;
        self.phase_step = TAU * freq_hz / self.sample_rate;
        self.remaining = (CLICK_LEN_SECS * self.sample_rate) as u32;
        self.elapsed = 0;
    }

    /// The next output sample (enveloped), or `0.0` when the burst is done.
    pub(crate) fn next_sample(&mut self) -> f32 {
        if self.remaining == 0 {
            return 0.0;
        }
        let t = self.elapsed as f32 / self.sample_rate;
        let remaining_secs = self.remaining as f32 / self.sample_rate;
        let attack = (t / ATTACK_SECS).min(1.0);
        let decay = (-t / DECAY_TAU_SECS).exp();
        let release = (remaining_secs / RELEASE_SECS).min(1.0);
        let sample = self.phase.sin() * attack * decay * release * GAIN;
        self.phase += self.phase_step;
        self.elapsed += 1;
        self.remaining -= 1;
        sample
    }
}
