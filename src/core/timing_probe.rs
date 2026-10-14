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
use midir::{MidiIO, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

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

/// Where this run's `"midiout"` loop writes.
enum ProbeSink {
    /// Recorder mode: `(arrival, sequence)` stamped per write, measuring
    /// Stev's scheduling alone.
    Recorder(Vec<(Instant, u32)>),
    /// Loopback mode: a real `midir` output port.
    Port(MidiOutputConnection),
}

impl MidiSink for ProbeSink {
    fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        match self {
            ProbeSink::Recorder(arrivals) => stamp_arrival(0, message, arrivals),
            ProbeSink::Port(port) => port.send(message)?,
        }
        Ok(())
    }
}

/// Stamps an arrival with its sequence number: the recorder's write, and the
/// midir input callback in loopback mode.
fn stamp_arrival(_timestamp: u64, message: &[u8], arrivals: &mut Vec<(Instant, u32)>) {
    let now = Instant::now();
    if let Some(seq) = decode(message) {
        arrivals.push((now, seq));
    }
}

/// The first of `io`'s ports whose name contains `wanted`, ignoring case.
fn find_port<T: MidiIO>(io: &T, wanted: &str) -> T::Port {
    let wanted = wanted.to_lowercase();
    io.ports()
        .into_iter()
        .find(|p| {
            io.port_name(p)
                .is_ok_and(|name| name.to_lowercase().contains(&wanted))
        })
        .unwrap_or_else(|| panic!("no MIDI port matches {wanted:?}"))
}

/// Opens the output port whose name contains `wanted`.
fn open_output(wanted: &str) -> ProbeSink {
    let midi_out = MidiOutput::new("Stev Probe Out").expect("MIDI output client");
    let port = find_port(&midi_out, wanted);
    ProbeSink::Port(
        midi_out
            .connect(&port, "stev-probe-out")
            .expect("output port opens"),
    )
}

/// Opens the input port whose name contains `wanted`, stamping arrivals.
fn open_input(wanted: &str) -> LoopbackInput {
    let midi_in = MidiInput::new("Stev Probe In").expect("MIDI input client");
    let port = find_port(&midi_in, wanted);
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
        ProbeMode::Recorder => (ProbeSink::Recorder(Vec::with_capacity(1 << 16)), None),
        ProbeMode::Loopback { out, input } => {
            let input = open_input(input);
            (open_output(out), Some(input))
        }
        #[cfg(unix)]
        ProbeMode::VirtualLoopback => {
            let input = open_virtual_input();
            (open_output(VIRTUAL_LOOPBACK_NAME), Some(input))
        }
    }
}

/// Starts `cores` busy threads; they spin until `stop` is set.
fn start_load(cores: usize, stop: &Arc<AtomicBool>) {
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

/// Starts the real [`Timer`] as clock and feeder in one: each firing from
/// `origin` on records its gap and sends every message whose intended instant
/// (`intended`, ascending) has passed. At `end` it drops `midi_out_tx` (ending
/// the `"midiout"` loop) and sends the gaps, in nanoseconds, on `gaps_tx`. The
/// timer thread can't be stopped, so it idles until the test process exits.
fn start_feeder(
    origin: Instant,
    intended: Vec<Instant>,
    end: Instant,
    midi_out_tx: Sender<MidiOutMessage>,
    gaps_tx: Sender<Vec<i64>>,
) {
    let mut gaps = Vec::with_capacity((end - origin).as_millis() as usize + 1024);
    let mut senders = Some((midi_out_tx, gaps_tx));
    let mut next = 0;
    let mut last_fire: Option<Instant> = None;

    Timer::new(CLOCK_PERIOD_MS).start(move |now| {
        let Some((midi_out_tx, _)) = &senders else {
            return;
        };
        if now >= origin {
            if let Some(last) = last_fire {
                gaps.push((now - last).as_nanos() as i64);
            }
            last_fire = Some(now);
        }
        while next < intended.len() && intended[next] <= now {
            midi_out_tx
                .send(MidiOutMessage::at(encode(next as u32), intended[next]))
                .ok();
            next += 1;
        }
        if now >= end
            && let Some((_, gaps_tx)) = senders.take()
        {
            gaps_tx.send(std::mem::take(&mut gaps)).ok();
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

/// One report line in the table's column layout.
fn cells(label: &str, values: [String; 8]) -> String {
    let [count, mean, p50, p99, p999, max, over, lost] = values;
    format!(
        "{label:<16}{count:>8}{mean:>10}{p50:>10}{p99:>10}{p999:>10}{max:>10}{over:>8}{lost:>7}"
    )
}

/// One report row: the label, then the summary in microseconds.
fn row(label: &str, summary: Option<Summary>, lost: Option<usize>) -> String {
    let Some(s) = summary else {
        return format!("{label:<16}no samples");
    };
    let us = |ns: i64| format!("{:.1}", ns as f64 / 1000.0);
    let lost = lost.map_or("-".to_string(), |n| n.to_string());
    cells(
        label,
        [
            s.count.to_string(),
            us(s.mean),
            us(s.p50),
            us(s.p99),
            us(s.p999),
            us(s.max),
            s.over.to_string(),
            lost,
        ],
    )
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
    let cores = thread::available_parallelism().map_or(1, |n| n.get());

    let stop_load = Arc::new(AtomicBool::new(false));
    if loaded {
        start_load(cores, &stop_load);
    }

    let (sink, input) = open_ports(&mode);
    let (midi_out_tx, midi_out_rx) = unbounded::<MidiOutMessage>();
    let (release_tx, _release_rx) = unbounded::<MidiOutMessage>();
    let (_command_tx, command_rx) = unbounded();
    let (_sink_tx, sink_rx) = unbounded::<ProbeSink>();
    let (gaps_tx, gaps_rx) = unbounded();

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

    let origin = Instant::now() + Duration::from_millis(100);
    let offset = Duration::from_millis(offset_ms as u64);
    let intended: Vec<Instant> = pattern(Duration::from_secs(secs), mode.is_loopback())
        .into_iter()
        .map(|at| origin + at)
        .collect();
    let deadlines: Vec<Instant> = intended.iter().map(|&at| at + offset).collect();
    let end = deadlines.last().map_or(origin, |&last| last + DRAIN);
    start_feeder(origin, intended, end, midi_out_tx, gaps_tx);

    let mut gaps = gaps_rx.recv().expect("feeder reports");
    let sink = midiout.join().expect("midiout loop returns");
    stop_load.store(true, Ordering::Relaxed);
    let arrivals = match sink {
        ProbeSink::Recorder(arrivals) => arrivals,
        ProbeSink::Port(_) => input.map_or_else(Vec::new, |input| input.close().1),
    };
    let (mut late, lost) = lateness(&deadlines, &arrivals);
    let late_label = if mode.is_loopback() {
        "loopback late"
    } else {
        "midiout late"
    };
    let clock_over = CLOCK_PERIOD_MS as i64 * ONE_MS_NANOS + ONE_MS_NANOS;

    // `println!`, not `dprintln!`: the probe runs in release, where
    // `dprintln!` compiles away, and the report is its whole point.
    println!();
    println!(
        "timing probe: {OS} {ARCH}, {cores} cores, {secs} s, {}, {}, offset {offset_ms} ms",
        if loaded { "loaded" } else { "idle" },
        mode.label(),
    );
    println!(
        "{}",
        cells(
            "µs",
            [
                "count", "mean", "p50", "p99", "p99.9", "max", ">1ms", "lost"
            ]
            .map(String::from)
        )
    );
    println!(
        "{}",
        row("clock gap", summarize(&mut gaps, clock_over), None)
    );
    println!(
        "{}",
        row(late_label, summarize(&mut late, ONE_MS_NANOS), Some(lost))
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
