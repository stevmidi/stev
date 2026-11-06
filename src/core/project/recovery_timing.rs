//! The recovery timing: an ignored release test that times the crash
//! recovery check and write on synthetic projects of growing size, to settle
//! `410-crash-recovery.md` decisions 3 to 5. It never ships and never runs in
//! CI, but `clippy --all-targets` compiles it, so it can't rot.
//!
//! Run with `cargo test --release recovery_timing -- --ignored --nocapture`.
//!
//! Every project is the 16-track maximum with [`EVENT_COUNTS`] events and
//! [`BLOB_MEGABYTES`] of plugin state in total. The check is the snapshot and
//! the JSON hash, as the check stood before decision 4; the direct column is
//! the [`fingerprint`] that replaced it. The write is the snapshot, the
//! pretty JSON and `replace_file`, as a save does them.
//!
//! The rounds run back to back, so the caches are warm and no audio is
//! playing, which a check every 5 s in the running app can't count on: read
//! these as a floor. A timing log on a real project confirmed them (the
//! figures are in that doc's § Measurements).

use std::env::consts::{ARCH, OS};
use std::env::temp_dir;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use crate::core::config::MAX_TRACKS;
use crate::core::sequencer::Sequencer;
use crate::core::sequencer::test_support::{bend_at, clip_at, param_at, test_sequencer};
use crate::core::xorshift::XorShift;
use crate::models::event::Event;
use crate::models::track::{InstrumentRef, TrackOutput};

use super::dto::ProjectData;
use super::fingerprint::{fingerprint, json_fingerprint};
use super::storage::replace_file;

/// Total events in a project, across all its clips. Each divides evenly into
/// the clips; about 100,000 is the design's stress case.
const EVENT_COUNTS: [usize; 5] = [2_000, 10_000, 50_000, 100_000, 200_000];

/// Total plugin state in a project, in megabytes, split evenly over the
/// tracks; about 50 is the design's stress case.
const BLOB_MEGABYTES: [usize; 3] = [0, 5, 50];

/// Clips on each track.
const CLIPS_PER_TRACK: usize = 5;

/// Ticks between one event and the next in a clip.
const EVENT_SPACING_TICKS: i32 = 24;

/// A note's length in ticks.
const NOTE_LENGTH_TICKS: i32 = 240;

/// Timed rounds per project.
const ROUNDS: usize = 11;

/// Bytes in a megabyte, as [`BLOB_MEGABYTES`] counts them.
const MEGABYTE: usize = 1_000_000;

/// Steps of a parameter move's value: enough that a value writes as many
/// digits as a real one.
const PARAM_VALUE_STEPS: u64 = 1 << 20;

/// The `index`th event of a clip. Four in ten are notes' edges and the rest
/// wheel and knob moves, as in a clip dense with them: a parameter move is
/// the dearest to write, with its float.
fn event(index: usize, rng: &mut XorShift) -> Event {
    let tick = index as i32 * EVENT_SPACING_TICKS;
    let pitch = 36 + rng.below(48) as u8;
    match index % 10 {
        0 | 1 => Event::new(tick, NOTE_LENGTH_TICKS, vec![0x90, pitch, 100]),
        2 | 3 => Event::new(tick, 0, vec![0x80, pitch, 0]),
        4..=6 => bend_at(tick, rng.below(128) as u8),
        _ => {
            let value = rng.below(PARAM_VALUE_STEPS) as f64 / PARAM_VALUE_STEPS as f64;
            param_at(tick, 714, value)
        }
    }
}

/// A plugin output holding `state_bytes` of state. What the bytes are makes
/// no difference to base64 or to the JSON around it.
fn instrument(state_bytes: usize) -> TrackOutput {
    TrackOutput::Instrument(InstrumentRef {
        bundle_path: "/Library/Audio/Plug-Ins/VST3/Timing.vst3".into(),
        plugin_id: "timing".to_owned(),
        display_name: "Timing".to_owned(),
        state: vec![0x5A; state_bytes],
    })
}

/// The maximum track count with `events` events and `blob_bytes` of plugin
/// state in total.
fn project(events: usize, blob_bytes: usize) -> Sequencer {
    let mut sequencer = test_sequencer();
    sequencer.set_track_count(MAX_TRACKS);
    let mut rng = XorShift::new(1);
    let per_clip = events / (MAX_TRACKS * CLIPS_PER_TRACK);
    let clip_length = per_clip as i32 * EVENT_SPACING_TICKS;
    for track in sequencer.tracks_mut() {
        track.set_output(instrument(blob_bytes / MAX_TRACKS));
        for index in 0..CLIPS_PER_TRACK {
            let mut clip = clip_at(index as i32 * clip_length, clip_length);
            clip.restore_events((0..per_clip).map(|i| event(i, &mut rng)).collect());
            track.add_clip(&clip);
        }
    }
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
    /// `ProjectData::from_sequencer`.
    snapshot: Vec<Duration>,
    /// [`json_fingerprint`].
    hash: Vec<Duration>,
    /// The snapshot and the fingerprint.
    check: Vec<Duration>,
    /// [`fingerprint`].
    direct: Vec<Duration>,
    /// The pretty JSON.
    json: Vec<Duration>,
    /// `replace_file`.
    disk: Vec<Duration>,
    /// The snapshot, the JSON and the disk.
    write: Vec<Duration>,
    /// How long the last JSON was, in bytes.
    file_bytes: usize,
}

impl Timings {
    /// Runs the rounds on `sequencer`, writing to `path`.
    fn measure(sequencer: &Sequencer, path: &Path) -> Timings {
        let mut timings = Timings::default();
        for _ in 0..ROUNDS {
            let (snapshot, data) = timed(|| ProjectData::from_sequencer(sequencer));
            let (json, text) = timed(|| serde_json::to_string_pretty(&data).unwrap());
            let (disk, written) = timed(|| replace_file(path, &text));
            written.unwrap();
            let (hash, json_hash) = timed(|| json_fingerprint(sequencer, data));
            black_box(json_hash);
            let (direct, direct_hash) = timed(|| fingerprint(sequencer));
            black_box(direct_hash);
            timings.file_bytes = text.len();
            timings.snapshot.push(snapshot);
            timings.hash.push(hash);
            timings.check.push(snapshot + hash);
            timings.direct.push(direct);
            timings.json.push(json);
            timings.disk.push(disk);
            timings.write.push(snapshot + json + disk);
        }
        timings
    }

    /// This project's row of the report.
    fn row(&mut self, events: usize, blob_megabytes: usize) -> String {
        format!(
            "{events:>7} {blob_megabytes:>5} {:>6.1} {:>9.2} {:>8.2} {:>8.2} {:>9.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>9.2}",
            self.file_bytes as f64 / MEGABYTE as f64,
            median_ms(&mut self.snapshot),
            median_ms(&mut self.hash),
            median_ms(&mut self.check),
            max_ms(&self.check),
            median_ms(&mut self.direct),
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
    let dir = temp_dir().join(format!("stev-recovery-timing-{}", process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("copy.json");

    // `println!`, not the log: this is a test, which installs no logger, and
    // the report on stdout is its whole point.
    println!();
    println!("recovery timing: {OS} {ARCH}, {MAX_TRACKS} tracks, {ROUNDS} rounds, medians in ms");
    println!(
        "{:>7} {:>5} {:>6} {:>9} {:>8} {:>8} {:>9} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "events",
        "blobMB",
        "fileMB",
        "snapshot",
        "hash",
        "CHECK",
        "check max",
        "direct",
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
