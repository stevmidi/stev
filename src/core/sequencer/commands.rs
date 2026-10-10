//! The [`SequencerCommand`] enum — the `"sequencer"` thread's inbound command
//! vocabulary. Handled in `event_handlers/sequencer_handler.rs`; see that file
//! and `050-undo-redo.md`.

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::core::project::{ProjectAction, StagedProject};
use crate::core::sequencer::TempoGesture;
use crate::core::time::Meter;
use crate::core::view_state::Pane;
use crate::models::clip::{Clip, ClipEdge, NoteDrag};
use crate::models::track::TrackOutput;

/// Everything the `"sequencer"` thread can be asked to do, other than the
/// transport-only subset ([`TransportCommand`](crate::core::transport::TransportCommand)).
///
/// One stream, one `select!` arm: `handle_sequencer_command` in
/// `event_handlers/sequencer_handler.rs` matches every variant. Variants that
/// mutate persisted data go through the undo system — they build a
/// [`SequencerEdit`](crate::core::sequencer::SequencerEdit) and record it (see
/// `050-undo-redo.md`); the rest call a `Sequencer` method / workflow helper
/// directly. The doc on each variant notes which, and anything a reader would
/// otherwise get wrong; a bare name with an obvious effect gets a short line.
///
/// Senders are stateless (`080-conventions.md`): the operand resolution
/// (range vs. selected clip, running vs. stopped) lives in the handler, not the
/// caller.
pub(crate) enum SequencerCommand {
    // --- Clip lifecycle ---
    /// Enter the selected clip's piano roll (`Arranger` → `Clip`).
    EnterClip,
    /// Leave the clip view (`Clip` → `Arranger`).
    ExitClip,
    /// Give the keyboard to `pane` — a click in the unfocused one of the two
    /// docked panes. Only the focus (`ViewState` `Arranger` ⇄ `Clip`) moves;
    /// nothing is shown or hidden. See `archive/210-docked-clip-panel.md`.
    FocusPane(Pane),
    /// The capture action (the `/` key): commit the live take to a new clip,
    /// or insert it into the lead clip — the last pass while running, the
    /// detected phrase while stopped. See `sequencer_handler.rs` and
    /// `220-capture-without-pending-view.md`.
    Commit,
    /// Place the clip cursor at an absolute tick — the clip-view equivalent of
    /// a timeline click.
    CommitClipClickByTicks(i32),
    /// Start or stop live MIDI recording on the armed track (see
    /// `090-live-recording.md`).
    ToggleLiveRecording,

    // --- Clip operations ---
    /// Inserts an empty clip on the selected track (`CommitClipEdit::from_empty_clip`,
    /// undoable): spanning the marquee's tick range when `time_bounds` has
    /// one, otherwise one bar from the cursor. A no-op when it would overlap
    /// a clip. Backs `⇧⌘/Ctrl+M` in the Arranger.
    InsertEmptyClip {
        /// The marquee selection, or `None` for one bar at the cursor.
        time_bounds: Option<TimeSelectionRect>,
    },
    /// Removes clips: an Ableton-style carve-out of the marquee's tick range
    /// across the marqueed tracks — every clip overlapping the range is
    /// trimmed or split so only the parts outside it survive
    /// (`DeleteInRangeEdit::from_track_span`), no rippling, a gap is left.
    /// Marquee-only, like `DuplicateClips`: there is no selected-clip
    /// fallback, the view never sends `Delete`/`Backspace` without a time
    /// selection. Backs `Delete`/`Backspace` in the Arranger.
    RemoveClips {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Splits clips at the current cursor tick (`SplitClipsEdit`). `Some(rect)`
    /// splits every clip on the marqueed tracks that overlaps the marquee's
    /// tick range and actually contains the cursor; `None` splits just the
    /// selected clip, no-op if nothing is selected or the cursor isn't
    /// strictly inside it — the one remaining range-or-selected-clip
    /// binding. Backs `⌘/Ctrl+E`.
    SplitClips {
        /// The marquee selection, or `None` to fall back to the selected clip.
        time_bounds: Option<TimeSelectionRect>,
    },
    /// Ableton/Bitwig-style "Insert Silence": inserts `end - start` ticks of
    /// empty time at `start`, across all tracks. Every clip starting at or
    /// after `start` moves right by that amount; any clip straddling
    /// `start` is split there first (`InsertSilenceEdit` reuses
    /// `SplitClipsEdit` for this) so its right-hand half moves along with
    /// everything else. No-ops if nothing needs to move.
    InsertSilenceInRange {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// The exact opposite of `InsertSilenceInRange` — "Delete Time": carves
    /// `[start, end)` out of every track non-rippling (`DeleteInRangeEdit`,
    /// reused verbatim), then shifts every clip at or after `end` left by
    /// `end - start` to close the gap it left behind (`DeleteTimeEdit`).
    /// No-ops if there is nothing to carve or shift.
    DeleteTimeInRange {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// Ableton-style "Duplicate Time" — the **Shift+⌘/Ctrl+D** global form:
    /// inserts `end - start` ticks of empty time at `end` — pushing every
    /// clip at or after `end` right on every track, splitting any straddler
    /// (`InsertSilenceEdit`) — then drops a copy of the `[start, end)` slice
    /// of every overlapping clip on every track into the freed span
    /// (`PasteClipsEdit`). Nothing is carved/destroyed — the gap is pre-cleared.
    /// The Arranger time selection then advances to `[end, 2*end - start)` so
    /// repeated presses chain down the timeline. No-ops if the range overlaps no
    /// clip on any track.
    DuplicateTimeInRange {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// Plain **⌘/Ctrl+D** — "Duplicate Clips" (`DuplicateClipsEdit`, a
    /// `PasteClipsEdit` anchored at the marquee's `end`): pastes the
    /// `[start, end)` slice of every overlapping clip on the marqueed tracks
    /// at `original + width`, carving out whatever already sits in the
    /// destination span on those tracks (the copy wins). Nothing is shifted
    /// and tracks outside the marquee are untouched — contrast
    /// `DuplicateTimeInRange`. The marquee then advances onto the copy so
    /// repeated presses chain. Marquee-only: no selected-clip fallback.
    /// No-ops if the marquee overlaps no clip.
    DuplicateClips {
        /// The marquee selection driving the track/tick span duplicated.
        rect: TimeSelectionRect,
    },
    /// **⌘/Ctrl+J** — "Merge Clips" (`PasteClipsEdit::merging`, Ableton's
    /// Consolidate): bakes the marquee into one clip per marqueed track,
    /// spanning exactly its tick range, carving what it replaces.
    /// Marquee-only: no selected-clip fallback. No-ops if the marquee
    /// overlaps no clip or the merge would change nothing.
    MergeClips {
        /// The marquee selection driving the track/tick span merged.
        rect: TimeSelectionRect,
    },
    /// Copies the portion of every clip on every track intersecting the time
    /// selection into the session clipboard (see `sequencer/clipboard.rs`).
    /// Bypasses the marquee's track range — always every track
    /// (`Shift+⌘/Ctrl+C`). Marquee-only, like `RemoveClips`: there is no
    /// selected-clip fallback, the view never sends this without a time
    /// selection. Not undoable — copying is not a data mutation.
    CopyClips {
        /// Grid-snapped, normalized time selection start tick (inclusive).
        start: i32,
        /// Grid-snapped, normalized time selection end tick (exclusive).
        end: i32,
    },
    /// Cut: copies the time selection into the session clipboard exactly like
    /// `CopyClips` with `Some` bounds, then carves `[start, end)` out across
    /// all tracks (`DeleteInRangeEdit`). Bypasses the marquee's track range,
    /// like `CopyClips` (`Shift+⌘/Ctrl+X`). Marquee-only, like `RemoveClips`:
    /// there is no selected-clip fallback, the view never sends this without
    /// a time selection. The clipboard fill is not undoable; the deletion is.
    CutClips {
        /// Grid-snapped, normalized time selection start tick (inclusive).
        start: i32,
        /// Grid-snapped, normalized time selection end tick (exclusive).
        end: i32,
    },
    /// Plain `⌘/Ctrl+C`: copies the tick-range intersection of every clip on
    /// the marqueed tracks into the session clipboard (see
    /// `sequencer/clipboard.rs::copy_clips_scoped_to_clipboard`). Distinct
    /// from `CopyClips` (`Shift+⌘/Ctrl+C`), which always spans every track
    /// regardless of the marquee. Marquee-only, like `RemoveClips`: no
    /// selected-clip fallback. Not undoable.
    CopyClipsScoped {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Plain `⌘/Ctrl+X`: copies exactly like `CopyClipsScoped`, then carves
    /// the marquee rectangle out of the marqueed tracks
    /// (`DeleteInRangeEdit::from_track_span`, the exact edit `RemoveClips`
    /// uses). Distinct from `CutClips` (`Shift+⌘/Ctrl+X`), which always
    /// carves across every track. Marquee-only, like `RemoveClips`: no
    /// selected-clip fallback. The clipboard fill is not undoable; the
    /// deletion is.
    CutClipsScoped {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Pastes the clipboard at the cursor tick (`PasteClipsEdit`). Each piece
    /// goes back on its absolute source track. Existing clips under a pasted
    /// clip are carved out Ableton-style, exactly like `RemoveClips`.
    PasteClips,
    /// Stretches the selected clip one bar longer (`direction > 0`) or shorter,
    /// retiming its notes to fill the new length — and, when it is the only
    /// clip, the global tempo with it (`RetimeClipEdit::rescale_selected`, undoable).
    /// Bound to `⌥=`/`⌥-` in `Clip`.
    RescaleSelectedClipTempo(i32),
    /// `M` in the Arranger: splits every clip on the marqueed tracks that
    /// overlaps the marquee's tick range at its edges and flips the mute flag
    /// on every piece that ends up fully inside the range
    /// (`MuteInRangeEdit::from_track_span`) — mute all if any is unmuted,
    /// otherwise unmute all. Marquee-only, like `RemoveClips`: no
    /// selected-clip fallback. Undoable.
    MuteClipsInRange {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },

    // --- Clip cursor and region ---
    /// Move the selected clip's cursor by a signed tick amount.
    NudgeClipCursor(i32),
    /// Move the selected clip's cursor to the next/previous grid boundary.
    /// In `Clip` the view resolves the grid — the zoom-adaptive snap,
    /// `Display::cursor_grid_ticks`, the same resolution as the mouse — the
    /// arranger's `MoveCursorByGrid` pattern; the input handler's own step
    /// is a 16th.
    NudgeClipCursorByGrid {
        /// Signed step: `±` one grid unit, in ticks.
        step_ticks: i32,
    },
    /// Jump the selected clip's cursor to its region start.
    MoveClipCursorToStart,
    /// Jump the selected clip's cursor to its region end.
    MoveClipCursorToEnd,
    /// The clip view's `⌥Space`: restart playback from the lead clip's
    /// cursor, in the pass under the arranger cursor, leaving the arranger
    /// cursor where it is. A no-op with no lead clip.
    PlayFromClipCursor,
    // --- Event editing ---
    /// Shift the selected events in time by a signed tick amount
    /// (`NudgeSelectedEventsEdit`, undoable).
    NudgeSelectedEvents(i32),
    /// Grow/shrink the selected events' note lengths by a signed tick amount
    /// (`NudgeSelectedEventsLengthEdit`, undoable).
    NudgeSelectedEventsLength(i32),
    /// Transpose the selected events by a signed number of semitones
    /// (`TransposeSelectedEventsEdit`, undoable); previews the resulting pitch.
    TransposeSelectedEvents(i32),
    /// Delete every selected event (`DeleteSelectedEventsEdit`, undoable).
    DeleteSelectedEvents,
    /// Duplicate the selected notes flush after the selection, as a paste
    /// there (`InsertNotesEdit::duplicating_selection`, undoable).
    DuplicateSelectedEvents,
    /// `⌘/Ctrl+C` in the clip view: copies the selected notes into the note
    /// clipboard (see `sequencer/clipboard.rs`). A no-op with nothing
    /// selected. Not undoable — copying is not a data mutation.
    CopyNotes,
    /// `⌘/Ctrl+X` in the clip view: `CopyNotes`, then deletes the selected
    /// notes (`DeleteSelectedEventsEdit`, undoable).
    CutNotes,
    /// `⌘/Ctrl+V` in the clip view: pastes the note clipboard into the lead
    /// clip with its earliest note at the clip cursor (`InsertNotesEdit`,
    /// undoable).
    PasteNotes,
    /// Toggles mute on every selected event (its `NoteOn` and paired
    /// `NoteOff` together): mute all if any is unmuted, otherwise unmute all
    /// — same toggle rule as `MuteClipsInRange`, scoped to events instead of
    /// whole clips (`MuteSelectedEventsEdit`, undoable). Bound to `M` in
    /// `Clip` with notes selected; the only other `M` binding is the
    /// Arranger's marquee mute.
    MuteSelectedEvents,
    /// Quantize the selected events, or the whole clip window with none
    /// selected (`QuantizeEventsEdit`, undoable — see
    /// `070-quantization.md`). Bound to `Q` in `Clip`; the Arranger has no
    /// quantize.
    QuantizeEvents,
    /// Click-to-select in `Clip`: `Some(id)` replaces the whole
    /// selection with that one event; `None` (empty-grid click) deselects
    /// everything.
    SelectClipEvent(Option<Uuid>),
    /// Marquee (rubber-band) select: replaces the selection with every
    /// `NoteOn` event overlapping the given tick/note rectangle. Sent
    /// continuously while a drag is in progress (see `Display::forward_input_event`).
    SelectEventsInRect {
        /// Low tick bound of the selection rectangle.
        tick_min: i32,
        /// High tick bound of the selection rectangle.
        tick_max: i32,
        /// Low note-number bound of the selection rectangle.
        note_min: u8,
        /// High note-number bound of the selection rectangle.
        note_max: u8,
    },
    /// Select every `NoteOn` event in the current clip.
    SelectAllEvents,
    /// Drop the whole event selection, and publish the empty selection
    /// (`SharedAtomics::has_event_selection`).
    ClearEventSelection,
    /// ⌘/Ctrl+drag velocity gesture: nudges the velocity of exactly
    /// `event_ids` (not necessarily the current selection — see
    /// `Display::begin_velocity_drag`) by a relative amount. Sent
    /// continuously while the drag is in progress; `drag_id` lets the undo
    /// stack coalesce every nudge from one drag into a single step.
    DragEventsVelocity {
        /// The events the gesture targets — not necessarily the current selection.
        event_ids: Vec<Uuid>,
        /// Signed relative amount.
        nudge: i32,
        /// Identifies the drag so the undo stack coalesces its nudges into one step.
        drag_id: u64,
    },
    /// Double-click on empty piano-roll grid: one note into the lead clip at
    /// event tick `tick` and `pitch` (`InsertNotesEdit`, undoable). It becomes
    /// the selection and is auditioned. The view resolves the snapped tick and
    /// the last-used `length`/`velocity`; the model clamps them into the clip.
    InsertNote {
        /// Event tick of the note's start, grid-snapped by the view.
        tick: i32,
        /// Note length in ticks.
        length: i32,
        /// Note number.
        pitch: i32,
        /// Note velocity.
        velocity: i32,
    },
    /// A note move / resize drag in the piano roll, applied live: sent on
    /// each pointer move that changes it, `drag` being the whole drag from
    /// the press (`DragNotesEdit`, undoable, `drag_id`'s steps merge into
    /// one step). Targets exactly `event_ids`; never changes the selection.
    DragNotes {
        /// The dragged notes' `NoteOn` ids.
        event_ids: Vec<Uuid>,
        /// The whole drag so far, from the press.
        drag: NoteDrag,
        /// Identifies the gesture so its steps merge into one undo step.
        drag_id: u64,
    },
    /// Audition `note` at `velocity` on the selected track
    /// (`Sequencer::preview_pitch`) — a note move drag's pitch change, before
    /// anything is committed.
    PreviewNote {
        /// Note number.
        note: u8,
        /// Note velocity.
        velocity: u8,
    },

    // --- Track ---
    /// Select the next track, wrapping.
    SelectNextTrack,
    /// Select the previous track, wrapping.
    SelectPrevTrack,
    /// Mouse-driven track selection: selects the track at `idx` directly,
    /// e.g. a click on an arranger lane. No-ops on an out-of-range index.
    SelectTrackAt(usize),
    /// Set a track's output routing (a MIDI channel or a hosted instrument
    /// plugin — see [`TrackOutput`]). Sent from the track header's output
    /// menu and the browser panel's Plugins category. Answered with
    /// `UiEvent::TrackOutputsChanged`.
    SetTrackOutput {
        /// Target track index.
        track: usize,
        /// The new routing.
        output: TrackOutput,
    },
    /// Sets a track's mixer volume (dB) or stereo balance (`-1.0`..=`1.0`).
    /// Direct mutation of the shared [`TrackMixAtomics`](crate::core::shared_atomics::TrackMixAtomics)
    /// — not undoable, like region length. Sent continuously while the arranger
    /// track-header bar is dragged. Out-of-range indices no-op.
    SetTrackVolume {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
        /// New volume in dB.
        volume_db: f32,
    },
    /// Companion to [`SetTrackVolume`](Self::SetTrackVolume) for stereo
    /// balance; same not-undoable, drag-continuous story.
    SetTrackPan {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
        /// New stereo balance in `[-1.0, 1.0]`.
        pan: f32,
    },
    /// Flips a track's mute / solo. Direct mutation of the shared
    /// [`TrackMixAtomics`](crate::core::shared_atomics::TrackMixAtomics) — not
    /// undoable, like volume/pan and metronome mute. On a change that silences a
    /// track mid-playback the sequencer also releases its sounding notes.
    /// Out-of-range indices no-op.
    ToggleTrackMute {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
    },
    /// Solo counterpart of [`ToggleTrackMute`](Self::ToggleTrackMute).
    ToggleTrackSolo {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
    },
    /// Stores a fresh plugin-state blob on the instrument of the track in
    /// engine slot `slot` so the next project save persists it. No-op if no
    /// track holds the slot or it is not routed to an instrument. See
    /// `130-plugin-host.md`.
    SetTrackInstrumentState {
        /// The track's engine slot (`Track::slot`).
        slot: usize,
        /// Opaque plugin-state blob.
        state: Vec<u8>,
    },
    /// Adds an empty MIDI track at `track_idx` (after the selected one when
    /// `None`) as one undo step (`AddTrackEdit`). Refused at `MAX_TRACKS` and
    /// during a live take, with a footer message.
    AddTrack {
        /// Where the track goes; `None` = after the selected track.
        track_idx: Option<usize>,
    },
    /// Removes `track_idx` (the selected track when `None`) as one undo step
    /// (`RemoveTrackEdit`). Refused on the last remaining track and during a
    /// live take, with a footer message.
    RemoveTrack {
        /// The track to remove; `None` = the selected track.
        track_idx: Option<usize>,
    },
    /// Names `track_idx` `name` as one undo step (`RenameTrackEdit`); `None`
    /// goes back to the track's number. Nothing when the name is unchanged.
    RenameTrack {
        /// The track to rename.
        track_idx: usize,
        /// Its new name, already through `track_name_from_input`.
        name: Option<String>,
    },

    // --- Arranger performance lane ---
    /// Arms the physical MIDI keyboard for bar-jump triggering instead of
    /// normal note capture — mirrors `SelectTrackAt`, but there's only one
    /// lane so it takes no index.
    SelectPerformanceLane,

    // --- Region ---
    /// Mouse drag-resize of a clip's right edge to an absolute target tick.
    /// Sent continuously while the drag is in progress — see
    /// `Display::extend_clip_resize_drag`. Each is a `ResizeClipEdit`; one
    /// drag's share `drag_id` and merge into one undo step.
    ResizeSelectedClipRegionEnd {
        /// Grid-snapped absolute tick the edge is dragged to.
        target_tick: i32,
        /// The drag gesture.
        drag_id: u64,
    },
    /// Mouse drag-resize of a clip's left edge to an absolute target tick.
    ResizeSelectedClipRegionStart {
        /// Grid-snapped absolute tick the edge is dragged to.
        target_tick: i32,
        /// The drag gesture.
        drag_id: u64,
    },
    /// Enter in the arranger or the clip view: fit the tempo to the
    /// project's only clip (`RetimeClipEdit`); a no-op with more than one
    /// clip, or when it's already whole bars. `220-capture-without-pending-view.md`.
    FitTempo,
    /// `[`/`]`: move a clip edge to the cursor, undoably (`ResizeClipEdit`).
    /// In the arranger an arrangement trim at the arranger cursor, on the
    /// clip under it or the nearest one on that side
    /// (`Sequencer::arranger_edge_clip_id`); in the clip view a start/end
    /// marker on the lead clip at the clip cursor
    /// (`Sequencer::selected_clip_start_marker_at`/`_end_marker_at`).
    SetClipEdgeToCursor(ClipEdge),
    /// Arranger clip band press: select the clip, put the cursor on its start
    /// and marquee its span (`select_clip_span_workflow`). Not undoable —
    /// selection only. See `020-views-and-state.md` § "Clip Band Press & Move
    /// Drag".
    SelectClipSpan {
        /// Track the clip is on.
        track_idx: usize,
        /// The clip's id.
        clip_id: Uuid,
    },
    /// Arranger clip band drag release: move the clip to `to_track_idx` /
    /// `to_start_tick`, carving out what it lands on (`MoveClipEdit`,
    /// undoable). Sole caller is the arranger band drag; carries explicit ids
    /// so it never depends on the press's asynchronous selection having
    /// landed.
    MoveClip {
        /// Track the clip is on now.
        track_idx: usize,
        /// The clip's id.
        clip_id: Uuid,
        /// Track it is dropped on.
        to_track_idx: usize,
        /// Grid-snapped start tick it is dropped at.
        to_start_tick: i32,
    },
    /// Arranger marquee band drag release: move everything inside `rect` by
    /// `delta_ticks` / `delta_tracks`, splitting the pieces out of their
    /// clips first and carving out what they land on (`MoveRangeEdit`,
    /// undoable). The marquee travels with the content.
    MoveRange {
        /// The marquee the drag started in, frozen at press time.
        rect: TimeSelectionRect,
        /// Grid-snapped tick shift.
        delta_ticks: i32,
        /// Lane shift.
        delta_tracks: i32,
    },

    // --- Project ---
    /// Run `action` — a new project, a load, quitting — unless
    /// `discard_changes` is false and the project changed since it was last
    /// saved or loaded: then ask the view to prompt
    /// (`UiEvent::UnsavedChanges`). See `060-persistence.md` § Unsaved
    /// changes.
    ProjectAction {
        /// What to do.
        action: ProjectAction,
        /// Skip the check: the prompt's "Don't Save".
        discard_changes: bool,
    },
    /// Apply a project the view staged (`InputEvent::ApplyStagedProject`).
    ApplyStagedProject(Box<StagedProject>),
    /// Write the current project to `folder/filename.stev` and say so in the
    /// footer (`UiEvent::Status`) — the save or why it failed.
    SaveProject {
        /// Project file name, without extension.
        filename: String,
        /// `None` means the root fallback — no project folder selected yet.
        folder: Option<String>,
    },
    /// Write the lead clip to a `.mid` in `folder`, named after the project
    /// (`Sequencer::export_lead_clip`, `storage::save_clip_export`). Says how
    /// it went in the footer (`UiEvent::Status`), refusals included.
    ExportClip {
        /// The arranger marquee; `None` in the clip view or without one.
        time_bounds: Option<TimeSelectionRect>,
        /// `None` means the root fallback — no project folder selected yet.
        folder: Option<String>,
        /// What the file name starts with (`InputEvent::ExportClip`).
        name: String,
    },
    /// Put an imported `.mid`'s clip on a track (`PasteClipsEdit::importing`,
    /// `InputEvent::ImportMidiClip`), one undoable step. Says how it went in
    /// the footer (`UiEvent::Status`).
    ImportMidiClip {
        /// The clip, built from the file.
        clip: Box<Clip>,
        /// The file's name (stem), for the footer.
        name: String,
        /// `(track index, start tick)`, or `None` for the selected track at
        /// the cursor.
        target: Option<(usize, i32)>,
    },

    // --- Undo / redo ---
    /// Undo the last recorded [`SequencerEdit`](crate::core::sequencer::SequencerEdit)
    /// and apply its [`EditResult`](crate::core::sequencer::EditResult). See
    /// `050-undo-redo.md`.
    Undo,
    /// Redo the last undone edit.
    Redo,

    // --- Tempo ---
    /// Register one tap on the tempo-tap button; once enough taps have landed
    /// their spacing sets the project tempo (`time::TapTempo`, last 8 taps),
    /// a burst of taps one undo step (`SetTempoEdit`).
    TapTempo,
    /// The header's BPM field or chip drag: set the project tempo, one undo
    /// step per typed value or per drag (`SetTempoEdit`).
    SetTempo {
        /// The new tempo, µs per quarter (clamped to the app's range).
        tempo_us: i32,
        /// The drag this step belongs to; `None` for a typed value.
        gesture: Option<TempoGesture>,
    },
    /// The header's meter field: set the project's time signature, one undo
    /// step per typed value (`SetMeterEdit`).
    SetMeter {
        /// The new meter.
        meter: Meter,
    },
}
