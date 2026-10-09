//! The per-frame render state — the drawable canvas rect, the clip/event shape
//! lists reconciled against `UiEvent`s, the arranger horizontal scroll and its
//! cursor-follow tracking, and the frozen per-frame snapshots that keep a
//! clip-view draw pass internally coherent.
//!
//! Grouped out of [`Display`](super::Display) as `Display::render`; every method
//! that drives it (the shape reconcilers, `sync_arranger_scroll`, the
//! `render_*` frozen-value accessors) stays on `Display` and reaches in through
//! `self.render`. See `030-ui-design.md`.

use egui::Rect;

use super::pane::{ClipPanel, Pane};
use super::status_message::StatusMessage;
use crate::shapes::{clip_shape::ClipShape, event_shape::EventShape};

/// The render state, grouped out of [`Display`](super::Display). `canvas_rect`
/// and the shape lists are rebuilt each frame / on `UiEvent`s; `arranger_scroll_x`
/// and the `arranger_follow_*` pair drive cursor-follow paging; the
/// `clip_frame_*` fields freeze a coherent snapshot for one draw pass.
pub(super) struct RenderState {
    /// The full drawable rect this frame.
    pub(super) canvas_rect: Rect,
    /// Arranger clip rectangles, reconciled against `UiEvent`s.
    pub(super) clip_shapes: Vec<ClipShape>,
    /// Piano-roll note bars, reconciled against `UiEvent`s.
    pub(super) event_shapes: Vec<EventShape>,
    /// Arranger horizontal scroll offset, in pixels. Written only by the
    /// arranger's scroll code (`sync_arranger_scroll`, `scroll_arranger_by`,
    /// the arranger zoom) — see `archive/210-docked-clip-panel.md`.
    pub(super) arranger_scroll_x: f32,
    /// Arranger vertical scroll offset of the track lanes, in pixels (`0.0` =
    /// track 1 at the top). Only moves once the lanes stop fitting
    /// (`ARRANGER_MIN_LANE_H`); written by the wheel
    /// (`scroll_arranger_lanes_by`) and by a track selection walking out of
    /// view (`reveal_selected_track`), clamped again by `arranger_layout` on
    /// every read so a resize or a shorter project can't strand it.
    pub(super) arranger_scroll_y: f32,
    /// The clip view's horizontal scroll offset, in pixels. Written only by the clip view's scroll
    /// code, as `arranger_scroll_x` is by the arranger's.
    pub(super) clip_scroll_x: f32,
    /// The pane the coordinate helpers answer for while set — a draw pass or
    /// hit-test scoped by `Display::in_pane`. `None` means the focused pane
    /// (`Display::active_pane`).
    pub(super) pane_override: Option<Pane>,
    /// The clip panel: shown or hidden, maximized or docked. See
    /// `archive/210-docked-clip-panel.md`.
    pub(super) clip_panel: ClipPanel,
    /// Arranger horizontal zoom, in pixels per beat. `None` until the first
    /// arranger frame latches the default (`BARS_IN_VIEWPORT` across the
    /// content width); fixed under window resize from then on. View-local
    /// like `arranger_scroll_x`, kept across project loads and clip visits. See
    /// `archive/190-arranger-zoom.md`.
    pub(super) arranger_px_per_beat: Option<f32>,
    /// Arranger framings `Z` (zoom to fit) replaced, newest last, for `X` to
    /// step back through. Cleared on project load. See `ZoomHistory`.
    pub(super) arranger_zoom_history: ZoomHistory,
    /// `Clip` horizontal zoom, in pixels per beat. Unlike
    /// `arranger_px_per_beat`, `None` is a real, lasting state — **home**:
    /// the home framing (`clip_home_span`) fills the content width
    /// (`clip_home_px_per_beat`). The home framing is frozen when the clip
    /// is opened, so an edit that changes the clip's length never re-fits
    /// the view. `Some` once the user zooms in, and
    /// back to `None` when they zoom out to the whole clip again. Reset on
    /// every clip entry and exit, since the whole clip is the piano roll's
    /// home. See `archive/200-clip-view-zoom.md`.
    pub(super) clip_px_per_beat: Option<f32>,
    /// The clip views' counterpart of `arranger_zoom_history`: the framings
    /// clip-view `Z` replaced, for `X`. Cleared with the zoom itself
    /// (`reset_clip_zoom`: clip entry/exit, project load, zooming out to fit)
    /// and by any manual zoom.
    pub(super) clip_zoom_history: ZoomHistory,
    /// The clip view's home framing, in event ticks: the lead clip's reach
    /// (window, notes kept outside it, headroom) when it was opened
    /// (`frame_clip_home`), held through edits so editing edges never moves
    /// the view — only the shading changes. `None` outside a clip view.
    /// `220-capture-without-pending-view.md`.
    pub(super) clip_home_span: Option<(i32, i32)>,
    /// The clip view's vertical home framing: the lead clip's note range,
    /// padded, when it was opened (`latch_clip_home_notes`). The row height
    /// comes from it and the pane height alone, so edits never re-fit the
    /// view. `None` outside a clip view.
    pub(super) clip_home_notes: Option<(u8, u8)>,
    /// The piano roll's vertical scroll: the view's centre in row units
    /// (`NoteAreaGeom::framed`), so the centre pitch holds when a resize
    /// changes the row height. `None` at home — centred on
    /// `clip_home_notes`. Written only by the user's scroll and key-column
    /// drag.
    pub(super) clip_centre_row: Option<f32>,
    /// The piano roll's row height set by the user's key-column zoom drag
    /// (`NoteAreaGeom::framed`). `None` = fitted to `clip_home_notes`. Kept
    /// through pane-size changes (held to the zoom's range); dropped with the
    /// home framing (`latch_clip_home_notes`, `reset_clip_zoom`).
    pub(super) clip_row_h: Option<f32>,
    /// The clip view's scale last frame, in pixels per tick, so a scale
    /// change the user didn't make (a window resize at the home framing)
    /// keeps the same tick at the left edge (`sync_clip_scroll`).
    pub(super) clip_last_ppt: Option<f32>,
    /// Region bounds captured for this frame's clip-pane render.
    pub(super) clip_frame_region_bounds: Option<(i32, i32)>,
    /// Transport running flag captured for this frame's clip-pane render,
    /// read just before `clip_frame_playback_tick`. The transport moves
    /// the playhead before it sets `running`, so this order keeps the pair
    /// consistent; reading `running` live later in the frame drew the
    /// stopped position for a frame when playback started in between.
    pub(super) clip_frame_running: Option<bool>,
    /// Playback tick captured for this frame's clip-pane render.
    pub(super) clip_frame_playback_tick: Option<i32>,
    /// Cursor tick captured for this frame's clip-pane render.
    pub(super) clip_frame_cursor_tick: Option<i32>,
    /// Scroll offset captured for this frame's clip-pane render.
    pub(super) clip_frame_scroll_x: Option<f32>,
    /// Set by a trackpad/wheel horizontal scroll in the arranger; suspends
    /// `sync_arranger_scroll`'s cursor-follow paging so the view holds where
    /// the user scrolled it, until the transport cursor next moves. Reset on
    /// leaving the arranger. See `030-ui-design.md`.
    pub(super) arranger_follow_suspended: bool,
    /// Cursor tick `sync_arranger_scroll` observed last frame, used to detect a
    /// cursor move and re-enable follow. `None` while outside the arranger.
    pub(super) arranger_follow_last_cursor_tick: Option<i32>,
    /// Transport running state last frame, to detect the first playing frame.
    pub(super) was_running_last_frame: bool,
    /// The footer's passing message (`UiEvent::Status`), until it has faded.
    pub(super) status: Option<StatusMessage>,
}

impl RenderState {
    /// An empty render state: no canvas yet (`Rect::NOTHING`, *not*
    /// `Rect::default()` — the zero rect would be a valid draw target), no
    /// shapes, no scroll.
    pub(super) fn new() -> Self {
        RenderState {
            canvas_rect: Rect::NOTHING,
            clip_shapes: Vec::new(),
            event_shapes: Vec::new(),
            arranger_scroll_x: 0.,
            arranger_scroll_y: 0.,
            clip_scroll_x: 0.,
            pane_override: None,
            clip_panel: ClipPanel::new(),
            arranger_px_per_beat: None,
            arranger_zoom_history: ZoomHistory::default(),
            clip_px_per_beat: None,
            clip_zoom_history: ZoomHistory::default(),
            clip_home_span: None,
            clip_home_notes: None,
            clip_centre_row: None,
            clip_row_h: None,
            clip_last_ppt: None,
            clip_frame_region_bounds: None,
            clip_frame_running: None,
            clip_frame_playback_tick: None,
            clip_frame_cursor_tick: None,
            clip_frame_scroll_x: None,
            arranger_follow_suspended: false,
            arranger_follow_last_cursor_tick: None,
            was_running_last_frame: false,
            status: None,
        }
    }
}

/// One framing of the arranger or a clip view: scale and horizontal scroll
/// together, so stepping back restores exactly what was on screen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Framing {
    /// The view's scale at the time, in pixels per beat
    /// (`arranger_px_per_beat`, or `clip_px_per_beat` — the fit scale when
    /// that was `None`).
    pub(super) px_per_beat: f32,
    /// The view's scroll offset at the time (`RenderState::arranger_scroll_x`
    /// or `clip_scroll_x`).
    pub(super) scroll_x: f32,
}

/// The framings `Z` (zoom to fit) replaced, for `X` to step back through —
/// Ableton's zoom-back. A stack, newest last, bounded at
/// [`ZoomHistory::CAPACITY`] (the oldest entry drops) so a long session of
/// `Z` presses can't grow it without end. Only `Z` pushes: `+`/`-`, wheel and
/// pinch are continuous adjustments you undo the same way you made them.
#[derive(Default)]
pub(super) struct ZoomHistory {
    /// Saved framings, oldest first.
    framings: Vec<Framing>,
}

impl ZoomHistory {
    /// How many framings `X` can step back through.
    pub(super) const CAPACITY: usize = 16;

    /// Remembers `framing`, dropping the oldest one when full.
    pub(super) fn push(&mut self, framing: Framing) {
        if self.framings.len() == Self::CAPACITY {
            self.framings.remove(0);
        }
        self.framings.push(framing);
    }

    /// The most recently saved framing, removed; `None` when empty.
    pub(super) fn pop(&mut self) -> Option<Framing> {
        self.framings.pop()
    }

    /// Rewrites every saved framing with `f` (a clip retime, which moves
    /// the ticks they point at).
    pub(super) fn map(&mut self, mut f: impl FnMut(Framing) -> Framing) {
        for framing in &mut self.framings {
            *framing = f(*framing);
        }
    }

    /// Forgets every saved framing.
    pub(super) fn clear(&mut self) {
        self.framings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{Framing, ZoomHistory};

    fn framing(px_per_beat: f32) -> Framing {
        Framing {
            px_per_beat,
            scroll_x: px_per_beat * 10.0,
        }
    }

    #[test]
    fn steps_back_newest_first() {
        let mut history = ZoomHistory::default();
        history.push(framing(1.0));
        history.push(framing(2.0));
        assert_eq!(history.pop(), Some(framing(2.0)));
        assert_eq!(history.pop(), Some(framing(1.0)));
        assert_eq!(history.pop(), None);
    }

    #[test]
    fn a_full_history_drops_the_oldest() {
        let mut history = ZoomHistory::default();
        for i in 0..=ZoomHistory::CAPACITY {
            history.push(framing(i as f32));
        }
        let mut popped = Vec::new();
        while let Some(f) = history.pop() {
            popped.push(f.px_per_beat);
        }
        assert_eq!(popped.len(), ZoomHistory::CAPACITY);
        // Newest survives, the very first push (0.0) is gone.
        assert_eq!(popped.first(), Some(&(ZoomHistory::CAPACITY as f32)));
        assert_eq!(popped.last(), Some(&1.0));
    }

    #[test]
    fn clear_forgets_everything() {
        let mut history = ZoomHistory::default();
        history.push(framing(1.0));
        history.clear();
        assert_eq!(history.pop(), None);
    }
}
