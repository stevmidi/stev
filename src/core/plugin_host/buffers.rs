//! The audio buffers a voice hands its plugin each block, shared by every
//! format: the declared port/bus layout ([`AudioIoLayout`]) and the
//! `[port][channel]` buffer set built from it ([`PortBuffers`]).
//!
//! Each format builds its own FFI view over these (CLAP `AudioPortBuffer`s, a
//! VST3 `AudioBusBuffers` array); the allocation, the per-block reset and the
//! read-back the mixer sums are the same for all of them.

/// A plugin's declared audio layout: the channel count of each input and each
/// output port (CLAP) or bus (VST3), in order.
///
/// Stev used to hand every plugin a single hard-coded stereo output port
/// and no input at all. A plugin that declares more than that — an Access Virus
/// emulation (OsTIrus), say, with a stereo analog-in bus and three stereo
/// output buses — then had `process()` called with null channel pointers for
/// the ports it was never given, and crashed clearing them. A voice now
/// allocates a [`PortBuffers`] matching this layout; only output port 0 (the
/// main pair) is summed into the engine mix, the rest are rendered and dropped.
pub(super) struct AudioIoLayout {
    /// Channels per input port, in order. Empty for a plain instrument.
    pub(super) inputs: Vec<u16>,
    /// Channels per output port, in order. Never empty — see
    /// [`new`](Self::new).
    pub(super) outputs: Vec<u16>,
}

impl AudioIoLayout {
    /// The layout a plugin reported. A plugin that reports no output ports (or
    /// cannot report any) is given one stereo port, so it still has somewhere
    /// to write.
    pub(super) fn new(inputs: Vec<u16>, outputs: Vec<u16>) -> Self {
        let outputs = if outputs.is_empty() { vec![2] } else { outputs };
        Self { inputs, outputs }
    }
}

/// A voice's audio buffers, `[port][channel]`, sized from an [`AudioIoLayout`]
/// once at load so a block never allocates on the audio thread.
pub(super) struct PortBuffers {
    /// Silent input feed: every sample zero, each channel sized to `max_frames`
    /// and never written again. A plugin that declares audio input ports (e.g.
    /// an Access Virus emulation's analog-in bus) must be handed real buffers —
    /// `process()` with a missing input dereferences a null channel pointer and
    /// crashes.
    pub(super) inputs: Vec<Vec<Vec<f32>>>,
    /// Output audio. Capacity pre-reserved to `max_frames` per channel so
    /// [`prepare_outputs`](Self::prepare_outputs) never allocates. Only the
    /// main output (port 0) is summed into the engine mix — the mixer reads it
    /// through [`main_sample`](Self::main_sample); the rest are rendered so the
    /// plugin has somewhere to write, then dropped.
    pub(super) outputs: Vec<Vec<Vec<f32>>>,
}

impl PortBuffers {
    /// Buffers for `io`, each channel with room for `max_frames`.
    pub(super) fn new(io: &AudioIoLayout, max_frames: usize) -> Self {
        Self {
            // The silent input feed is filled to `max_frames` now and never
            // touched again; `process` reads only the first `frames` of each.
            inputs: alloc(&io.inputs, max_frames, max_frames),
            outputs: alloc(&io.outputs, 0, max_frames),
        }
    }

    /// Zeroes every output channel to `frames` samples, ready for a block.
    /// Within the reserved capacity, so it never allocates.
    pub(super) fn prepare_outputs(&mut self, frames: usize) {
        for port in &mut self.outputs {
            for channel in port {
                channel.clear();
                channel.resize(frames, 0.0);
            }
        }
    }

    /// Reads the main output port's `channel` at `frame`. A mono main port
    /// feeds every mix channel from its single buffer; a missing or empty main
    /// port — or a frame past the block — is silent.
    pub(super) fn main_sample(&self, channel: usize, frame: usize) -> f32 {
        let Some(main) = self.outputs.first() else {
            return 0.0;
        };
        let Some(last) = main.len().checked_sub(1) else {
            return 0.0;
        };
        main[channel.min(last)].get(frame).copied().unwrap_or(0.0)
    }

    /// Whether the main output's first `frames` samples are all digital
    /// silence. A plugin with no main output counts as silent.
    pub(super) fn main_output_silent(&self, frames: usize) -> bool {
        self.outputs
            .first()
            .is_none_or(|port| port.iter().all(|c| c[..frames].iter().all(|s| *s == 0.0)))
    }
}

/// Total channel count across a port layout.
pub(super) fn channel_total(layout: &[u16]) -> usize {
    layout.iter().map(|&c| usize::from(c)).sum()
}

/// Allocates a `[port][channel]` buffer set for `layout`: each channel is a
/// `Vec<f32>` of length `len`, with capacity reserved to `capacity` so later
/// `resize`s up to `capacity` never allocate on the audio thread.
fn alloc(layout: &[u16], len: usize, capacity: usize) -> Vec<Vec<Vec<f32>>> {
    layout
        .iter()
        .map(|&channels| {
            (0..channels)
                .map(|_| {
                    let mut buf = Vec::with_capacity(capacity);
                    buf.resize(len, 0.0);
                    buf
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_outputs(outputs: Vec<Vec<Vec<f32>>>) -> PortBuffers {
        PortBuffers {
            inputs: Vec::new(),
            outputs,
        }
    }

    #[test]
    fn channel_total_sums_every_port() {
        assert_eq!(channel_total(&[]), 0);
        assert_eq!(channel_total(&[2]), 2);
        assert_eq!(channel_total(&[2, 2, 2]), 6);
        assert_eq!(channel_total(&[1, 2]), 3);
    }

    #[test]
    fn empty_output_layout_falls_back_to_one_stereo_port() {
        assert_eq!(AudioIoLayout::new(Vec::new(), Vec::new()).outputs, vec![2]);
    }

    #[test]
    fn a_reported_output_layout_is_kept_verbatim() {
        assert_eq!(
            AudioIoLayout::new(Vec::new(), vec![2, 2, 1]).outputs,
            vec![2, 2, 1]
        );
        assert_eq!(AudioIoLayout::new(vec![2], vec![1]).inputs, vec![2]);
    }

    #[test]
    fn buffers_match_the_layout_and_reserve_capacity() {
        let bufs = PortBuffers::new(&AudioIoLayout::new(Vec::new(), vec![2, 1]), 512);
        assert!(bufs.inputs.is_empty());
        assert_eq!(bufs.outputs.len(), 2);
        assert_eq!(bufs.outputs[0].len(), 2);
        assert_eq!(bufs.outputs[1].len(), 1);
        for port in &bufs.outputs {
            for channel in port {
                assert!(channel.is_empty());
                assert!(channel.capacity() >= 512);
            }
        }
    }

    #[test]
    fn the_silent_input_feed_is_prefilled() {
        // A plugin with an audio-in port must be handed real, readable buffers.
        let bufs = PortBuffers::new(&AudioIoLayout::new(vec![2], Vec::new()), 256);
        assert_eq!(bufs.inputs[0][0], vec![0.0; 256]);
    }

    #[test]
    fn prepare_outputs_zeroes_to_the_block_length_without_reallocating() {
        let mut bufs = PortBuffers::new(&AudioIoLayout::new(Vec::new(), vec![2]), 512);
        bufs.outputs[0][0].push(1.0);
        let ptr = bufs.outputs[0][0].as_ptr();
        bufs.prepare_outputs(256);
        assert_eq!(bufs.outputs[0][0], vec![0.0; 256]);
        assert_eq!(bufs.outputs[0][0].as_ptr(), ptr);
    }

    #[test]
    fn main_sample_only_ever_reads_port_zero() {
        let bufs = with_outputs(vec![
            vec![vec![1.0, 2.0], vec![3.0, 4.0]], // main port, stereo
            vec![vec![9.0, 9.0], vec![9.0, 9.0]], // aux port, never read
        ]);
        assert_eq!(bufs.main_sample(0, 1), 2.0);
        assert_eq!(bufs.main_sample(1, 0), 3.0);
    }

    #[test]
    fn a_mono_main_port_feeds_both_mix_channels() {
        let bufs = with_outputs(vec![vec![vec![0.5, 0.75]]]);
        assert_eq!(bufs.main_sample(0, 0), 0.5);
        assert_eq!(bufs.main_sample(1, 1), 0.75);
    }

    #[test]
    fn a_missing_or_empty_main_port_is_silent() {
        assert_eq!(with_outputs(Vec::new()).main_sample(0, 0), 0.0);
        assert_eq!(with_outputs(vec![vec![]]).main_sample(0, 0), 0.0);
        // Past the end of the block, too.
        assert_eq!(with_outputs(vec![vec![vec![1.0]]]).main_sample(0, 99), 0.0);
    }

    #[test]
    fn main_output_silent_checks_only_the_main_port_and_the_block() {
        let bufs = with_outputs(vec![
            vec![vec![0.0, 0.0, 1.0], vec![0.0, 0.0, 0.0]],
            vec![vec![5.0; 3]], // aux output is ignored
        ]);
        assert!(bufs.main_output_silent(2));
        assert!(!bufs.main_output_silent(3));
        assert!(with_outputs(Vec::new()).main_output_silent(64));
    }
}
