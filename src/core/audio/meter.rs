//! What a track meter reads (`310-audio-meters.md`): one channel's level as
//! the instrument mixer publishes it ([`ChannelLevel`], its ballistics in
//! `plugin_host::meter`), and the dB conversion the UI draws it with.
//!
//! Every level is a linear amplitude (`0.0` = silence, `1.0` = 0 dBFS), so the
//! all-zero state is silence, which is what [`TrackMixAtomics`] needs of its
//! default.
//!
//! [`TrackMixAtomics`]: crate::core::shared_atomics::TrackMixAtomics

/// `amplitude` in dBFS; `f32::NEG_INFINITY` for silence.
pub(crate) fn amplitude_db(amplitude: f32) -> f32 {
    20.0 * amplitude.log10()
}

/// One channel's published meter reading, each a linear amplitude.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ChannelLevel {
    /// The fast peak: instant attack, falling 20 dB/s.
    pub(crate) peak: f32,
    /// The RMS level, averaged over 300 ms.
    pub(crate) rms: f32,
    /// The peak-hold line: the highest recent peak, held 1.5 s.
    pub(crate) hold: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_minus_infinity_and_full_scale_zero() {
        assert_eq!(amplitude_db(0.0), f32::NEG_INFINITY);
        assert_eq!(amplitude_db(1.0), 0.0);
    }
}
