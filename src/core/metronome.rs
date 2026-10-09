//! Built-in metronome: decides the click class for each beat and schedules it
//! on the audio engine.
//!
//! Lives as a sequencer-thread-local, like [`Transport`](crate::core::transport::Transport)
//! — its only cross-thread state is the mute flag (shared with the UI toggle)
//! and the click ring (drained by the [`MetronomeSource`](crate::core::audio::MetronomeSource)
//! in the audio engine). Fed one position per clock tick from the sequencer
//! thread; fires a click on every counted beat of the project's [`Meter`]
//! (each quarter in x/4, each eighth in x/8), strong on the bar downbeat while
//! the transport is running, weak otherwise (and on every beat while stopped).
//!
//! While running, the position is the **playback** tick, where the bar lines
//! are drawn from — never the free-running clock tick, which matches playback
//! only modulo the loop region's length (`150-clock-position-sync.md`), so a
//! region that isn't whole bars would move the downbeat at every wrap. While
//! stopped, playback is parked and the clock tick is what counts in.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rtrb::Producer;

use crate::core::audio::{ClickClass, ClickEvent};
use crate::core::time::Meter;

/// Per-beat click decision + mute gate. A sequencer-thread-local. See the
/// module docs.
pub(crate) struct Metronome {
    /// The mute flag, shared with the UI toggle.
    mute: Arc<AtomicBool>,
    /// Scheduled clicks to the `MetronomeSource` in the audio engine.
    click_tx: Producer<ClickEvent>,
    /// Beat-within-bar the last click fired on, so the same beat being
    /// signalled twice — a clock resync can re-emit a tick value — doesn't
    /// double-trigger. Cleared on any non-contiguous tick (see `on_tick`).
    last_beat: Option<i32>,
    /// Previous position seen, to detect a discontinuity: a loop wrap, a
    /// jump, a clock realignment, or the switch between the clock and
    /// playback on a start or stop.
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

    /// Called once per clock tick from the sequencer thread, at `at`, with
    /// the position `tick` it counts from (the module docs say which) and the
    /// project's current meter.
    pub(crate) fn on_tick(&mut self, at: Instant, tick: i32, meter: Meter, running: bool) {
        // A wrap, a jump or a clock realignment moves the position; treat
        // any non-unit step as a discontinuity and re-arm the click so the
        // next beat always sounds even if it lands on the same beat-within-bar
        // as the previous one.
        if let Some(prev) = self.prev_tick
            && tick != prev + 1
        {
            self.last_beat = None;
        }
        self.prev_tick = Some(tick);

        let beat_ticks = meter.beat_ticks();
        if tick.rem_euclid(beat_ticks) != 0 || self.mute.load(Ordering::Relaxed) {
            return;
        }

        let beat = tick.rem_euclid(meter.bar_ticks()) / beat_ticks;
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
        let _ = self.click_tx.push(ClickEvent { class, at });
        self.last_beat = Some(beat);
    }

    /// Flips the click mute flag.
    pub(crate) fn toggle_mute(&self) {
        self.mute.fetch_xor(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::iter;

    use rtrb::{Consumer, RingBuffer};

    use super::*;
    use crate::core::time::PPQN;

    fn make(mute: bool) -> (Metronome, Consumer<ClickEvent>) {
        let (tx, rx) = RingBuffer::new(16);
        let metronome = Metronome::new(Arc::new(AtomicBool::new(mute)), tx);
        (metronome, rx)
    }

    #[test]
    fn strong_click_on_bar_downbeat_while_running() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), 0, Meter::FOUR_FOUR, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
    }

    #[test]
    fn weak_click_on_other_beats_while_running() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), PPQN, Meter::FOUR_FOUR, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
    }

    #[test]
    fn weak_click_on_downbeat_while_stopped() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), 0, Meter::FOUR_FOUR, false);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
    }

    #[test]
    fn no_click_between_beats() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), 1, Meter::FOUR_FOUR, true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn mute_suppresses_the_click() {
        let (mut m, mut rx) = make(true);
        m.on_tick(Instant::now(), 0, Meter::FOUR_FOUR, true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn the_same_beat_signalled_twice_clicks_once() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), PPQN, Meter::FOUR_FOUR, true);
        assert!(rx.pop().is_ok());
        // A resync re-emits the same tick value.
        m.prev_tick = Some(PPQN - 1);
        m.on_tick(Instant::now(), PPQN, Meter::FOUR_FOUR, true);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn a_clock_discontinuity_re_arms_the_click_on_the_same_beat() {
        let (mut m, mut rx) = make(false);
        m.on_tick(Instant::now(), 0, Meter::FOUR_FOUR, true);
        assert!(rx.pop().is_ok());
        // Clock snapped back a full bar — same beat-within-bar, but the jump
        // must let it click again.
        m.on_tick(Instant::now(), -PPQN * 4, Meter::FOUR_FOUR, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
    }

    /// Feeds one bar of `meter` tick by tick, from tick 0, and returns the
    /// clicks it sounded.
    fn clicks_in_one_bar(meter: Meter) -> Vec<ClickClass> {
        let (mut m, mut rx) = make(false);
        for n in 0..meter.bar_ticks() {
            m.on_tick(Instant::now(), n, meter, true);
        }
        iter::from_fn(|| rx.pop().ok()).map(|e| e.class).collect()
    }

    #[test]
    fn six_eight_clicks_six_eighths_with_one_strong() {
        let mut expected = vec![ClickClass::Weak; 6];
        expected[0] = ClickClass::Strong;
        assert_eq!(clicks_in_one_bar(Meter::new(6, 8).unwrap()), expected);
    }

    #[test]
    fn three_four_clicks_three_quarters_with_one_strong() {
        assert_eq!(
            clicks_in_one_bar(Meter::new(3, 4).unwrap()),
            [ClickClass::Strong, ClickClass::Weak, ClickClass::Weak]
        );
    }

    #[test]
    fn the_downbeat_follows_the_meter_not_the_quarter_count() {
        // Bar 2 of 3/4 starts on quarter 3: strong there, weak on quarter 4.
        let (mut m, mut rx) = make(false);
        let meter = Meter::new(3, 4).unwrap();
        m.on_tick(Instant::now(), PPQN * 3, meter, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
        m.on_tick(Instant::now(), PPQN * 4, meter, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
    }

    /// Feeds `ticks` in order, running, and returns the clicks sounded.
    fn clicks_for(meter: Meter, ticks: impl Iterator<Item = i32>) -> Vec<ClickClass> {
        let (mut m, mut rx) = make(false);
        let mut clicks = Vec::new();
        for n in ticks {
            m.on_tick(Instant::now(), n, meter, true);
            clicks.extend(iter::from_fn(|| rx.pop().ok()).map(|e| e.class));
        }
        clicks
    }

    /// Regression: the downbeat follows the bar lines through a loop wrap
    /// even when the region isn't whole bars. A 2½-bar 6/8 loop (15 eighths)
    /// strikes on eighths 0, 6 and 12 of every pass; counted from a clock
    /// that runs on past the wrap, the second pass struck on eighth 3.
    #[test]
    fn a_loop_of_two_and_a_half_bars_keeps_the_downbeat_on_the_bar_lines() {
        let meter = Meter::new(6, 8).unwrap();
        let region = meter.bar_ticks() * 5 / 2;
        let one_pass: Vec<ClickClass> = (0..15)
            .map(|eighth| {
                if eighth % 6 == 0 {
                    ClickClass::Strong
                } else {
                    ClickClass::Weak
                }
            })
            .collect();
        let two_passes = (0..region).chain(0..region);
        assert_eq!(
            clicks_for(meter, two_passes),
            [one_pass.clone(), one_pass].concat()
        );
    }

    /// Starting on a bar line strikes its downbeat first, whatever beat the
    /// clock was counting in on while stopped.
    #[test]
    fn a_start_on_a_bar_line_strikes_its_downbeat() {
        let meter = Meter::new(6, 8).unwrap();
        let (mut m, mut rx) = make(false);
        let clock = 5 * meter.beat_ticks();
        m.on_tick(Instant::now(), clock, meter, false);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Weak);
        m.on_tick(Instant::now(), meter.bar_ticks(), meter, true);
        assert_eq!(rx.pop().unwrap().class, ClickClass::Strong);
    }
}
