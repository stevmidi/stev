//! The `"clock"` thread's [`Clock`] — the BPM pulse that drives everything
//! musical.
//!
//! A [`Timer`] fires roughly every millisecond; each firing credits the
//! *real* elapsed time since the previous one as musical time (not a nominal
//! 1 ms — see `150-clock-position-sync.md` for why), producing zero or more
//! [`ClockTick`]s with correctly-spaced intended `Instant`s. Each tick steps
//! two counters: the free-running `tick` (a *position*, held to
//! `clock_tick ≡ playback_tick (mod region_length)` via
//! [`ClockCommand::AlignToPlayback`]) and, while running, the `elapsed_ticks`
//! odometer (a *duration*, never repositioned). `archive/140-device-frame-clock.md` is
//! a deferred proposal to credit from the audio device frame count instead.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::core::time::{self, PPQN};
use crate::core::timer::Timer;

/// A message to the `"clock"` thread.
pub(crate) enum ClockCommand {
    /// Realign the free-running clock counter with playback inside the current
    /// loop region — by **phase only**, never by assignment.
    ///
    /// Both counters advance in lockstep while the transport runs (one `+1` per
    /// delivered [`ClockTick`]), so a loop wrap — where playback jumps back by
    /// the region length and the free-running clock does not — leaves the two at
    /// the *same phase within the region*. In practice the wrap still shows a
    /// few ticks of difference, because `playback_tick` here is a snapshot the
    /// `"sequencer"` thread took at the wrap while the clock reads its own
    /// counter live a firing later. Correcting that by moving the clock onto
    /// playback's *phase* costs those ticks; assigning `playback_tick` would
    /// instead drop the clock into the region's first iteration on every wrap.
    /// See `150-clock-position-sync.md` and [`Clock::aligned_tick`].
    AlignToPlayback {
        /// Playback position at the moment the `"sequencer"` thread sent this
        /// (a snapshot — the clock reads its own counter live, a firing later).
        playback_tick: i32,
        /// Loop-region start tick.
        region_start: i32,
        /// Loop-region length in ticks — the modulus the two counters share.
        region_length: i32,
    },
}

/// One clock tick handed to the sequencer thread. `at` is the [`Instant`] the
/// tick was *intended* to occur at — interpolated across the real elapsed time
/// of the timer firing that produced it, so a burst of ticks from one firing
/// carries distinct, correctly-spaced timestamps rather than collapsing onto a
/// single instant. The plugin host turns `at` into a sample-accurate block
/// offset; `is_beat` and `tick` drive the metronome.
pub(crate) struct ClockTick {
    /// True on a quarter-note boundary — drives the metronome click.
    pub(crate) is_beat: bool,
    /// The [`Instant`] this tick was *intended* to occur at.
    pub(crate) at: Instant,
    /// The absolute clock-tick counter value this tick carries. Normally steps
    /// by one; a [`ClockCommand::AlignToPlayback`] correction can move it.
    pub(crate) tick: i32,
}

/// The BPM pulse generator. Lives on the `"clock"` thread. See the module docs.
pub(crate) struct Clock {
    // --- Core state ---
    /// The ~1 ms firing source.
    timer: Timer,
    /// Tempo in µs per quarter note, shared.
    tempo: Arc<AtomicI32>,
    /// The free-running musical tick counter, shared (`SharedAtomics.clock_tick`).
    tick: Arc<AtomicI32>,
    /// `time::monotonic_nanos` value published each time `tick` advances, so
    /// the MIDI-input callback can interpolate a fractional tick between firings.
    tick_instant_nanos: Arc<AtomicU64>,
    /// The transport odometer — see [`SharedAtomics::elapsed_ticks`]. Stepped in
    /// the same place as `tick` so one `tick_instant_nanos` stamp interpolates
    /// both, but only while `running`, and never realigned.
    ///
    /// [`SharedAtomics::elapsed_ticks`]: crate::core::shared_atomics::SharedAtomics::elapsed_ticks
    elapsed_ticks: Arc<AtomicI32>,
    /// Whether the transport is playing — gates `elapsed_ticks` only. `tick`
    /// free-runs regardless, since the metronome counts in and running capture
    /// records while stopped.
    running: Arc<AtomicBool>,

    // --- Communication ---
    /// Inbound [`ClockCommand`]s, moved into the timer closure by
    /// [`start`](Self::start).
    clock_command_rx: Receiver<ClockCommand>,
}

impl Clock {
    // --- Constants ---
    /// Real elapsed time between two firings is clamped to this before being
    /// credited as musical time, so a debugger pause or a sleep/wake does not
    /// dump a huge burst of ticks — it extends the older "drop the accumulated
    /// fractional backlog" intent. 100 ms ≈ 100 nominal firings.
    const MAX_ELAPSED_NS: i64 = 100_000_000;

    // --- Constructor ---
    /// Wires the clock to its timer, the shared atomics it credits, and the
    /// command receiver.
    pub(crate) fn new(
        timer: Timer,
        clock_command_rx: Receiver<ClockCommand>,
        tempo: Arc<AtomicI32>,
        tick: Arc<AtomicI32>,
        tick_instant_nanos: Arc<AtomicU64>,
        elapsed_ticks: Arc<AtomicI32>,
        running: Arc<AtomicBool>,
    ) -> Self {
        Clock {
            timer,
            tempo,
            tick,
            tick_instant_nanos,
            elapsed_ticks,
            running,
            clock_command_rx,
        }
    }

    // --- Main clock control ---
    /// Starts the timer. Each firing drains pending [`ClockCommand`]s, credits
    /// the real elapsed span as musical time, and calls `callback` once per
    /// whole tick produced. Consumes the clock: its state moves into the timer
    /// closure.
    pub(crate) fn start<F>(self, mut callback: F)
    where
        F: FnMut(ClockTick) + Send + 'static,
    {
        let Clock {
            mut timer,
            tempo,
            tick: clock_tick,
            tick_instant_nanos,
            elapsed_ticks,
            running,
            clock_command_rx,
        } = self;
        let fractional_ticks = Cell::new(0i64);
        let last_fire: Cell<Option<Instant>> = Cell::new(None);

        timer.start(move |now| {
            while let Ok(cmd) = clock_command_rx.try_recv() {
                match cmd {
                    ClockCommand::AlignToPlayback {
                        playback_tick,
                        region_start,
                        region_length,
                    } => {
                        let current = clock_tick.load(Ordering::Relaxed);
                        if let Some(aligned) = Clock::aligned_tick(
                            current,
                            playback_tick,
                            region_start,
                            region_length,
                        ) {
                            dprintln!(
                                "Clock realigned with playback: {} -> {} ({:+} ticks, region phase {} -> {}); keeping capture buffer intact",
                                current,
                                aligned,
                                aligned - current,
                                (current - region_start).rem_euclid(region_length),
                                (playback_tick - region_start).rem_euclid(region_length)
                            );
                            clock_tick.store(aligned, Ordering::Relaxed);
                            // Drop the sub-tick backlog so the snap isn't
                            // immediately followed by a stale extra tick. Left
                            // untouched when nothing was corrected, so a loop
                            // wrap no longer bleeds accumulated credit.
                            fractional_ticks.set(0);
                        }
                    }
                }
            }

            let Some(prev) = last_fire.get() else {
                // First firing only establishes the span origin.
                last_fire.set(Some(now));
                return;
            };

            let elapsed_ns = now
                .saturating_duration_since(prev)
                .as_nanos()
                .min(Self::MAX_ELAPSED_NS as u128) as i64;
            if elapsed_ns <= 0 {
                // Two firings at the same instant — coalesce into the next one.
                return;
            }

            let tempo_us = i64::from(tempo.load(Ordering::Relaxed));
            if tempo_us <= 0 {
                last_fire.set(Some(now));
                fractional_ticks.set(0);
                return;
            }

            let credit = Clock::musical_credit_ppqn_us(elapsed_ns);
            if credit <= 0 {
                // Sub-microsecond span — keep `prev` so it folds into the next.
                return;
            }
            last_fire.set(Some(now));

            let fractional_before = fractional_ticks.get();
            let (count, fractional_after) = Clock::tick_batch(fractional_before, credit, tempo_us);
            fractional_ticks.set(fractional_after);

            for k in 1..=count {
                let offset_ns =
                    Clock::tick_offset_ns(k, fractional_before, tempo_us, elapsed_ns, credit);
                let at = prev + Duration::from_nanos(offset_ns);

                let tick = clock_tick.fetch_add(1, Ordering::Relaxed);
                // The odometer steps here too, so both counters advance at the
                // same instant and the single stamp below interpolates either.
                // It is deliberately absent from the `AlignToPlayback` arm
                // above: never repositioning it is what makes it a duration.
                if running.load(Ordering::Relaxed) {
                    elapsed_ticks.fetch_add(1, Ordering::Relaxed);
                }
                // Publish when the counter reached this value, for the
                // MIDI-input callback's fractional-tick interpolation.
                tick_instant_nanos.store(time::monotonic_nanos_at(at), Ordering::Relaxed);
                let is_beat = tick % PPQN == 0;
                callback(ClockTick { is_beat, at, tick });
            }
        });
    }

    // --- Private helpers ---

    /// Musical time credited for `elapsed_ns` of real time, in PPQN·µs — the
    /// same unit the fractional accumulator carries. Dividing this by a tempo
    /// in µs-per-quarter yields whole ticks: (ticks/quarter · µs) / (µs/quarter).
    fn musical_credit_ppqn_us(elapsed_ns: i64) -> i64 {
        i64::from(PPQN) * elapsed_ns / 1_000
    }

    /// Whole ticks produced by adding `credit` to `fractional_before`, and the
    /// new leftover to carry forward.
    fn tick_batch(fractional_before: i64, credit: i64, tempo_us: i64) -> (i64, i64) {
        let pending = fractional_before + credit;
        let count = pending / tempo_us;
        (count, pending - count * tempo_us)
    }

    /// Nanoseconds after the firing's start `Instant` at which tick `k` (1-based
    /// within this firing) is intended to land: the point where cumulative
    /// credit reaches `k · tempo_us`, expressed as a fraction of `elapsed_ns`.
    fn tick_offset_ns(
        k: i64,
        fractional_before: i64,
        tempo_us: i64,
        elapsed_ns: i64,
        credit: i64,
    ) -> u64 {
        if credit <= 0 {
            return 0;
        }
        // Credit that must be spent *within this firing* to reach tick k. Never
        // exceeds `credit` for the last tick; clamped at 0 in case a mid-firing
        // tempo drop left `fractional_before` above the new `tempo_us`.
        let consumed = (k * tempo_us - fractional_before).clamp(0, credit);
        (i128::from(elapsed_ns) * i128::from(consumed) / i128::from(credit)) as u64
    }

    /// The clock counter value that realigns it with `playback_tick` inside the
    /// loop region, or `None` when the two already share a phase and nothing
    /// needs correcting.
    ///
    /// The counters are required to satisfy `clock_tick ≡ playback_tick (mod
    /// region_length)` — **not** absolute equality. Playback wraps at the region
    /// end while the clock free-runs past it, so across a long loop the clock
    /// legitimately sits whole loop lengths ahead; live-recording length and the
    /// running-capture window both depend on that absolute progression surviving.
    ///
    /// The correction is therefore the **smallest signed move that fixes the
    /// phase**, not an assignment of `playback_tick`. Assigning would collapse
    /// the clock into the region's first iteration, and it would do so on *every
    /// loop wrap*: the `playback_tick` handed to this function is a snapshot the
    /// `"sequencer"` thread took at the wrap, while `clock_tick` is read live on
    /// the `"clock"` thread one timer firing later, so the two are routinely a
    /// few ticks apart even when nothing is actually wrong. With the phases
    /// pinned but the loop index kept, that latency costs a few ticks and
    /// nothing more. See `150-clock-position-sync.md`.
    fn aligned_tick(
        clock_tick: i32,
        playback_tick: i32,
        region_start: i32,
        region_length: i32,
    ) -> Option<i32> {
        if region_length <= 0 {
            return None;
        }

        let clock_phase = (clock_tick - region_start).rem_euclid(region_length);
        let playback_phase = (playback_tick - region_start).rem_euclid(region_length);
        let delta = Clock::signed_phase_diff(clock_phase, playback_phase, region_length);

        (delta != 0).then_some(clock_tick - delta)
    }

    /// `a - b` reduced into `[-modulo / 2, modulo / 2)`, i.e. the shorter way
    /// round the circle. Used so a phase correction moves the clock by the least
    /// it can rather than a whole region length.
    fn signed_phase_diff(a: i32, b: i32, modulo: i32) -> i32 {
        if modulo <= 0 {
            return a - b;
        }

        let half = modulo / 2;
        (a - b + half).rem_euclid(modulo) - half
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 120 BPM.
    const TEMPO_120: i64 = 500_000;

    #[test]
    fn nominal_one_millisecond_firing_at_120_bpm_yields_one_tick() {
        let credit = Clock::musical_credit_ppqn_us(1_000_000);
        assert_eq!(credit, 960_000);
        let (count, frac) = Clock::tick_batch(0, credit, TEMPO_120);
        assert_eq!(count, 1);
        assert_eq!(frac, 460_000);
    }

    #[test]
    fn a_stretched_period_credits_proportionally_more_musical_time() {
        // Regression for the Apple-Silicon timebase truncation: each "1 ms"
        // period actually ran ~1.016 ms, and the old clock credited it as a
        // flat 1 ms — running the sequencer ~1.6% slow. Real elapsed time must
        // now drive the credit.
        let nominal = Clock::musical_credit_ppqn_us(1_000_000);
        let stretched = Clock::musical_credit_ppqn_us(1_016_000);
        assert!(stretched > nominal);
        assert_eq!(stretched, 975_360);
    }

    #[test]
    fn a_firing_can_yield_several_ticks_with_distinct_spaced_instants() {
        let elapsed = 1_100_000;
        let credit = Clock::musical_credit_ppqn_us(elapsed);
        let (count, _) = Clock::tick_batch(0, credit, TEMPO_120);
        assert_eq!(count, 2);

        let o1 = Clock::tick_offset_ns(1, 0, TEMPO_120, elapsed, credit);
        let o2 = Clock::tick_offset_ns(2, 0, TEMPO_120, elapsed, credit);
        assert!(o1 > 0 && o1 < o2 && o2 <= elapsed as u64);
        // Tick 2 needs twice the credit of tick 1, so ~twice the offset.
        assert!((o2 as i64 - 2 * o1 as i64).abs() < 2_000);
    }

    #[test]
    fn first_tick_of_a_firing_never_precedes_the_firing_start() {
        // A mid-firing tempo drop can leave `fractional_before` above the new
        // tempo; the offset must clamp to 0, not go negative.
        let elapsed = 1_000_000;
        let credit = Clock::musical_credit_ppqn_us(elapsed);
        let offset = Clock::tick_offset_ns(1, 900_000, 500_000, elapsed, credit);
        assert_eq!(offset, 0);
    }

    #[test]
    fn a_long_stall_is_clamped_to_a_bounded_tick_burst() {
        let credit = Clock::musical_credit_ppqn_us(Clock::MAX_ELAPSED_NS);
        let (count, _) = Clock::tick_batch(0, credit, TEMPO_120);
        // 100 ms at 120 BPM is 0.2 quarter notes = 192 ticks, and no more —
        // the real gap could have been seconds.
        assert_eq!(count, 192);
    }

    #[test]
    fn aligned_tick_is_none_when_a_loop_wrap_leaves_phases_equal() {
        // The defining case. Playback wrapped to `region_start + 1`; the clock
        // free-ran one whole loop past it and sits at the same phase. Snapping
        // here would drag the clock back a loop every wrap and break
        // live-recording length, which measures a clock-tick difference.
        let start = time::bars_to_ticks(4);
        let len = time::bars_to_ticks(2);
        assert_eq!(
            Clock::aligned_tick(start + len + 1, start + 1, start, len),
            None
        );
        // Still true many loops in.
        assert_eq!(
            Clock::aligned_tick(start + len * 9 + 137, start + 137, start, len),
            None
        );
    }

    #[test]
    fn aligned_tick_corrects_a_sub_tolerance_phase_error_without_leaving_the_loop() {
        // Regression for the old `SYNC_TOLERANCE = 10`: a phase difference this
        // small was left uncorrected, leaving recorded input in a slightly
        // different coordinate space than playback. It is corrected now — but
        // by moving the clock the three ticks it is out, staying in the loop it
        // had free-run to.
        let start = time::bars_to_ticks(4);
        let len = time::bars_to_ticks(2);
        assert_eq!(
            Clock::aligned_tick(start + len + 100, start + 103, start, len),
            Some(start + len + 103)
        );
    }

    #[test]
    fn aligned_tick_corrects_a_real_seek_by_the_shorter_way_round() {
        // Half a region away: the correction takes the near side of the circle
        // and still lands on playback's phase, three loops in.
        let start = time::bars_to_ticks(4);
        let len = time::bars_to_ticks(2);
        let target = start + time::bars_to_ticks(1);
        let aligned = Clock::aligned_tick(start + len * 3 + 5, target, start, len).unwrap();

        assert_eq!(aligned, start + len * 3 + time::bars_to_ticks(1));
        assert_eq!(
            (aligned - start).rem_euclid(len),
            (target - start).rem_euclid(len)
        );
    }

    #[test]
    fn aligned_tick_keeps_the_free_running_loop_index_when_playback_lags_by_command_latency() {
        // The running-capture regression guard. A loop wrap sends
        // `AlignToPlayback` from the `"sequencer"` thread carrying a *snapshot*
        // of `playback_tick` (`region_start`, just wrapped); the `"clock"`
        // thread drains it a firing later with `clock_tick` already a few ticks
        // further on. Assigning `playback_tick` here would drag the clock back
        // into the region's first iteration on every single wrap, so every
        // cycle of a take would be stamped into the same tick span and the
        // capture crop window would keep all of them. Only the phase may move.
        let start = time::bars_to_ticks(4);
        let len = time::bars_to_ticks(2);

        for lag in 1..=8 {
            let clock = start + len * 3 + lag;
            let aligned = Clock::aligned_tick(clock, start, start, len).unwrap();

            assert_eq!(
                aligned,
                start + len * 3,
                "lag {lag} must cost only its own ticks"
            );
            assert_eq!(
                (aligned - start).div_euclid(len),
                3,
                "lag {lag} must not move the clock out of the loop it free-ran to"
            );
        }
    }

    #[test]
    fn aligned_tick_is_none_for_a_degenerate_region() {
        // A zero- or negative-length region has no phase space to compare in;
        // leave the free-running clock alone rather than storing into it.
        assert_eq!(Clock::aligned_tick(5_000, 10, 100, 0), None);
        assert_eq!(Clock::aligned_tick(5_000, 10, 100, -960), None);
    }

    #[test]
    fn aligned_tick_handles_a_position_before_region_start() {
        // `rem_euclid` keeps the phase non-negative on either side of the
        // region, so a clock or cursor behind `region_start` still compares.
        let start = time::bars_to_ticks(4);
        let len = time::bars_to_ticks(2);
        assert_eq!(
            Clock::aligned_tick(start - len + 7, start + 7, start, len),
            None
        );
        assert_eq!(
            Clock::aligned_tick(start + 7, start - len + 9, start, len),
            Some(start + 9)
        );
    }

    #[test]
    fn fractional_credit_carries_so_no_musical_time_is_lost() {
        // 10 firings of exactly 1 ms at 120 BPM: 10 ms = 0.02 beat = 19.2
        // ticks. The leftover after each firing must roll into the next so the
        // running total lands on 19, not 10 (which flooring each firing gives).
        let credit = Clock::musical_credit_ppqn_us(1_000_000);
        let mut frac = 0;
        let mut total = 0;
        for _ in 0..10 {
            let (count, next) = Clock::tick_batch(frac, credit, TEMPO_120);
            total += count;
            frac = next;
        }
        assert_eq!(total, 19);
    }
}
