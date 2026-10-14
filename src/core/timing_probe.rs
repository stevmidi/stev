//! The timing probe: an ignored release test that measures the `"clock"`
//! timer's period jitter and how late the `"midiout"` loop writes clip
//! messages, so each timing fix has numbers to beat (`260-porting.md` §
//! Timing). It never ships and never runs in CI, but `clippy --all-targets`
//! compiles it, so it can't rot.
//!
//! Run with `cargo test --release timing_probe -- --ignored --nocapture`.
//! Settings by environment variable:
//!
//! - `STEV_PROBE_SECS`: run length in seconds (default 60).
//! - `STEV_PROBE_LOAD=1`: one busy thread per logical core at normal
//!   priority, standing in for a heavy session.
//! - `STEV_PROBE_OUT` / `STEV_PROBE_IN`: loopback mode through real ports,
//!   each matched as a case-insensitive substring of the port name. On Unix,
//!   `STEV_PROBE_OUT=virtual` loops through the probe's own virtual input
//!   instead, with no interface needed.
//!
//! The clock and the feeder share one real [`Timer`]: each firing records its
//! gap and hands `"midiout"` every message whose intended instant has passed,
//! as the sequencer does on a tick.

use std::env;
use std::env::consts::{ARCH, OS};
use std::error::Error;
use std::hint::black_box;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};
#[cfg(unix)]
use midir::os::unix::VirtualInput;
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::core::config::MIDI_OUT_OFFSET_DEFAULT_MS;
use crate::core::midi::out_queue::MidiOutMessage;
use crate::core::note_logger::NoteLogger;
use crate::core::threads::{MidiSink, run_midi_output_loop};
use crate::core::timer::Timer;

/// Run length variable, in seconds.
const SECS_VAR: &str = "STEV_PROBE_SECS";
/// Background-load switch variable.
const LOAD_VAR: &str = "STEV_PROBE_LOAD";
/// Loopback output port variable.
const OUT_VAR: &str = "STEV_PROBE_OUT";
/// Loopback input port variable.
const IN_VAR: &str = "STEV_PROBE_IN";
/// The name of the probe's own virtual input (Unix, `STEV_PROBE_OUT=virtual`).
#[cfg(unix)]
const VIRTUAL_LOOPBACK_NAME: &str = "Stev Probe Loopback";

/// The clock period the app runs, in milliseconds.
const CLOCK_PERIOD_MS: u64 = 1;
/// One 16th note at 120 BPM.
const SIXTEENTH: Duration = Duration::from_millis(125);
/// The gap between loopback notes: at least 5 ms, so a 3-byte message (~1 ms
/// on a 31.25 kbaud DIN wire) never queues behind the one before.
const LOOPBACK_GAP: Duration = Duration::from_millis(25);
/// How long after the last deadline the feeder waits before ending the run,
/// so nothing still queued in `"midiout"` is lost to the shutdown.
const DRAIN: Duration = Duration::from_millis(200);
/// One millisecond, the "late" threshold in the report.
const ONE_MS_NANOS: i64 = 1_000_000;

/// A loopback input connection, collecting `(arrival, sequence)` stamps.
type LoopbackInput = MidiInputConnection<Vec<(Instant, u32)>>;

/// How a run writes and where it measures arrival.
enum ProbeMode {
    /// A recorder stamps each write: Stev's scheduling alone.
    Recorder,
    /// A real output port looped back into a real input.
    Loopback {
        /// The output port's name, or a substring of it.
        out: String,
        /// The input port's name, or a substring of it.
        input: String,
    },
    /// The output looped into the probe's own virtual input (Unix only).
    #[cfg(unix)]
    VirtualLoopback,
}

impl ProbeMode {
    /// The mode the environment asks for.
    fn from_env() -> Self {
        match (env::var(OUT_VAR).ok(), env::var(IN_VAR).ok()) {
            #[cfg(unix)]
            (Some(out), _) if out.eq_ignore_ascii_case("virtual") => ProbeMode::VirtualLoopback,
            (Some(out), Some(input)) => ProbeMode::Loopback { out, input },
            _ => ProbeMode::Recorder,
        }
    }

    /// The mode's name in the report header.
    fn label(&self) -> String {
        match self {
            ProbeMode::Recorder => "recorder".to_string(),
            ProbeMode::Loopback { out, input } => format!("loopback {out} -> {input}"),
            #[cfg(unix)]
            ProbeMode::VirtualLoopback => "loopback via virtual port".to_string(),
        }
    }

    /// Whether arrivals come back through a MIDI input.
    fn is_loopback(&self) -> bool {
        !matches!(self, ProbeMode::Recorder)
    }
}

/// Percentiles and counts over one set of nanosecond samples.
#[derive(Debug, PartialEq)]
struct Summary {
    /// Number of samples.
    count: usize,
    /// Arithmetic mean.
    mean: i64,
    /// Median.
    p50: i64,
    /// 99th percentile.
    p99: i64,
    /// 99.9th percentile.
    p999: i64,
    /// Largest sample.
    max: i64,
    /// Samples above the `over` threshold passed to [`summarize`].
    over: usize,
}

/// The nearest-rank percentile `p` (0–1) of `sorted`, which is non-empty and
/// ascending.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Summarises `samples` (sorting them in place), counting those above `over`.
/// `None` when there are none.
fn summarize(samples: &mut [i64], over: i64) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let total: i128 = samples.iter().map(|&s| i128::from(s)).sum();
    Some(Summary {
        count: samples.len(),
        mean: (total / samples.len() as i128) as i64,
        p50: percentile(samples, 0.5),
        p99: percentile(samples, 0.99),
        p999: percentile(samples, 0.999),
        max: samples[samples.len() - 1],
        over: samples.iter().filter(|&&s| s > over).count(),
    })
}

/// A tiny xorshift generator, so the pattern needs no dependency and the same
/// seed gives the same run.
struct XorShift(u64);

impl XorShift {
    /// The next value in `0..bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// When each message is intended to sound, as offsets from the run's start,
/// ascending. Recorder mode: a three-note chord every 16th at 120 BPM, plus a
/// single note at a random sub-millisecond phase inside each 16th, so
/// deadlines don't all fall on whole milliseconds. Loopback mode: single notes
/// [`LOOPBACK_GAP`] apart, each nudged by up to 1 ms.
fn pattern(length: Duration, loopback: bool) -> Vec<Duration> {
    let mut rng = XorShift(0x5EED_1234_ABCD_0001);
    let mut at = Vec::new();
    let mut step = Duration::ZERO;
    while step < length {
        if loopback {
            at.push(step + Duration::from_nanos(rng.below(1_000_000)));
            step += LOOPBACK_GAP;
        } else {
            at.extend([step; 3]);
            at.push(step + Duration::from_nanos(rng.below(SIXTEENTH.as_nanos() as u64)));
            step += SIXTEENTH;
        }
    }
    at.sort_unstable();
    at
}

/// Encodes a sequence number into a note-on's channel, note and velocity
/// (velocity kept above zero, which some stacks rewrite as a note-off).
fn encode(seq: u32) -> Vec<u8> {
    let note = (seq % 128) as u8;
    let velocity = 1 + ((seq / 128) % 127) as u8;
    let channel = ((seq / (128 * 127)) % 16) as u8;
    vec![0x90 | channel, note, velocity]
}

/// The sequence number [`encode`] put in `msg`, if it's one of the probe's.
fn decode(msg: &[u8]) -> Option<u32> {
    match msg {
        [status, note, velocity] if status & 0xF0 == 0x90 && *velocity > 0 => {
            let channel = u32::from(status & 0x0F);
            Some(channel * 128 * 127 + u32::from(velocity - 1) * 128 + u32::from(*note))
        }
        _ => None,
    }
}

/// Recorder-mode sink: stamps each write with its sequence number.
struct Recorder {
    /// `(arrival, sequence)` per write.
    arrivals: Vec<(Instant, u32)>,
}

impl MidiSink for Recorder {
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        let now = Instant::now();
        if let Some(seq) = decode(message) {
            self.arrivals.push((now, seq));
        }
        Ok(())
    }
}

/// Loopback-mode sink: a real `midir` output port.
struct PortSink(MidiOutputConnection);

impl MidiSink for PortSink {
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        self.0.send(message)?;
        Ok(())
    }
}

/// A sink for this run, recorder or real port.
enum ProbeSink {
    /// See [`Recorder`].
    Recorder(Recorder),
    /// See [`PortSink`].
    Port(PortSink),
}

impl MidiSink for ProbeSink {
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        match self {
            ProbeSink::Recorder(recorder) => recorder.send(message),
            ProbeSink::Port(port) => port.send(message),
        }
    }
}

/// The midir input callback: stamps each arrival with its sequence number.
fn stamp_arrival(_timestamp: u64, message: &[u8], arrivals: &mut Vec<(Instant, u32)>) {
    let now = Instant::now();
    if let Some(seq) = decode(message) {
        arrivals.push((now, seq));
    }
}

/// Opens the first output port whose name contains `wanted`, ignoring case.
fn open_output(wanted: &str) -> PortSink {
    let midi_out = MidiOutput::new("Stev Probe Out").expect("MIDI output client");
    let port = midi_out
        .ports()
        .into_iter()
        .find(|p| {
            midi_out
                .port_name(p)
                .is_ok_and(|name| name.to_lowercase().contains(&wanted.to_lowercase()))
        })
        .unwrap_or_else(|| panic!("no MIDI output port matches {wanted:?}"));
    PortSink(
        midi_out
            .connect(&port, "stev-probe-out")
            .expect("output port opens"),
    )
}

/// Opens the first input port whose name contains `wanted`, ignoring case,
/// stamping arrivals.
fn open_input(wanted: &str) -> LoopbackInput {
    let midi_in = MidiInput::new("Stev Probe In").expect("MIDI input client");
    let port = midi_in
        .ports()
        .into_iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .is_ok_and(|name| name.to_lowercase().contains(&wanted.to_lowercase()))
        })
        .unwrap_or_else(|| panic!("no MIDI input port matches {wanted:?}"));
    midi_in
        .connect(
            &port,
            "stev-probe-in",
            stamp_arrival,
            Vec::with_capacity(1 << 16),
        )
        .expect("input port opens")
}

/// The probe's own virtual input, stamping arrivals (Unix only).
#[cfg(unix)]
fn open_virtual_input() -> LoopbackInput {
    MidiInput::new("Stev Probe In")
        .expect("MIDI input client")
        .create_virtual(
            VIRTUAL_LOOPBACK_NAME,
            stamp_arrival,
            Vec::with_capacity(1 << 16),
        )
        .expect("virtual input opens")
}

/// The sink and (in loopback mode) the input connection for `mode`.
fn open_ports(mode: &ProbeMode) -> (ProbeSink, Option<LoopbackInput>) {
    match mode {
        ProbeMode::Recorder => (
            ProbeSink::Recorder(Recorder {
                arrivals: Vec::with_capacity(1 << 16),
            }),
            None,
        ),
        ProbeMode::Loopback { out, input } => {
            let input = open_input(input);
            (ProbeSink::Port(open_output(out)), Some(input))
        }
        #[cfg(unix)]
        ProbeMode::VirtualLoopback => {
            let input = open_virtual_input();
            (
                ProbeSink::Port(open_output(VIRTUAL_LOOPBACK_NAME)),
                Some(input),
            )
        }
    }
}

/// Starts one busy thread per logical core; they spin until `stop` is set.
fn start_load(stop: &Arc<AtomicBool>) {
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    for _ in 0..cores {
        let stop = Arc::clone(stop);
        thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) {
                x = black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        });
    }
}

/// What the feeder hands back when its run is over.
struct FeederReport {
    /// Nanoseconds between consecutive timer firings.
    gaps: Vec<i64>,
    /// Each message's deadline (intended instant plus the offset), by
    /// sequence number.
    deadlines: Vec<Instant>,
}

/// Starts the real platform [`Timer`] as clock and feeder in one: each firing
/// records its gap and sends every message whose intended instant has passed.
/// Once the last deadline plus [`DRAIN`] is behind it, it drops `midi_out_tx`
/// (ending the `"midiout"` loop) and reports on `report_tx`. The timer thread
/// can't be stopped, so it idles until the test process exits.
fn start_feeder(
    intended: Vec<Duration>,
    offset: Duration,
    midi_out_tx: Sender<MidiOutMessage>,
    report_tx: Sender<FeederReport>,
) {
    let origin = Instant::now() + Duration::from_millis(100);
    let deadlines: Vec<Instant> = intended.iter().map(|&at| origin + at + offset).collect();
    let end = deadlines.last().map_or(origin, |&last| last + DRAIN);
    let mut gaps = Vec::with_capacity((end - origin).as_millis() as usize + 1024);
    let mut report = Some((midi_out_tx, report_tx, deadlines));
    let mut next = 0;
    let mut last_fire: Option<Instant> = None;

    Timer::new(CLOCK_PERIOD_MS).start(move |now| {
        let Some((midi_out_tx, _, _)) = &report else {
            return;
        };
        if now >= origin {
            if let Some(last) = last_fire {
                gaps.push((now - last).as_nanos() as i64);
            }
            last_fire = Some(now);
        }
        while next < intended.len() && origin + intended[next] <= now {
            let at = origin + intended[next];
            midi_out_tx
                .send(MidiOutMessage::at(encode(next as u32), at))
                .ok();
            next += 1;
        }
        if now >= end
            && let Some((_, report_tx, deadlines)) = report.take()
        {
            report_tx
                .send(FeederReport {
                    gaps: std::mem::take(&mut gaps),
                    deadlines,
                })
                .ok();
        }
    });
}

/// Nanoseconds each arrival landed after its own deadline, plus how many
/// messages never arrived.
fn lateness(deadlines: &[Instant], arrivals: &[(Instant, u32)]) -> (Vec<i64>, usize) {
    let mut seen = vec![false; deadlines.len()];
    let mut late = Vec::with_capacity(arrivals.len());
    for &(arrival, seq) in arrivals {
        let Some(&deadline) = deadlines.get(seq as usize) else {
            continue;
        };
        if std::mem::replace(&mut seen[seq as usize], true) {
            continue;
        }
        late.push(if arrival >= deadline {
            (arrival - deadline).as_nanos() as i64
        } else {
            -((deadline - arrival).as_nanos() as i64)
        });
    }
    let lost = seen.iter().filter(|&&s| !s).count();
    (late, lost)
}

/// One report row: the label, then the summary in microseconds.
fn row(label: &str, summary: Option<&Summary>, lost: Option<usize>) -> String {
    let us = |ns: i64| format!("{:.1}", ns as f64 / 1000.0);
    let lost = lost.map_or("-".to_string(), |n| n.to_string());
    match summary {
        Some(s) => format!(
            "{label:<16}{:>8}{:>10}{:>10}{:>10}{:>10}{:>10}{:>8}{:>7}",
            s.count,
            us(s.mean),
            us(s.p50),
            us(s.p99),
            us(s.p999),
            us(s.max),
            s.over,
            lost
        ),
        None => format!("{label:<16}{:>8}", 0),
    }
}

/// Dev tool, not a test: see the module docs.
#[test]
#[ignore = "dev tool: run in release with --ignored --nocapture"]
fn timing_probe() {
    let secs: u64 = env::var(SECS_VAR)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let loaded = env::var(LOAD_VAR).is_ok_and(|v| v == "1");
    let mode = ProbeMode::from_env();
    let offset_ms = MIDI_OUT_OFFSET_DEFAULT_MS;

    let stop_load = Arc::new(AtomicBool::new(false));
    if loaded {
        start_load(&stop_load);
    }

    let (sink, input) = open_ports(&mode);
    let (midi_out_tx, midi_out_rx) = unbounded::<MidiOutMessage>();
    let (release_tx, _release_rx) = unbounded::<MidiOutMessage>();
    let (_command_tx, command_rx) = unbounded();
    let (_sink_tx, sink_rx) = unbounded::<ProbeSink>();
    let (report_tx, report_rx) = unbounded();

    let midiout = thread::Builder::new()
        .name("midiout".to_string())
        .spawn(move || {
            run_midi_output_loop(
                sink,
                midi_out_rx,
                NoteLogger::new(release_tx),
                command_rx,
                sink_rx,
                Arc::new(AtomicI32::new(offset_ms)),
            )
        })
        .expect("midiout thread spawns");

    start_feeder(
        pattern(Duration::from_secs(secs), mode.is_loopback()),
        Duration::from_millis(offset_ms as u64),
        midi_out_tx,
        report_tx,
    );

    let FeederReport {
        mut gaps,
        deadlines,
    } = report_rx.recv().expect("feeder reports");
    let sink = midiout.join().expect("midiout loop returns");
    stop_load.store(true, Ordering::Relaxed);
    let arrivals = match (sink, input) {
        (ProbeSink::Recorder(recorder), _) => recorder.arrivals,
        (ProbeSink::Port(_), Some(input)) => input.close().1,
        (ProbeSink::Port(_), None) => Vec::new(),
    };
    let (mut late, lost) = lateness(&deadlines, &arrivals);

    // `println!`, not `dprintln!`: the probe runs in release, where
    // `dprintln!` compiles away, and the report is its whole point.
    println!();
    println!(
        "timing probe: {} {}, {} cores, {secs} s, {}, {}, offset {offset_ms} ms",
        OS,
        ARCH,
        thread::available_parallelism().map_or(1, |n| n.get()),
        if loaded { "loaded" } else { "idle" },
        mode.label(),
    );
    println!(
        "{:<16}{:>8}{:>10}{:>10}{:>10}{:>10}{:>10}{:>8}{:>7}",
        "µs", "count", "mean", "p50", "p99", "p99.9", "max", ">1ms", "lost"
    );
    println!(
        "{}",
        row(
            "clock gap",
            summarize(
                &mut gaps,
                CLOCK_PERIOD_MS as i64 * ONE_MS_NANOS + ONE_MS_NANOS
            )
            .as_ref(),
            None,
        )
    );
    println!(
        "{}",
        row(
            if mode.is_loopback() {
                "loopback late"
            } else {
                "midiout late"
            },
            summarize(&mut late, ONE_MS_NANOS).as_ref(),
            Some(lost),
        )
    );
    println!("(clock >1ms counts gaps over 2 ms: one period plus 1 ms)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_reports_nearest_rank_percentiles() {
        let mut samples: Vec<i64> = (1..=1000).rev().collect();
        let summary = summarize(&mut samples, 990).unwrap();
        assert_eq!(
            summary,
            Summary {
                count: 1000,
                mean: 500,
                p50: 500,
                p99: 990,
                p999: 999,
                max: 1000,
                over: 10,
            }
        );
    }

    #[test]
    fn summarize_of_one_sample_is_that_sample() {
        let summary = summarize(&mut [7], 0).unwrap();
        assert_eq!((summary.p50, summary.p999, summary.max), (7, 7, 7));
        assert_eq!(summary.over, 1);
    }

    #[test]
    fn summarize_of_nothing_is_none() {
        assert_eq!(summarize(&mut [], 0), None);
    }

    #[test]
    fn every_sequence_number_round_trips() {
        for seq in [0, 1, 127, 128, 128 * 127 - 1, 128 * 127, 128 * 127 * 16 - 1] {
            assert_eq!(decode(&encode(seq)), Some(seq), "seq {seq}");
        }
    }

    #[test]
    fn encoded_messages_are_note_ons_with_velocity() {
        for seq in 0..40_000 {
            let msg = encode(seq);
            assert_eq!(msg[0] & 0xF0, 0x90);
            assert!(msg[1] < 128 && (1..128).contains(&msg[2]));
        }
    }

    #[test]
    fn recorder_pattern_has_a_chord_and_an_off_grid_note_per_sixteenth() {
        let at = pattern(Duration::from_secs(1), false);
        assert_eq!(at.len(), 8 * 4);
        assert!(at.is_sorted());
        assert!(at.iter().any(|d| d.subsec_nanos() % 1_000_000 != 0));
    }

    #[test]
    fn loopback_pattern_keeps_notes_apart() {
        let at = pattern(Duration::from_secs(1), true);
        assert_eq!(at.len(), 40);
        assert!(
            at.windows(2)
                .all(|w| w[1] - w[0] >= Duration::from_millis(5))
        );
    }

    #[test]
    fn lateness_matches_each_arrival_to_its_own_deadline_and_counts_losses() {
        let origin = Instant::now();
        let deadlines = [
            origin,
            origin + Duration::from_millis(10),
            origin + Duration::from_millis(20),
        ];
        // Out of order, one duplicate, one missing.
        let arrivals = [
            (origin + Duration::from_millis(11), 1),
            (origin + Duration::from_micros(300), 0),
            (origin + Duration::from_millis(50), 1),
        ];
        let (late, lost) = lateness(&deadlines, &arrivals);
        assert_eq!(late, vec![1_000_000, 300_000]);
        assert_eq!(lost, 1);
    }
}
