//! The command sweep: an ignored release test that times each
//! [`SequencerCommand`] on the stress project at the design's target size,
//! through the same handlers the `"sequencer"` thread runs, to find whatever
//! passes the one-tick budget (`420-large-projects.md` phase 1). It never
//! ships and never runs in CI, but `clippy --all-targets` compiles it, so it
//! can't rot.
//!
//! Run with `cargo test --release command_timing -- --ignored --nocapture`.
//!
//! Each case starts from a fresh stress project, with the lead clip under
//! the clip cursor at the project's start (1,250 events), runs its setup
//! untimed, then times its command and, when that recorded an undo step,
//! the undo and the redo. The marquee cases cover every track and the whole
//! project, the worst a marquee can be. A playback row times
//! [`Sequencer::tick`] alone. Like the recovery timing, the rounds run with
//! warm caches and no audio: read the figures as a floor.
//!
//! Not swept, and why: the capture commits (`Commit`, `CommitClipClickByTicks`,
//! `FitTempo`; they need a take), `TapTempo` (wall-clock taps), the commands
//! that touch the user's files or settings (`ProjectAction::Open` and
//! `OpenFile`, `NewFromTemplate`, `ApplyStagedProject`, `AnswerRecovery`,
//! `ProjectRenamed`, `ProjectFolderRenamed`, `ExportClip`), the audio export
//! (it needs the audio thread) and the agent bridge, whose reads the design
//! leaves out of scope. `SaveProject` and `SaveTemplate` time the handover
//! alone, the snapshot and the fingerprint: the writing is the `"storage"`
//! thread's (`420-large-projects.md` phase 2), and the harness has none.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use undo::Record;
use uuid::Uuid;

use crate::core::config::MAX_TRACKS;
use crate::core::input_event::TimeSelectionRect;
use crate::core::midi::message::Controller;
use crate::core::project::{ProjectAction, STRESS_BLOB_BYTES, STRESS_EVENTS, fill_stress_project};
use crate::core::sequencer::test_support::{clip_at, note_off, note_on, rect};
use crate::core::sequencer::{SequencerCommand, SequencerEdit};
use crate::core::time::Meter;
use crate::core::view_state::Pane;
use crate::models::clip::{Clip, ClipEdge, NoteDrag};
use crate::models::event::EventType;
use crate::models::track::TrackOutput;

use super::SavedProject;
use super::test_harness::{Harness, harness, run_command};

/// Timed rounds per case, each on a fresh project.
const ROUNDS: usize = 5;

/// Ticks the playback row times.
const PLAYBACK_TICKS: i32 = 20_000;

/// The one-tick budget, in ms (`420-large-projects.md` decision 2).
const BUDGET_MS: f64 = 1.0;

/// One bar of 4/4, in ticks.
const BAR: i32 = 3840;

/// A step of the sweep: untimed setup, then the timed command, built from
/// the project as the setup left it.
struct Case {
    /// The row's label: the command, and what it was given when that
    /// matters.
    name: &'static str,
    /// Run untimed before the command.
    setup: fn(&mut Harness, &mut Record<SequencerEdit>),
    /// The command to time.
    command: fn(&Harness) -> SequencerCommand,
}

/// No setup.
fn none(_: &mut Harness, _: &mut Record<SequencerEdit>) {}

/// Every event of the lead clip selected.
fn select_all(h: &mut Harness, record: &mut Record<SequencerEdit>) {
    run_command(h, record, SequencerCommand::SelectAllEvents);
}

/// The lead clip's notes copied.
fn copy_notes(h: &mut Harness, record: &mut Record<SequencerEdit>) {
    select_all(h, record);
    run_command(h, record, SequencerCommand::CopyNotes);
}

/// The whole project copied.
fn copy_clips(h: &mut Harness, record: &mut Record<SequencerEdit>) {
    let end = project_end(h);
    run_command(h, record, SequencerCommand::CopyClips { start: 0, end });
}

/// The transport cursor halfway through the project, inside a clip on
/// every track.
fn cursor_mid(h: &mut Harness, _: &mut Record<SequencerEdit>) {
    h.cursor_tick.store(project_end(h) / 2, Ordering::Relaxed);
}

/// The clip view focused on the lead clip.
fn enter_clip(h: &mut Harness, record: &mut Record<SequencerEdit>) {
    run_command(h, record, SequencerCommand::EnterClip);
}

/// The tick the last clip ends at.
fn project_end(h: &Harness) -> i32 {
    h.sequencer
        .tracks()
        .iter()
        .flat_map(|track| track.clips())
        .map(Clip::end_tick)
        .max()
        .unwrap_or(0)
}

/// A marquee over every track and the whole project.
fn everything(h: &Harness) -> TimeSelectionRect {
    rect(0, project_end(h), 0, MAX_TRACKS - 1)
}

/// The lead clip's note-ons.
fn lead_notes(h: &Harness) -> Vec<Uuid> {
    h.sequencer.tracks()[0].clips()[0]
        .events()
        .iter()
        .filter(|event| event.event_type() == Some(EventType::NoteOn))
        .map(|event| event.id())
        .collect()
}

/// The sweep, in the order `SequencerCommand` declares them.
const CASES: &[Case] = &[
    Case {
        name: "EnterClip",
        setup: none,
        command: |_| SequencerCommand::EnterClip,
    },
    Case {
        name: "ExitClip",
        setup: enter_clip,
        command: |_| SequencerCommand::ExitClip,
    },
    Case {
        name: "FocusPane(Clip)",
        setup: none,
        command: |_| SequencerCommand::FocusPane(Pane::Clip),
    },
    Case {
        name: "InsertEmptyClip (after the end)",
        setup: none,
        command: |h| {
            let end = project_end(h);
            SequencerCommand::InsertEmptyClip {
                time_bounds: Some(rect(end, end + BAR, 0, 0)),
            }
        },
    },
    Case {
        name: "RemoveClips (all)",
        setup: none,
        command: |h| SequencerCommand::RemoveClips {
            rect: everything(h),
        },
    },
    Case {
        name: "SplitClips (all, mid)",
        setup: cursor_mid,
        command: |h| SequencerCommand::SplitClips {
            time_bounds: Some(everything(h)),
        },
    },
    Case {
        name: "InsertSilenceInRange (1 bar at 0)",
        setup: none,
        command: |_| SequencerCommand::InsertSilenceInRange { start: 0, end: BAR },
    },
    Case {
        name: "DeleteTimeInRange (1 bar at 0)",
        setup: none,
        command: |_| SequencerCommand::DeleteTimeInRange { start: 0, end: BAR },
    },
    Case {
        name: "DuplicateTimeInRange (all)",
        setup: none,
        command: |h| SequencerCommand::DuplicateTimeInRange {
            start: 0,
            end: project_end(h),
        },
    },
    Case {
        name: "DuplicateClips (all)",
        setup: none,
        command: |h| SequencerCommand::DuplicateClips {
            rect: everything(h),
        },
    },
    Case {
        name: "MergeClips (all)",
        setup: none,
        command: |h| SequencerCommand::MergeClips {
            rect: everything(h),
        },
    },
    Case {
        name: "CopyClips (all)",
        setup: none,
        command: |h| SequencerCommand::CopyClips {
            start: 0,
            end: project_end(h),
        },
    },
    Case {
        name: "CutClips (all)",
        setup: none,
        command: |h| SequencerCommand::CutClips {
            start: 0,
            end: project_end(h),
        },
    },
    Case {
        name: "CopyClipsScoped (all)",
        setup: none,
        command: |h| SequencerCommand::CopyClipsScoped {
            rect: everything(h),
        },
    },
    Case {
        name: "CutClipsScoped (all)",
        setup: none,
        command: |h| SequencerCommand::CutClipsScoped {
            rect: everything(h),
        },
    },
    Case {
        name: "PasteClips (all)",
        setup: copy_clips,
        command: |_| SequencerCommand::PasteClips,
    },
    Case {
        name: "RescaleSelectedClipTempo",
        setup: none,
        command: |_| SequencerCommand::RescaleSelectedClipTempo(400_000),
    },
    Case {
        name: "MuteClipsInRange (all)",
        setup: none,
        command: |h| SequencerCommand::MuteClipsInRange {
            rect: everything(h),
        },
    },
    Case {
        name: "NudgeClipCursor",
        setup: none,
        command: |_| SequencerCommand::NudgeClipCursor(240),
    },
    Case {
        name: "NudgeClipCursorByGrid",
        setup: none,
        command: |_| SequencerCommand::NudgeClipCursorByGrid { step_ticks: 240 },
    },
    Case {
        name: "MoveClipCursorToStart",
        setup: none,
        command: |_| SequencerCommand::MoveClipCursorToStart,
    },
    Case {
        name: "MoveClipCursorToEnd",
        setup: none,
        command: |_| SequencerCommand::MoveClipCursorToEnd,
    },
    Case {
        name: "PlayFromClipCursor",
        setup: none,
        command: |_| SequencerCommand::PlayFromClipCursor,
    },
    Case {
        name: "NudgeSelectedEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::NudgeSelectedEvents(24),
    },
    Case {
        name: "NudgeSelectedEventsLength (clip)",
        setup: select_all,
        command: |_| SequencerCommand::NudgeSelectedEventsLength(24),
    },
    Case {
        name: "TransposeSelectedEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::TransposeSelectedEvents(1),
    },
    Case {
        name: "DeleteSelectedEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::DeleteSelectedEvents,
    },
    Case {
        name: "DuplicateSelectedEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::DuplicateSelectedEvents { span: None },
    },
    Case {
        name: "CopyNotes (clip)",
        setup: select_all,
        command: |_| SequencerCommand::CopyNotes,
    },
    Case {
        name: "CutNotes (clip)",
        setup: select_all,
        command: |_| SequencerCommand::CutNotes,
    },
    Case {
        name: "PasteNotes (clip)",
        setup: copy_notes,
        command: |_| SequencerCommand::PasteNotes,
    },
    Case {
        name: "MuteSelectedEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::MuteSelectedEvents,
    },
    Case {
        name: "QuantizeEvents (clip)",
        setup: select_all,
        command: |_| SequencerCommand::QuantizeEvents,
    },
    Case {
        name: "ClearControllerSpan (bend, clip)",
        setup: none,
        command: |h| SequencerCommand::ClearControllerSpan {
            controller: Controller::PitchBend,
            start: 0,
            end: h.sequencer.tracks()[0].clips()[0].region_length(),
        },
    },
    Case {
        name: "SelectClipEvent",
        setup: none,
        command: |h| SequencerCommand::SelectClipEvent(lead_notes(h).first().copied()),
    },
    Case {
        name: "SelectEventsInRect (clip)",
        setup: none,
        command: |_| SequencerCommand::SelectEventsInRect {
            tick_min: 0,
            tick_max: i32::MAX,
            note_min: 0,
            note_max: 127,
        },
    },
    Case {
        name: "SelectAllEvents (clip)",
        setup: none,
        command: |_| SequencerCommand::SelectAllEvents,
    },
    Case {
        name: "ClearEventSelection (clip)",
        setup: select_all,
        command: |_| SequencerCommand::ClearEventSelection,
    },
    Case {
        name: "DragEventsVelocity (clip)",
        setup: none,
        command: |h| SequencerCommand::DragEventsVelocity {
            event_ids: lead_notes(h),
            nudge: -10,
            drag_id: 1,
        },
    },
    Case {
        name: "InsertNote",
        setup: none,
        command: |_| SequencerCommand::InsertNote {
            tick: 12,
            length: 240,
            pitch: 60,
            velocity: 100,
        },
    },
    Case {
        name: "DragNotes (clip)",
        setup: none,
        command: |h| SequencerCommand::DragNotes {
            event_ids: lead_notes(h),
            drag: NoteDrag::Move {
                delta_ticks: 24,
                delta_pitch: 1,
            },
            drag_id: 1,
        },
    },
    Case {
        name: "PreviewNote",
        setup: none,
        command: |_| SequencerCommand::PreviewNote {
            note: 60,
            velocity: 100,
        },
    },
    Case {
        name: "SelectNextTrack",
        setup: none,
        command: |_| SequencerCommand::SelectNextTrack,
    },
    Case {
        name: "SelectPrevTrack",
        setup: none,
        command: |_| SequencerCommand::SelectPrevTrack,
    },
    Case {
        name: "SelectTrackAt",
        setup: none,
        command: |_| SequencerCommand::SelectTrackAt(MAX_TRACKS - 1),
    },
    Case {
        name: "SetTrackOutput (to MIDI Out)",
        setup: none,
        command: |_| SequencerCommand::SetTrackOutput {
            track: 0,
            output: TrackOutput::MidiOut { channel: 0 },
        },
    },
    Case {
        name: "SetTrackVolume",
        setup: none,
        command: |_| SequencerCommand::SetTrackVolume {
            track_idx: 0,
            volume_db: -6.0,
        },
    },
    Case {
        name: "SetTrackPan",
        setup: none,
        command: |_| SequencerCommand::SetTrackPan {
            track_idx: 0,
            pan: 0.5,
        },
    },
    Case {
        name: "ToggleTrackMute",
        setup: none,
        command: |_| SequencerCommand::ToggleTrackMute { track_idx: 0 },
    },
    Case {
        name: "ToggleTrackSolo",
        setup: none,
        command: |_| SequencerCommand::ToggleTrackSolo { track_idx: 0 },
    },
    Case {
        name: "SetTrackInstrumentState (a track's share)",
        setup: none,
        command: |_| SequencerCommand::SetTrackInstrumentState {
            slot: 0,
            state: vec![0xA5; STRESS_BLOB_BYTES / MAX_TRACKS].into(),
        },
    },
    Case {
        name: "RemoveTrack",
        setup: none,
        command: |_| SequencerCommand::RemoveTrack { track_idx: Some(0) },
    },
    Case {
        name: "RenameTrack",
        setup: none,
        command: |_| SequencerCommand::RenameTrack {
            track_idx: 0,
            name: Some("Renamed".to_owned()),
        },
    },
    Case {
        name: "SetKnobParam",
        setup: none,
        command: |_| SequencerCommand::SetKnobParam {
            track_idx: 0,
            knob: 0,
            param: None,
        },
    },
    Case {
        name: "PluginStateChanged",
        setup: none,
        command: |_| SequencerCommand::PluginStateChanged { track_idx: 0 },
    },
    Case {
        name: "SelectPerformanceLane",
        setup: none,
        command: |_| SequencerCommand::SelectPerformanceLane,
    },
    Case {
        name: "ResizeSelectedClipRegionEnd",
        setup: none,
        command: |_| SequencerCommand::ResizeSelectedClipRegionEnd {
            target_tick: BAR,
            drag_id: 1,
        },
    },
    Case {
        name: "ResizeSelectedClipRegionStart",
        setup: none,
        command: |_| SequencerCommand::ResizeSelectedClipRegionStart {
            target_tick: BAR,
            drag_id: 1,
        },
    },
    Case {
        name: "SetClipWindowEdge",
        setup: none,
        command: |_| SequencerCommand::SetClipWindowEdge {
            edge: ClipEdge::End,
            tick: BAR,
        },
    },
    Case {
        name: "SetClipEdgeToCursor",
        setup: none,
        command: |_| SequencerCommand::SetClipEdgeToCursor(ClipEdge::Start),
    },
    Case {
        name: "SelectClipSpan",
        setup: none,
        command: |h| SequencerCommand::SelectClipSpan {
            track_idx: 0,
            clip_id: h.sequencer.tracks()[0].clips()[0].id(),
        },
    },
    Case {
        name: "MoveClip (to the end)",
        setup: none,
        command: |h| SequencerCommand::MoveClip {
            track_idx: 0,
            clip_id: h.sequencer.tracks()[0].clips()[0].id(),
            to_track_idx: 0,
            to_start_tick: project_end(h),
        },
    },
    Case {
        name: "MoveRange (all, a bar on)",
        setup: none,
        command: |h| SequencerCommand::MoveRange {
            rect: everything(h),
            delta_ticks: BAR,
            delta_tracks: 0,
        },
    },
    Case {
        name: "ProjectAction::New",
        setup: none,
        command: |_| SequencerCommand::ProjectAction {
            action: ProjectAction::New,
            discard_changes: true,
        },
    },
    Case {
        name: "ImportMidiClip",
        setup: none,
        command: |h| {
            let mut clip = clip_at(0, BAR);
            clip.add_event(note_on(0));
            clip.add_event(note_off(240));
            clip.calculate_note_lengths();
            SequencerCommand::ImportMidiClip {
                clip: Box::new(clip),
                name: "Imported".to_owned(),
                target: Some((0, project_end(h))),
            }
        },
    },
    Case {
        name: "SetTempo",
        setup: none,
        command: |_| SequencerCommand::SetTempo {
            tempo_us: 400_000,
            gesture: None,
        },
    },
    Case {
        name: "SetMeter (3/4)",
        setup: none,
        command: |_| SequencerCommand::SetMeter {
            meter: Meter::new(3, 4).unwrap(),
        },
    },
    Case {
        name: "SetMarquee (all)",
        setup: none,
        command: |h| SequencerCommand::SetMarquee(Some(everything(h))),
    },
    Case {
        name: "SetModalOpen",
        setup: none,
        command: |_| SequencerCommand::SetModalOpen(true),
    },
];

/// A harness holding the stress project, saved as it stands, with the lead
/// clip under the clip cursor at its start.
fn stress_harness() -> Harness {
    let mut h = harness();
    fill_stress_project(&mut h.sequencer, STRESS_EVENTS, STRESS_BLOB_BYTES);
    h.handlers
        .sync_clip_selection_to_cursor_workflow(&mut h.sequencer);
    h.open.saved = SavedProject::of(&h.sequencer, None);
    h
}

/// How long `work` took.
fn timed(work: impl FnOnce()) -> Duration {
    let start = Instant::now();
    work();
    start.elapsed()
}

/// One row's times, each kept per round.
#[derive(Default)]
struct Row {
    /// The command.
    command: Vec<Duration>,
    /// Undoing it, when it recorded a step.
    undo: Vec<Duration>,
    /// Redoing it.
    redo: Vec<Duration>,
}

impl Row {
    /// The row as printed: medians and worsts in ms, `!` past the budget.
    fn line(&mut self, name: &str) -> String {
        let mut cells = format!("{name:<42}");
        for times in [&mut self.command, &mut self.undo, &mut self.redo] {
            if times.is_empty() {
                cells.push_str(&format!("{:>9} {:>9}", "-", "-"));
            } else {
                times.sort();
                let median = ms(times[times.len() / 2]);
                let worst = ms(times[times.len() - 1]);
                let flag = if worst > BUDGET_MS { "!" } else { " " };
                cells.push_str(&format!("{median:>9.2} {worst:>8.2}{flag}"));
            }
        }
        cells
    }
}

/// `time` in milliseconds.
fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

/// Times one case over [`ROUNDS`].
fn measure(case: &Case) -> Row {
    let mut row = Row::default();
    for _ in 0..ROUNDS {
        let mut h = stress_harness();
        let mut record: Record<SequencerEdit> = Record::new();
        (case.setup)(&mut h, &mut record);
        let steps = record.len();
        let command = (case.command)(&h);
        row.command
            .push(timed(|| run_command(&mut h, &mut record, command)));
        if record.len() > steps {
            row.undo.push(timed(|| {
                run_command(&mut h, &mut record, SequencerCommand::Undo)
            }));
            row.redo.push(timed(|| {
                run_command(&mut h, &mut record, SequencerCommand::Redo)
            }));
        }
    }
    row
}

/// A save's or a template's handover, as the `"sequencer"` thread runs it:
/// the snapshot and, for a save, the fingerprint. The job, and its drop,
/// stay out of the timing, as they are the `"storage"` thread's.
fn measure_save(template: bool) -> Row {
    let mut row = Row::default();
    let mut h = stress_harness();
    let mut record: Record<SequencerEdit> = Record::new();
    for _ in 0..ROUNDS {
        let cmd = if template {
            SequencerCommand::SaveTemplate {
                name: "Stress".to_owned(),
            }
        } else {
            SequencerCommand::SaveProject {
                filename: "Stress".to_owned(),
                folder: None,
            }
        };
        row.command
            .push(timed(|| run_command(&mut h, &mut record, cmd)));
        h.storage_jobs.try_iter().for_each(drop);
    }
    row
}

/// [`Sequencer::tick`] while playing the stress project from its start,
/// one row over [`PLAYBACK_TICKS`].
fn measure_playback() -> Row {
    let mut h = stress_harness();
    let end = project_end(&h);
    h.region.0.store(0, Ordering::Relaxed);
    h.region.1.store(end, Ordering::Relaxed);
    h.running.store(true, Ordering::Relaxed);
    h.sequencer.reset_to_tick(0);
    let mut row = Row::default();
    for _ in 0..PLAYBACK_TICKS {
        row.command.push(timed(|| h.sequencer.tick(Instant::now())));
    }
    row
}

/// Prints the report. See the module docs.
#[test]
#[ignore = "a measurement, not a check: run it by hand, in release"]
fn command_timing() {
    // `println!`, not the log: this is a test, which installs no logger, and
    // the report on stdout is its whole point.
    println!();
    println!(
        "command timing: {STRESS_EVENTS} events, {} MB of plugin state, {ROUNDS} rounds, ms, ! past {BUDGET_MS} ms",
        STRESS_BLOB_BYTES / 1_000_000
    );
    println!(
        "{:<42}{:>9} {:>9}{:>9} {:>9}{:>9} {:>9}",
        "command", "median", "worst", "undo", "worst", "redo", "worst"
    );
    for case in CASES {
        let mut row = measure(case);
        println!("{}", row.line(case.name));
    }
    println!("{}", measure_save(false).line("SaveProject (the handover)"));
    println!("{}", measure_save(true).line("SaveTemplate (the handover)"));
    println!(
        "{}",
        measure_playback().line("Sequencer::tick (playing, per tick)")
    );
}
