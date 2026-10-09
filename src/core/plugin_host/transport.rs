//! The transport state hosted plugins are told about, in two forms: the shared
//! atomics the mixer holds ([`TransportState`]) and the plain snapshot it takes
//! from them once per block ([`BlockTransport`]).
//!
//! The snapshot is deliberately format-neutral — app-native units, no plugin
//! types — so the mixer reads the atomics exactly once per block and each
//! [`InstrumentVoice`](super::voice::InstrumentVoice) implementation converts
//! it into whatever its own format wants (a CLAP `TransportEvent`, a VST3
//! `ProcessContext`). The conversion is a handful of float operations, utterly
//! negligible next to a plugin's `process()`, so doing it per voice rather than
//! once per block costs nothing and keeps every plugin type out of the mixer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, Ordering};

use crate::core::time::{MICROSECONDS_PER_MINUTE, Meter, ticks_to_beats_f64};

/// The transport atomics the mixer reads (once per sub-block). All live in
/// `SharedAtomics`; cloned in here so the audio thread never locks.
pub(crate) struct TransportState {
    /// Shared "transport running" flag.
    pub(crate) running: Arc<AtomicBool>,
    /// Shared tempo, µs per quarter.
    pub(crate) tempo_us: Arc<AtomicI32>,
    /// Shared project meter, packed by [`Meter::to_bits`].
    pub(crate) meter: Arc<AtomicU16>,
    /// Shared playback position, ticks.
    pub(crate) playback_tick: Arc<AtomicI32>,
    /// Shared loop-region start.
    pub(crate) region_start: Arc<AtomicI32>,
    /// Shared loop-region end.
    pub(crate) region_end: Arc<AtomicI32>,
    /// Shared "loop enabled" flag.
    pub(crate) loop_enabled: Arc<AtomicBool>,
}

impl TransportState {
    /// Reads every atomic once, for the block about to render.
    pub(crate) fn snapshot(&self) -> BlockTransport {
        BlockTransport {
            running: self.running.load(Ordering::Relaxed),
            looping: self.loop_enabled.load(Ordering::Relaxed),
            tempo_us: self.tempo_us.load(Ordering::Relaxed).max(1),
            meter: Meter::from_bits(self.meter.load(Ordering::Relaxed)),
            playback_tick: self.playback_tick.load(Ordering::Relaxed),
            region_start: self.region_start.load(Ordering::Relaxed),
            region_end: self.region_end.load(Ordering::Relaxed),
        }
    }
}

/// A once-per-block snapshot of [`TransportState`], in the app's own units.
/// Copied into each voice's render call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockTransport {
    /// Whether the transport is rolling.
    pub(crate) running: bool,
    /// Whether the loop region is active.
    pub(crate) looping: bool,
    /// Tempo, µs per quarter note. Never below 1 — `snapshot` clamps it, so
    /// [`bpm`](Self::bpm) can't divide by zero.
    pub(crate) tempo_us: i32,
    /// The project's time signature.
    pub(crate) meter: Meter,
    /// Playhead, in arrangement ticks. Tick-granular: it only advances once per
    /// sequencer tick, so a plugin polling it sees it step, not glide.
    pub(crate) playback_tick: i32,
    /// Loop-region start, in arrangement ticks.
    pub(crate) region_start: i32,
    /// Loop-region end, in arrangement ticks.
    pub(crate) region_end: i32,
}

impl BlockTransport {
    /// Tempo in beats per minute — what every plugin format asks for, rather
    /// than the µs-per-quarter the app stores.
    pub(crate) fn bpm(&self) -> f64 {
        f64::from(MICROSECONDS_PER_MINUTE) / f64::from(self.tempo_us)
    }

    /// Zero-based bar the playhead is in, in the project's meter. Floors
    /// (`div_euclid`), so a pre-roll tick lands in bar `-1` rather than
    /// rounding toward zero.
    pub(crate) fn bar_number(&self) -> i32 {
        self.playback_tick.div_euclid(self.meter.bar_ticks())
    }

    /// Where [`bar_number`](Self::bar_number)'s bar starts, in quarter notes
    /// (the "beats" every plugin format counts positions in).
    pub(crate) fn bar_start_beats(&self) -> f64 {
        ticks_to_beats_f64(self.meter.bars_to_ticks(self.bar_number()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tempo_us: i32, tick: i32) -> TransportState {
        TransportState {
            running: Arc::new(AtomicBool::new(true)),
            tempo_us: Arc::new(AtomicI32::new(tempo_us)),
            meter: Arc::new(AtomicU16::new(Meter::FOUR_FOUR.to_bits())),
            playback_tick: Arc::new(AtomicI32::new(tick)),
            region_start: Arc::new(AtomicI32::new(0)),
            region_end: Arc::new(AtomicI32::new(1920)),
            loop_enabled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn snapshot_reads_every_atomic() {
        let s = state(500_000, 960);
        assert_eq!(
            s.snapshot(),
            BlockTransport {
                running: true,
                looping: false,
                tempo_us: 500_000,
                meter: Meter::FOUR_FOUR,
                playback_tick: 960,
                region_start: 0,
                region_end: 1920,
            }
        );
    }

    #[test]
    fn snapshot_clamps_a_zero_tempo_so_bpm_cannot_divide_by_zero() {
        let s = state(0, 0);
        let snap = s.snapshot();
        assert_eq!(snap.tempo_us, 1);
        assert!(snap.bpm().is_finite());
    }

    #[test]
    fn bpm_converts_microseconds_per_quarter() {
        // 500_000 µs per quarter is the canonical 120 BPM.
        assert!((state(500_000, 0).snapshot().bpm() - 120.0).abs() < 1e-9);
        assert!((state(1_000_000, 0).snapshot().bpm() - 60.0).abs() < 1e-9);
    }

    #[test]
    fn bar_number_and_start_at_two_bars_in() {
        let snap = state(500_000, 2 * 4 * 960 + 10).snapshot();
        assert_eq!(snap.bar_number(), 2);
        assert!((snap.bar_start_beats() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn bar_number_floors_for_a_negative_playhead() {
        // `div_euclid`, not `/` — a pre-roll tick must not round toward zero.
        let snap = state(500_000, -1).snapshot();
        assert_eq!(snap.bar_number(), -1);
        assert!((snap.bar_start_beats() - -4.0).abs() < 1e-9);
    }

    #[test]
    fn bar_number_and_start_follow_a_three_four_meter() {
        let s = state(500_000, 2 * 3 * 960 + 10);
        s.meter
            .store(Meter::new(3, 4).unwrap().to_bits(), Ordering::Relaxed);
        let snap = s.snapshot();
        assert_eq!(snap.bar_number(), 2);
        assert!((snap.bar_start_beats() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn bar_start_counts_quarters_in_an_eighth_note_meter() {
        // 7/8 bars are 3.5 quarters: bar 2 starts on quarter 7.
        let s = state(500_000, 7 * 960);
        s.meter
            .store(Meter::new(7, 8).unwrap().to_bits(), Ordering::Relaxed);
        let snap = s.snapshot();
        assert_eq!(snap.bar_number(), 2);
        assert!((snap.bar_start_beats() - 7.0).abs() < 1e-9);
    }
}
