//! The built-in metronome as an [`AudioSource`]: a monophonic click voice fed
//! by a lock-free ring of scheduled click events.

use std::time::Instant;

use rtrb::Consumer;

use super::click_voice::ClickVoice;
use super::{AudioSource, RenderCtx};

/// Which click to sound. `Metronome::on_tick` picks it; the voice maps it to a
/// pitch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClickClass {
    /// Off-beats, and every beat while the transport is stopped.
    Weak,
    /// Bar downbeat while the transport is running.
    Strong,
}

/// One scheduled click: the `Instant` the beat was *intended* to occur at (from
/// the clock tick) and which click to play.
pub(crate) struct ClickEvent {
    /// Strong (bar downbeat, running) or weak.
    pub(crate) class: ClickClass,
    /// The [`Instant`] the beat was intended to sound at.
    pub(crate) at: Instant,
}

/// Pre-reserved capacity of the pending-click buffer. Clicks are >250 ms apart
/// at musical tempos, so at most a couple are ever queued; an overflow only
/// logs.
const PENDING_CAPACITY: usize = 8;

/// The always-present [`AudioSource`] that renders the
/// synthesized metronome click, sample-scheduled from its `ClickEvent`'s
/// intended instant.
pub(crate) struct MetronomeSource {
    /// Inbound scheduled clicks from `Metronome` (sequencer thread).
    rx: Consumer<ClickEvent>,
    /// The single click voice.
    voice: ClickVoice,
    /// `(target_frame, class)` waiting for the sample they should sound at.
    pending: Vec<(i64, ClickClass)>,
}

impl MetronomeSource {
    /// A source draining `rx`, synthesizing at `sample_rate`.
    pub(crate) fn new(rx: Consumer<ClickEvent>, sample_rate: f64) -> Self {
        Self {
            rx,
            voice: ClickVoice::new(sample_rate as f32),
            pending: Vec::with_capacity(PENDING_CAPACITY),
        }
    }
}

impl AudioSource for MetronomeSource {
    fn name(&self) -> &'static str {
        "click"
    }

    fn render_into(&mut self, mix: &mut [Vec<f32>; 2], ctx: &RenderCtx<'_>) {
        let RenderCtx {
            frames,
            first_frame,
            ..
        } = *ctx;

        while let Ok(ev) = self.rx.pop() {
            let target = ctx.scheduled_frame(ev.at);
            if self.pending.len() < self.pending.capacity() {
                self.pending.push((target, ev.class));
            } else {
                dprintln!("metronome: pending-click buffer full, dropping a click");
            }
        }

        let [left, right] = mix;
        debug_assert_eq!(left.len(), frames);
        for (f, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let cur = first_frame.wrapping_add(f as u64) as i64;
            let voice = &mut self.voice;
            self.pending.retain(|&(target, class)| {
                let due = target <= cur;
                if due {
                    voice.trigger(class);
                }
                !due
            });
            let s = self.voice.next_sample();
            *l += s;
            *r += s;
        }
    }
}

#[cfg(test)]
mod tests {
    use rtrb::RingBuffer;

    use super::*;
    use crate::core::audio::{AudioClock, AudioLoad, WorkerPool};

    /// Renders one `frames`-long block starting at frame 0, against a clock
    /// with no anchor yet — so `frame_for` returns 0 and any queued event
    /// clamps to the block start. The pool is empty: this source has no
    /// per-item work to spread. The meter is a throwaway: this source
    /// attributes nothing.
    fn render(src: &mut MetronomeSource, frames: usize) -> [Vec<f32>; 2] {
        let mut mix = [vec![0.0; frames], vec![0.0; frames]];
        let clock = AudioClock::new(48_000.0);
        let pool = WorkerPool::new(0);
        let load = AudioLoad::new();
        let ctx = RenderCtx {
            frames,
            first_frame: 0,
            clock: &clock,
            pool: &pool,
            load: &load,
        };
        src.render_into(&mut mix, &ctx);
        mix
    }

    #[test]
    fn a_queued_click_makes_the_source_audible() {
        let (mut tx, rx) = RingBuffer::new(4);
        let mut src = MetronomeSource::new(rx, 48_000.0);
        tx.push(ClickEvent {
            class: ClickClass::Strong,
            at: Instant::now(),
        })
        .unwrap();

        // No `AudioClock` anchor yet → `frame_for` returns 0 → clamped to the
        // block start, so the click sounds within this block.
        let mix = render(&mut src, 512);

        assert!(mix[0].iter().any(|s| s.abs() > 0.0));
        assert_eq!(mix[0], mix[1]);
    }

    #[test]
    fn silent_with_nothing_queued() {
        let (_tx, rx) = RingBuffer::<ClickEvent>::new(4);
        let mut src = MetronomeSource::new(rx, 48_000.0);
        let mix = render(&mut src, 256);
        assert!(mix[0].iter().all(|s| *s == 0.0));
    }
}
