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
//! - `commands.rs` — the [`SequencerCommand`] enum, the thread's inbound
//!   vocabulary.
//! - `capture/` — the capture buffer into clips: the running `/` in `mod.rs`
//!   (`090-recording-midi-input.md`, `100-running-capture.md`), the stopped
//!   `/`'s phrase detection and the take session beside it
//!   (`040-phrase-detection.md`, `220-capture-without-pending-view.md`).
//! - `region.rs` — the selected clip's edge targets and trims, and the clip
//!   cursor.
//! - `tempo.rs` — first-clip tempo detection and the clip-relative tempo
//!   rescale behind Enter's fit.
//! - `selection.rs` / `view.rs` — clip & event selection, clip-view snapshots.
//! - `clipboard.rs` — the clip and note clipboards and the copies that fill
//!   them.
//! - `mix.rs` — the not-undoable per-track mixer writers.
//! - `knobs.rs` — what the knobs drive, set by a load, and the hand's turns
//!   for the unsaved check (`400-parameter-knobs.md`).
//! - `export.rs` — the MIDI clip export: which clip, and its `.mid` bytes.
//! - `audio_export.rs` — the audio export's plan (which tracks get a stem,
//!   their file names, the span) and the run's start/end marks
//!   (`130-plugin-host.md` § Audio export).
//! - `performance_lane.rs` — arming / trigger routing for the arranger lane
//!   (`110-performance-lane.md`).
//! - `instrument_event.rs` / `instrument_notes.rs` — the CLAP clip-event feed
//!   and the note-tracking safety net (`130-plugin-host.md`).
//! - `test_support.rs` (tests) — fixtures shared by the sequencer's unit
//!   tests.

mod audio_export;
mod capture;
mod clipboard;
mod commands;
mod edit;
mod export;
mod instrument_event;
mod instrument_notes;
mod knobs;
mod mix;
mod performance_lane;
mod playback;
mod region;
mod selection;
mod state;
mod tempo;
#[cfg(test)]
pub(crate) mod test_support;
mod view;

use capture::CaptureInsert;

pub(crate) use audio_export::{
    AudioExportJob, AudioExportOutcome, AudioExportRefusal, AudioExportRun, ExportMark,
};
// Only the plugin host's export tap (macOS) reads the slots from outside.
#[cfg(target_os = "macos")]
pub(crate) use audio_export::SlotMask;
#[cfg(test)]
pub(crate) use audio_export::{AudioExportPlan, StemPlan};
pub(crate) use capture::TakePlace;
pub(crate) use commands::SequencerCommand;
pub(crate) use edit::{
    AddTrackEdit, ClearControllerSpanEdit, CommitClipEdit, CreateClipsEdit, DeleteInRangeEdit,
    DeleteSelectedEventsEdit, DeleteTimeEdit, DragEventsVelocityEdit, DragNotesEdit,
    DuplicateClipsEdit, DuplicateTimeEdit, EditResult, InsertCaptureEdit, InsertNotesEdit,
    InsertSilenceEdit, MoveClipEdit, MoveRangeEdit, MuteInRangeEdit, MuteSelectedEventsEdit,
    NudgeSelectedEventsEdit, NudgeSelectedEventsLengthEdit, PasteClipsEdit, PasteLead,
    QuantizeEventsEdit, RemoveTrackEdit, RenameTrackEdit, ReplaceClipNotesEdit, ResizeClipEdit,
    RetimeClipEdit, SequencerEdit, SetKnobParamEdit, SetMeterEdit, SetTempoEdit,
    SetTrackOutputEdit, SplitClipsEdit, StepOutput, TempoGesture, TransposeSelectedEventsEdit,
    UndoStep, ViewSnapshot, top_step,
};
pub(crate) use export::ExportRefusal;
pub(crate) use instrument_event::ClipInstrumentEvent;
#[cfg(target_os = "macos")]
pub(crate) use instrument_event::{NoteSource, VoiceMessage};
// Only the plugin host's mixer (macOS) reads the timing from outside.
#[cfg(target_os = "macos")]
pub(crate) use instrument_event::EventTime;
pub(crate) use region::EdgeTarget;
pub(crate) use state::Sequencer;
