//! A format-free [`InstrumentVoice`] for the mixer's and the export tap's
//! tests.

use crate::core::midi::message::Midi3;

use super::transport::BlockTransport;
use super::voice::{InstrumentVoice, VoiceMix};

/// A voice that plays a constant: `level` on the left, `-level` on the
/// right.
pub(super) struct ConstantVoice {
    /// The left channel's sample.
    level: f32,
    /// The mixer's state for it.
    mix: VoiceMix,
}

impl ConstantVoice {
    /// A voice playing `level`.
    pub(super) fn new(level: f32) -> Self {
        ConstantVoice {
            level,
            mix: VoiceMix::default(),
        }
    }
}

impl InstrumentVoice for ConstantVoice {
    fn queue_midi(&mut self, _bytes: Midi3, _time: u32) {}

    fn render_block(&mut self, _frames: usize, _steady: u64, _t: &BlockTransport) -> bool {
        true
    }

    fn sample(&self, channel: usize, _frame: usize) -> f32 {
        if channel == 0 {
            self.level
        } else {
            -self.level
        }
    }

    fn clear_events(&mut self) {}

    fn mix(&self) -> &VoiceMix {
        &self.mix
    }

    fn mix_mut(&mut self) -> &mut VoiceMix {
        &mut self.mix
    }
}
