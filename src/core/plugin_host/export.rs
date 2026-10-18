//! The audio export's tap on the [`InstrumentMixer`](super::mixer::InstrumentMixer)
//! (`300-export-to-audio.md`): between the sequencer's start and end marks,
//! each exported slot's raw samples go to its own ring, and the gained sum of
//! those slots goes to the mix ring, for the `"audio-export"` thread
//! (`export_writer.rs`) to write.
//!
//! Every ring is built once, when the plugin host starts, so nothing is
//! allocated or freed on the audio thread when an export starts or ends. A
//! stem is the voice before its gain, so it comes out at unity and centred;
//! the mix is the mixer's own contribution to the engine mix, so the click
//! (a separate `AudioSource`) can never reach it. Both are taken before the
//! engine's safety clamp.
//!
//! Every file keeps the same length: a slot whose voice slept, failed to
//! render or isn't loaded pushes silence for the block.

use std::iter::repeat_n;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::core::audio::MAX_FRAMES;
use crate::core::config::MAX_TRACKS;
use crate::core::sequencer::ExportMark;

use super::voice::InstrumentVoice;

/// Every file is stereo, interleaved in the rings as in the WAV.
const CHANNELS: usize = 2;

// The silent slots travel as a bit mask.
const _: () = assert!(MAX_TRACKS <= u32::BITS as usize);

/// What the mixer tells the writer besides the samples. Written by the audio
/// thread while an export runs, and reset by the writer once it has read
/// them at the end, so there are no locks.
#[derive(Default)]
pub(super) struct ExportShared {
    /// Set once the mixer has pushed the last frame before the end mark.
    ended: AtomicBool,
    /// The exported slots with no voice loaded when the start resolved, as a
    /// bit mask: their stems are silence.
    silent_slots: AtomicU32,
    /// Set when a ring was too full for a block, which was then dropped.
    overflowed: AtomicBool,
}

impl ExportShared {
    /// (writer) Clears everything once an export is finished, ready for the
    /// next.
    pub(super) fn reset(&self) {
        self.ended.store(false, Ordering::Relaxed);
        self.silent_slots.store(0, Ordering::Relaxed);
        self.overflowed.store(false, Ordering::Relaxed);
    }

    /// (writer) Whether the mixer has pushed its last frame. Once it has,
    /// draining every ring once more gets the rest.
    pub(super) fn has_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// (writer) Whether `slot` had no voice when the export started.
    pub(super) fn is_silent(&self, slot: usize) -> bool {
        self.silent_slots.load(Ordering::Relaxed) & (1 << slot) != 0
    }

    /// (writer) Whether samples were dropped.
    pub(super) fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Relaxed)
    }
}

/// The writer's ends of the rings: one per engine slot, then the mix.
pub(super) struct ExportRings {
    /// Indexed by engine slot.
    pub(super) stems: Vec<Consumer<f32>>,
    /// The gained sum of the exported slots.
    pub(super) mix: Consumer<f32>,
}

/// Builds the tap, the writer's rings and what they share. Each ring holds
/// about a second of stereo at `sample_rate`, the writer's slack.
pub(super) fn export_tap(
    marks: Consumer<ExportMark>,
    sample_rate: f64,
) -> (ExportTap, ExportRings, Arc<ExportShared>) {
    let capacity = (sample_rate.round() as usize).max(MAX_FRAMES) * CHANNELS;
    let (stem_tx, stem_rx): (Vec<_>, Vec<_>) =
        (0..MAX_TRACKS).map(|_| RingBuffer::new(capacity)).unzip();
    let (mix_tx, mix_rx) = RingBuffer::new(capacity);
    let shared = Arc::new(ExportShared::default());
    let tap = ExportTap {
        marks,
        stems: stem_tx,
        mix: mix_tx,
        mix_scratch: vec![0.0; MAX_FRAMES * CHANNELS],
        armed: None,
        shared: Arc::clone(&shared),
    };
    let rings = ExportRings {
        stems: stem_rx,
        mix: mix_rx,
    };
    (tap, rings, shared)
}

/// An export the mixer is cutting: which slots, and the frames it starts
/// and ends on in the `steady` timeline.
#[derive(Clone, Copy)]
struct Armed {
    /// The exported slots.
    slots: [bool; MAX_TRACKS],
    /// The first frame of every file.
    start: i64,
    /// The first frame past every file, once the end mark has come.
    end: Option<i64>,
}

/// The audio-thread half of the export. See the module docs.
pub(super) struct ExportTap {
    /// The sequencer's start and end marks.
    marks: Consumer<ExportMark>,
    /// One ring per engine slot.
    stems: Vec<Producer<f32>>,
    /// The mix ring.
    mix: Producer<f32>,
    /// One block of the mix, interleaved, summed before it's pushed.
    mix_scratch: Vec<f32>,
    /// The export being cut, if any.
    armed: Option<Armed>,
    /// Shared with the writer.
    shared: Arc<ExportShared>,
}

impl ExportTap {
    /// Takes the sequencer's marks, turning each one's `Instant` into its
    /// frame with `frame_for` (`RenderCtx::scheduled_frame`). A start arms
    /// the tap and notes which exported slots have no voice (`has_voice`).
    pub(super) fn take_marks(
        &mut self,
        frame_for: impl Fn(Instant) -> i64,
        has_voice: impl Fn(usize) -> bool,
    ) {
        while let Ok(mark) = self.marks.pop() {
            match mark {
                ExportMark::Start { at, slots } => {
                    let silent = (0..MAX_TRACKS)
                        .filter(|&slot| slots[slot] && !has_voice(slot))
                        .fold(0, |mask, slot| mask | 1 << slot);
                    self.shared.silent_slots.store(silent, Ordering::Relaxed);
                    self.armed = Some(Armed {
                        slots,
                        start: frame_for(at),
                        end: None,
                    });
                }
                ExportMark::End { at } => {
                    if let Some(armed) = &mut self.armed {
                        armed.end = Some(frame_for(at));
                    }
                }
            }
        }
    }

    /// The part of the block `[block_start, block_start + frames)` that is
    /// inside the export, as offsets into the block; `None` when none is.
    /// Starts the block's mix from silence.
    pub(super) fn begin_block(&mut self, block_start: i64, frames: usize) -> Option<Range<usize>> {
        let armed = self.armed?;
        let span = export_span(armed.start, armed.end, block_start, frames)?;
        self.mix_scratch[..span.len() * CHANNELS].fill(0.0);
        Some(span)
    }

    /// Pushes `slot`'s samples over `span` to its stem ring and adds them,
    /// gained, to the block's mix — if `slot` is exported. `voice` is `None`
    /// when it has nothing for this block (none loaded, asleep, failed), and
    /// the stem gets silence. The gain ramps from `gain` by `step` per frame,
    /// reaching `gain + step * (f + 1)` at frame `f`, as in the mixer's
    /// summing pass.
    pub(super) fn tap(
        &mut self,
        slot: usize,
        voice: Option<&dyn InstrumentVoice>,
        gain: (f32, f32),
        step: (f32, f32),
        span: Range<usize>,
    ) {
        if !self.armed.is_some_and(|armed| armed.slots[slot]) {
            return;
        }
        let len = span.len() * CHANNELS;
        let ring = &mut self.stems[slot];
        let Some(voice) = voice else {
            push(ring, repeat_n(0.0, len), len, &self.shared.overflowed);
            return;
        };
        for (frame, mixed) in span
            .clone()
            .zip(self.mix_scratch.as_chunks_mut::<CHANNELS>().0)
        {
            let ramp = (frame + 1) as f32;
            mixed[0] += voice.sample(0, frame) * (gain.0 + step.0 * ramp);
            mixed[1] += voice.sample(1, frame) * (gain.1 + step.1 * ramp);
        }
        let samples = span.flat_map(|frame| [voice.sample(0, frame), voice.sample(1, frame)]);
        push(ring, samples, len, &self.shared.overflowed);
    }

    /// Pushes the block's mix over `span`, then ends the export if its last
    /// frame was in the block ending at `block_end`.
    pub(super) fn end_block(&mut self, span: Option<Range<usize>>, block_end: i64) {
        if let Some(span) = span {
            let mix = &self.mix_scratch[..span.len() * CHANNELS];
            if self.mix.push_entire_slice(mix).is_err() {
                self.shared.overflowed.store(true, Ordering::Relaxed);
            }
        }
        let ends_here = self
            .armed
            .is_some_and(|armed| armed.end.is_some_and(|end| end <= block_end));
        if ends_here {
            self.armed = None;
            self.shared.ended.store(true, Ordering::Release);
        }
    }
}

/// The part of the block `[block_start, block_start + frames)` inside the
/// export `[start, end)` (`end` `None`: not ended yet), as offsets into the
/// block. A block can be cut part-way through at either end, or both.
fn export_span(
    start: i64,
    end: Option<i64>,
    block_start: i64,
    frames: usize,
) -> Option<Range<usize>> {
    let block_end = block_start + frames as i64;
    let from = start.max(block_start);
    let to = end.map_or(block_end, |end| end.min(block_end));
    (from < to).then(|| (from - block_start) as usize..(to - block_start) as usize)
}

/// Pushes `len` samples to `ring` whole, or none and flags the overflow.
fn push(
    ring: &mut Producer<f32>,
    samples: impl IntoIterator<Item = f32>,
    len: usize,
    overflowed: &AtomicBool,
) {
    match ring.write_chunk_uninit(len) {
        Ok(chunk) => {
            chunk.fill_from_iter(samples);
        }
        Err(_) => overflowed.store(true, Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use rtrb::Producer;

    use crate::core::midi::message::Midi3;
    use crate::core::plugin_host::transport::BlockTransport;
    use crate::core::plugin_host::voice::VoiceMix;

    use super::*;

    /// A voice that plays a constant: `level` on the left, `-level` on the
    /// right.
    struct ConstantVoice {
        /// The left channel's sample.
        level: f32,
        /// Unused mixer state.
        mix: VoiceMix,
    }

    impl ConstantVoice {
        fn new(level: f32) -> Self {
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

    fn tap_with_marks() -> (
        ExportTap,
        ExportRings,
        Arc<ExportShared>,
        Producer<ExportMark>,
    ) {
        let (marks_tx, marks_rx) = RingBuffer::new(4);
        let (tap, rings, shared) = export_tap(marks_rx, 1_000.0);
        (tap, rings, shared, marks_tx)
    }

    fn slots(exported: &[usize]) -> [bool; MAX_TRACKS] {
        std::array::from_fn(|slot| exported.contains(&slot))
    }

    fn drained(ring: &mut Consumer<f32>) -> Vec<f32> {
        std::iter::from_fn(|| ring.pop().ok()).collect()
    }

    /// Marks resolve to `frame` for every `Instant` in these tests.
    fn at(frame: i64) -> impl Fn(Instant) -> i64 {
        move |_| frame
    }

    #[test]
    fn export_span_cuts_a_block_at_either_end() {
        assert_eq!(export_span(0, None, 512, 256), Some(0..256));
        assert_eq!(export_span(600, None, 512, 256), Some(88..256));
        assert_eq!(export_span(0, Some(700), 512, 256), Some(0..188));
        assert_eq!(export_span(520, Some(530), 512, 256), Some(8..18));
    }

    #[test]
    fn export_span_is_none_outside_the_export() {
        assert_eq!(export_span(768, None, 512, 256), None, "not started");
        assert_eq!(export_span(0, Some(512), 512, 256), None, "already ended");
        assert_eq!(export_span(600, Some(600), 512, 256), None, "empty");
    }

    #[test]
    fn stems_are_at_unity_and_the_mix_is_gained() {
        let (mut tap, mut rings, _shared, mut marks) = tap_with_marks();
        marks
            .push(ExportMark::Start {
                at: Instant::now(),
                slots: slots(&[0, 1]),
            })
            .unwrap();
        tap.take_marks(at(2), |_| true);

        let span = tap.begin_block(0, 4).unwrap();
        assert_eq!(span, 2..4);
        let loud = ConstantVoice::new(0.5);
        let soft = ConstantVoice::new(0.25);
        tap.tap(0, Some(&loud), (0.5, 0.5), (0.0, 0.0), span.clone());
        tap.tap(1, Some(&soft), (1.0, 2.0), (0.0, 0.0), span.clone());
        tap.end_block(Some(span), 4);

        assert_eq!(drained(&mut rings.stems[0]), [0.5, -0.5, 0.5, -0.5]);
        assert_eq!(drained(&mut rings.stems[1]), [0.25, -0.25, 0.25, -0.25]);
        // 0.5 × 0.5 + 0.25 × 1.0 on the left, −0.5 × 0.5 − 0.25 × 2.0 on the right.
        assert_eq!(drained(&mut rings.mix), [0.5, -0.75, 0.5, -0.75]);
    }

    #[test]
    fn the_mix_follows_the_gain_ramp() {
        let (mut tap, mut rings, _shared, mut marks) = tap_with_marks();
        marks
            .push(ExportMark::Start {
                at: Instant::now(),
                slots: slots(&[0]),
            })
            .unwrap();
        tap.take_marks(at(0), |_| true);
        let span = tap.begin_block(0, 2).unwrap();
        tap.tap(
            0,
            Some(&ConstantVoice::new(1.0)),
            (0.0, 0.0),
            (0.5, 0.25),
            span.clone(),
        );
        tap.end_block(Some(span), 2);
        assert_eq!(drained(&mut rings.mix), [0.5, -0.25, 1.0, -0.5]);
    }

    #[test]
    fn a_slot_outside_the_export_pushes_nothing() {
        let (mut tap, mut rings, _shared, mut marks) = tap_with_marks();
        marks
            .push(ExportMark::Start {
                at: Instant::now(),
                slots: slots(&[0]),
            })
            .unwrap();
        tap.take_marks(at(0), |_| true);
        let span = tap.begin_block(0, 2).unwrap();
        tap.tap(
            3,
            Some(&ConstantVoice::new(1.0)),
            (1.0, 1.0),
            (0.0, 0.0),
            span.clone(),
        );
        tap.end_block(Some(span), 2);
        assert!(drained(&mut rings.stems[3]).is_empty());
        assert_eq!(drained(&mut rings.mix), [0.0; 4], "nor reaches the mix");
    }

    #[test]
    fn a_slot_with_nothing_to_play_pushes_silence_and_is_reported() {
        let (mut tap, mut rings, shared, mut marks) = tap_with_marks();
        marks
            .push(ExportMark::Start {
                at: Instant::now(),
                slots: slots(&[0, 2]),
            })
            .unwrap();
        tap.take_marks(at(0), |slot| slot != 2);
        let span = tap.begin_block(0, 2).unwrap();
        tap.tap(0, None, (1.0, 1.0), (0.0, 0.0), span.clone());
        tap.tap(2, None, (1.0, 1.0), (0.0, 0.0), span.clone());
        tap.end_block(Some(span), 2);
        assert_eq!(drained(&mut rings.stems[0]), [0.0; 4]);
        assert_eq!(drained(&mut rings.stems[2]), [0.0; 4]);
        assert!(!shared.is_silent(0));
        assert!(shared.is_silent(2), "no voice loaded");
    }

    #[test]
    fn the_end_mark_cuts_the_block_and_ends_the_export() {
        let (mut tap, mut rings, shared, mut marks) = tap_with_marks();
        let start = ExportMark::Start {
            at: Instant::now(),
            slots: slots(&[0]),
        };
        marks.push(start).unwrap();
        tap.take_marks(at(2), |_| true);
        let span = tap.begin_block(0, 4);
        tap.end_block(span, 4);
        assert!(!shared.has_ended());

        marks.push(ExportMark::End { at: Instant::now() }).unwrap();
        tap.take_marks(at(5), |_| true);
        let span = tap.begin_block(4, 4).unwrap();
        assert_eq!(span, 0..1);
        tap.tap(
            0,
            Some(&ConstantVoice::new(1.0)),
            (1.0, 1.0),
            (0.0, 0.0),
            span.clone(),
        );
        tap.end_block(Some(span), 8);
        assert!(shared.has_ended());
        assert_eq!(drained(&mut rings.stems[0]), [1.0, -1.0]);
        assert_eq!(
            drained(&mut rings.mix).len(),
            3 * CHANNELS,
            "frames 2, 3 and 4"
        );

        assert_eq!(tap.begin_block(8, 4), None, "disarmed");
    }

    #[test]
    fn a_full_ring_drops_the_block_and_says_so() {
        let (mut tap, mut rings, shared, mut marks) = tap_with_marks();
        marks
            .push(ExportMark::Start {
                at: Instant::now(),
                slots: slots(&[0]),
            })
            .unwrap();
        tap.take_marks(at(0), |_| true);
        let voice = ConstantVoice::new(1.0);
        let capacity = rings.stems[0].buffer().capacity();
        for block in 0..=(capacity / (MAX_FRAMES * CHANNELS)) as i64 {
            let block_start = block * MAX_FRAMES as i64;
            let span = tap.begin_block(block_start, MAX_FRAMES).unwrap();
            tap.tap(0, Some(&voice), (1.0, 1.0), (0.0, 0.0), span.clone());
            tap.end_block(Some(span), block_start + MAX_FRAMES as i64);
        }
        assert!(shared.overflowed());
        assert_eq!(
            drained(&mut rings.stems[0]).len() % (MAX_FRAMES * CHANNELS),
            0
        );
    }
}
