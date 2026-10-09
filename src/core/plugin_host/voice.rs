//! The format-agnostic audio-thread view of one hosted instrument.
//!
//! [`InstrumentMixer`](super::mixer::InstrumentMixer) owns a bank of these —
//! one per `TrackOutput::Instrument` track — and is their sole caller, once per
//! engine sub-block on the `"audio-engine"` callback thread. Everything a
//! plugin format has to supply to be hostable by Stev is in this trait; the
//! scheduling, gain ramp and mute/solo handling around it live in the mixer and
//! are written once for every format.
//!
//! Implemented by `clap::ClapVoice` and `vst3::Vst3Voice`. See `130-plugin-host.md`.

use std::time::Duration;

use super::transport::BlockTransport;
use crate::core::midi::message::Midi3;

/// The mixer-owned state every voice carries, regardless of plugin format:
/// the two per-block render flags and the gain-ramp endpoints.
///
/// It lives on the voice rather than in a parallel array in the mixer because
/// the render pass is the loop that gets spread across worker threads, where
/// each worker may only touch its own voice.
pub(crate) struct VoiceMix {
    /// Set when the plugin has told us it needs no further processing until
    /// its next event — CLAP's `ProcessStatus::Sleep`, or whatever the format's
    /// equivalent idle signal is. The mixer skips
    /// [`render_block`](InstrumentVoice::render_block) entirely for a sleeping
    /// voice; [`queue_midi`](InstrumentVoice::queue_midi) must clear it, so any
    /// new event wakes the voice again on its next callback.
    pub(crate) sleeping: bool,
    /// Whether the *current* block's render pass left usable audio for
    /// [`sample`](InstrumentVoice::sample) to read — false for a sleeping voice
    /// (never called into) and for a plugin whose process call failed. Written
    /// by the mixer's render pass and read by its summing pass; meaningless
    /// outside one block.
    pub(crate) rendered: bool,
    /// Per-channel linear gain applied as this voice is summed into the mix —
    /// the value at the *end* of the last block. Each block ramps linearly from
    /// here to the track's current target (from `TrackMixAtomics` via
    /// `mix::gains_for`) so a fader move doesn't zipper. Starts at `0.0` so a
    /// freshly loaded plugin fades in over one buffer instead of clicking in.
    pub(crate) gain_l: f32,
    /// Right-channel companion to [`gain_l`](Self::gain_l).
    pub(crate) gain_r: f32,
    /// Wall time the *current* block's [`render_block`](InstrumentVoice::render_block)
    /// took — `ZERO` for a voice that was skipped. Written in the render pass
    /// (possibly on a worker thread; each voice's slot is its own) and read
    /// back on the audio thread to attribute the block's load per track. See
    /// `AudioLoad::record_track`.
    pub(crate) render_time: Duration,
}

impl Default for VoiceMix {
    /// A fresh voice: awake, nothing rendered yet, silent so its first block
    /// ramps in from zero.
    fn default() -> Self {
        Self {
            sleeping: false,
            rendered: false,
            gain_l: 0.0,
            gain_r: 0.0,
            render_time: Duration::ZERO,
        }
    }
}

/// One hosted instrument as the audio thread sees it: a plugin that can be fed
/// MIDI, asked to render a block, and read back a sample at a time.
///
/// `Send` because the voice is built on the eframe main thread and handed to
/// the audio callback through the mixer's command ring, and because the render
/// pass may run it on a worker thread. Any `!Send` half of the plugin (the CLAP
/// `PluginInstance`, a VST3 `IEditController`) stays behind on the main thread
/// in the matching [`InstrumentEditor`](super::editor::InstrumentEditor).
pub(crate) trait InstrumentVoice: Send {
    /// Queues a MIDI message at sample `time` within the next processed block.
    /// Must clear [`VoiceMix::sleeping`] if the message was accepted — see the
    /// field doc. A message the format cannot express is dropped silently.
    fn queue_midi(&mut self, bytes: Midi3, time: u32);

    /// Runs one process block of `frames` samples, leaving the audio where
    /// [`sample`](Self::sample) can read it. `steady` is the absolute index of
    /// the block's first frame in the engine's sample timeline.
    ///
    /// Returns `false` if the plugin failed to process — the mixer then treats
    /// this voice as silent for the block. A successful call is what updates
    /// [`VoiceMix::sleeping`]; the block just rendered is valid output either
    /// way, `sleeping` only affects whether the *next* callback calls in.
    fn render_block(&mut self, frames: usize, steady: u64, transport: &BlockTransport) -> bool;

    /// Reads the main output bus's `channel` at `frame` — the audio the last
    /// [`render_block`](Self::render_block) left behind. Only valid when
    /// [`VoiceMix::rendered`] is set. A mono main bus feeds both mix channels;
    /// a plugin with no usable main bus is silent.
    fn sample(&self, channel: usize, frame: usize) -> f32;

    /// Clears the per-block input event buffer once the mixer has consumed the
    /// events it queued. Called at the end of every render pass.
    fn clear_events(&mut self);

    /// Clears [`VoiceMix::sleeping`] if the plugin has work waiting that no
    /// MIDI event will deliver: a CLAP `request_process`, or VST3 UI parameter
    /// changes still in the ring. Called once per block, before the render
    /// pass decides which voices to call into. The default does nothing.
    fn wake_on_request(&mut self) {}

    /// The mixer-owned render flags and gain ramp for this voice.
    fn mix(&self) -> &VoiceMix;

    /// Mutable [`mix`](Self::mix), for the mixer's two passes.
    fn mix_mut(&mut self) -> &mut VoiceMix;
}

/// Capacity of a voice's per-block event list, in both formats: room for a
/// burst of note-offs (the load-time [`note_reset_messages`], or
/// `Sequencer::release_instrument_notes`) plus whatever clip and live events
/// land in the same block. A full list drops further events rather than
/// allocating on the audio thread.
pub(super) const EVENT_CAPACITY: usize = 256;

/// The MIDI messages every freshly loaded voice gets in its first block, so it
/// starts from silence: All Sound Off, All Notes Off, Reset All Controllers,
/// then an explicit Note Off for every key (channel 0). A plugin restored from
/// saved state can come back believing notes are held (u-he Repro's on-screen
/// keyboard showed stuck keys); the per-key offs matter because some plugins'
/// keyboard widgets only clear a key on its matching note-off, not on CC 123.
/// Queued by [`load_instrument`](super::load_instrument) on the main thread,
/// before the voice reaches the audio thread. See `130-plugin-host.md`.
pub(super) fn note_reset_messages() -> impl Iterator<Item = Midi3> {
    [0x78u8, 0x7B, 0x79]
        .into_iter()
        .map(|cc| [0xB0, cc, 0x00])
        .chain((0u8..=127).map(|note| [0x80, note, 0x00]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_reset_covers_all_keys_and_the_panic_ccs() {
        let msgs: Vec<Midi3> = note_reset_messages().collect();
        // 3 channel-mode CCs + 128 note-offs.
        assert_eq!(msgs.len(), 131);
        assert_eq!(
            &msgs[..3],
            &[[0xB0, 0x78, 0], [0xB0, 0x7B, 0], [0xB0, 0x79, 0]]
        );
        assert_eq!(msgs[3], [0x80, 0, 0]);
        assert_eq!(msgs[130], [0x80, 127, 0]);
    }

    #[test]
    fn the_note_reset_burst_leaves_room_in_the_event_list() {
        // Worst case, every message becomes an event (CLAP; VST3 drops the
        // CCs unless mapped) — the block's own events still need room.
        assert!(EVENT_CAPACITY - note_reset_messages().count() >= 64);
    }
}
