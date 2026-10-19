//! The `"audio-export"` thread (`300-export-to-audio.md`): spawned with the
//! plugin host, it waits for an [`AudioExportJob`], opens a WAV file per stem
//! plus the mix, drains the mixer's export rings (`export.rs`) into them
//! until the mixer reports the end, finishes the files and tells the
//! `"sequencer"` thread how it went (`SequencerCommand::AudioExportWritten`).
//! On the way it publishes how far it has got for the footer
//! (`AudioExportAtomics::written_ms`; the UI repaints on its own while the
//! transport runs). It stops early, reporting why, if the audio output
//! changes or stops.
//!
//! A file that fails to open or write stops being written, and what it got
//! stays on disk; its ring is still drained, so the rest of the export goes
//! on and the mixer never fills up waiting. Removing the files of a
//! cancelled or stopped run is the `"sequencer"` thread's, which knows about
//! the cancel.

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use rtrb::Consumer;

use crate::core::audio::{AudioLoad, wav::WavWriter};
use crate::core::sequencer::{AudioExportJob, AudioExportOutcome, SequencerCommand};
use crate::core::shared_atomics::AudioExportAtomics;

use super::export::{CHANNELS, ExportRings, ExportShared};

/// How long the writer sleeps when the rings are drained and the export
/// hasn't ended. A tenth of the rings' second of slack.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long the mixer may send nothing before the writer decides the audio
/// output has stopped. Far past the start mark's latency (a few
/// milliseconds) and any block.
const STALL_TIMEOUT: Duration = Duration::from_secs(3);

/// One file being written: its writer, until an error stops it.
type OpenWav = Option<WavWriter<BufWriter<File>>>;

/// The writer's ends of everything it reads and reports through.
pub(super) struct ExportWriter {
    /// The mixer's export rings.
    rings: ExportRings,
    /// What the mixer reports besides the samples.
    shared: Arc<ExportShared>,
    /// The run's mirror, which the progress goes out on.
    progress: Arc<AudioExportAtomics>,
    /// The engine's meter: its overrun, xrun and stream-error counts.
    load: Arc<AudioLoad>,
    /// The device rate the files are written at.
    sample_rate: f64,
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
    /// A writer on the mixer's `rings` and `shared` state, writing at
    /// `sample_rate`.
    pub(super) fn new(
        rings: ExportRings,
        shared: Arc<ExportShared>,
        progress: Arc<AudioExportAtomics>,
        load: Arc<AudioLoad>,
        sample_rate: f64,
    ) -> Self {
        ExportWriter {
            rings,
            shared,
            progress,
            load,
            sample_rate,
            abandoned: false,
        }
    }

    /// Writes one export: opens the files, drains the rings into them until
    /// the mixer has ended the export — or the audio output went wrong — and
    /// finishes them.
    fn write(&mut self, job: &AudioExportJob) -> AudioExportOutcome {
        // Whatever a stopped export left behind. The job comes before the
        // run plays, so its own start mark is still to come.
        if self.abandoned {
            self.discard_leftovers();
        }
        // The counts read "this export", like the header's do "this take".
        self.load.reset_counts();
        let stream_errors = self.load.stream_errors();

        let mut error = None;
        let mut open = |path: &Path| {
            let opened = File::create(path)
                .and_then(|file| WavWriter::new(BufWriter::new(file), self.sample_rate));
            keep_first_error(&mut error, opened)
        };
        let mut stems: Vec<(usize, OpenWav)> = job
            .stems
            .iter()
            .map(|(slot, path)| (*slot, open(path)))
            .collect();
        let mut mix = open(&job.mix);

        let mut frames = 0;
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
                let ms = (frames as f64 * 1000.0 / self.sample_rate) as u32;
                self.progress.written_ms.store(ms, Ordering::Relaxed);
            }
            if ended {
                break None;
            }
            let errors_changed = self.load.stream_errors() != stream_errors;
            if let Some(reason) = stop_reason(errors_changed, last_heard.elapsed()) {
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
            overflowed: self.shared.overflowed(),
            overruns: self.load.overruns(),
            xruns: self.load.xruns(),
        }
    }

    /// Empties every ring.
    fn discard_leftovers(&mut self) {
        for ring in self.rings.stems.iter_mut().chain([&mut self.rings.mix]) {
            drain::<File>(ring, &mut None, &mut None);
        }
    }
}

/// Why a run should stop before its end, if it should: the stream reported an
/// error (the device went away or changed), or the mixer has sent nothing for
/// [`STALL_TIMEOUT`] (the output stopped).
fn stop_reason(stream_errors_changed: bool, quiet_for: Duration) -> Option<&'static str> {
    if stream_errors_changed {
        Some("the audio device changed")
    } else if quiet_for >= STALL_TIMEOUT {
        Some("the audio output stopped")
    } else {
        None
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

    use rtrb::{Producer, RingBuffer};

    use crate::core::audio::wav::HEADER_LEN;
    use crate::core::sequencer::ExportMark;

    use super::super::export::{ExportTap, export_tap};
    use super::*;

    /// A writer at 1 kHz and the tap feeding it, with its marks ring.
    fn writer() -> (ExportWriter, ExportTap, Producer<ExportMark>) {
        let (marks, marks_rx) = RingBuffer::new(4);
        let (tap, rings, shared) = export_tap(marks_rx, 1_000.0);
        let writer = ExportWriter::new(
            rings,
            shared,
            Arc::new(AudioExportAtomics::default()),
            Arc::new(AudioLoad::new()),
            1_000.0,
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
        let span = tap.begin_block(0, 8);
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
    fn a_run_stops_when_the_stream_errs_or_goes_quiet() {
        assert_eq!(stop_reason(false, Duration::from_millis(100)), None);
        assert_eq!(
            stop_reason(true, Duration::ZERO),
            Some("the audio device changed")
        );
        assert_eq!(
            stop_reason(false, STALL_TIMEOUT),
            Some("the audio output stopped")
        );
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
