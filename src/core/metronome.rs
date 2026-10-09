//! Built-in metronome: decides the click class for each beat and schedules it
//! on the audio engine.
//!
//! Lives as a sequencer-thread-local, like [`Transport`](crate::core::transport::Transport)
//! — its only cross-thread state is the mute flag (shared with the UI toggle)
//! and the click ring (drained by the [`MetronomeSource`](crate::core::audio::MetronomeSource)
//! in the audio engine). Fed one [`ClockTick`] per clock firing from the
//! sequencer thread; fires a click on quarter-note boundaries, strong on the
//! bar downbeat while the transport is running, weak otherwise (and on every
//! beat while stopped).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rtrb::Producer;

use crate::core::audio::{ClickClass, ClickEvent};
use crate::core::clock::ClockTick;
use crate::core::time::PPQN;

/// Per-beat click decision + mute gate. A sequencer-thread-local. See the
/// module docs.
pub(crate) struct Metronome {
    /// The mute flag, shared with the UI toggle.
    mute: Arc<AtomicBool>,
    /// Scheduled clicks to the `MetronomeSource` in the audio engine.
    click_tx: Producer<ClickEvent>,
    /// Beat-within-bar (0–3) the last click fired on, so the same beat being
    /// signalled twice — a clock resync can re-emit a tick value — doesn't
    /// double-trigger. Cleared on any non-contiguous tick (see `on_tick`).
    last_beat: Option<i32>,
    /// Previous tick number seen, to detect a clock discontinuity.
    prev_tick: Option<i32>,
}

impl Metronome {
    /// A metronome writing clicks to `click_tx`, gated by `mute`.
    pub(crate) fn new(mute: Arc<AtomicBool>, click_tx: Producer<ClickEvent>) -> Self {
        Self {
            mute,
            click_tx,
            last_beat: None,
            prev_tick: None,
        }
    }

    /// Called once per clock tick from the sequencer thread.
    pub(crate) fn on_tick(&mut self, tick: &ClockTick, running: bool) {
        // `ClockCommand::AlignToPlayback` can move the clock counter
        // backward or jump it forward; treat any non-unit step as a
        // discontinuity and re-arm the click so the next beat always sounds
        // even if it lands on the same beat-within-bar as the previous one.
        if let Some(prev) = self.prev_tick
            && tick.tick != prev + 1
        {
            self.last_beat = None;
        }
        self.prev_tick = Some(tick.tick);

        if !tick.is_beat || self.mute.load(Ordering::Relaxed) {
            return;
        }

        let beat = (tick.tick / PPQN).rem_euclid(4);
        if self.last_beat == Some(beat) {
            return;
        }

        let class = if beat == 0 && running {
            ClickClass::Strong
        } else {
            ClickClass::Weak
        };
        // A full ring just means the audio thread is far behind; dropping the
        // click is the right degradation.
        let _ = self.click_tx.push(ClickEvent { class, at: tick.at });
        self.last_beat = Some(beat);
    }

    /// Flips the click mute flag.
    pub(crate) fn toggle_mute(&self) {
        self.mute.fetch_xor(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rtrb::{Consumer, RingBuffer};

    use super::*;

    fn make(mute: bool) -> (Metronome, Consumer<ClickEvent>) {
        let (tx, rx) = RingBuffer::new(16);
        let metronome = Metronome::new(Arc::new(AtomicBool::new(mute)), tx);
        (metronome, rx)
    }

    fn tick(n: i32) -> ClockTick {
        ClockTick {
            is_beat: n % PPQN == 0,
            at: Instant::now(),
            tick: n,
        }
    }

    #[test]
    fn strong_click_on_bar_downbeat_while_running() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(0), true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
    }

    #[test]
    fn weak_click_on_other_beats_while_running() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(PPQN), true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
    }

    #[test]
    fn weak_click_on_downbeat_while_stopped() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(0), false);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
    }

    #[test]
    fn no_click_between_beats() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(1), true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn mute_suppresses_the_click() {
        let (mut m, mut rx) = make(true);
        m.on_tick(&tick(0), true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn the_same_beat_signalled_twice_clicks_once() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(PPQN), true);
        assert!(rx.pop().is_ok());
        // A resync re-emits the same tick value.
        m.prev_tick = Some(PPQN - 1);
        m.on_tick(&tick(PPQN), true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn a_clock_discontinuity_re_arms_the_click_on_the_same_beat() {
        let (mut m, mut rx) = make(false);
        m.on_tick(&tick(0), true);
        assert!(rx.pop().is_ok());
        // Clock snapped back a full bar — same beat-within-bar, but the jump
        // must let it click again.
        m.on_tick(&tick(-PPQN * 4), true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
    }
}
