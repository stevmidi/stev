//! The view-local selection- and pointer-gesture state — the arranger time
//! selection, the event marquee, the ⌘/Ctrl velocity drag, the piano roll's
//! note move / resize drag, the clip-edge, clip-band (move) and track-header
//! drags, the piano roll's key-column zoom drag, a `.mid` dragged in for
//! import, a plugin dragged onto a track, and their hover
//! counterparts, plus the in-lane cursor-line hover override.
//!
//! Grouped out of [`Display`](super::Display) as `Display::gesture`; every
//! method that drives it (`begin_*` / `extend_*` / `clear_*`, the hit-tests)
//! stays on `Display` and reaches in through `self.gesture`. The payload types
//! themselves live in [`pointer_state`](super::pointer_state) — this struct
//! holds fields *of* those types. See `020-views-and-state.md`.

use uuid::Uuid;

use crate::core::{input_event::TimeSelectionRect, view_state::Pane};

use super::pointer_state::{
    ClipMoveDrag, ClipResizeDrag, KeyZoomDrag, MarqueeAnchor, MidiDrag, NoteMouseDrag, NotePart,
    PluginDrag, TrackButton, TrackMixDrag, TrackMixParam, VelocityDrag,
};

/// The selection / gesture state, grouped out of [`Display`](super::Display).
/// Each drag payload is `Some` only while its gesture is live, so it doubles as
/// the drag-active flag; the hover fields track what is under the pointer with
/// no button held. All of it is scoped to a single view visit and never seen by
/// the sequencer.
#[derive(Default)]
pub(super) struct GestureState {
    /// Where the in-lane cursor line (`draw_cursor_line`) is drawn: the
    /// pointer's snapped tick, and the pane it was measured in — the tick
    /// means nothing in the other one, so only that pane draws it. `Some`
    /// only while the pointer is over a pane (lanes or timeline strip); `None`
    /// draws no line. See `020-views-and-state.md`.
    pub(super) hover_cursor: Option<(i32, Pane)>,
    /// The pane the primary button went down in, `Some` until it is released:
    /// a drag stays with the pane it started in, wherever the pointer
    /// wanders.
    pub(super) press_pane: Option<Pane>,
    /// A `.mid` dragged in from the file manager or the browser panel, `Some`
    /// from the drag's start to its drop or cancel. See [`MidiDrag`].
    pub(super) midi_drag: Option<MidiDrag>,
    /// A Plugins row dragged out of the browser panel, `Some` from the drag's
    /// start to its drop or cancel. See [`PluginDrag`].
    pub(super) plugin_drag: Option<PluginDrag>,
    /// Arranger time selection — a normalized tick range × track range
    /// marquee.
    ///
    /// `Some` only while a real range is selected — never a collapsed pair. The
    /// collapsed state is not stored at all: it *is* the cursor, so it is derived
    /// in `draw_time_selection` and tracks every cursor move for free.
    pub(super) time_selection: Option<TimeSelectionRect>,
    /// `(tick, track)` the current time selection drag was anchored at — the
    /// track resolved via `track_idx_at` (clamped to track 0 for an anchor on
    /// the performance-lane row). `Some` while the primary mouse button is
    /// held, so it doubles as the drag-active flag.
    pub(super) time_selection_anchor: Option<(i32, usize)>,
    /// Cursor tick observed last frame, used only to detect that the cursor moved
    /// so the time selection can collapse. `None` until the first observation.
    pub(super) last_cursor_tick: Option<i32>,
    /// Selected track observed last frame, used only to detect that it changed
    /// so the time selection can collapse. `None` until the first observation.
    pub(super) last_selected_track_idx: Option<usize>,
    /// Anchor of an in-progress event marquee-select drag. `Some` only while
    /// the primary button is held in `Clip` — like `time_selection_anchor`,
    /// doubles as the drag-active flag.
    pub(super) event_marquee_anchor: Option<MarqueeAnchor>,
    /// Normalized `(tick_min, tick_max, row_min, row_max)` marquee rectangle,
    /// for rendering only — `Clip::event_selection` (reached via
    /// `SequencerCommand::SelectEventsInRect`) is the source of truth for
    /// which events are actually selected. The rows are content-space row
    /// units (`NoteAreaGeom::row_at`), not snapped to note rows, so the box
    /// tracks the pointer smoothly instead of jumping a whole row height at a
    /// time, and stays on its notes when the view scrolls under it. `Some` only
    /// once the pointer has moved past `DRAG_THRESHOLD_PX` from the anchor —
    /// the drag latch, which guards against a plain click's sub-pixel
    /// `MouseMoved` jitter clobbering its own `SelectClipEvent` selection
    /// with a spurious rectangle update.
    pub(super) event_marquee_rect: Option<(i32, i32, f32, f32)>,
    /// In-progress ⌘/Ctrl+drag velocity gesture. `Some` only while the
    /// primary button is held after a ⌘/Ctrl-modified press landed on a
    /// note — like the other drag anchors above, doubles as the drag-active
    /// flag. Unlike the marquee/time-selection anchors, this one also
    /// carries the resolved target and a running total so each `MouseMoved`
    /// can diff against it.
    pub(super) velocity_drag: Option<VelocityDrag>,
    /// In-progress piano-roll note move / resize drag. `Some` only while the
    /// primary button is held after a plain press landed on a note — doubles
    /// as the drag-active flag. Applied live: each change sends a
    /// `DragNotes` step. Clip pane only.
    pub(super) note_drag: Option<NoteMouseDrag>,
    /// Hover-only counterpart of `note_drag`: the part of the note under the
    /// pointer with no button held, driving the resize cursor on an edge.
    /// Clip pane only.
    pub(super) note_hover: Option<NotePart>,
    /// In-progress key-column zoom / scroll drag. `Some` only while the
    /// primary button is held after a press landed on the piano roll's
    /// octave-legend column — doubles as the drag-active flag. Clip pane only.
    pub(super) key_zoom_drag: Option<KeyZoomDrag>,
    /// Hover-only counterpart of `key_zoom_drag`: the pointer is over the octave
    /// legend with no button held, driving its cursor icon. Clip pane only.
    pub(super) key_zoom_hover: bool,
    /// Whether the pointer is currently held for the zoom drag — the grab
    /// last sent to the window, so `sync_pointer_grab` acts only on an edge.
    pub(super) pointer_grabbed: bool,
    /// `(length, velocity)` of the last note created or resized — what the
    /// next double-click draws. `None` until set: a beat at the default
    /// velocity. View-local, not persisted, and kept across view visits.
    pub(super) last_note: Option<(i32, i32)>,
    /// Monotonically increasing id handed out to each new velocity, note,
    /// clip edge or BPM chip drag, so the sequencer's undo coalescing can tell two separate
    /// gestures on the same target apart. Never reset.
    pub(super) next_drag_id: u64,
    /// In-progress clip edge drag-resize. `Some` only while the primary
    /// button is held after a press landed on a clip's edge — like the other
    /// drag anchors above, doubles as the drag-active flag. Undoable as one
    /// step per drag (`clip_resize_drag_id`) — see `020-views-and-state.md`.
    pub(super) clip_resize_drag: Option<ClipResizeDrag>,
    /// The id every `ResizeSelectedClipRegion*` of the current edge drag
    /// carries, drawn from `next_drag_id` when the drag begins.
    pub(super) clip_resize_drag_id: u64,
    /// Hover-only counterpart of `clip_resize_drag`: which edge (if any) is
    /// under the pointer right now, with no button held. Drives the resize
    /// cursor icon and the edge grip glyph; cleared whenever a real drag
    /// starts. Arranger only.
    pub(super) clip_resize_hover: Option<ClipResizeDrag>,
    /// In-progress clip band drag (move). `Some` only while the primary
    /// button is held after a press landed on a clip's header band off its
    /// edges — doubles as the drag-active flag. View-local ghost only; the
    /// single `MoveClip` edit is sent on release. Arranger only — see
    /// `020-views-and-state.md`.
    pub(super) clip_move_drag: Option<ClipMoveDrag>,
    /// Hover-only counterpart of `clip_move_drag`: `(track_idx, clip_id)` of
    /// the clip whose band is under the pointer with no button held, driving
    /// the `Grab` cursor icon. An edge hit (`clip_resize_hover`) takes
    /// priority. Arranger only.
    pub(super) clip_move_hover: Option<(usize, Uuid)>,
    /// In-progress arranger track-header volume/pan bar drag. `Some` only while
    /// the primary button is held — doubles as the drag-active flag, like the
    /// other drag states above.
    pub(super) track_mix_drag: Option<TrackMixDrag>,
    /// Hover-only counterpart: which track's which bar is under the pointer
    /// right now, driving the `ResizeVertical` cursor icon. Arranger only.
    pub(super) track_mix_hover: Option<(usize, TrackMixParam)>,
    /// Which track's S/M button is under the pointer right now, driving the
    /// `PointingHand` cursor icon. Arranger only.
    pub(super) track_button_hover: Option<(usize, TrackButton)>,
    /// The track whose output menu is open (`output_menu.rs`), `None` when
    /// it is closed. Opened by a click on the track's output chip; closed by
    /// the next click or key.
    pub(super) output_menu: Option<usize>,
}

impl GestureState {
    /// A fresh drag id from [`next_drag_id`](Self::next_drag_id), for a drag
    /// that is beginning.
    pub(super) fn take_drag_id(&mut self) -> u64 {
        let drag_id = self.next_drag_id;
        self.next_drag_id += 1;
        drag_id
    }
}
