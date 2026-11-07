//! The recovery timing: an ignored release test that times the crash
//! recovery check and write on synthetic projects of growing size, as
//! `410-crash-recovery.md` decisions 3 to 5 weighed them. It never ships and never runs in
//! CI, but `clippy --all-targets` compiles it, so it can't rot.
//!
//! Run with `cargo test --release recovery_timing -- --ignored --nocapture`.
//!
//! Every project is the stress project (`stress`) at the 16-track maximum
//! with [`EVENT_COUNTS`] events and [`BLOB_MEGABYTES`] of plugin state in
//! total. The check is the
//! [`fingerprint`] and the snapshot is [`ProjectSnapshot::of`], the two that
//! run on the `"sequencer"` thread. The write adds what the `"recovery"`
//! thread does: the encoding, the JSON and `replace_file`.
//!
//! The rounds run back to back, so the caches are warm and no audio is
//! playing, which a check every 5 s in the running app can't count on: read
//! these as a floor. A timing log on a real project confirmed them (the
//! figures are in that doc's § Measurements).

use std::env::consts::{ARCH, OS};
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::core::config::MAX_TRACKS;
use crate::core::paths::scratch_dir;
use crate::core::sequencer::Sequencer;
use crate::core::sequencer::test_support::test_sequencer;

use super::dto::{ProjectData, ProjectSnapshot};
use super::fingerprint::fingerprint;
use super::storage::replace_file;
use super::stress::{MEGABYTE, fill_stress_project};

/// Total events in a project, across all its clips. Each divides evenly into
/// the clips; about 100,000 is the design's stress case.
const EVENT_COUNTS: [usize; 5] = [2_000, 10_000, 50_000, 100_000, 200_000];

/// Total plugin state in a project, in megabytes, split evenly over the
/// tracks; about 50 is the design's stress case.
const BLOB_MEGABYTES: [usize; 3] = [0, 5, 50];

/// Timed rounds per project.
const ROUNDS: usize = 11;

/// The maximum track count with `events` events and `blob_bytes` of plugin
/// state in total: the stress project at that size.
fn project(events: usize, blob_bytes: usize) -> Sequencer {
    let mut sequencer = test_sequencer();
    fill_stress_project(&mut sequencer, events, blob_bytes);
    sequencer
}

/// How long `work` took, and what it returned.
fn timed<T>(work: impl FnOnce() -> T) -> (Duration, T) {
    let start = Instant::now();
    let result = work();
    (start.elapsed(), result)
}

/// The middle of `times`, in milliseconds.
fn median_ms(times: &mut [Duration]) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1000.0
}

/// The longest of `times`, in milliseconds.
fn max_ms(times: &[Duration]) -> f64 {
    times.iter().max().map_or(0.0, Duration::as_secs_f64) * 1000.0
}

/// One project's timings over [`ROUNDS`], each stage kept per round.
#[derive(Default)]
struct Timings {
    /// [`fingerprint`].
    check: Vec<Duration>,
    /// [`ProjectSnapshot::of`].
    snapshot: Vec<Duration>,
    /// [`ProjectSnapshot::encoded`].
    encode: Vec<Duration>,
    /// The JSON.
    json: Vec<Duration>,
    /// `replace_file`.
    disk: Vec<Duration>,
    /// The snapshot, the encoding, the JSON and the disk.
    write: Vec<Duration>,
    /// How long the last JSON was, in bytes.
    file_bytes: usize,
}

impl Timings {
    /// Runs the rounds on `sequencer`, writing to `path`.
    fn measure(sequencer: &Sequencer, path: &Path) -> Timings {
        let mut timings = Timings::default();
        for _ in 0..ROUNDS {
            let (check, hash) = timed(|| fingerprint(sequencer));
            black_box(hash);
            let (snapshot, raw) = timed(|| ProjectSnapshot::of(sequencer));
            let (encode, data) = timed(|| raw.encoded());
            let (json, text) = timed(|| serde_json::to_string(&data).unwrap());
            let (disk, written) = timed(|| replace_file(path, &text));
            written.unwrap();
            timings.file_bytes = text.len();
            timings.check.push(check);
            timings.snapshot.push(snapshot);
            timings.encode.push(encode);
            timings.json.push(json);
            timings.disk.push(disk);
            timings.write.push(snapshot + encode + json + disk);
        }
        timings
    }

    /// This project's row of the report.
    fn row(&mut self, events: usize, blob_megabytes: usize) -> String {
        format!(
            "{events:>7} {blob_megabytes:>6} {:>6.1} {:>8.2} {:>9.2} {:>8.2} {:>9.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>9.2}",
            self.file_bytes as f64 / MEGABYTE as f64,
            median_ms(&mut self.check),
            max_ms(&self.check),
            median_ms(&mut self.snapshot),
            max_ms(&self.snapshot),
            median_ms(&mut self.encode),
            median_ms(&mut self.json),
            median_ms(&mut self.disk),
            median_ms(&mut self.write),
            max_ms(&self.write),
        )
    }
}

/// The projects a report covers hold the events they were asked for.
#[test]
fn a_timing_project_holds_its_events_and_blobs() {
    let sequencer = project(EVENT_COUNTS[0], MAX_TRACKS * 3);
    let data = ProjectData::from_sequencer(&sequencer);
    assert_eq!(data.tracks.len(), MAX_TRACKS);
    let events: usize = data
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.events.len())
        .sum();
    assert_eq!(events, EVENT_COUNTS[0]);
    let json = serde_json::to_string(&data).unwrap();
    assert_eq!(json.matches(r#""state":"Wlpa""#).count(), MAX_TRACKS);
}

/// Prints the report. See the module docs.
#[test]
#[ignore = "a measurement, not a check: run it by hand, in release"]
fn recovery_timing() {
    let dir = scratch_dir("recovery-timing");
    let path = dir.join("copy.json");

    // `println!`, not the log: this is a test, which installs no logger, and
    // the report on stdout is its whole point.
    println!();
    println!("recovery timing: {OS} {ARCH}, {MAX_TRACKS} tracks, {ROUNDS} rounds, medians in ms");
    println!(
        "{:>7} {:>6} {:>6} {:>8} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "events",
        "blobMB",
        "fileMB",
        "CHECK",
        "check max",
        "snapshot",
        "snap max",
        "encode",
        "json",
        "disk",
        "WRITE",
        "write max",
    );
    for blob_megabytes in BLOB_MEGABYTES {
        for events in EVENT_COUNTS {
            let sequencer = project(events, blob_megabytes * MEGABYTE);
            let mut timings = Timings::measure(&sequencer, &path);
            println!("{}", timings.row(events, blob_megabytes));
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}
