//! The view-local pointer-interaction state types — hover targets, in-progress
//! drag payloads, and the "which part of a widget" enums — the values behind
//! `Display`'s hover / drag gesture fields.
//!
//! Each drag payload doubles as its gesture's "active" flag: a drag is in
//! progress exactly while its field is `Some`. They live here rather than in
//! `input/gestures.rs` (which constructs and extends them) because `rendering/`
//! hit-tests *produce* them — `clip_edge_at`, `track_mix_bar_at`,
//! `track_button_at` — and `mod.rs` stores them. None of this state is ever
//! seen by the sequencer or undone; see `020-views-and-state.md`.

use uuid::Uuid;

use super::browser::BrowserPlugin;
use crate::{
    models::clip::{Clip, NoteBounds, NoteDrag},
    shapes::clip_shape::ClipShape,
};

/// A `.mid` being dragged over the window — from the file manager or out of
/// the browser panel — on its way to an arranger lane (the MIDI clip
/// import, `060-persistence.md` § MIDI clip import). The file is read when
/// the drag starts, so the ghost can show the clip it will become; see
/// `Display::begin_midi_drag`.
pub(super) struct MidiDrag {
    /// The file's name (stem), for the footer.
    pub(super) name: String,
    /// The clip the file became, or why it couldn't (said on the drop).
    pub(super) loaded: Result<LoadedMidi, String>,
    /// `(track index, start tick)` the clip would land on: the arranger lane
    /// under the pointer, at the cursor line's snapped tick. `None` while the
    /// pointer is anywhere else — a drop there imports nothing.
    pub(super) target: Option<(usize, i32)>,
}

/// An instrument plugin dragged out of the browser panel on its way to a
/// track; see `Display::begin_plugin_drag`.
pub(super) struct PluginDrag {
    /// The plugin the drop puts on the track.
    pub(super) plugin: BrowserPlugin,
    /// The track under the pointer (its lane or its header), highlighted
    /// while the drag is over it. `None` anywhere else — a drop there does
    /// nothing.
    pub(super) target: Option<usize>,
}

/// A readable `.mid`, as [`MidiDrag::loaded`] holds it.
pub(super) struct LoadedMidi {
    /// The clip, as `Clip::imported` built it (window from tick 0).
    pub(super) clip: Clip,
    /// Its arranger shape — bounds from tick 0, note thumbnails — drawn as
    /// the ghost at [`MidiDrag::target`].
    pub(super) ghost: ClipShape,
}

/// State of an in-progress ⌘/Ctrl+drag velocity gesture — see
/// `Display::begin_velocity_drag`/`extend_velocity_drag`.
pub(super) struct VelocityDrag {
    /// Screen-space y of the press that started the drag.
    pub(super) anchor_y: f32,
    /// Identifies this gesture to the sequencer's undo coalescing.
    pub(super) drag_id: u64,
    /// Events the drag applies to: the whole selection if the pressed note
    /// was part of it, otherwise just that single note.
    pub(super) target_event_ids: Vec<Uuid>,
    /// Total nudge already sent for this drag, so each `MouseMoved` only
    /// sends the incremental delta since the last one.
    pub(super) last_total_nudge: i32,
}

/// Press point of an event marquee drag — see `Display::begin_event_marquee`.
/// Screen space for the drag threshold, and content space — the tick and the
/// note row — for the box, so the anchor stays on its notes when the user
/// scrolls the view under a held drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MarqueeAnchor {
    /// Screen-space x of the press.
    pub(super) x: f32,
    /// Screen-space y of the press.
    pub(super) y: f32,
    /// Raw (unsnapped) tick of the press.
    pub(super) tick: i32,
    /// Note row of the press in row units (see `NoteAreaGeom::row_at`).
    pub(super) row: f32,
    /// The `(tick_min, tick_max, note_min, note_max)` hit-test last sent to
    /// the sequencer, so an update that wouldn't change it — a pointer move
    /// within the same rows and ticks — sends nothing.
    pub(super) last_sent: Option<(i32, i32, u8, u8)>,
}

/// Which part of a piano-roll note the pointer is over — see
/// `Display::note_hit_at`. Decides what a press on the note drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NotePart {
    /// The left edge zone: resizes the start, keeping the end.
    Start,
    /// Between the edge zones: moves the note.
    Body,
    /// The right edge zone: resizes the end, keeping the start.
    End,
}

/// In-progress piano-roll note move / resize drag — see
/// `Display::begin_note_drag`/`extend_note_drag`/`finish_note_drag`. `Some`
/// while the primary button is held after a plain press landed on a note.
/// Applied live: each change sends `InputEvent::DragNotes` with the whole
/// drag from the press, and the dragged notes are drawn where
/// [`NoteDrag::dragged_spans`] puts their `origin` spans, ahead of the model's
/// round-trip; Esc sends the drag back to nothing. See
/// `020-views-and-state.md`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct NoteMouseDrag {
    /// The note pressed — the one the drag is measured from and auditioned.
    pub(super) pressed_id: Uuid,
    /// The notes dragged — the whole selection if the pressed note was in
    /// it, else just the pressed note (the velocity drag's target rule) —
    /// each with its `(start, end, pitch)` at the press. The drag is measured
    /// from these, not from the shapes, which follow the model as each step
    /// lands.
    pub(super) origin: Vec<(Uuid, NoteBounds)>,
    /// Identifies the gesture, so its steps merge into one undo step.
    pub(super) drag_id: u64,
    /// The pressed note's velocity, for the pitch audition and the last-used
    /// velocity.
    pub(super) pressed_velocity: u8,
    /// Raw (unsnapped) tick of the press — the drag's time delta is measured
    /// from it, in whole grid steps.
    pub(super) press_tick: i32,
    /// The note row of the press — the move's pitch delta is measured from
    /// it.
    pub(super) press_note: u8,
    /// Screen-space x of the press, for the drag threshold.
    pub(super) press_x: f32,
    /// Screen-space y of the press, for the drag threshold.
    pub(super) press_y: f32,
    /// `Some(snapped press tick)` when the press landed on a note already in
    /// the selection: nothing was sent then (so the selection can move as a
    /// group), and a release that never became a drag sends the plain click
    /// it held back — select that note alone, cursor to the tick.
    pub(super) deferred_click_tick: Option<i32>,
    /// Latched once the pointer has moved `DRAG_THRESHOLD_PX` off the press
    /// point — before that a press is just a click.
    pub(super) dragging: bool,
    /// The drag so far, unclamped — its kind is the part pressed (the body
    /// moves, an edge resizes); [`NoteDrag::dragged_spans`] clamps it.
    pub(super) drag: NoteDrag,
    /// The pressed note's pitch last auditioned, so a move sounds each new
    /// pitch once.
    pub(super) auditioned_pitch: u8,
}

impl NoteMouseDrag {
    /// The dragged notes' ids.
    pub(super) fn target_ids(&self) -> Vec<Uuid> {
        self.origin.iter().map(|&(id, _)| id).collect()
    }

    /// Whether an edge was pressed — a resize, not a move.
    pub(super) fn is_resize(&self) -> bool {
        !matches!(self.drag, NoteDrag::Move { .. })
    }
}

/// Which edge of a clip a resize gesture targets — see `Display::clip_edge_at`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ClipResizeEdge {
    /// Left edge (moves `start_tick` + `region.start`, phase-locked).
    Start,
    /// Right edge (moves `region.end` / the clip length).
    End,
}

/// Identifies the clip and edge a resize hover/drag targets. Doubles as both
/// the hover-highlight target and the active-drag state — see
/// `Display::clip_edge_at`,
/// `begin_clip_resize_drag`/`extend_clip_resize_drag`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClipResizeDrag {
    /// Track the clip is on.
    pub(super) track_idx: usize,
    /// The clip's id.
    pub(super) clip_id: Uuid,
    /// Which edge.
    pub(super) edge: ClipResizeEdge,
}

/// What an arranger band drag moves — decided at press time by
/// `Display::begin_clip_move_drag`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ClipMoveKind {
    /// One whole clip, by id: the press landed outside any marquee.
    Clip(Uuid),
    /// Everything inside the marquee the press landed in — the marqueed
    /// tick range of every clip it overlaps on the marqueed tracks.
    Marquee,
}

/// In-progress arranger clip band drag — see `Display::clip_band_at`,
/// `begin_clip_move_drag`/`extend_clip_move_drag`, and its keyboard sibling
/// `nudge_clip_move_drag`. `Some` while the primary button is held after a
/// press landed on a clip's header band (off its edges), *or* while `⌘/Ctrl+←`/`→`
/// have armed a marquee nudge — either way doubling as the drag-active flag.
/// Purely a ghost preview: nothing here reaches the sequencer until
/// `MouseReleased` (mouse drag) or `MoveModifierReleased` (keyboard nudge) sends one
/// `InputEvent::MoveClip` / `MoveRange` — see `020-views-and-state.md`.
///
/// The dragged block is a tick span × an inclusive track range: the clip's
/// own bounds on its one track for a `Clip` drag, the marquee's rect for a
/// `Marquee` drag. Both move by one grid-snapped tick delta and one lane
/// delta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClipMoveDrag {
    /// What is being moved.
    pub(super) kind: ClipMoveKind,
    /// True when `⌘/Ctrl+←`/`→` armed this drag instead of a mouse press —
    /// the commit trigger is `MoveModifierReleased` rather than `MouseReleased`, and
    /// `press_x`/`press_y`/`last_x`/`last_y`/`grab_offset_ticks` are unused
    /// placeholders (there is no pointer driving the gesture).
    pub(super) via_keyboard: bool,
    /// The clip whose band the mouse actually pressed on, `None` for a
    /// keyboard-armed nudge (there is no press to name one). Used only by
    /// `finish_clip_move_drag`: a `Marquee` drag that never became a real
    /// drag (a plain click inside the marquee) reselects this clip and
    /// re-marquees its span, exactly as a click outside the marquee already
    /// does at press time — see `020-views-and-state.md`.
    pub(super) pressed_clip_id: Option<Uuid>,
    /// Start tick of the dragged block at press time, for the ghost and the
    /// release-time no-op check.
    pub(super) start_tick: i32,
    /// End tick of the dragged block at press time.
    pub(super) end_tick: i32,
    /// First lane of the dragged block.
    pub(super) track_start: usize,
    /// Last lane of the dragged block (inclusive).
    pub(super) track_end: usize,
    /// Lane the press landed on — the anchor the lane delta is measured
    /// from, so the block stays under the hand vertically too.
    pub(super) press_track_idx: usize,
    /// Raw (unsnapped) press tick minus `start_tick`, so the block stays
    /// under the hand instead of jumping its start to the pointer.
    pub(super) grab_offset_ticks: i32,
    /// Latched once the pointer has moved `DRAG_THRESHOLD_PX` off
    /// the press point — before that a press is just a band click.
    pub(super) dragging: bool,
    /// Live ghost position: the grid-snapped start tick of the block under
    /// the pointer.
    pub(super) target_start_tick: i32,
    /// Live ghost position: lanes moved, clamped so the whole block stays on
    /// the lanes (`clamped_track_delta`).
    pub(super) delta_tracks: i32,
    /// Screen-space x of the press, for the drag threshold.
    pub(super) press_x: f32,
    /// Screen-space y of the press, for the drag threshold.
    pub(super) press_y: f32,
    /// Last pointer x seen while held, so the release — which carries no
    /// coordinates — can re-run the band hover test where the button went up.
    pub(super) last_x: f32,
    /// Last pointer y seen while held — see `last_x`.
    pub(super) last_y: f32,
}

impl ClipMoveDrag {
    /// True once the ghost sits somewhere other than the block's own position
    /// — the only case a release should commit a move.
    pub(super) fn has_moved(&self) -> bool {
        self.delta_ticks() != 0 || self.delta_tracks != 0
    }

    /// Ticks the ghost sits from the block's own start.
    pub(super) fn delta_ticks(&self) -> i32 {
        self.target_start_tick - self.start_tick
    }
}

/// Which of a track header's two bars a hover/drag targets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum TrackMixParam {
    /// The volume bar.
    Volume,
    /// The pan bar.
    Pan,
}

/// Which of a track header's buttons a hover/click targets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum TrackButton {
    /// The solo button.
    Solo,
    /// The mute button.
    Mute,
    /// The output chip, which opens the output menu.
    Output,
}

/// In-progress vertical drag on an arranger track-header volume/pan bar.
/// `Some` only while the primary button is held after a press landed on a
/// bar — doubles as the drag-active flag, like the other drag states. Not
/// undoable — see `020-views-and-state.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TrackMixDrag {
    /// Track being adjusted.
    pub(super) track_idx: usize,
    /// Which bar.
    pub(super) param: TrackMixParam,
    /// Screen-space y of the press.
    pub(super) anchor_y: f32,
    /// The parameter's value at press time — a fader position (`0.0..=1.0`)
    /// for `Volume`, the raw pan (`-1.0..=1.0`) for `Pan`. The drag recomputes
    /// an absolute value from this each move rather than accumulating deltas.
    pub(super) anchor_value: f32,
    /// Last value sent to the sequencer, so an unchanged move sends nothing.
    pub(super) last_sent: f32,
}

/// In-progress drag on the piano roll's octave-legend column: a
/// horizontal drag zooms the rows, a vertical one scrolls, the grabbed row
/// moving with the (hidden) pointer — see `Display::extend_key_zoom_drag`.
/// `Some` only while the primary button is held — doubles as the drag-active
/// flag. View state only, never undone. Clip pane only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct KeyZoomDrag {
    /// Drops the drift of a one-axis hand movement, so a vertical scroll
    /// doesn't zoom and a zoom doesn't scroll.
    pub(super) axes: AxisFilter,
    /// Content row under the press (`NoteAreaGeom::row_at`), which each
    /// motion moves by its `dy`.
    pub(super) anchor_row: f32,
}

/// Separates a drag's two axes the way a hand means them (the octave-legend
/// zoom drag, `KeyZoomDrag`): from a smoothed recent speed per axis, the
/// dominant axis always passes and the other only when it is more than
/// [`MINOR_AXIS_RATIO`](Self::MINOR_AXIS_RATIO) of it — a deliberate
/// diagonal. The drift of a hand moving along one axis is dropped. Smoothed
/// (rather than judged per motion event, whose raw deltas are a pixel or two
/// and noisy) so one jittery frame can't hand the drag to the other axis.
/// With no history yet, the first [`START_PX`](Self::START_PX) of motion are
/// held back and judged as one, so a drag doesn't open with drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct AxisFilter {
    /// Smoothed recent horizontal speed, points per motion event.
    x: f32,
    /// Smoothed recent vertical speed, points per motion event.
    y: f32,
    /// Motion held back until it reaches `START_PX`; `None` once released.
    pending: Option<(f32, f32)>,
}

impl AxisFilter {
    /// Share of the smoothed speed carried over to the next motion event.
    const DECAY: f32 = 0.8;
    /// How large the minor axis must be against the dominant one to count:
    /// below it (~27° off the axis) it is drift.
    const MINOR_AXIS_RATIO: f32 = 0.5;
    /// Motion, in points (summed over both axes), held back at the start of
    /// a drag to seed the speeds with a real direction.
    const START_PX: f32 = 6.0;

    /// A filter for a drag that is just beginning.
    pub(super) fn new() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            pending: Some((0.0, 0.0)),
        }
    }

    /// Feeds one motion `(dx, dy)` and returns it with a drifting axis
    /// zeroed — `(0, 0)` while the opening motion is still held back, then
    /// all of it at once.
    pub(super) fn filter(&mut self, dx: f32, dy: f32) -> (f32, f32) {
        if let Some((px, py)) = self.pending {
            let (dx, dy) = (px + dx, py + dy);
            if dx.abs() + dy.abs() < Self::START_PX {
                self.pending = Some((dx, dy));
                return (0.0, 0.0);
            }
            self.pending = None;
            (self.x, self.y) = (dx.abs(), dy.abs());
            return self.keep_dominant(dx, dy);
        }
        self.x = self.x * Self::DECAY + dx.abs() * (1.0 - Self::DECAY);
        self.y = self.y * Self::DECAY + dy.abs() * (1.0 - Self::DECAY);
        self.keep_dominant(dx, dy)
    }

    /// `(dx, dy)` with an axis zeroed when the smoothed speeds call it drift.
    fn keep_dominant(&self, dx: f32, dy: f32) -> (f32, f32) {
        let keep_x = self.x > self.y * Self::MINOR_AXIS_RATIO;
        let keep_y = self.y > self.x * Self::MINOR_AXIS_RATIO;
        (if keep_x { dx } else { 0.0 }, if keep_y { dy } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `moves` and returns the summed output.
    fn run(filter: &mut AxisFilter, moves: &[(f32, f32)]) -> (f32, f32) {
        moves.iter().fold((0.0, 0.0), |(sx, sy), &(dx, dy)| {
            let (fx, fy) = filter.filter(dx, dy);
            (sx + fx, sy + fy)
        })
    }

    #[test]
    fn sloppy_vertical_movement_drops_the_horizontal_drift() {
        let mut filter = AxisFilter::new();
        let moves: Vec<_> = (0..40)
            .map(|i| (if i % 3 == 0 { 2.0 } else { 0.5 }, 4.0))
            .collect();
        let (sx, sy) = run(&mut filter, &moves);
        assert_eq!(sx, 0.0);
        assert_eq!(sy, 160.0);
    }

    #[test]
    fn sloppy_horizontal_movement_drops_the_vertical_drift() {
        let mut filter = AxisFilter::new();
        let moves: Vec<_> = (0..40)
            .map(|i| (-4.0, if i % 2 == 0 { 1.0 } else { -1.0 }))
            .collect();
        let (sx, sy) = run(&mut filter, &moves);
        assert_eq!(sx, -160.0);
        assert_eq!(sy, 0.0);
    }

    #[test]
    fn the_opening_motion_is_held_back_then_released_whole() {
        let mut filter = AxisFilter::new();
        assert_eq!(filter.filter(0.5, 2.0), (0.0, 0.0));
        assert_eq!(filter.filter(0.5, 2.0), (0.0, 0.0));
        // Past `START_PX`: the held-back vertical motion arrives, its drift
        // doesn't.
        assert_eq!(filter.filter(0.5, 2.0), (0.0, 6.0));
    }

    #[test]
    fn a_diagonal_keeps_both_axes() {
        let mut filter = AxisFilter::new();
        let (sx, sy) = run(&mut filter, &[(3.0, 3.0); 20]);
        assert_eq!((sx, sy), (60.0, 60.0));
        // Nothing is lost to the opening hold-back.
    }

    #[test]
    fn one_jittery_event_does_not_switch_axes() {
        let mut filter = AxisFilter::new();
        run(&mut filter, &[(0.0, 4.0); 20]);
        // A single sideways twitch mid-scroll stays drift.
        assert_eq!(filter.filter(6.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn turning_from_one_axis_to_the_other_hands_over() {
        let mut filter = AxisFilter::new();
        run(&mut filter, &[(0.0, 4.0); 20]);
        let (sx, _) = run(&mut filter, &[(4.0, 0.0); 20]);
        assert!(sx > 0.0);
        // Settled on the new axis: everything passes.
        assert_eq!(filter.filter(4.0, 0.0), (4.0, 0.0));
    }
}
