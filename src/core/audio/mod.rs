//! The unified audio engine: one `cpal` output stream, on the `"audio-engine"`
//! thread, summing every [`AudioSource`] into the device buffer.
//!
//! Sources are the always-present [`MetronomeSource`] (the built-in click, all
//! platforms) and — on macOS — the plugin host's instrument mixer, handed in
//! at runtime once it exists (see `core::plugin_host`). Everything shares one
//! [`AudioClock`], so every source places its events on the same output-sample
//! timeline.

mod click_voice;
mod clock;
mod denormals;
mod engine;
mod journal;
mod load;
mod metronome_source;
pub(crate) mod mix;
mod thread_role;
mod topology;
mod worker_pool;

use std::time::Instant;

pub(crate) use clock::AudioClock;
pub(crate) use engine::{AudioEngine, EngineHandle};
pub(crate) use load::AudioLoad;
pub(crate) use metronome_source::{ClickClass, ClickEvent, MetronomeSource};
pub(crate) use thread_role::enter_render_thread;
/// Only the CLAP host (macOS) asks which thread it is on.
#[cfg(target_os = "macos")]
pub(crate) use thread_role::is_audio_thread;
pub(crate) use topology::max_dsp_threads;
pub(crate) use worker_pool::WorkerPool;

/// Largest block the engine renders in one un-split pass, and the pre-reserved
/// capacity of every per-channel mix buffer. Real `cpal` callback buffers are
/// far smaller; a larger one is split into passes of this size.
pub(crate) const MAX_FRAMES: usize = 4096;

/// Requested audio callback buffer size, in frames — the "buffer size" a DAW
/// exposes. Lower = less latency, higher = more headroom against xruns. Clamped
/// to the device's supported range; a device that reports no range keeps its
/// own default.
pub(crate) const DESIRED_BUFFER_FRAMES: u32 = 256;

/// Delay added to every scheduled event's target frame — the metronome click
/// and instrument clip events alike, both through
/// [`RenderCtx::scheduled_frame`] — so an event that arrived up to one buffer late
/// still lands at a positive in-block offset instead of clamped to 0. The cost
/// is that constant latency (~5.3 ms at 48 kHz / 256); a later event still
/// degrades gracefully to block offset 0. One buffer, matching
/// [`DESIRED_BUFFER_FRAMES`]. Live keyboard events bypass it — see
/// `plugin_host::mixer`.
pub(crate) const SCHEDULE_DELAY_FRAMES: u32 = DESIRED_BUFFER_FRAMES;

/// Everything a source needs to know about the block it is rendering, other
/// than the mix buffers themselves. Passed by reference so the parameter list
/// stays one argument as the engine gains more per-block state to hand out.
#[derive(Clone, Copy)]
pub(crate) struct RenderCtx<'a> {
    /// Frames to render — the length of each channel of `mix`. Never above
    /// [`MAX_FRAMES`].
    pub(crate) frames: usize,
    /// `steady` sample index of `mix[..][0]`.
    pub(crate) first_frame: u64,
    /// Maps an intended wall-clock [`Instant`] onto the `steady` timeline.
    pub(crate) clock: &'a AudioClock,
    /// Spreads an independent per-item workload across the calling thread and
    /// the engine's worker threads. A source whose work is *not* independent
    /// per item — or is too cheap to be worth a wakeup — simply doesn't use it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) pool: &'a WorkerPool,
    /// The engine's deadline meter. A source with per-track work reports each
    /// track's render time through [`AudioLoad::record_track`] so an overrun
    /// can be attributed; the engine itself times each source as a whole.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) load: &'a AudioLoad,
}

/// One additive contributor to the engine's output mix. Implementors run on the
/// audio callback thread and must not block or allocate.
pub(crate) trait AudioSource: Send {
    /// Adds this source's audio into `mix` — two channels, each already sized
    /// and zeroed for `[0, ctx.frames)` by the engine.
    fn render_into(&mut self, mix: &mut [Vec<f32>; 2], ctx: &RenderCtx<'_>);

    /// Short label for the overrun journal's per-source split (`"click"`,
    /// `"instruments"`). `'static` so it can ride the journal's `Copy`
    /// record without an allocation on the audio thread.
    fn name(&self) -> &'static str;
}

/// The instrument mixer's [`AudioSource::name`] — the source the overrun
/// journal attaches the per-track split to.
pub(crate) const INSTRUMENTS_SOURCE: &str = "instruments";

impl RenderCtx<'_> {
    /// The frame an event intended for wall-clock `at` should sound at: its
    /// place on the `steady` timeline plus [`SCHEDULE_DELAY_FRAMES`], so the
    /// click and a hosted synth playing the same beat land on the same output
    /// sample. Never before this block — a straggler plays now, not in the
    /// past.
    pub(crate) fn scheduled_frame(&self, at: Instant) -> i64 {
        (self.clock.frame_for(at) + i64::from(SCHEDULE_DELAY_FRAMES)).max(self.first_frame as i64)
    }
}
