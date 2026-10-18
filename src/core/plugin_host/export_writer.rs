//! The `"audio-export"` thread (`300-export-to-audio.md`): spawned with the
//! plugin host, it waits for an [`AudioExportJob`], opens a WAV file per stem
//! plus the mix, drains the mixer's export rings (`export.rs`) into them
//! until the mixer reports the end, finishes the files and tells the
//! `"sequencer"` thread how it went (`SequencerCommand::AudioExportWritten`).
//!
//! A file that fails to open or write stops being written, and what it got
//! stays on disk; its ring is still drained, so the rest of the export goes
//! on and the mixer never fills up waiting.

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use rtrb::Consumer;

use crate::core::audio::wav::WavWriter;
use crate::core::sequencer::{AudioExportJob, AudioExportOutcome, SequencerCommand};

use super::export::{ExportRings, ExportShared};

/// How long the writer sleeps when the rings are drained and the export
/// hasn't ended. A tenth of the rings' second of slack.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// One file being written: its writer, until an error stops it.
type OpenWav = Option<WavWriter<BufWriter<File>>>;

/// Spawns the `"audio-export"` thread. It runs one job at a time and exits
/// when `jobs` disconnects.
pub(super) fn start_export_writer(
    jobs: Receiver<AudioExportJob>,
    rings: ExportRings,
    shared: Arc<ExportShared>,
    sample_rate: f64,
    done_tx: Sender<SequencerCommand>,
) {
    thread::Builder::new()
        .name("audio-export".to_string())
        .spawn(move || {
            let mut rings = rings;
            for job in jobs {
                let outcome = write_export(&job, &mut rings, &shared, sample_rate);
                done_tx
                    .send(SequencerCommand::AudioExportWritten(outcome))
                    .ok();
            }
        })
        .expect("failed to spawn audio-export thread");
}

/// Writes one export: opens the files, drains the rings into them until the
/// mixer has ended the export, then finishes them.
fn write_export(
    job: &AudioExportJob,
    rings: &mut ExportRings,
    shared: &ExportShared,
    sample_rate: f64,
) -> AudioExportOutcome {
    let mut error = None;
    let mut open = |path: &Path| {
        let opened =
            File::create(path).and_then(|file| WavWriter::new(BufWriter::new(file), sample_rate));
        keep_first_error(&mut error, opened)
    };
    let mut stems: Vec<(usize, OpenWav)> = job
        .stems
        .iter()
        .map(|(slot, path)| (*slot, open(path)))
        .collect();
    let mut mix = open(&job.mix);

    loop {
        // Read before draining: every sample pushed before the flag was set
        // is then in this pass.
        let ended = shared.has_ended();
        for (slot, wav) in &mut stems {
            drain(&mut rings.stems[*slot], wav, &mut error);
        }
        drain(&mut rings.mix, &mut mix, &mut error);
        if ended {
            break;
        }
        thread::sleep(DRAIN_INTERVAL);
    }

    for wav in stems.into_iter().map(|(_, wav)| wav).chain([mix]).flatten() {
        keep_first_error(&mut error, wav.finish());
    }
    let outcome = AudioExportOutcome {
        error: error.map(|e| e.to_string()),
        silent: job
            .stems
            .iter()
            .map(|(slot, _)| *slot)
            .filter(|&slot| shared.is_silent(slot))
            .collect(),
        overflowed: shared.overflowed(),
    };
    // Cleared here, not when the next job comes: by then its start mark may
    // already have armed the mixer. The rings are empty: the mixer pushes
    // nothing once it has ended.
    shared.reset();
    outcome
}

/// Moves everything in `ring` into `wav`. A write error is kept (the first
/// one) and stops `wav`; the ring is drained either way.
fn drain<W: Write + Seek>(
    ring: &mut Consumer<f32>,
    wav: &mut Option<WavWriter<W>>,
    error: &mut Option<io::Error>,
) {
    let Ok(chunk) = ring.read_chunk(ring.slots()) else {
        return;
    };
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
    use std::io::Cursor;

    use rtrb::RingBuffer;

    use crate::core::audio::wav::HEADER_LEN;

    use super::*;

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
