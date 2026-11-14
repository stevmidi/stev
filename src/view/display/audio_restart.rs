//! An audio restart's plugins, spread over frames (`480-audio-settings.md`
//! decision 8): the stream came back at another rate, and every plugin
//! activated at the old one is asked back from the mixer, reactivated, one
//! per frame, under the "Restarting audio" panel, and held until all of
//! them are ready, to go back in together. Reactivating a plugin blocks the
//! eframe main thread (its instance is `!Send`), which is why it's one per
//! frame and why the panel repaints between them.
//!
//! This is the bookkeeping alone, generic over what it holds so it can be
//! tested without plugins: `A` is a voice the mixer handed back, `R` a voice
//! ready to go back in. `Display` drives it from `instrument.rs`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use log::warn;

use super::instrument_restore::{progress_fraction, progress_step};

/// How long a restart waiting for voices from the mixer, with none in hand,
/// holds the app after the last one came (or was asked for). The mixer
/// hands them back from the audio callback, within a buffer or two; one
/// that doesn't (the stream stopped before the callback ran) leaves the
/// restart waiting for a device, and the panel lets the app go meanwhile.
pub(super) const STALLED_AFTER: Duration = Duration::from_secs(1);

/// The plugins of one audio restart, by engine slot: asked back from the
/// mixer, back and waiting to be reactivated, or ready to go back in.
#[derive(Debug)]
pub(super) struct AudioRestart<A, R> {
    /// Slots whose voice has been asked back and hasn't come yet.
    asked: Vec<usize>,
    /// Voices back from the mixer, to reactivate, front first.
    arrived: VecDeque<(usize, A)>,
    /// Voices reactivated (or their plugin loaded again), waiting for the
    /// rest.
    ready: Vec<(usize, R)>,
    /// How many plugins the restart has had: the panel's "of 8".
    total: usize,
    /// When a voice was last asked for or came back.
    last_news: Option<Instant>,
}

/// The voice a restart was holding for a slot whose plugin was removed
/// ([`AudioRestart::forget`]).
#[derive(Debug, PartialEq)]
pub(super) enum Held<A, R> {
    /// Its voice was back, waiting to be reactivated.
    Arrived(A),
    /// Its voice was ready to go back in.
    Ready(R),
}

impl<A, R> AudioRestart<A, R> {
    /// A restart with nothing in it yet.
    pub(super) fn new() -> Self {
        AudioRestart {
            asked: Vec::new(),
            arrived: VecDeque::new(),
            ready: Vec::new(),
            total: 0,
            last_news: None,
        }
    }

    /// `slot`'s voice has been asked back, `now`.
    pub(super) fn ask(&mut self, slot: usize, now: Instant) {
        self.asked.push(slot);
        self.total += 1;
        self.last_news = Some(now);
    }

    /// `slot`'s voice came back, `now`, to be reactivated. One that wasn't
    /// asked for isn't expected (every voice the mixer hands back was asked
    /// for, and a restart ends only once none is out); it is taken all the
    /// same, and counted, rather than lost.
    pub(super) fn arrive(&mut self, slot: usize, voice: A, now: Instant) {
        match self.asked.iter().position(|s| *s == slot) {
            Some(pos) => {
                self.asked.remove(pos);
            }
            None => {
                warn!("audio restart: a voice came back for slot {slot}, which wasn't asked for");
                self.total += 1;
            }
        }
        self.arrived.push_back((slot, voice));
        self.last_news = Some(now);
    }

    /// Whether the restart holds the app, `now`: its panel up and input
    /// dropped. It does while it has a voice in hand, and while it waits
    /// for voices that came or were asked for less than [`STALLED_AFTER`]
    /// ago; a restart whose voices stopped coming lets the app go, and goes
    /// on when they come.
    pub(super) fn holds_app(&self, now: Instant) -> bool {
        !self.arrived.is_empty()
            || self
                .last_news
                .is_none_or(|at| now.saturating_duration_since(at) < STALLED_AFTER)
    }

    /// The next voice to reactivate.
    pub(super) fn pop_arrived(&mut self) -> Option<(usize, A)> {
        self.arrived.pop_front()
    }

    /// `slot`'s voice is ready to go back in.
    pub(super) fn ready(&mut self, slot: usize, voice: R) {
        self.ready.push((slot, voice));
    }

    /// Whether every plugin is ready, or popped and left out (one that
    /// wouldn't reactivate or load again): the ready voices can go back in,
    /// together.
    pub(super) fn is_done(&self) -> bool {
        self.asked.is_empty() && self.arrived.is_empty()
    }

    /// The ready voices, to go back in.
    pub(super) fn into_ready(self) -> Vec<(usize, R)> {
        self.ready
    }

    /// The plugin in `slot` was removed: takes the voice the restart held
    /// for it, and stops counting it. `None` if it held none, including one
    /// asked back that hasn't come yet: it comes to be dropped
    /// (`Display::take_back_voice`).
    pub(super) fn forget(&mut self, slot: usize) -> Option<Held<A, R>> {
        let held = if let Some(pos) = self.asked.iter().position(|s| *s == slot) {
            self.asked.remove(pos);
            self.total -= 1;
            return None;
        } else if let Some(pos) = self.arrived.iter().position(|(s, _)| *s == slot) {
            Held::Arrived(self.arrived.remove(pos)?.1)
        } else {
            let pos = self.ready.iter().position(|(s, _)| *s == slot)?;
            Held::Ready(self.ready.remove(pos).1)
        };
        self.total -= 1;
        Some(held)
    }

    /// The slot whose plugin the panel names: the next to reactivate, else
    /// the next whose voice is still on its way.
    pub(super) fn next_slot(&self) -> Option<usize> {
        self.arrived
            .front()
            .map(|(slot, _)| *slot)
            .or_else(|| self.asked.first().copied())
    }

    /// The step the next reactivation is, 1-based, and how many steps there
    /// are — "3 of 8".
    pub(super) fn step(&self) -> (usize, usize) {
        progress_step(self.done(), self.total)
    }

    /// How much of the restart is done, `0.0..=1.0` — the progress bar.
    pub(super) fn fraction(&self) -> f32 {
        progress_fraction(self.done(), self.total)
    }

    /// How many plugins are ready or left out.
    fn done(&self) -> usize {
        self.total - self.asked.len() - self.arrived.len()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{AudioRestart, Held, STALLED_AFTER};

    type Restart = AudioRestart<&'static str, &'static str>;

    #[test]
    fn reactivates_in_arrival_order_and_counts_the_steps() {
        let now = Instant::now();
        let mut restart = Restart::new();
        restart.ask(0, now);
        restart.ask(3, now);
        restart.ask(5, now);
        assert_eq!(restart.step(), (1, 3));
        assert_eq!(restart.next_slot(), Some(0));
        assert!(!restart.is_done());

        restart.arrive(3, "voice 3", now);
        restart.arrive(0, "voice 0", now);
        assert_eq!(restart.next_slot(), Some(3));
        assert_eq!(restart.pop_arrived(), Some((3, "voice 3")));
        restart.ready(3, "ready 3");
        assert_eq!(restart.step(), (2, 3));
        assert!((restart.fraction() - 1.0 / 3.0).abs() < f32::EPSILON);

        assert_eq!(restart.pop_arrived(), Some((0, "voice 0")));
        // Slot 0's plugin wouldn't come back: it's left out, and done.
        // Slot 5's voice is still on its way: nothing goes back in yet.
        assert_eq!(restart.next_slot(), Some(5));
        assert!(!restart.is_done());

        restart.arrive(5, "voice 5", now);
        assert_eq!(restart.pop_arrived(), Some((5, "voice 5")));
        restart.ready(5, "ready 5");
        assert!(restart.is_done());
        assert_eq!(restart.step(), (3, 3));
        assert_eq!(restart.fraction(), 1.0);
        assert_eq!(restart.into_ready(), vec![(3, "ready 3"), (5, "ready 5")]);
    }

    #[test]
    fn a_voice_nobody_asked_for_joins_the_count() {
        let now = Instant::now();
        let mut restart = Restart::new();
        restart.ask(1, now);
        restart.arrive(2, "voice 2", now);
        assert_eq!(restart.step(), (1, 2));
        assert!(!restart.is_done());
    }

    #[test]
    fn forgetting_a_removed_plugin_hands_back_what_was_held() {
        let now = Instant::now();
        let mut restart = Restart::new();
        restart.ask(0, now);
        restart.ask(1, now);
        restart.ask(2, now);
        restart.ask(4, now);
        restart.arrive(1, "voice 1", now);
        restart.arrive(2, "voice 2", now);
        assert_eq!(restart.pop_arrived(), Some((1, "voice 1")));
        restart.ready(1, "ready 1");

        assert_eq!(restart.forget(0), None);
        assert_eq!(restart.forget(2), Some(Held::Arrived("voice 2")));
        assert_eq!(restart.forget(1), Some(Held::Ready("ready 1")));
        assert_eq!(restart.forget(3), None);
        // Only slot 4 is left, and the count follows.
        assert_eq!(restart.step(), (1, 1));
        assert_eq!(restart.next_slot(), Some(4));
        assert!(restart.into_ready().is_empty());
    }

    #[test]
    fn a_restart_whose_voices_stop_coming_lets_the_app_go() {
        let now = Instant::now();
        let mut restart = Restart::new();
        restart.ask(0, now);
        restart.ask(1, now);
        assert!(restart.holds_app(now));
        assert!(restart.holds_app(now + STALLED_AFTER - Duration::from_millis(1)));
        assert!(!restart.holds_app(now + STALLED_AFTER));

        // A voice coming late takes the app again: there is work in hand,
        // then the wait for the last starts over.
        let later = now + STALLED_AFTER * 3;
        restart.arrive(0, "voice 0", later);
        assert!(restart.holds_app(later + STALLED_AFTER * 2));
        assert_eq!(restart.pop_arrived(), Some((0, "voice 0")));
        assert!(restart.holds_app(later));
        assert!(!restart.holds_app(later + STALLED_AFTER));
    }
}
