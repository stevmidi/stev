//! The overrun journal — a post-mortem log line for every audio block that
//! missed its deadline, written to a file off the audio thread.
//!
//! The header's `OVR`/`XRUN` chips say *that* a block ran over; this says
//! *what was going on when it did*: the block size and budget, how far over the
//! render went, and how that time split across sources and instrument tracks.
//! An overrun is one block out of ~187 a second and nobody is watching the
//! meter at that instant, so a file that can be read afterwards is the only way
//! to turn "it glitched once in a ten-minute take" into "track 3 took 4.9 of
//! the 5.3 ms".
//!
//! Real-time discipline: the audio callback builds one fixed-size `Copy`
//! [`OverrunRecord`] and pushes it onto an `rtrb` ring — no allocation, no
//! lock, no formatting. The `"audio-journal"` thread drains the ring every
//! quarter second, formats, and appends to the log file. A sustained overload
//! would produce a record per block, so a drain that finds more than
//! [`BURST_LINES`] records writes that many and one summary line for the rest;
//! the ring itself is bounded and drops on overflow, which the summary also
//! reports.
//!
//! Release builds keep all of this: unlike `dprintln!`, the journal is meant
//! for the build that is actually played. It costs nothing while no overrun
//! happens.

use std::cmp::Reverse;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};
use rtrb::{Consumer, Producer, RingBuffer};

use super::INSTRUMENTS_SOURCE;
use crate::core::config::MAX_TRACKS;

/// Records the ring can hold between drains. At one record per block that is
/// ~a third of a second of continuous overload at 256 / 48 kHz; anything past
/// it is dropped and counted.
const RING_CAPACITY: usize = 64;

/// Most sources one record can name — the engine's fixed source list is far
/// smaller (click + instruments); the rest is headroom.
pub(crate) const MAX_JOURNAL_SOURCES: usize = 8;

/// Lines written per drain before the rest of that drain collapses into one
/// summary line.
const BURST_LINES: usize = 8;

/// How often the journal thread wakes to drain the ring.
const DRAIN_INTERVAL: Duration = Duration::from_millis(250);

/// The file is rotated to `.1` (replacing any previous `.1`) once it passes
/// this size, so a long-forgotten log can't grow without bound.
const ROTATE_AT_BYTES: u64 = 4 * 1024 * 1024;

/// Everything the audio thread knows about one overrun, fixed-size and `Copy`
/// so building and queuing it is real-time safe. Names are `&'static str`
/// ([`AudioSource::name`](super::AudioSource::name)) so no string travels
/// across the ring.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OverrunRecord {
    /// Wall-clock time the block finished rendering.
    pub(crate) at: SystemTime,
    /// Frames in the callback buffer.
    pub(crate) frames: u32,
    /// Device sample rate.
    pub(crate) sample_rate: f32,
    /// Whole-callback render time, in microseconds.
    pub(crate) total_us: u32,
    /// Per-source render time, `(name, microseconds)`, in source order; only
    /// the first `source_count` entries are meaningful.
    pub(crate) sources: [(&'static str, u32); MAX_JOURNAL_SOURCES],
    /// How many of `sources` are filled.
    pub(crate) source_count: u8,
    /// Per-instrument-track render time in microseconds, indexed by track;
    /// zero for a track that didn't render this block.
    pub(crate) tracks_us: [u32; MAX_TRACKS],
}

impl OverrunRecord {
    /// The block's real-time budget in microseconds.
    fn budget_us(&self) -> u32 {
        if self.sample_rate <= 0.0 {
            return 0;
        }
        (f64::from(self.frames) / f64::from(self.sample_rate) * 1e6).round() as u32
    }
}

/// Producer half held by the audio callback; `push` is the whole API.
pub(crate) struct JournalWriter {
    /// The ring's producer end.
    tx: Producer<OverrunRecord>,
    /// Records that found the ring full, for the drain thread to report.
    dropped: Arc<AtomicU32>,
}

impl JournalWriter {
    /// (audio thread) Queues one record; counted as dropped if the ring is
    /// full, and the drain thread's next summary line says how many.
    pub(crate) fn push(&mut self, record: OverrunRecord) {
        if self.tx.push(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Spawns the `"audio-journal"` thread and returns the audio thread's writer.
/// The log file is not touched until the first record arrives, so a session
/// with no overruns leaves no trace.
pub(crate) fn start() -> JournalWriter {
    let (tx, rx) = RingBuffer::new(RING_CAPACITY);
    let dropped = Arc::new(AtomicU32::new(0));
    let drain_dropped = dropped.clone();
    thread::Builder::new()
        .name("audio-journal".to_string())
        .spawn(move || drain_loop(rx, drain_dropped))
        .expect("failed to spawn audio-journal thread");
    JournalWriter { tx, dropped }
}

/// Where the journal lives: `~/Library/Logs/stev/` on macOS, the
/// platform's local-data directory (`…/stev/logs/`) elsewhere, falling
/// back to the current directory.
pub(crate) fn journal_path() -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs/stev"))
    } else {
        dirs::data_local_dir().map(|d| d.join("stev/logs"))
    };
    dir.unwrap_or_else(|| PathBuf::from("."))
        .join("audio-overruns.log")
}

/// The journal thread body: wake, drain, write, repeat — for as long as the
/// audio thread holds the producer.
fn drain_loop(mut rx: Consumer<OverrunRecord>, dropped: Arc<AtomicU32>) {
    let mut file: Option<File> = None;
    let mut wrote_header = false;
    let mut batch: Vec<OverrunRecord> = Vec::new();
    loop {
        thread::sleep(DRAIN_INTERVAL);
        batch.clear();
        while let Ok(record) = rx.pop() {
            batch.push(record);
        }
        if batch.is_empty() {
            if rx.is_abandoned() {
                return;
            }
            continue;
        }

        if file.is_none() {
            file = open_journal().ok();
        }
        // Unwritable location: keep draining so the ring never backs up, and
        // try again next time.
        let Some(out) = file.as_mut() else {
            continue;
        };
        if !wrote_header {
            let _ = writeln!(out, "{}", session_header(&batch[0]));
            wrote_header = true;
        }
        let (write, suppressed) = plan_burst(batch.len());
        for record in &batch[..write] {
            let _ = writeln!(out, "{}", format_record(record));
        }
        // Whatever the ring couldn't hold since the last drain belongs to this
        // same burst, so it is reported on the same summary line.
        let suppressed = suppressed + dropped.swap(0, Ordering::Relaxed) as usize;
        if suppressed > 0 {
            let _ = writeln!(
                out,
                "{}  … {suppressed} more overrun(s) in this burst",
                timestamp(batch[batch.len() - 1].at)
            );
        }
        let _ = out.flush();
    }
}

/// Opens the log for appending, creating its directory and rotating a file
/// that has outgrown [`ROTATE_AT_BYTES`].
fn open_journal() -> Result<File, io::Error> {
    let path = journal_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_AT_BYTES) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// How many records of a `count`-record drain to write in full, and how many
/// to fold into the summary line. Pure, unit-tested.
fn plan_burst(count: usize) -> (usize, usize) {
    let write = count.min(BURST_LINES);
    (write, count - write)
}

/// The once-per-session line that precedes the first record: what build, and
/// what block size / rate the stream opened at.
fn session_header(first: &OverrunRecord) -> String {
    format!(
        "--- Stev {} session, {} frames @ {} Hz (budget {} µs) ---",
        env!("CARGO_PKG_VERSION"),
        first.frames,
        first.sample_rate,
        first.budget_us()
    )
}

/// Local wall-clock time with milliseconds.
fn timestamp(at: SystemTime) -> String {
    let local: DateTime<Local> = at.into();
    local.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// One journal line. Pure, unit-tested. Reads as:
///
/// `<time>  OVERRUN  256f  used 6120/5333 µs (115%)  | click 3 µs |
/// instruments 6050 µs [t3 5900, t1 120] | other 67 µs`
///
/// `other` is the callback time no source accounts for — the mix-down, format
/// conversion and any worker-pool hand-off — so a spike there rather than in a
/// source is itself a finding (see `170-multicore-scheduling.md`).
fn format_record(r: &OverrunRecord) -> String {
    let budget_us = r.budget_us();
    let pct = if budget_us == 0 {
        0
    } else {
        (u64::from(r.total_us) * 100 / u64::from(budget_us)) as u32
    };
    let mut line = format!(
        "{}  OVERRUN  {}f  used {}/{} µs ({pct}%)",
        timestamp(r.at),
        r.frames,
        r.total_us,
        budget_us
    );

    let mut accounted: u32 = 0;
    for (name, us) in &r.sources[..usize::from(r.source_count).min(MAX_JOURNAL_SOURCES)] {
        accounted = accounted.saturating_add(*us);
        let _ = write!(line, "  | {name} {us} µs");
        if *name == INSTRUMENTS_SOURCE {
            line.push_str(&format_tracks(&r.tracks_us));
        }
    }
    let other = r.total_us.saturating_sub(accounted);
    let _ = write!(line, "  | other {other} µs");
    line
}

/// ` [t3 5900, t1 120]` — the tracks that rendered, heaviest first, 1-based
/// as the arranger numbers them; empty when none did.
fn format_tracks(tracks_us: &[u32; MAX_TRACKS]) -> String {
    let mut busy: Vec<(usize, u32)> = tracks_us
        .iter()
        .enumerate()
        .filter(|(_, us)| **us > 0)
        .map(|(track, us)| (track, *us))
        .collect();
    if busy.is_empty() {
        return String::new();
    }
    busy.sort_by_key(|&(track, us)| (Reverse(us), track));
    let mut out = String::from(" [");
    for (i, (track, us)) in busy.iter().enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        let _ = write!(out, "{sep}t{} {us}", track + 1);
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    /// A fixed instant, so records are reproducible.
    fn at_epoch_secs(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn record() -> OverrunRecord {
        let mut sources = [("", 0); MAX_JOURNAL_SOURCES];
        sources[0] = ("click", 3);
        sources[1] = ("instruments", 6050);
        let mut tracks_us = [0; MAX_TRACKS];
        tracks_us[0] = 120;
        tracks_us[2] = 5900;
        OverrunRecord {
            at: at_epoch_secs(1_700_000_000),
            frames: 256,
            sample_rate: 48_000.0,
            total_us: 6120,
            sources,
            source_count: 2,
            tracks_us,
        }
    }

    #[test]
    fn budget_is_frames_over_rate() {
        assert_eq!(record().budget_us(), 5333);
        let mut r = record();
        r.sample_rate = 0.0;
        assert_eq!(r.budget_us(), 0);
    }

    #[test]
    fn the_line_names_every_part_and_the_remainder() {
        let line = format_record(&record());
        assert!(
            line.contains("OVERRUN  256f  used 6120/5333 µs (114%)"),
            "{line}"
        );
        assert!(line.contains("| click 3 µs"), "{line}");
        assert!(
            line.contains("| instruments 6050 µs [t3 5900, t1 120]"),
            "{line}"
        );
        // 6120 − (3 + 6050) = 67
        assert!(line.ends_with("| other 67 µs"), "{line}");
    }

    #[test]
    fn tracks_are_listed_heaviest_first_one_based_and_only_when_busy() {
        let mut tracks = [0; MAX_TRACKS];
        assert_eq!(format_tracks(&tracks), "");
        tracks[1] = 10;
        tracks[3] = 40;
        tracks[0] = 40;
        // Ties keep track order.
        assert_eq!(format_tracks(&tracks), " [t1 40, t4 40, t2 10]");
    }

    #[test]
    fn sources_past_the_count_are_ignored_and_other_never_underflows() {
        let mut r = record();
        r.source_count = 1; // only the click counts
        let line = format_record(&r);
        assert!(!line.contains("instruments"), "{line}");
        r.total_us = 1; // less than the sources claim
        assert!(format_record(&r).ends_with("| other 0 µs"));
    }

    #[test]
    fn a_zero_budget_record_formats_without_dividing_by_zero() {
        let mut r = record();
        r.sample_rate = 0.0;
        assert!(format_record(&r).contains("used 6120/0 µs (0%)"));
    }

    #[test]
    fn small_drains_are_written_in_full_and_large_ones_summarised() {
        assert_eq!(plan_burst(0), (0, 0));
        assert_eq!(plan_burst(BURST_LINES), (BURST_LINES, 0));
        assert_eq!(plan_burst(BURST_LINES + 5), (BURST_LINES, 5));
    }

    #[test]
    fn the_session_header_states_the_stream_shape() {
        let header = session_header(&record());
        assert!(
            header.contains("256 frames @ 48000 Hz (budget 5333 µs)"),
            "{header}"
        );
    }

    #[test]
    fn the_journal_path_ends_with_the_log_name() {
        assert!(journal_path().ends_with("audio-overruns.log"));
    }
}
