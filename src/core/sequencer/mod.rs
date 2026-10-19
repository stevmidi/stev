//! The sequencer: the model the `"sequencer"` thread owns and drives.
//!
//! [`Sequencer`] (in `state.rs`) holds the tracks (up to `MAX_TRACKS`), the live
//! capture clip, selection, the performance lane and the
//! clip clipboard. The thread ticks it once per clock pulse and applies every
//! [`SequencerCommand`] against it (via `event_handlers/`). Data mutations that
//! must be undoable go through the `edit/` submodule as a
//! [`SequencerEdit`] producing an [`EditResult`] (`050-undo-redo.md`).
//!
//! Submodules by concern:
//! - `state.rs` — the `Sequencer` struct, construction, and the bulk of the
//!   accessors / simple mutators.
//! - `playback.rs` — the per-tick pump (`tick` / `chase_notes`) and the
//!   note-release + re-seek paths every discontinuity goes through.
//! - `capture.rs` — turning the running capture buffer into committed clips
//!   (`090-live-recording.md`, `100-running-capture.md`).
//! - `capture_fixture.rs` — debug builds: dumping stopped captures as test
//!   fixtures for the phrase-start detection (`220`).
//! - `stopped_capture.rs` / `region/` — the stopped `/`: framing a take by
//!   phrase detection into a new clip or an insert (`040-phrase-detection.md`,
//!   `220-capture-without-pending-view.md`).
//! - `take_framing.rs` — the take session the stopped `/` into a clip opens:
//!   framing the notes to insert, heard in place (`290-framing-a-take.md`).
//! - `live_recording.rs` — the dual-state live-record path.
//! - `selection.rs` / `view.rs` — clip & event selection, clip-view snapshots.
//! - `mix.rs` — the not-undoable per-track mixer writers.
//! - `export.rs` — the MIDI clip export: which clip, and its `.mid` bytes.
//! - `audio_export.rs` — the audio export's plan (which tracks get a stem,
//!   their file names, the span) and the run's start/end marks
//!   (`300-export-to-audio.md`).
//! - `performance_lane.rs` — arming / trigger routing for the arranger lane
//!   (`110-performance-lane.md`).
//! - `instrument_event.rs` / `instrument_notes.rs` — the CLAP clip-event feed
//!   and the note-tracking safety net (`130-plugin-host.md`).

mod audio_export;
mod capture;
#[cfg(debug_assertions)]
mod capture_fixture;
mod clipboard;
mod commands;
mod edit;
mod export;
mod instrument_event;
mod instrument_notes;
mod live_recording;
mod mix;
mod performance_lane;
mod playback;
mod region;
mod selection;
mod state;
mod stopped_capture;
mod take_framing;
#[cfg(test)]
pub(crate) mod test_support;
mod view;

use capture::CaptureInsert;

pub(crate) use audio_export::{
    AudioExportEnd, AudioExportJob, AudioExportOutcome, AudioExportRefusal, AudioExportRun,
    ExportMark,
};
// Only the plugin host's export tap (macOS) reads the slots from outside.
#[cfg(target_os = "macos")]
pub(crate) use audio_export::SlotMask;
#[cfg(test)]
pub(crate) use audio_export::{AudioExportPlan, StemPlan};
pub(crate) use commands::SequencerCommand;
pub(crate) use edit::{
    AddTrackEdit, CommitClipEdit, DeleteInRangeEdit, DeleteSelectedEventsEdit, DeleteTimeEdit,
    DragEventsVelocityEdit, DragNotesEdit, DuplicateClipsEdit, DuplicateTimeEdit, EditResult,
    InsertCaptureEdit, InsertNotesEdit, InsertSilenceEdit, MoveClipEdit, MoveRangeEdit,
    MuteInRangeEdit, MuteSelectedEventsEdit, NudgeSelectedEventsEdit,
    NudgeSelectedEventsLengthEdit, PasteClipsEdit, PasteLead, QuantizeEventsEdit, RemoveTrackEdit,
    RenameTrackEdit, ResizeClipEdit, RetimeClipEdit, SequencerEdit, SetMeterEdit, SetTempoEdit,
    SetTrackOutputEdit, SplitClipsEdit, TempoGesture, TransposeSelectedEventsEdit,
};
pub(crate) use export::ExportRefusal;
pub(crate) use instrument_event::ClipInstrumentEvent;
// Only the plugin host's mixer (macOS) reads the timing from outside.
#[cfg(target_os = "macos")]
pub(crate) use instrument_event::EventTime;
pub(crate) use region::EdgeTarget;
pub(crate) use state::LiveRecResult;
pub(crate) use state::Sequencer;
