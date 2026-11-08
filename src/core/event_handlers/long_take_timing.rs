//! The long-take timing: an ignored release test that times what
//! `420-large-projects.md` decision 9 leaves open, the single huge clip. The
//! recovery check reuses an unchanged clip's hash, so an edit costs the
//! clip it touched. That clip's events are copied by its first edit after a
//! check (the check's cache holds them too), and hashed again by the next
//! check. Both grow with the clip's events, so one long take dense with
//! wheel rides is the case to measure. It never ships and never runs in CI,
//! but `clippy --all-targets` compiles it, so it can't rot.
//!
//! Run with `cargo test --release long_take_timing -- --ignored --nocapture`.
//!
//! Each project is the stress project with track 0's clips replaced by one
//! long take of [`TAKE_BARS`] bars at 120 BPM in 4/4: [`NOTES_PER_BAR`]
//! notes a bar, and wheel rides at a common controller's rate,
//! [`MOVES_PER_SECOND`] moves a second while a wheel moves. Pitch bend
//! rides the first bar of every four and the mod wheel the third, so each
//! wheel moves a quarter of the take and some wheel half of it; a wheel
//! held still sends nothing. The take is the lead clip.
//!
//! A round runs the check as the tick arm does (`OpenProject::tick`), untimed,
//! so the cache holds the take; then times one edit of the take and the
//! check after it, which finds the project changed and hands a recovery
//! write over; then undoes the edit, untimed. It then empties the check's
//! cache and times the same edit again, the `no cache` column: the edit
//! without the copy the cache forces, which tells the copy (decision 9)
//! from the edit's own work. The sum is what the
//! `"sequencer"` thread spends on that edit across the two passes; each
//! part, not the sum, is held to the one-tick budget. Like the other
//! timings, the rounds run with warm caches and no audio: read the figures
//! as a floor.

use std::time::{Duration, Instant};

use undo::Record;
use uuid::Uuid;

use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::Controller;
use crate::core::project::{STRESS_BLOB_BYTES, STRESS_EVENTS, fill_stress_project};
use crate::core::sequencer::test_support::{clip_at, mod_at};
use crate::core::sequencer::{SequencerCommand, SequencerEdit};
use crate::core::time::PPQN;
use crate::models::clip::NoteDrag;
use crate::models::event::{Event, EventType};

use super::SavedProject;
use super::test_harness::{Harness, harness, run_command};

/// The take's lengths, in bars; at 120 BPM a bar lasts two seconds.
const TAKE_BARS: [i32; 6] = [16, 32, 64, 128, 256, 512];

/// Notes in a bar of the take: eighths.
const NOTES_PER_BAR: i32 = 8;

/// Moves a second a wheel sends while it moves: one every 10 ms, a common
/// rate for a keyboard's wheels (they send on change, so a fast ride is
/// what fills the clip).
const MOVES_PER_SECOND: i32 = 100;

/// Timed rounds per take and edit.
const ROUNDS: usize = 7;

/// The one-tick budget, in ms (`420-large-projects.md` decision 2).
const BUDGET_MS: f64 = 1.0;

/// One bar of 4/4, in ticks.
const BAR: i32 = 4 * PPQN;

/// One bar at 120 BPM, in milliseconds.
const BAR_MS: i32 = 2000;

/// Further apart than the recovery check's interval, so each `tick` of the
/// open project runs it.
const PAST_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// A pitch-bend move to the 14-bit `value` at `tick`.
fn bend(tick: i32, value: u16) -> Event {
    Event::new(
        tick,
        0,
        vec![0xE0, (value & 0x7F) as u8, (value >> 7) as u8],
    )
}

/// Where a ride stands `step` of `steps` moves in: up from rest and back,
/// a triangle, as `0.0..=1.0`.
fn ride(step: i32, steps: i32) -> f64 {
    let half = f64::from(steps) / 2.0;
    1.0 - (f64::from(step) - half).abs() / half
}

/// The take's events: [`NOTES_PER_BAR`] notes in every bar, a bend ride in
/// the first bar of every four and a mod-wheel ride in the third.
fn take_events(bars: i32) -> Vec<Event> {
    let note_ticks = BAR / NOTES_PER_BAR;
    let moves = MOVES_PER_SECOND * BAR_MS / 1000;
    let mut events = Vec::new();
    for bar in 0..bars {
        let start = bar * BAR;
        for note in 0..NOTES_PER_BAR {
            let tick = start + note * note_ticks;
            let pitch = 48 + ((bar * NOTES_PER_BAR + note) % 24) as u8;
            events.push(Event::new(tick, 0, vec![0x90, pitch, 100]));
            events.push(Event::new(tick + note_ticks / 2, 0, vec![0x80, pitch, 0]));
        }
        for step in 0..moves {
            let tick = start + step * BAR / moves;
            match bar % 4 {
                0 => events.push(bend(tick, 0x2000 + (ride(step, moves) * 8191.0) as u16)),
                2 => events.push(mod_at(tick, (ride(step, moves) * 127.0) as u8)),
                _ => {}
            }
        }
    }
    events.sort_by_key(Event::tick);
    events
}

/// The stress project with track 0 holding one long take of `bars` bars,
/// saved as it stands, the take the lead clip.
fn long_take_harness(bars: i32) -> Harness {
    let mut h = harness();
    fill_stress_project(&mut h.sequencer, STRESS_EVENTS, STRESS_BLOB_BYTES);
    let mut take = clip_at(0, bars * BAR);
    take.restore_events(take_events(bars));
    take.calculate_note_lengths();
    let track = &mut h.sequencer.tracks_mut()[0];
    track.clear_clips();
    track.add_clip(&take);
    h.handlers
        .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
    h.open.saved = SavedProject::of(&h.sequencer, None);
    h
}

/// The take's events.
fn take(h: &Harness) -> &[Event] {
    h.sequencer.tracks()[0].clips()[0].events()
}

/// The take's first note-on.
fn first_note(h: &Harness) -> Uuid {
    take(h)
        .iter()
        .find(|event| event.event_type() == Some(EventType::NoteOn))
        .map(Event::id)
        .unwrap()
}

/// An edit of the take: untimed setup, then the timed command.
struct Edit {
    /// The row's label.
    name: &'static str,
    /// Run untimed before the command, after the warm check.
    setup: fn(&mut Harness, &mut Record<SequencerEdit>),
    /// The command to time.
    command: fn(&Harness) -> SequencerCommand,
}

/// No setup.
fn none(_: &mut Harness, _: &mut Record<SequencerEdit>) {}

/// The take's first note selected.
fn select_first_note(h: &mut Harness, record: &mut Record<SequencerEdit>) {
    let id = first_note(h);
    run_command(h, record, SequencerCommand::SelectClipEvent(Some(id)));
}

/// The edits timed: the small ones a take gets after it lands.
const EDITS: &[Edit] = &[
    Edit {
        name: "drag a note",
        setup: none,
        command: |h| SequencerCommand::DragNotes {
            event_ids: vec![first_note(h)],
            drag: NoteDrag::Move {
                delta_ticks: 24,
                delta_pitch: 1,
            },
            drag_id: 1,
        },
    },
    Edit {
        name: "mute a note",
        setup: select_first_note,
        command: |_| SequencerCommand::MuteSelectedEvents,
    },
    Edit {
        name: "clear a bar of bend",
        setup: none,
        command: |_| SequencerCommand::ClearControllerSpan {
            controller: Controller::PitchBend,
            start: 0,
            end: BAR,
        },
    },
];

/// How long `work` took.
fn timed(work: impl FnOnce()) -> Duration {
    let start = Instant::now();
    work();
    start.elapsed()
}

/// `time` in milliseconds.
fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

/// One row's times, each kept per round.
#[derive(Default)]
struct Row {
    /// The edit.
    edit: Vec<Duration>,
    /// The check after it.
    check: Vec<Duration>,
    /// The two together.
    sum: Vec<Duration>,
    /// The same edit with the check's cache empty: what it costs without
    /// the copy the cache forces.
    cold: Vec<Duration>,
}

impl Row {
    /// The row as printed: medians and worsts in ms, `!` past the budget
    /// on the edit and the check.
    fn line(&mut self, bars: i32, events: usize, name: &str) -> String {
        let mut cells = format!("{bars:>5} {events:>7}  {name:<20}");
        for (times, budgeted) in [
            (&mut self.edit, true),
            (&mut self.check, true),
            (&mut self.sum, false),
            (&mut self.cold, true),
        ] {
            times.sort();
            let median = ms(times[times.len() / 2]);
            let worst = ms(times[times.len() - 1]);
            let flag = if budgeted && worst > BUDGET_MS {
                "!"
            } else {
                " "
            };
            cells.push_str(&format!("{median:>9.2} {worst:>8.2}{flag}"));
        }
        cells
    }
}

/// Runs the recovery check as the tick arm does, at `at`, and drops what
/// it handed to the `"storage"` thread and told the view.
fn check(h: &mut Harness, at: &mut Instant) {
    *at += PAST_CHECK_INTERVAL;
    h.open.tick(*at, &h.sequencer);
    drain(h);
}

/// Drops what the handlers sent, so nothing piles up between rounds.
fn drain(h: &Harness) {
    h.storage_jobs.try_iter().for_each(drop);
    h.ui_events.try_iter().for_each(drop);
    h.transport_commands.try_iter().for_each(drop);
    h.sequencer_commands.try_iter().for_each(drop);
}

/// Times `edit` of a take of `bars` bars over [`ROUNDS`].
fn measure(bars: i32, edit: &Edit) -> Row {
    let mut h = long_take_harness(bars);
    let mut record: Record<SequencerEdit> = Record::new();
    let mut at = Instant::now();
    let mut row = Row::default();
    for _ in 0..ROUNDS {
        check(&mut h, &mut at);
        (edit.setup)(&mut h, &mut record);
        let head = record.head();
        let command = (edit.command)(&h);
        let edited = timed(|| run_command(&mut h, &mut record, command));
        assert!(record.head() > head, "{} recorded no step", edit.name);
        drain(&h);
        let checked = timed(|| {
            at += PAST_CHECK_INTERVAL;
            h.open.tick(at, &h.sequencer);
        });
        drain(&h);
        row.edit.push(edited);
        row.check.push(checked);
        row.sum.push(edited + checked);
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        drain(&h);

        h.sequencer.fingerprint_cache().clear();
        (edit.setup)(&mut h, &mut record);
        let command = (edit.command)(&h);
        row.cold
            .push(timed(|| run_command(&mut h, &mut record, command)));
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        drain(&h);
    }
    row
}

/// The take holds what the module docs say: its notes, and each wheel's
/// rides at the controller's rate in a quarter of its bars.
#[test]
fn a_long_take_holds_its_notes_and_rides() {
    let bars = 8;
    let h = long_take_harness(bars);
    let events = take(&h);
    let count = |controller: Controller| {
        events
            .iter()
            .filter(|event| event.controller_move().map(|(c, _)| c) == Some(controller))
            .count()
    };
    let notes = (bars * NOTES_PER_BAR) as usize;
    let moves = (MOVES_PER_SECOND * BAR_MS / 1000 * bars / 4) as usize;
    let note_ons = events
        .iter()
        .filter(|event| event.event_type() == Some(EventType::NoteOn))
        .count();
    assert_eq!(note_ons, notes);
    assert_eq!(count(Controller::PitchBend), moves);
    assert_eq!(count(Controller::Cc(1)), moves);
    assert_eq!(events.len(), 2 * notes + 2 * moves);
    assert_eq!(h.sequencer.tracks()[0].clips().len(), 1);
    assert_eq!(h.sequencer.tracks().len(), MAX_TRACKS);
}

/// Prints the report. See the module docs.
#[test]
#[ignore = "a measurement, not a check: run it by hand, in release"]
fn long_take_timing() {
    // `println!`, not the log: this is a test, which installs no logger, and
    // the report on stdout is its whole point.
    println!();
    println!(
        "long-take timing: one take on the stress project, {MOVES_PER_SECOND} wheel moves/s, {ROUNDS} rounds, ms, ! past {BUDGET_MS} ms"
    );
    println!(
        "{:>5} {:>7}  {:<20}{:>9} {:>9}{:>9} {:>9}{:>9} {:>9}{:>9} {:>9}",
        "bars",
        "events",
        "edit",
        "EDIT",
        "worst",
        "CHECK",
        "worst",
        "sum",
        "worst",
        "no cache",
        "worst"
    );
    for bars in TAKE_BARS {
        let events = take_events(bars).len();
        for edit in EDITS {
            let mut row = measure(bars, edit);
            println!("{}", row.line(bars, events, edit.name));
        }
    }
}
