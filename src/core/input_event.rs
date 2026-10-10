//! The input the `"main"` thread hands `EventHandlers` each frame.
//!
//! `InputPoller` (`view/input_poller.rs`) reads egui's native input every frame
//! and emits these; the dispatch loop in `view/display/input/mod.rs` drains
//! them, resolving anything view-local (scroll, cursor placement, the drag
//! rectangles) against `Display` state first and forwarding the rest whole to
//! `EventHandlers`.
//!
//! Range-scoped variants come in two shapes. `...InTimeSelectionOrClip`
//! (split): the view has *already* resolved the operand — `Some` when a
//! real arranger time selection spans time (grid-snapped, normalized
//! `start < end`), `None` to fall back to the selected clip — but the
//! precedence between the two lives with the handler, not here
//! (`080-conventions.md`). `...InSelection`/`...InSelectionScoped` (delete,
//! copy, cut, mute, insert/delete time, duplicate): marquee-only — the
//! view sends nothing without a time selection, so the payload is bare.
//! Clipboard chords (`Copy`/`Cut`/`Paste`) arrive as bare intents because
//! egui-winit swallows the raw key press. See `020-views-and-state.md`.

use std::path::PathBuf;

use egui::{Key, Modifiers};
use uuid::Uuid;

use crate::{
    core::{
        project::{ProjectAction, StagedProject},
        time::Meter,
        view_state::Pane,
    },
    models::{
        clip::{Clip, NoteDrag},
        track::TrackOutput,
    },
};

/// The arranger's 2D marquee selection: a tick range × a track range.
/// `start`/`end` are grid-snapped and normalized (`start <= end` — a pure
/// vertical drag, spanning tracks without moving in time, produces
/// `start == end`); `track_start`/`track_end` are normalized
/// (`track_start <= track_end`), inclusive. Built view-side in
/// `Display::extend_time_selection`/`nudge_time_selection_edge` and forwarded
/// whole to operations scoped to the marqueed tracks; operations that bypass
/// the track range (Shift+⌘C/X, Insert Silence, Shift+⌘D) extract just
/// `(start, end)` instead. See `020-views-and-state.md`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimeSelectionRect {
    /// Low tick bound, inclusive.
    pub(crate) start: i32,
    /// High tick bound, exclusive.
    pub(crate) end: i32,
    /// Low track index, inclusive.
    pub(crate) track_start: usize,
    /// High track index, inclusive.
    pub(crate) track_end: usize,
}

impl TimeSelectionRect {
    /// Whether this selection has a real tick width, as opposed to being the
    /// point-in-time / track-only shape a purely vertical marquee drag
    /// produces (`start == end`, spanning tracks without carving out any
    /// time). Content-facing rendering (clip-body tint, the timeline
    /// rail/arrows) should treat `false` here the same as no selection at
    /// all — there is no tick range to visualize or for `⌘/Ctrl+C`/`X` to
    /// grab. `draw_selected_track_cursor`'s track-row expansion is the one
    /// consumer that deliberately ignores this and reads `track_start`/
    /// `track_end` regardless — see `020-views-and-state.md`.
    pub(crate) fn has_tick_range(&self) -> bool {
        self.end > self.start
    }

    /// The normalized selection spanning ticks `tick_a`..`tick_b` and tracks
    /// `track_a`..=`track_b`, each pair in either order.
    pub(crate) fn spanning(tick_a: i32, tick_b: i32, track_a: usize, track_b: usize) -> Self {
        Self {
            start: tick_a.min(tick_b),
            end: tick_a.max(tick_b),
            track_start: track_a.min(track_b),
            track_end: track_a.max(track_b),
        }
    }
}

/// One resolved user input, ready for `EventHandlers`. See the module docs for
/// the `...InTimeSelectionOrClip` / `...InSelection` operand conventions.
#[derive(Clone, Debug)]
pub(crate) enum InputEvent {
    /// A key went down, with the modifier state at that moment. The app is
    /// keyboard-first; most bindings resolve from here in
    /// `event_handlers/input_handler.rs`.
    KeyPressed {
        /// The key that went down.
        key: Key,
        /// Modifier state at the moment of the event.
        modifiers: KeyModifiers,
    },

    /// Pointer moved to `(x, y)` in screen space. Drives hover and any
    /// in-progress drag.
    MouseMoved {
        /// Pointer x in screen space.
        x: f32,
        /// Pointer y in screen space.
        y: f32,
        /// Modifier state sampled with the move — needed by drags whose rate
        /// depends on Shift being held mid-gesture (the track-header mix bars).
        modifiers: KeyModifiers,
    },
    /// Raw pointer motion `(dx, dy)` this frame while the primary button is
    /// held (egui's `pointer.motion()`, from winit's device events): unlike
    /// `MouseMoved` it keeps arriving once the pointer has left the window or
    /// hit the screen edge. Drives the piano roll's octave-legend zoom drag
    /// and the header's BPM chip drag, which must not stop at the window
    /// edge. View-local; never reaches
    /// `EventHandlers`.
    PointerMotion {
        /// Horizontal motion, rightward positive.
        dx: f32,
        /// Vertical motion, downward positive.
        dy: f32,
    },
    /// Primary-button press at `(x, y)` in screen space — the down edge that
    /// begins a click or a drag.
    MouseClicked {
        /// Pointer x in screen space.
        x: f32,
        /// Pointer y in screen space.
        y: f32,
        /// Modifier state at the moment of the event.
        modifiers: KeyModifiers,
    },
    /// Second press of a primary-button double-click at `(x, y)` in screen
    /// space — on the press, not egui's release-time `button_double_clicked`
    /// (`InputPoller`'s `DoubleClick`). Arrives right after that press's own
    /// `MouseClicked`. View-local:
    /// on empty piano-roll grid `Display` turns it into `InsertNote`; it
    /// never reaches `EventHandlers`.
    MouseDoubleClicked {
        /// Pointer x in screen space.
        x: f32,
        /// Pointer y in screen space.
        y: f32,
    },
    /// Primary button release edge. Carries no position: the poller emits the
    /// `MouseMoved` for the same pointer position earlier in the same batch, so
    /// any in-progress drag is already up to date by the time this arrives.
    MouseReleased,
    /// Files dragged from the file manager came over the window. `path` is
    /// the first MIDI file among them (`project::is_midi_file`), else the
    /// first file. While they hover, the pointer comes from the OS
    /// (`InputPoller::poll`'s `drag_pointer`): the windowing layer reports
    /// none during a drag.
    FileDragEntered {
        /// The file the drag is about.
        path: PathBuf,
    },
    /// The file drag left the window, or was cancelled, without a drop.
    FileDragLeft,
    /// Files from the file manager were dropped on the window; `path` is
    /// chosen as for `FileDragEntered`.
    FileDropped {
        /// The file dropped.
        path: PathBuf,
    },
    /// ⌘/Ctrl release edge — the commit trigger for the `⌘/Ctrl+←`/`→`
    /// marquee nudge (`020-views-and-state.md`), the one binding in the app
    /// that cares about a modifier's own up-edge rather than sampling it
    /// alongside a key/mouse event. Named for its role rather than the key
    /// so a rebind doesn't ripple. Fires on every ⌘/Ctrl-up regardless of
    /// context; the Arranger handler no-ops unless a keyboard-armed
    /// `ClipMoveDrag` is actually pending.
    MoveModifierReleased,
    /// Two-finger trackpad / wheel scroll over the arranger or the clip view.
    /// The deltas are egui's smoothed scroll delta in points. View-local:
    /// resolved in `Display::forward_input_event`, never reaches
    /// `EventHandlers`. Positive x = content moves right = scroll toward bar
    /// 1; positive y = content moves down = toward higher pitches in the
    /// piano roll / toward track 1 in the arranger (both already OS
    /// natural-scroll aware via egui-winit).
    TimelineScroll {
        /// egui's smoothed horizontal scroll delta, in points.
        delta_x: f32,
        /// egui's smoothed vertical scroll delta, in points — the piano
        /// roll's pitch scroll.
        delta_y: f32,
        /// Pointer x in canvas space — negative over the browser panel,
        /// which then scrolls instead; `None` with no pointer over the window.
        pointer_x: Option<f32>,
        /// Pointer y in screen space — picks the pane the scroll goes to
        /// when both are showing; `None` with no pointer over the window.
        pointer_y: Option<f32>,
    },
    /// ⌘/Ctrl+wheel or trackpad pinch: zoom the arranger or clip view
    /// horizontally. View-local like `TimelineScroll`, resolved in
    /// `Display::forward_input_event`, never reaches `EventHandlers`. egui
    /// folds a ⌘/Ctrl-modified wheel into this *instead of* the scroll delta,
    /// so one frame never yields both. See `archive/190-arranger-zoom.md` and
    /// `archive/200-clip-view-zoom.md`.
    TimelineZoom {
        /// Multiplicative scale change this frame (`> 1` zooms in), egui's
        /// `zoom_delta()`.
        factor: f32,
        /// Pointer x in screen space — the zoom's anchor when it is over the
        /// content area.
        pointer_x: Option<f32>,
        /// Pointer y in screen space — picks the pane, like
        /// `TimelineScroll::pointer_y`.
        pointer_y: Option<f32>,
    },
    /// A click the view has resolved into timeline space — used to place the
    /// cursor and (in the arranger) select the clip / lane under it.
    MouseClickedTicks {
        /// The pane the click landed in — the arranger or the clip view. Not
        /// the focus: a click in the unfocused pane focuses it
        /// (`FocusPane`, sent just before), and this must not depend on that
        /// having reached the sequencer yet.
        pane: Pane,
        /// Absolute tick at current mouse x (timeline-space), snapped to the
        /// active view's cursor grid resolution.
        tick_x: i32,
        /// Event under the click in `Clip`, resolved by the view
        /// via hit-testing; always `None` in `Arranger`.
        event_id: Option<Uuid>,
        /// Arranger track lane under the click, resolved by the view via
        /// hit-testing; always `None` outside `Arranger`, and also `None`
        /// when the click landed on the performance lane row instead (see
        /// `performance_lane_hit`).
        track_idx: Option<usize>,
        /// True when the click landed on the arranger performance lane row
        /// (reserved above track 1) rather than a track lane. Always
        /// `false` outside `Arranger`.
        performance_lane_hit: bool,
    },
    /// Select track `track_idx`, leaving the cursor where it is — a plugin
    /// dropped on a track from the browser panel.
    SelectTrack {
        /// The track to select.
        track_idx: usize,
    },
    /// A plain press on an arranger track header — left of the timeline, off
    /// its S/M buttons and mix bars: select that row and nothing else. No
    /// time lies under a header, so unlike `MouseClickedTicks` the cursor
    /// stays where it is and no marquee starts.
    TrackHeaderClicked {
        /// Track lane under the click; `None` when it landed on the
        /// performance lane row instead (see `performance_lane_hit`).
        track_idx: Option<usize>,
        /// True when the click landed on the performance lane row's header.
        performance_lane_hit: bool,
    },
    /// A press in the pane that doesn't have the keyboard: give it the focus
    /// (`SequencerCommand::FocusPane`). Sent ahead of the press's own
    /// `MouseClickedTicks`, so the keys that follow go to the pane clicked.
    /// See `archive/210-docked-clip-panel.md`.
    FocusPane {
        /// The pane to focus.
        pane: Pane,
    },
    /// Snap the loop region to the arranger time selection, or toggle looping.
    ///
    /// The view resolves the operand, not the command: `time_bounds` carries the
    /// time selection when one spans real time — already snapped to the view's
    /// cursor grid and normalized so `start < end` — and `None` means there is
    /// no selection, so the press just toggles `loop_enabled` in place. Operand
    /// precedence lives with the handler.
    SetRegionToTimeSelectionOrToggleLoop {
        /// Grid-snapped, normalized time selection `[start, end)`, or `None` to toggle looping in place.
        time_bounds: Option<(i32, i32)>,
    },
    /// Remove clips overlapping the marquee's tick range, on the marqueed
    /// tracks. Like `MuteClipsInSelection` there is no single-clip fallback:
    /// the view only sends this with a real time selection, so
    /// `Delete`/`Backspace` without one is a no-op. Forwarded to
    /// `SequencerCommand::RemoveClips`.
    RemoveClipsInSelection {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Insert an empty clip on the selected track: over the marquee's tick
    /// range, or one bar at the cursor with none (`⇧⌘/Ctrl+M`). Forwarded
    /// whole to `SequencerCommand::InsertEmptyClip`, which resolves it.
    InsertEmptyClip {
        /// The marquee selection, or `None` for one bar at the cursor.
        time_bounds: Option<TimeSelectionRect>,
    },
    /// Split clips at the cursor; `Some` splits every clip overlapping the
    /// marquee's tick range on the marqueed tracks, `None` splits just the
    /// selected clip. Forwarded whole to `SequencerCommand::SplitClips`,
    /// which resolves the precedence.
    SplitClipsInTimeSelectionOrClip {
        /// The marquee selection, or `None` to fall back to the selected clip.
        time_bounds: Option<TimeSelectionRect>,
    },
    /// Toggle clip mute across the marquee: splits every clip overlapping
    /// the marquee's tick range on the marqueed tracks at its edges and
    /// mutes the interior (Ableton "Deactivate Time Selection"). Marquee-only
    /// like `RemoveClipsInSelection`: the view only sends this with a real
    /// time selection. Forwarded to `SequencerCommand::MuteClipsInRange`.
    /// There is no whole-clip mute anywhere any more — `M` inside `Clip`
    /// mutes the selected *events* (the bare `Key::M` arm), and does nothing
    /// with none selected.
    MuteClipsInSelection {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Ableton/Bitwig-style "Insert Silence": insert `end - start` ticks of
    /// empty time at `start`, across all tracks. Marquee-only like every
    /// other `...InSelection` event — here because the operand is meaningless
    /// without a real duration — so the view only ever sends this when a
    /// real time selection exists.
    InsertSilenceInSelection {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// The exact opposite of `InsertSilenceInSelection` — "Delete Time":
    /// closes the `[start, end)` gap across all tracks, carving that span
    /// out non-rippling and moving everything at or after `end` back left by
    /// `end - start`. Same no-single-clip-fallback shape as
    /// `InsertSilenceInSelection` — the view only ever sends this when a real
    /// time selection exists. Bound to **⌘/Ctrl+Delete**.
    DeleteTimeInSelection {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// Ableton-style "Duplicate Time" — the **Shift+⌘/Ctrl+D** global form:
    /// insert `end - start` ticks of empty time at `end` (pushing every
    /// track's later clips right), then place a copy of the `[start, end)`
    /// slice of every track into the freed span. Like `InsertSilenceInSelection`
    /// there is no single-clip fallback — the view only sends this when a real
    /// time selection exists. The handler advances the time selection
    /// afterward so repeated presses chain down the timeline.
    DuplicateTimeInSelection {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
    },
    /// Plain **⌘/Ctrl+D** — "Duplicate Clips": paste a copy of the marquee's
    /// content flush after itself on the marqueed tracks only, carving out
    /// whatever already sits there (the copy wins). Nothing is shifted and no
    /// time is inserted — contrast `DuplicateTimeInSelection`. No single-clip
    /// fallback — the view only sends this with a real marquee selection. The
    /// handler advances the marquee onto the copy so repeated presses chain.
    DuplicateClipsInSelection {
        /// The marquee selection driving the track/tick span duplicated.
        rect: TimeSelectionRect,
    },
    /// **⌘/Ctrl+J** — "Merge Clips": bake the marquee into one clip per
    /// marqueed track (`PasteClipsEdit::merging`). No single-clip fallback — the view
    /// only sends this with a real marquee selection.
    MergeClipsInSelection {
        /// The marquee selection driving the track/tick span merged.
        rect: TimeSelectionRect,
    },
    /// Copy the time selection into the session clipboard: the portion of
    /// every clip on every track intersecting it. Like
    /// `RemoveClipsInSelection` there is no single-clip fallback: the view
    /// only sends this with a real time selection, so `Shift+⌘/Ctrl+C`
    /// without one is a no-op.
    CopyClipsInSelection {
        /// Grid-snapped, normalized time selection start tick (inclusive).
        start: i32,
        /// Grid-snapped, normalized time selection end tick (exclusive).
        end: i32,
    },
    /// Cut the time selection: copy it into the session clipboard exactly
    /// like `CopyClipsInSelection`, then carve the range out across all
    /// tracks (like `Delete`/`Backspace`). Marquee-only for the same reason.
    CutClipsInSelection {
        /// Grid-snapped, normalized time selection start tick (inclusive).
        start: i32,
        /// Grid-snapped, normalized time selection end tick (exclusive).
        end: i32,
    },
    /// Copy every clip within the marquee rectangle into the session
    /// clipboard — the plain `⌘/Ctrl+C` binding: the tick-range intersection
    /// of every clip on the marqueed tracks. Distinct from
    /// `CopyClipsInSelection` (Shift+⌘/Ctrl+C), which always spans every
    /// track regardless of the marquee. Marquee-only, like
    /// `RemoveClipsInSelection`: the view only sends this with a real time
    /// selection.
    CopyClipsInSelectionScoped {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Cut every clip within the marquee rectangle: copies exactly like
    /// `CopyClipsInSelectionScoped`, then carves the marquee rectangle out of
    /// the marqueed tracks — the plain `⌘/Ctrl+X` binding. Marquee-only for
    /// the same reason.
    CutClipsInSelectionScoped {
        /// The marquee selection.
        rect: TimeSelectionRect,
    },
    /// Event marquee-select in `Clip`: the view resolves the drag
    /// rectangle (view-local, like the time selection above) and forwards it
    /// unconditionally so the sequencer can recompute event selection. The
    /// rectangle is a tick span `[tick_min, tick_max]` × a note span
    /// `[note_min, note_max]`.
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
    /// ⌘/Ctrl+drag velocity gesture in `Clip`: the view resolves
    /// which events the drag targets (the whole selection if the pressed
    /// note is part of it, otherwise just that single note) and forwards a
    /// relative velocity nudge for the current pointer position each time it
    /// changes. `drag_id` identifies the gesture so the sequencer can
    /// coalesce every nudge from one drag into a single undo step.
    DragEventsVelocity {
        /// The events the gesture targets — not necessarily the current selection.
        event_ids: Vec<Uuid>,
        /// Signed relative amount.
        nudge: i32,
        /// Identifies the drag so the undo stack coalesces its nudges into one step.
        drag_id: u64,
    },
    /// Double-click on empty piano-roll grid in `Clip`: insert a note. Maps
    /// 1:1 to `SequencerCommand::InsertNote`.
    InsertNote {
        /// Event tick of the note's start, grid-snapped by the view.
        tick: i32,
        /// Note length in ticks (the view's last-used length).
        length: i32,
        /// Note number.
        pitch: i32,
        /// Note velocity (the view's last-used velocity).
        velocity: i32,
    },
    /// A step of a note move / resize drag in `Clip`, sent on each pointer
    /// move that changes it. Maps 1:1 to `SequencerCommand::DragNotes`.
    DragNotes {
        /// The dragged notes' `NoteOn` ids.
        event_ids: Vec<Uuid>,
        /// The whole drag so far, from the press.
        drag: NoteDrag,
        /// Identifies the gesture so its steps merge into one undo step.
        drag_id: u64,
    },
    /// A note move drag in `Clip` reached a new pitch: audition it. Maps 1:1
    /// to `SequencerCommand::PreviewNote`.
    PreviewNote {
        /// Note number.
        note: u8,
        /// Note velocity.
        velocity: u8,
    },
    /// Arranger clip edge drag-resize (right edge): the view resolves which
    /// clip is targeted at press time (selecting it first if needed) and
    /// forwards the pointer's grid-snapped absolute tick each time it moves.
    ResizeSelectedClipRegionEnd {
        /// Grid-snapped absolute tick the edge is dragged to.
        target_tick: i32,
        /// Identifies the drag so the undo stack coalesces it into one step.
        drag_id: u64,
    },
    /// Arranger clip edge drag-resize (left edge).
    ResizeSelectedClipRegionStart {
        /// Grid-snapped absolute tick the edge is dragged to.
        target_tick: i32,
        /// Identifies the drag so the undo stack coalesces it into one step.
        drag_id: u64,
    },
    /// Arranger clip band press: select this clip and marquee exactly its
    /// span (cursor to its start, time selection over `[start, end)` on its
    /// track). Sent at press time by `Display::begin_clip_move_drag`, whether
    /// or not the press turns into a drag — see `020-views-and-state.md`
    /// § "Clip Band Press & Move Drag".
    SelectClipSpan {
        /// Track the clip is on.
        track_idx: usize,
        /// The clip's id.
        clip_id: Uuid,
    },
    /// Place a pane's cursor and change nothing else — no track, clip or
    /// note selection. Two senders: `⌥←`/`⌥→` in the Arranger (the next clip
    /// edge in that direction; the view resolves it because it owns the clip
    /// shapes the edges come from — the same "view-local operand" reason as
    /// `SetRegionToTimeSelectionOrToggleLoop`), and a click on either pane's
    /// timeline strip. In the arranger the handler routes it to
    /// `TransportCommand::SetCursorAndSelectClip`, the click-to-place path
    /// minus the track select, so the lead clip re-syncs and the marquee
    /// collapses exactly as after an arrow-key move; in the clip view to
    /// `SequencerCommand::CommitClipClickByTicks`, a lane click minus the
    /// note selection. See `020-views-and-state.md` § "Arranger Time
    /// Selection — the Marquee".
    SetCursorTick {
        /// The pane whose cursor moves.
        pane: Pane,
        /// Absolute target tick — a clip edge (deliberately *not* snapped to
        /// the cursor grid: the edge is the point) or a timeline click,
        /// snapped like a lane click.
        tick: i32,
    },
    /// Plain `←`/`→` in the Arranger or `Clip`: step the cursor one
    /// *visible* grid line. The view resolves the step
    /// (`Display::cursor_grid_ticks` — the zoom-adaptive snap, `grid.rs`)
    /// because the zoom is view-local; the handler routes it to
    /// `TransportCommand::MoveCursor` (arranger) or
    /// `SequencerCommand::NudgeClipCursorByGrid` (`Clip`), both of which land
    /// on the next multiple of the step when the cursor sits off-grid.
    MoveCursorByGrid {
        /// Signed step: `±` one snap-grid unit, in ticks.
        step_ticks: i32,
    },
    /// Arranger clip band drag released over a new position: move the clip
    /// there, carving out whatever it lands on (`MoveClipEdit`, undoable).
    /// The only mutation a whole-clip drag ever sends — the drag itself is a
    /// view-local ghost. Explicit ids rather than "the selected clip" because
    /// the press's selection round-trip is asynchronous. A drag that started
    /// inside an active marquee sends `MoveRange` instead.
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
    /// Arranger band drag that started *inside* an active marquee, released
    /// somewhere new: move everything inside the marquee — the marqueed tick
    /// range of every clip it overlaps on the marqueed tracks, split out
    /// first — by the same tick and track delta, carving out whatever the
    /// pieces land on (`MoveRangeEdit`, undoable). Carries the marquee
    /// itself, frozen at press time, since it is view-local state.
    MoveRange {
        /// The marquee the drag started in.
        rect: TimeSelectionRect,
        /// Grid-snapped tick shift, `>= -rect.start`.
        delta_ticks: i32,
        /// Lane shift, already clamped so the track span stays on the lanes.
        delta_tracks: i32,
    },
    /// The header's BPM chip: a typed value (Enter in its field) or a step of
    /// a vertical drag on it, already an absolute tempo. One undo step per
    /// typed value or per drag (`SetTempoEdit`).
    SetTempo {
        /// The new tempo, µs per quarter.
        tempo_us: i32,
        /// The drag this step belongs to (`GestureState::take_drag_id`);
        /// `None` for a typed value.
        drag_id: Option<u64>,
    },
    /// The header's meter chip: a typed meter (Enter in its field). One undo
    /// step per typed value (`SetMeterEdit`).
    SetMeter {
        /// The new meter.
        meter: Meter,
    },
    /// Arranger track-header volume/pan bar drag: the view resolves the target
    /// track and the absolute value for the current pointer position and
    /// forwards it each time it changes. Not undoable — see
    /// `020-views-and-state.md`.
    SetTrackVolume {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
        /// New volume in dB.
        volume_db: f32,
    },
    /// Stereo-balance companion to [`SetTrackVolume`](Self::SetTrackVolume).
    SetTrackPan {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
        /// New stereo balance in `[-1.0, 1.0]`.
        pan: f32,
    },
    /// Arranger track-header S/M button click — flips that track's solo / mute.
    /// Not undoable, like the volume/pan drag. See `020-views-and-state.md`.
    ToggleTrackMute {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
    },
    /// Solo counterpart of [`ToggleTrackMute`](Self::ToggleTrackMute).
    ToggleTrackSolo {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
    },
    /// ⌘/Ctrl+S, the Save As field, the unsaved-changes prompt's Save: save
    /// under this name. `folder` `None` is the root fallback (no project
    /// folder selected yet).
    ConfirmFilename {
        /// Project file name, without extension.
        filename: String,
        /// Project folder, or the root fallback when `None`.
        folder: Option<String>,
    },
    /// **⌘/Ctrl+⇧+E** — "Export MIDI Clip": write the lead clip to a `.mid`
    /// in the project folder (`Sequencer::export_lead_clip`). The view adds
    /// what only it knows: the arranger marquee (`None` from the clip view)
    /// and where the project lives.
    ExportClip {
        /// The arranger marquee; `None` in the clip view or without one.
        time_bounds: Option<TimeSelectionRect>,
        /// Project folder, or the root fallback when `None`.
        folder: Option<String>,
        /// What the file name starts with: the open project's name, else the
        /// folder's, else `project`.
        name: String,
    },
    /// A `.mid` dropped on a track lane (the ghost clip's target, from the
    /// file manager or the browser panel) or, `target` `None`, Enter on one in
    /// the browser — put the clip on the selected track at the cursor. The
    /// view has already read the file (`Clip::imported`): it needed the clip
    /// to draw the ghost. See `060-persistence.md` § MIDI clip import.
    ImportMidiClip {
        /// The clip, built from the file.
        clip: Box<Clip>,
        /// The file's name (stem), for the footer.
        name: String,
        /// `(track index, start tick)`, or `None` for the selected track at
        /// the cursor.
        target: Option<(usize, i32)>,
    },
    /// Replace or end the open project — ⌘/Ctrl+N, a project opened from the
    /// browser panel, quitting. Unless `discard_changes`, the sequencer first
    /// checks for unsaved changes and asks the view to prompt
    /// (`UiEvent::UnsavedChanges`) instead. See `060-persistence.md`
    /// § Unsaved changes.
    ProjectAction {
        /// What to do.
        action: ProjectAction,
        /// Skip the check: the prompt's "Don't Save".
        discard_changes: bool,
    },
    /// The view has loaded a staged project's plugins (`UiEvent::StageProject`):
    /// the sequencer applies it now, with no unsaved-changes check — that ran
    /// before it was staged. See `130-plugin-host.md` § Project persistence.
    ApplyStagedProject(Box<StagedProject>),
    /// The settings modal's MIDI tab connected a port: save both connected
    /// ports (empty: none).
    ConfirmMidiPorts {
        /// The connected MIDI input port name.
        in_port: String,
        /// The connected MIDI output port name.
        out_port: String,
    },
    /// The settings modal's MIDI tab nudged the output offset: publish it to
    /// the `"midiout"` thread and save it.
    SetMidiOutOffset {
        /// The new MIDI-output offset, in milliseconds.
        out_offset_ms: i32,
    },
    /// The theme changed (the settings modal's Appearance tab, ⇧F5/⇧F6):
    /// save it as the theme the app starts with. The view has already
    /// applied it.
    SaveTheme {
        /// Index into `view::theme`'s palette list.
        theme_index: usize,
    },
    /// Route `track` to `output`: a MIDI channel picked in the track
    /// header's output menu, or a plugin put on the track from the browser
    /// panel. For a plugin `Display` has already loaded it (and for a channel
    /// already removed any plugin); this only tells the sequencer where the
    /// track's clips go and what to persist.
    SetTrackOutput {
        /// Target track index.
        track: usize,
        /// The new output.
        output: TrackOutput,
    },
    /// Fresh plugin state captured from the live plugin in engine slot
    /// `slot`, to be stored on the track in that slot so the next project save
    /// persists the current preset. Sent by the macOS plugin host from the ⌘S
    /// handler, just before `ConfirmFilename`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    CaptureTrackInstrumentState {
        /// The plugin's engine slot (`Track::slot`).
        slot: usize,
        /// Opaque plugin-state blob.
        state: Vec<u8>,
    },
    /// ⌘/Ctrl+T, or the `+` row under the last lane: add an empty MIDI track
    /// at `track_idx` — after the selected one when `None`.
    AddTrack {
        /// Where the track goes; `None` = after the selected track.
        track_idx: Option<usize>,
    },
    /// Delete/Backspace in track header focus, or the output menu's `Delete
    /// track`: remove a track — `track_idx`, or the selected one when `None`.
    RemoveTrack {
        /// The track to remove; `None` = the selected track.
        track_idx: Option<usize>,
    },
    /// The header's rename field committed (Enter, a click away): name
    /// `track_idx` `name` — `None` (a blank entry) goes back to its number.
    RenameTrack {
        /// The track to rename.
        track_idx: usize,
        /// Its new name, already through `track_name_from_input`.
        name: Option<String>,
    },
    /// ⌘/Ctrl+C (optionally with Shift). egui-winit turns the copy chord into
    /// `egui::Event::Copy` regardless of Shift and swallows the raw `C` key
    /// press, so this is the only way the app sees it; `shift` is sampled
    /// separately from `i.modifiers` (egui-winit updates that from
    /// `WindowEvent::ModifiersChanged`, ahead of the `C` keydown) since the
    /// bare `Event::Copy` carries no modifier payload of its own.
    /// In the Arranger, Shift+⌘/Ctrl+C resolves to `CopyClipsInSelection` (always every
    /// track); plain ⌘/Ctrl+C resolves to `CopyClipsInSelectionScoped` (the
    /// marqueed tracks). Both are marquee-only: with no active marquee the
    /// chord is consumed and nothing is sent — see
    /// `Display::forward_input_event`. In the clip view plain ⌘/Ctrl+C copies
    /// the selected notes (`SequencerCommand::CopyNotes`, routed in
    /// `input_handler.rs`) and the Shift form does nothing.
    Copy {
        /// Shift held at the moment of the chord.
        shift: bool,
    },
    /// ⌘/Ctrl+X (optionally with Shift). Same story as `Copy`, including the
    /// `shift` sampling.
    Cut {
        /// Shift held at the moment of the chord.
        shift: bool,
    },
    /// ⌘/Ctrl+V, as a bare intent (no payload). Same story as `Copy`: the raw
    /// `V` key press never arrives. Emitted for every `egui::Event::Paste`
    /// regardless of clipboard contents; the Arranger pastes the clip
    /// clipboard, the clip view the note clipboard.
    Paste,
}

/// Written to the OS text clipboard whenever clips or notes are copied,
/// purely so a subsequent ⌘/Ctrl+V reliably produces an `egui::Event::Paste`
/// (egui-winit only emits that event when the OS clipboard holds non-empty
/// text). The real payload lives in `Sequencer::clip_clipboard` /
/// `Sequencer::note_clipboard`, in memory.
pub(crate) const CLIP_CLIPBOARD_SENTINEL: &str = "stev:clips";

/// The modifier state sampled with a key press or mouse move. Only the
/// modifiers the app actually binds against. `Default` is none held.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct KeyModifiers {
    /// Shift held.
    pub(crate) shift: bool,
    /// Cross-platform primary modifier: ⌘ on macOS, Ctrl on Windows/Linux.
    pub(crate) command: bool,
    /// Option on macOS, Alt on Windows/Linux. Bound by `⌥←`/`⌥→` (jump to
    /// the next clip edge in the arranger, `⇧⌥←/→` for the marquee edge) and
    /// `⌥=`/`⌥-` (stretch the clip in `Clip`); see `010-keybindings.md`. The
    /// parked keypad controller thread and the plugin-editor key guard have
    /// no alt key of their own and always report `false`.
    pub(crate) alt: bool,
    /// The Control key on every platform — on Windows/Linux the same key as
    /// `command`, on macOS a key of its own (⌃): for a chord that is Control
    /// everywhere, such as `Ctrl+Tab`. The keypad controller thread and the
    /// plugin-editor key guard always report `false`.
    pub(crate) ctrl: bool,
}

impl From<Modifiers> for KeyModifiers {
    /// The bound subset of egui's modifier state.
    fn from(modifiers: Modifiers) -> Self {
        KeyModifiers {
            shift: modifiers.shift,
            command: modifiers.command,
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TimeSelectionRect;

    fn rect(start: i32, end: i32) -> TimeSelectionRect {
        TimeSelectionRect {
            start,
            end,
            track_start: 0,
            track_end: 3,
        }
    }

    #[test]
    fn spanning_normalizes_both_axes() {
        let sel = TimeSelectionRect::spanning(2000, 1000, 3, 1);
        assert_eq!(
            (sel.start, sel.end, sel.track_start, sel.track_end),
            (1000, 2000, 1, 3)
        );
    }

    #[test]
    fn has_tick_range_is_false_for_a_point_selection() {
        assert!(!rect(480, 480).has_tick_range());
    }

    #[test]
    fn has_tick_range_is_true_for_a_real_span() {
        assert!(rect(0, 480).has_tick_range());
    }
}
