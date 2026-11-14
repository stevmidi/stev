//! The audio export rendered offline, end to end on one thread
//! (`490-offline-export.md`): a real [`Sequencer`] stepped by the engine's
//! offline loop, its events and marks through the [`InstrumentMixer`] and
//! the export tap, and what comes out of the writer's rings counted. No
//! plugin, no device, no `"sequencer"` thread: the run steps the sequencer
//! itself, as `EventHandlers::step_audio_export` does.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rtrb::{Consumer, RingBuffer};

use crate::core::audio::{AudioSource, OfflineRun, render_offline_for_test};
use crate::core::sequencer::test_support::{
    clip_with_marker_notes, rect, sequencer_with_mixer_feeds, stub_instrument,
};
use crate::core::sequencer::{Sequencer, StepAnswer};
use crate::core::shared_atomics::TrackMixAtomics;
use crate::core::time::{Meter, PPQN};

use super::export::{CHANNELS, ExportRings, ExportShared, export_tap};
use super::mixer::{InstrumentMixer, PluginHostCommand};
use super::shutdown::HostShutdown;
use super::test_voice::ConstantVoice;
use super::transport::TransportState;

const BAR: i32 = Meter::FOUR_FOUR.bars_to_ticks(1);

/// What the test voice plays on the left; the right is its negative.
const LEVEL: f32 = 0.5;

/// An export stepped on the rendering thread itself, and the writer's side
/// of it: the rings emptied before each block, what came out kept.
struct SteppedExport {
    /// The sequencer the run steps.
    sequencer: Sequencer,
    /// The run's first instant.
    start: Instant,
    /// The export's id.
    id: u32,
    /// What the mixer says about the export.
    shared: Arc<ExportShared>,
    /// The writer's ends of the rings.
    rings: ExportRings,
    /// The exported slot.
    slot: usize,
    /// Everything the stem's ring has given.
    stem: Vec<f32>,
    /// Everything the mix's ring has given.
    mix: Vec<f32>,
    /// How many blocks have been asked for.
    blocks: usize,
    /// The block Esc comes before, if it does.
    cancel_before: Option<usize>,
}

impl SteppedExport {
    /// Takes what the rings hold, as the writer does.
    fn drain(&mut self) {
        let take = |ring: &mut Consumer<f32>, into: &mut Vec<f32>| {
            into.extend(std::iter::from_fn(|| ring.pop().ok()));
        };
        take(&mut self.rings.stems[self.slot], &mut self.stem);
        take(&mut self.rings.mix, &mut self.mix);
    }
}

impl OfflineRun for SteppedExport {
    fn start(&self) -> Instant {
        self.start
    }

    fn advance(&mut self, until: Instant) -> bool {
        self.drain();
        if self.cancel_before == Some(self.blocks) {
            self.sequencer.cancel_audio_export();
        }
        self.blocks += 1;
        if self.sequencer.audio_export_step_answer() != StepAnswer::Advanced {
            return false;
        }
        while let Some(at) = self.sequencer.next_audio_export_tick(until) {
            self.sequencer.tick(at);
        }
        true
    }

    fn is_done(&self) -> bool {
        self.shared.has_ended(self.id)
    }
}

/// Exports ticks `[from, to)` of a one-plugin-track arrangement at
/// `tempo_us`, rendered offline at `rate` in blocks of `buffer`, Esc pressed
/// before block `cancel_before` if any. Returns the run, drained.
fn export(
    tempo_us: i32,
    (from, to): (i32, i32),
    rate: u32,
    buffer: u32,
    cancel_before: Option<usize>,
) -> SteppedExport {
    let (mut sequencer, clip_events, marks) = sequencer_with_mixer_feeds();
    sequencer.set_tempo(tempo_us);
    sequencer.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
    sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, to + BAR));
    let slot = sequencer.tracks()[0].slot();

    let rate_hz = f64::from(rate);
    let track_mix = Arc::new(TrackMixAtomics::new());
    track_mix.volume_db[slot].store(0.0f32.to_bits(), Ordering::Relaxed);
    let (mut commands, cmd_rx) = RingBuffer::new(4);
    let (tap, rings, shared) = export_tap(marks, rate_hz);
    let mixer = InstrumentMixer::new(
        cmd_rx,
        RingBuffer::new(4).0,
        clip_events,
        RingBuffer::new(4).1,
        TransportState::for_test(tempo_us, from),
        track_mix,
        Arc::new(HostShutdown::default()),
        tap,
        rate_hz,
    );
    commands
        .push(PluginHostCommand::Insert {
            track: slot,
            voice: Box::new(ConstantVoice::new(LEVEL)),
            sample_rate: rate_hz,
        })
        .ok()
        .unwrap();

    let plan = sequencer
        .audio_export_plan(Some(rect(from, to, 0, 0)))
        .unwrap();
    let start = Instant::now();
    let id = sequencer.begin_audio_export(plan, PathBuf::new(), start);
    sequencer.reset_to_tick(from);
    sequencer.audio_export_seeked();

    let mut run = SteppedExport {
        sequencer,
        start,
        id,
        shared,
        rings,
        slot,
        stem: Vec::new(),
        mix: Vec::new(),
        blocks: 0,
        cancel_before,
    };
    let sources = vec![Box::new(mixer) as Box<dyn AudioSource>];
    render_offline_for_test(sources, &mut run, rate_hz, buffer);
    run.drain();
    run
}

/// The frames a stretch of `ticks` at `tempo_us` comes to at `rate`: the
/// end tick's instant, in whole nanoseconds, on the sample grid.
fn frames_for(ticks: i32, tempo_us: i32, rate: u32) -> usize {
    let nanos = ticks as u64 * tempo_us as u64 * 1000 / PPQN as u64;
    (Duration::from_nanos(nanos).as_secs_f64() * f64::from(rate)).round() as usize
}

#[test]
fn an_offline_export_is_the_selection_to_the_sample() {
    // The measured case (`490-offline-export.md` § Measured): bars 1 to 6 at
    // 96 bpm, 48 kHz in buffers of 256, is 12.5 s, 600,000 frames. The
    // real-time run came out 4,759 short with overruns and 26 over without.
    let run = export(625_000, (0, 5 * BAR), 48_000, 256, None);
    assert_eq!(run.mix.len(), 600_000 * CHANNELS);
    assert_eq!(run.stem.len(), 600_000 * CHANNELS);
    // The voice was there from the first frame to the last, at unity.
    assert!(
        run.stem
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .all(|frame| *frame == [LEVEL, -LEVEL])
    );
    assert!(!run.shared.is_silent(run.slot));
}

#[test]
fn an_offline_export_is_exact_off_the_block_and_sample_grids_too() {
    // A selection starting and ending between beats, a tempo whose tick is
    // no whole number of nanoseconds, a rate it shares no factor with.
    for (rate, buffer) in [(44_100, 512), (48_000, 64), (96_000, 4096)] {
        let (from, to) = (7, 3 * BAR + 311);
        let run = export(487_805, (from, to), rate, buffer, None);
        let frames = frames_for(to - from, 487_805, rate);
        assert_eq!(run.mix.len(), frames * CHANNELS, "{rate} Hz / {buffer}");
        assert_eq!(run.stem.len(), frames * CHANNELS, "{rate} Hz / {buffer}");
    }
}

#[test]
fn the_block_size_does_not_change_the_length() {
    let lengths = [64, 256, 1024, 2048].map(|buffer| {
        export(500_000, (BAR, 2 * BAR), 48_000, buffer, None)
            .mix
            .len()
    });
    assert_eq!(lengths, [96_000 * CHANNELS; 4]);
}

#[test]
fn esc_ends_the_files_where_the_run_has_got_to() {
    let run = export(625_000, (0, 5 * BAR), 48_000, 256, Some(40));
    // Forty blocks were asked for before Esc: the files end on the tick
    // after the last one played, a block or so on, not on the selection's.
    let frames = run.mix.len() / CHANNELS;
    assert!((39 * 256..42 * 256).contains(&frames), "{frames} frames");
    assert_eq!(run.stem.len(), run.mix.len(), "every file the same length");
    assert!(run.shared.has_ended(run.id));
    assert!(run.blocks < 45, "and the render stops there");
}

#[test]
fn esc_before_the_first_block_ends_an_empty_export() {
    let run = export(625_000, (0, 5 * BAR), 48_000, 256, Some(0));
    assert!(run.mix.is_empty() && run.stem.is_empty());
    assert!(run.shared.has_ended(run.id));
    assert_eq!(run.blocks, 1);
}
