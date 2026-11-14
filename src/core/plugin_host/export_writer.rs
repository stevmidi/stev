//! The `"audio-export"` thread (`130-plugin-host.md` § Audio export): spawned with the
//! plugin host, it waits for an [`AudioExportJob`], opens a WAV file per stem
//! plus the mix, has the engine render the export offline ([`ExportRender`],
//! `490-offline-export.md`), drains the mixer's export rings (`export.rs`)
//! into the files until the mixer reports the end, finishes them and tells
//! the `"sequencer"` thread how it went
//! (`SequencerCommand::AudioExportWritten`). On the way it publishes how far
//! it has got for the footer (`AudioExportAtomics::written_ms`; the UI
//! repaints on its own while the transport reads as running).
//!
//! The render waits for the writer, never the other way round: the tap holds
//! a block until its rings have room. The writer only gives a run up when
//! nothing has come for [`STALL_TIMEOUT`], and then says so
//! (`ExportShared::give_up`), so the render stops too.
//!
//! A file that fails to open or write stops being written, and what it got
//! stays on disk; its ring is still drained, so the rest of the export goes
//! on and the render never waits on it. Removing the files of a
//! cancelled or stopped run is the `"sequencer"` thread's, which knows about
//! the cancel.

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use rtrb::Consumer;

use crate::core::audio::{OfflineRun, StreamStatus, wav::WavWriter};
use crate::core::sequencer::{AudioExportJob, AudioExportOutcome, ExportStep, SequencerCommand};
use crate::core::shared_atomics::AudioExportAtomics;

use super::export::{CHANNELS, ExportRings, ExportShared};

/// How long the writer sleeps between passes over the rings. The render
/// runs as fast as the plugins compute and waits when a ring is full, so
/// this bounds its speed: a ring holds a second of audio.
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);

/// How long the mixer may send nothing, once the render has started, before
/// the writer decides it has stopped advancing. Far past any block.
const STALL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the writer waits for the render to start. The engine thread may
/// be in the middle of a restart when it is asked, opening a device that
/// takes its time, and then has its stream to drop: longer than any of
/// that, so only an engine that is gone runs it out.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the render waits for the `"sequencer"` thread's answer to a
/// step before giving the run up. Under [`STALL_TIMEOUT`], so it is the
/// render that says what went wrong, and the engine thread is held no
/// longer than the writer would have waited.
const STEP_TIMEOUT: Duration = Duration::from_secs(2);

/// One file being written: its writer, until an error stops it.
type OpenWav = Option<WavWriter<BufWriter<File>>>;

/// One export as the `"audio-engine"` thread renders it offline
/// ([`OfflineRun`], `490-offline-export.md` decision 3): before each block
/// it has the `"sequencer"` thread play the run's ticks due before it, and
/// it is done when the mixer has ended the export.
pub(super) struct ExportRender {
    /// The export's id.
    id: u32,
    /// The instant of the run's first tick.
    start: Instant,
    /// What the mixer and the writer say about the export.
    shared: Arc<ExportShared>,
    /// Steps to the `"sequencer"` thread.
    steps: Sender<ExportStep>,
    /// The answers' sender, cloned into each step, and their receiver. The
    /// run's own, so a late answer to another run's step can't be read here.
    answers: (Sender<bool>, Receiver<bool>),
}

impl ExportRender {
    /// The render of export `id`, its first tick at `start`.
    fn new(id: u32, start: Instant, shared: Arc<ExportShared>, steps: Sender<ExportStep>) -> Self {
        ExportRender {
            id,
            start,
            shared,
            steps,
            answers: bounded(1),
        }
    }
}

impl OfflineRun for ExportRender {
    fn start(&self) -> Instant {
        self.start
    }

    /// Asks the `"sequencer"` thread for the ticks due before `until` and
    /// waits for its answer. `false` when there is no run any more or the
    /// writer has given up; and when the step can't be sent or isn't
    /// answered within [`STEP_TIMEOUT`], which gives the export up here, so
    /// the writer stops at once and says so. The first step tells the writer
    /// the render has started.
    fn advance(&mut self, until: Instant) -> bool {
        self.shared.begin(self.id);
        if self.shared.gave_up(self.id) {
            return false;
        }
        let step = ExportStep {
            until,
            answer: self.answers.0.clone(),
        };
        let answer = self
            .steps
            .send(step)
            .ok()
            .and_then(|()| self.answers.1.recv_timeout(STEP_TIMEOUT).ok());
        if answer.is_none() {
            self.shared.give_up(self.id);
        }
        answer.unwrap_or(false)
    }

    fn is_done(&self) -> bool {
        self.shared.has_ended(self.id) || self.shared.gave_up(self.id)
    }
}

/// The writer's ends of everything it reads and reports through.
pub(super) struct ExportWriter {
    /// The mixer's export rings.
    rings: ExportRings,
    /// What the mixer reports besides the samples.
    shared: Arc<ExportShared>,
    /// The run's mirror, which the progress goes out on.
    progress: Arc<AudioExportAtomics>,
    /// The running stream: its rate, which each export is rendered and its
    /// files written at.
    stream: Arc<StreamStatus>,
    /// Handed to each export's [`ExportRender`].
    steps: Sender<ExportStep>,
    /// Has the engine render an export offline
    /// (`AudioControl::render_offline`).
    render: Box<dyn FnMut(ExportRender) + Send>,
    /// The last export stopped before the mixer ended it, so the mixer may
    /// have pushed more of it since.
    abandoned: bool,
}

/// Spawns the `"audio-export"` thread. It runs one job at a time and exits
/// when `jobs` disconnects.
pub(super) fn start_export_writer(
    jobs: Receiver<AudioExportJob>,
    mut writer: ExportWriter,
    done_tx: Sender<SequencerCommand>,
) {
    thread::Builder::new()
        .name("audio-export".to_string())
        .spawn(move || {
            for job in jobs {
                let outcome = writer.write(&job);
                done_tx
                    .send(SequencerCommand::AudioExportWritten(outcome))
                    .ok();
            }
        })
        .expect("failed to spawn audio-export thread");
}

impl ExportWriter {
    /// A writer on the mixer's `rings` and `shared` state, writing at the
    /// rate of `stream`, whose exports `render` has the engine render and
    /// step the sequencer through `steps`.
    pub(super) fn new(
        rings: ExportRings,
        shared: Arc<ExportShared>,
        progress: Arc<AudioExportAtomics>,
        stream: Arc<StreamStatus>,
        steps: Sender<ExportStep>,
        render: Box<dyn FnMut(ExportRender) + Send>,
    ) -> Self {
        ExportWriter {
            rings,
            shared,
            progress,
            stream,
            steps,
            render,
            abandoned: false,
        }
    }

    /// Writes one export: opens the files, has the engine render the run,
    /// drains the rings into the files until the mixer has ended the export
    /// — or nothing more comes — and finishes them.
    fn write(&mut self, job: &AudioExportJob) -> AudioExportOutcome {
        // Whatever a stopped export left behind. Its render was told
        // (`give_up`), and this one's hasn't been asked for yet.
        if self.abandoned {
            self.discard_leftovers();
        }
        // The rate the plugins run at, which the engine renders at.
        let sample_rate = self.stream.sample_rate();

        let mut error = None;
        let mut open = |path: &Path| {
            let opened = File::create(path)
                .and_then(|file| WavWriter::new(BufWriter::new(file), sample_rate));
            keep_first_error(&mut error, opened)
        };
        let mut stems: Vec<(usize, OpenWav)> = job
            .stems
            .iter()
            .map(|(slot, path)| (*slot, open(path)))
            .collect();
        let mut mix = open(&job.mix);

        // An engine that is gone renders nothing, which the stall below
        // finds.
        (self.render)(ExportRender::new(
            job.id,
            job.start,
            Arc::clone(&self.shared),
            self.steps.clone(),
        ));

        let mut frames = 0;
        let mut begun = false;
        let mut last_heard = Instant::now();
        let stopped = loop {
            // Read before draining: every sample pushed before the flag was
            // set is then in this pass.
            let ended = self.shared.has_ended(job.id);
            for (slot, wav) in &mut stems {
                drain(&mut self.rings.stems[*slot], wav, &mut error);
            }
            let mixed = drain(&mut self.rings.mix, &mut mix, &mut error);
            if mixed > 0 {
                frames += mixed;
                last_heard = Instant::now();
                let ms = (frames as f64 * 1000.0 / sample_rate) as u32;
                self.progress.written_ms.store(ms, Ordering::Relaxed);
            }
            if ended {
                break None;
            }
            // The stall clock starts when the render does: the engine may
            // have had other work to finish first.
            if !begun && self.shared.has_begun(job.id) {
                begun = true;
                last_heard = Instant::now();
            }
            // The render got no answer to a step and gave up itself.
            if self.shared.gave_up(job.id) {
                break Some(STALLED);
            }
            if let Some(reason) = stop_reason(begun, last_heard.elapsed()) {
                // The render stops pushing, and waiting for room.
                self.shared.give_up(job.id);
                break Some(reason);
            }
            thread::sleep(DRAIN_INTERVAL);
        };

        self.abandoned = stopped.is_some();
        for wav in stems.into_iter().map(|(_, wav)| wav).chain([mix]).flatten() {
            keep_first_error(&mut error, wav.finish());
        }
        AudioExportOutcome {
            stopped,
            error: error.map(|e| e.to_string()),
            silent: job
                .stems
                .iter()
                .map(|(slot, _)| *slot)
                .filter(|&slot| self.shared.is_silent(slot))
                .collect(),
        }
    }

    /// Empties every ring.
    fn discard_leftovers(&mut self) {
        for ring in self.rings.stems.iter_mut().chain([&mut self.rings.mix]) {
            drain::<File>(ring, &mut None, &mut None);
        }
    }
}

/// The footer's reason for a render that stopped advancing.
const STALLED: &str = "the render stalled";

/// Why a run should stop before its end, if it should, having heard nothing
/// for `quiet_for`: a render that has `begun` and sent nothing for
/// [`STALL_TIMEOUT`] has stopped advancing; one that hasn't begun within
/// [`START_TIMEOUT`] never will.
fn stop_reason(begun: bool, quiet_for: Duration) -> Option<&'static str> {
    if begun {
        (quiet_for >= STALL_TIMEOUT).then_some(STALLED)
    } else {
        (quiet_for >= START_TIMEOUT).then_some("the render never started")
    }
}

/// Moves everything in `ring` into `wav` and returns how many frames that
/// was. A write error is kept (the first one) and stops `wav`; the ring is
/// drained either way.
fn drain<W: Write + Seek>(
    ring: &mut Consumer<f32>,
    wav: &mut Option<WavWriter<W>>,
    error: &mut Option<io::Error>,
) -> usize {
    let Ok(chunk) = ring.read_chunk(ring.slots()) else {
        return 0;
    };
    let len = chunk.len();
    if let Some(writer) = wav {
        let (first, second) = chunk.as_slices();
        let written = writer
            .write_interleaved(first)
            .and_then(|()| writer.write_interleaved(second));
        if keep_first_error(error, written).is_none() {
            *wav = None;
        }
    }
    chunk.commit_all();
    len / CHANNELS
}

/// `result`'s value, or `None` with its error kept in `error` unless an
/// earlier one is there already.
fn keep_first_error<T>(error: &mut Option<io::Error>, result: io::Result<T>) -> Option<T> {
    result
        .map_err(|e| {
            error.get_or_insert(e);
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;

    use crossbeam_channel::unbounded;
    use rtrb::{Producer, RingBuffer};

    use crate::core::audio::wav::HEADER_LEN;
    use crate::core::sequencer::ExportMark;

    use super::super::export::{ExportTap, export_tap};
    use super::*;

    /// A writer at 1 kHz and the tap feeding it, with its marks ring. The
    /// engine it asks to render is none: a test pushes through the tap
    /// itself.
    fn writer() -> (ExportWriter, ExportTap, Producer<ExportMark>) {
        let (marks, marks_rx) = RingBuffer::new(4);
        let (tap, rings, shared) = export_tap(marks_rx, 1_000.0);
        let writer = ExportWriter::new(
            rings,
            shared,
            Arc::new(AudioExportAtomics::default()),
            Arc::new(StreamStatus::new(1_000)),
            unbounded().0,
            Box::new(|_| {}),
        );
        (writer, tap, marks)
    }

    /// Export `id` of slot 0, four frames long, pushed whole through `tap`.
    fn play(tap: &mut ExportTap, marks: &mut Producer<ExportMark>, id: u32) {
        let start = Instant::now();
        let end = start + Duration::from_millis(4);
        marks
            .push(ExportMark::Start {
                id,
                at: start,
                slots: 1,
            })
            .unwrap();
        marks.push(ExportMark::End { at: end }).unwrap();
        // Frames 0–3 of an eight-frame block.
        tap.take_marks(|at| if at == start { 0 } else { 4 });
        let span = tap.begin_block(0, 8, true);
        if let Some(span) = span.clone() {
            tap.tap_silence(0, span);
        }
        tap.end_block(span, 8);
    }

    /// A job for slot 0 in a fresh folder under the temp dir, and the folder.
    fn job(id: u32, tag: &str) -> (AudioExportJob, PathBuf) {
        let folder = std::env::temp_dir().join(format!("stev-export-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&folder).ok();
        fs::create_dir(&folder).unwrap();
        let job = AudioExportJob {
            id,
            start: Instant::now(),
            stems: vec![(0, folder.join("01 Synth.wav"))],
            mix: folder.join("Mix.wav"),
        };
        (job, folder)
    }

    fn wav_len(path: &Path) -> u64 {
        fs::metadata(path).unwrap().len()
    }

    #[test]
    fn a_finished_export_leaves_its_files_and_says_how_far_it_got() {
        let (mut writer, mut tap, mut marks) = writer();
        let (job, folder) = job(1, "written");
        play(&mut tap, &mut marks, 1);
        let outcome = writer.write(&job);
        assert_eq!(outcome.stopped, None);
        assert_eq!(outcome.error, None);
        let four_frames = u64::from(HEADER_LEN) + 4 * CHANNELS as u64 * 4;
        assert_eq!(wav_len(&job.stems[0].1), four_frames);
        assert_eq!(wav_len(&job.mix), four_frames);
        assert_eq!(writer.progress.written_ms.load(Ordering::Relaxed), 4);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn an_abandoned_exports_leftovers_are_discarded_and_its_end_is_its_own() {
        let (mut writer, mut tap, mut marks) = writer();
        // Export 1 ended after its writer gave up on it.
        play(&mut tap, &mut marks, 1);
        assert!(writer.shared.has_ended(1));
        assert!(!writer.shared.has_ended(2));
        writer.discard_leftovers();
        assert!(writer.rings.mix.is_empty());
        assert!(writer.rings.stems[0].is_empty());
    }

    #[test]
    fn a_run_stops_when_the_render_goes_quiet() {
        assert_eq!(stop_reason(true, Duration::from_millis(100)), None);
        assert_eq!(stop_reason(true, STALL_TIMEOUT), Some(STALLED));
    }

    #[test]
    fn a_render_slow_to_start_is_waited_for_longer_than_one_that_stalls() {
        // The engine thread finishing a restart before it takes the run.
        assert_eq!(stop_reason(false, STALL_TIMEOUT), None);
        assert_eq!(
            stop_reason(false, START_TIMEOUT),
            Some("the render never started")
        );
        // The render gives up on a step before the writer would on it.
        assert!(STEP_TIMEOUT < STALL_TIMEOUT);
    }

    #[test]
    fn an_export_the_render_gave_up_stops_the_writer_at_once() {
        let (mut writer, _tap, _marks) = writer();
        let (job, folder) = job(1, "given-up");
        // The render got no answer to a step.
        writer.shared.give_up(job.id);
        let started = Instant::now();
        let outcome = writer.write(&job);
        assert_eq!(outcome.stopped, Some(STALLED));
        assert!(started.elapsed() < STALL_TIMEOUT);
        fs::remove_dir_all(&folder).unwrap();
    }

    /// A render of export 1 and the `"sequencer"` thread's end of its steps.
    fn render() -> (ExportRender, Arc<ExportShared>, Receiver<ExportStep>) {
        let (_marks, marks_rx) = RingBuffer::new(4);
        let (_tap, _rings, shared) = export_tap(marks_rx, 1_000.0);
        let (steps, asked) = unbounded();
        let render = ExportRender::new(1, Instant::now(), Arc::clone(&shared), steps);
        (render, shared, asked)
    }

    #[test]
    fn the_render_waits_for_the_sequencers_answer_to_each_step() {
        let (mut render, _shared, asked) = render();
        let until = render.start() + Duration::from_millis(5);
        let sequencer = thread::spawn(move || {
            let step = asked.recv().unwrap();
            step.answer.send(true).unwrap();
            step.until
        });
        assert!(render.advance(until));
        assert_eq!(sequencer.join().unwrap(), until);
    }

    #[test]
    fn the_render_gives_up_when_the_run_is_gone() {
        let (mut render, _shared, asked) = render();
        let sequencer = thread::spawn(move || {
            asked.recv().unwrap().answer.send(false).unwrap();
        });
        assert!(!render.advance(render.start()));
        sequencer.join().unwrap();

        // No `"sequencer"` thread to ask: the render gives the export up
        // itself, so the writer hears of it.
        let (mut render, shared, asked) = self::render();
        drop(asked);
        assert!(!shared.has_begun(1));
        assert!(!render.advance(render.start()));
        assert!(shared.has_begun(1), "the first step says it started");
        assert!(shared.gave_up(1));
    }

    #[test]
    fn the_render_is_done_when_the_mixer_ended_it_or_the_writer_gave_up() {
        let (mut render, shared, asked) = render();
        assert!(!render.is_done());
        shared.give_up(2);
        assert!(!render.is_done(), "another export's");
        shared.give_up(1);
        assert!(render.is_done());
        // And asks for nothing more.
        assert!(!render.advance(render.start()));
        assert!(asked.try_recv().is_err());
    }

    #[test]
    fn drain_writes_across_the_rings_wrap() {
        let (mut tx, mut rx) = RingBuffer::new(4);
        let mut wav = Some(WavWriter::new(Cursor::new(Vec::new()), 48_000.0).unwrap());
        let mut error = None;
        tx.push_entire_slice(&[0.1, 0.2]).unwrap();
        drain(&mut rx, &mut wav, &mut error);
        // The next four wrap around the ring's end: two slices.
        tx.push_entire_slice(&[0.3, 0.4, 0.5, 0.6]).unwrap();
        drain(&mut rx, &mut wav, &mut error);
        assert!(error.is_none());
        assert_eq!(wav.as_ref().unwrap().frames(), 3);
        assert!(rx.is_empty());
    }

    #[test]
    fn a_failed_write_stops_the_file_but_still_drains() {
        /// Takes the header, then fails every write.
        struct Broken {
            /// Bytes it still takes.
            room: usize,
        }
        impl Write for Broken {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.room = self
                    .room
                    .checked_sub(buf.len())
                    .ok_or_else(|| io::Error::other("disk full"))?;
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Seek for Broken {
            fn seek(&mut self, _pos: io::SeekFrom) -> io::Result<u64> {
                Ok(0)
            }
        }
        let room = HEADER_LEN as usize;
        let mut wav = Some(WavWriter::new(Broken { room }, 48_000.0).unwrap());
        let (mut tx, mut rx) = RingBuffer::new(4);
        tx.push_entire_slice(&[0.1, 0.2]).unwrap();
        let mut error = None;
        drain(&mut rx, &mut wav, &mut error);
        assert!(wav.is_none());
        assert_eq!(error.unwrap().to_string(), "disk full");
        assert!(rx.is_empty());
    }

    #[test]
    fn the_first_error_is_the_one_kept() {
        let mut error = None;
        assert_eq!(keep_first_error(&mut error, Ok(1)), Some(1));
        assert_eq!(
            keep_first_error::<()>(&mut error, Err(io::Error::other("first"))),
            None
        );
        keep_first_error::<()>(&mut error, Err(io::Error::other("second")));
        assert_eq!(error.unwrap().to_string(), "first");
    }
}
