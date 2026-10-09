//! `Display`'s horizontal zoom. The arranger's: the pixels-per-beat scale
//! behind its `pixels_per_tick`, its lazy default, anchored zooming — a zoom
//! keeps one tick (the pointer's, or the cursor's for the keys) on the same
//! screen pixel — and `Z` / `X`, zoom to fit and step back (phases 1 and 3 of
//! `archive/190-arranger-zoom.md`). The grid follows the scale (`grid.rs`). The clip
//! view's (`Clip`, phases 1 and 3 of `archive/200-clip-view-zoom.md`): the
//! same anchored zoom over a scale whose home — and zoom-out floor — is the
//! whole clip across the width ("fit", `clip_px_per_beat == None`), and the
//! same `Z` / `X`, framing the selected notes.

use crate::core::config::{
    ARRANGER_MIN_PX_PER_BEAT, ARRANGER_ZOOM_OUT_HEADROOM, BARS_IN_VIEWPORT, MAX_PX_PER_BEAT,
    ZOOM_FIT_MARGIN,
};
use crate::core::time::{bars_to_beats, beats_to_ticks, px_per_beat_to_ppt};
use crate::models::clip::EventSpaceRetime;
use crate::view::display::render_state::Framing;

use super::scroll::{arranger_max_scroll_x, clip_scroll_range};
use super::*;

impl Display {
    /// The arranger scale, in pixels per beat: the latched value, or the
    /// default (`BARS_IN_VIEWPORT` across the content width) before the first
    /// arranger frame latches it.
    pub(in crate::view::display) fn arranger_px_per_beat(&self) -> f32 {
        self.render
            .arranger_px_per_beat
            .unwrap_or_else(|| default_arranger_px_per_beat(self.content_w()))
    }

    /// Fixes the default arranger scale on the first arranger frame, so a
    /// window resize from then on shows more or fewer bars rather than
    /// stretching them. Called from `sync_arranger_scroll`, after `ui` has set
    /// this frame's `canvas_rect`.
    pub(super) fn latch_arranger_zoom(&mut self) {
        if self.render.arranger_px_per_beat.is_none() {
            self.render.arranger_px_per_beat = Some(self.arranger_px_per_beat());
        }
    }

    /// Zooms whichever timeline is showing by `factor` (> 1 zooms in) — the
    /// ⌘/Ctrl+wheel, pinch and `+`/`-` entry point. See `zoom_arranger_by` /
    /// `zoom_clip_by`; a no-op in every other view.
    ///
    /// A pointer zoom (wheel, pinch) goes to the pane under `pointer`; the
    /// keys (`pointer` `None`) to the focused one.
    pub(in crate::view::display) fn zoom_timeline_by(
        &mut self,
        factor: f32,
        pointer: Option<(f32, f32)>,
    ) {
        let pane = pointer
            .and_then(|(x, y)| self.pane_at(x, y))
            .unwrap_or_else(|| self.focused_pane());
        let pointer_x = pointer.map(|(x, _)| x);
        match pane {
            Pane::Arranger => self.in_pane(pane, |d| d.zoom_arranger_by(factor, pointer_x)),
            Pane::Clip => self.in_pane(pane, |d| d.zoom_clip_by(factor, pointer_x)),
        }
    }

    /// Multiplies the arranger scale by `factor` (> 1 zooms in), keeping the
    /// anchor tick on the same screen pixel: the tick under `pointer_x` when
    /// that is over the content area (wheel / pinch), else the cursor if it
    /// is on screen (the `+`/`-` keys), else the viewport centre. Suspends
    /// cursor-follow like a scroll does, or `sync_arranger_scroll` would page
    /// straight back to the cursor and undo the framing. A no-op outside the
    /// arranger (`scroll_x` belongs to the clip views there) and at the zoom
    /// limits — the zoom-out one content-relative, see `min_arranger_px_per_beat`.
    pub(in crate::view::display) fn zoom_arranger_by(
        &mut self,
        factor: f32,
        pointer_x: Option<f32>,
    ) {
        if !self.is_pane_visible(Pane::Arranger) || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_px_per_beat = self.arranger_px_per_beat();
        let last_content_tick = self.arranger_last_content_tick();
        let floor = min_arranger_px_per_beat(self.content_w(), last_content_tick);
        let new_px_per_beat = zoomed_px_per_beat(old_px_per_beat, factor, floor);
        if new_px_per_beat == old_px_per_beat {
            return;
        }
        let old_ppt = px_per_beat_to_ppt(old_px_per_beat);
        let new_ppt = px_per_beat_to_ppt(new_px_per_beat);
        let anchor_tick = self.zoom_anchor(pointer_x, self.render.arranger_scroll_x, old_ppt);

        self.render.arranger_px_per_beat = Some(new_px_per_beat);
        self.render.arranger_follow_suspended = true;
        // `X` undoes the last `Z`; once the user has zoomed by hand there is
        // no `Z` left to undo. Keeping the history let `X` from a manual
        // zoom-out jump *in* to a stale pre-`Z` framing — with no way back,
        // since nothing records the manual framing.
        self.render.arranger_zoom_history.clear();
        let max_scroll_x = arranger_max_scroll_x(last_content_tick, new_ppt);
        self.render.arranger_scroll_x =
            zoomed_scroll_x(self.render.arranger_scroll_x, anchor_tick, old_ppt, new_ppt)
                .clamp(0.0, max_scroll_x);
    }

    /// `Z` — zoom to the time selection: frames the marquee across the full
    /// width less `ZOOM_FIT_MARGIN` (4% of the width) a side,
    /// centred — the same screen span for any selection length.
    /// Selection-only (see `zoom_to_fit_range`): with no marquee spanning real
    /// time it does nothing. The framing it replaces is pushed onto
    /// `arranger_zoom_history` for `X`, unless nothing changes (a repeated `Z`
    /// doesn't bury the history under copies). Suspends cursor-follow like
    /// every zoom. A no-op outside the arranger.
    pub(in crate::view::display) fn zoom_arranger_to_fit(&mut self) {
        if !self.is_pane_visible(Pane::Arranger) {
            return;
        }
        let Some((start, end)) = zoom_to_fit_range(self.gesture.time_selection) else {
            return;
        };

        let target = fit_framing(
            start,
            end,
            self.content_w(),
            self.arranger_last_content_tick(),
        );
        let current = self.arranger_framing();
        if target == current {
            return;
        }
        self.render.arranger_zoom_history.push(current);
        self.apply_arranger_framing(target);
    }

    /// `X` — step back to the framing the last `Z` replaced (repeatable, up
    /// to `ZoomHistory::CAPACITY` deep, through consecutive `Z`s; any manual
    /// zoom in between clears it — see `zoom_arranger_by`). The restored
    /// scroll is re-clamped, since the arrangement may have shrunk since. A
    /// no-op outside the arranger and with an empty history.
    pub(in crate::view::display) fn zoom_arranger_back(&mut self) {
        if !self.is_pane_visible(Pane::Arranger) {
            return;
        }
        let Some(framing) = self.render.arranger_zoom_history.pop() else {
            return;
        };
        let ppt = px_per_beat_to_ppt(framing.px_per_beat);
        let max_scroll_x = arranger_max_scroll_x(self.arranger_last_content_tick(), ppt);
        self.apply_arranger_framing(Framing {
            px_per_beat: framing.px_per_beat,
            scroll_x: framing.scroll_x.clamp(0.0, max_scroll_x),
        });
    }

    /// What the arranger shows right now, as a restorable framing.
    fn arranger_framing(&self) -> Framing {
        Framing {
            px_per_beat: self.arranger_px_per_beat(),
            scroll_x: self.render.arranger_scroll_x,
        }
    }

    /// Shows `framing` (already clamped) and suspends cursor-follow, or the
    /// next frame's paging would scroll straight back to the cursor.
    fn apply_arranger_framing(&mut self, framing: Framing) {
        self.render.arranger_px_per_beat = Some(framing.px_per_beat);
        self.render.arranger_scroll_x = framing.scroll_x;
        self.render.arranger_follow_suspended = true;
    }

    /// The tick a zoom of the active pane pivots on (`zoom_anchor_tick`), for
    /// a pointer at screen x `pointer_x` and the pane at `scroll_x` /
    /// `old_ppt`.
    fn zoom_anchor(&self, pointer_x: Option<f32>, scroll_x: f32, old_ppt: f32) -> f32 {
        let content_x = self.content_origin_x();
        zoom_anchor_tick(
            pointer_x.map(|x| x - content_x),
            self.cursor_tick_atomic().load(Ordering::Relaxed) as f32,
            scroll_x,
            old_ppt,
            self.content_w(),
        )
    }
}

impl Display {
    /// The clip view's home scale (`clip_px_per_beat == None`), in pixels
    /// per beat: the home framing (`clip_home_span` — the clip's reach when
    /// it was opened, held through edits) across the content width. Also the
    /// zoom-out floor. No cap for long clips: the app doesn't choose a
    /// framing for the user (`220-capture-without-pending-view.md`).
    pub(in crate::view::display) fn clip_home_px_per_beat(&self) -> f32 {
        let (start, end) = self.clip_home_span();
        self.content_w() / (end - start).max(1) as f32 * beats_to_ticks(1.0) as f32
    }

    /// The clip view's scale right now, in pixels per beat: the zoom, or home.
    pub(in crate::view::display) fn clip_px_per_beat(&self) -> f32 {
        self.render
            .clip_px_per_beat
            .unwrap_or_else(|| self.clip_home_px_per_beat())
    }

    /// Multiplies the clip-view scale by `factor` (> 1 zooms in), keeping the
    /// anchor tick on its pixel exactly like `zoom_arranger_by` (pointer, else
    /// visible cursor, else centre). Zooming out stops at the home framing;
    /// zooming onto (or across) the home scale returns home
    /// (`zoomed_clip_state`). A no-op outside the clip view.
    pub(in crate::view::display) fn zoom_clip_by(&mut self, factor: f32, pointer_x: Option<f32>) {
        if !self.is_pane_visible(Pane::Clip) || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let home_px_per_beat = self.clip_home_px_per_beat();
        let old_px_per_beat = self.clip_px_per_beat();
        let new_state = zoomed_clip_state(old_px_per_beat, factor, home_px_per_beat);
        if new_state == self.render.clip_px_per_beat {
            return;
        }
        let new_px_per_beat = new_state.unwrap_or(home_px_per_beat);
        let old_ppt = px_per_beat_to_ppt(old_px_per_beat);
        let new_ppt = px_per_beat_to_ppt(new_px_per_beat);
        let anchor_tick = self.zoom_anchor(pointer_x, self.render.clip_scroll_x, old_ppt);

        self.render.clip_px_per_beat = new_state;
        // As in the arranger: `X` undoes the last `Z`, and a manual zoom
        // leaves no `Z` to undo.
        self.render.clip_zoom_history.clear();
        let (min_scroll_x, max_scroll_x) =
            self.clip_scroll_bounds(self.region_bounds_snapshot(), new_ppt);
        self.set_clip_scroll(
            zoomed_scroll_x(self.render.clip_scroll_x, anchor_tick, old_ppt, new_ppt)
                .clamp(min_scroll_x, max_scroll_x),
            new_ppt,
        );
    }

    /// Back home, with the frozen home framing and the `Z` history dropped: on
    /// every clip entry and exit, lead-clip change and project load (home is
    /// the piano roll's starting framing, not a preference, and another
    /// clip's framings mean nothing here). The caller frames the new clip
    /// (`frame_clip_home`) where a clip view is showing.
    pub(in crate::view::display) fn reset_clip_zoom(&mut self) {
        self.render.clip_px_per_beat = None;
        self.render.clip_home_span = None;
        self.render.clip_home_notes = None;
        self.render.clip_centre_row = None;
        self.render.clip_row_h = None;
        self.render.clip_last_ppt = None;
        self.render.clip_zoom_history.clear();
    }

    /// Clip-view `Z` — zoom to the selected notes: frames the hull of every
    /// selected note (at least one beat, `clip_zoom_to_fit_range`) across the
    /// width less `ZOOM_FIT_MARGIN` a side, centred and kept within the clip
    /// (`clip_fit_framing`) — the arranger's `Z` with the event selection in
    /// place of the marquee. A no-op with nothing selected, so "fit every
    /// note" is `⌘/Ctrl+A` then `Z`. The framing it replaces goes on
    /// `clip_zoom_history` for `X`, unless nothing changes.
    pub(in crate::view::display) fn zoom_clip_to_fit(&mut self) {
        if !self.is_pane_visible(Pane::Clip) {
            return;
        }
        let selected_spans = self
            .render
            .event_shapes
            .iter()
            .filter(|shape| shape.is_selected())
            .map(|shape| (shape.start_tick(), shape.end_tick()));
        let Some((start, end)) = clip_zoom_to_fit_range(selected_spans) else {
            return;
        };

        let target = clip_fit_framing(
            (start, end),
            self.clip_scroll_span(self.region_bounds_snapshot()),
            self.content_w(),
            self.clip_home_px_per_beat(),
        );
        let current = self.clip_framing();
        if target == current {
            return;
        }
        self.render.clip_zoom_history.push(current);
        self.apply_clip_framing(target);
    }

    /// Clip-view `X` — step back to the framing the last `Z` replaced, like
    /// `zoom_arranger_back`. The restored framing is re-clamped to the clip
    /// as it is now (`apply_clip_framing`). A no-op outside `Clip`
    /// and with an empty history.
    pub(in crate::view::display) fn zoom_clip_back(&mut self) {
        if !self.is_pane_visible(Pane::Clip) {
            return;
        }
        if let Some(framing) = self.render.clip_zoom_history.pop() {
            self.apply_clip_framing(framing);
        }
    }

    /// What the clip view shows right now, as a restorable framing — the home
    /// scale standing in for `None`.
    fn clip_framing(&self) -> Framing {
        Framing {
            px_per_beat: self.clip_px_per_beat(),
            scroll_x: self.render.clip_scroll_x,
        }
    }

    /// Shows `framing` in the clip view, re-judged against the clip as it is
    /// now (`clip_zoom_state`): home when its scale is the home scale (or
    /// below it); else that scale, floored at the home framing. The scroll is
    /// clamped to `clip_scroll_span` either way. Leaves the history alone.
    fn apply_clip_framing(&mut self, framing: Framing) {
        let home = self.clip_home_px_per_beat();
        self.render.clip_px_per_beat = clip_zoom_state(framing.px_per_beat, home, home);
        let ppt = px_per_beat_to_ppt(self.clip_px_per_beat());
        let (min_scroll_x, max_scroll_x) =
            self.clip_scroll_bounds(self.region_bounds_snapshot(), ppt);
        self.set_clip_scroll(framing.scroll_x.clamp(min_scroll_x, max_scroll_x), ppt);
    }

    /// A retime moved `clip_id`'s event ticks (`UiEvent::ClipRetimed` — Enter's
    /// tempo fit, its undo/redo, `⌥=`/`⌥-`): if that clip is open, maps the
    /// home span, the `Z` history and the framing on screen through it
    /// (`retimed_framing`), so every note stays on its pixel and only the
    /// grid changes under it. Not the app scrolling on the user's behalf:
    /// without it the view kept the old ticks while the notes moved, far off
    /// screen for a long capture buffer (`220-capture-without-pending-view.md`).
    pub(in crate::view::display) fn follow_clip_retime(
        &mut self,
        clip_id: Uuid,
        retime: EventSpaceRetime,
    ) {
        if !self.is_open_clip(clip_id) {
            return;
        }
        self.in_pane(Pane::Clip, |d| {
            // Read before the home span moves: home's scale depends on it.
            let framing = retimed_framing(d.clip_framing(), retime);
            d.render.clip_home_span = d
                .render
                .clip_home_span
                .map(|(start, end)| (retime.map_tick(start), retime.map_tick(end)));
            d.render
                .clip_zoom_history
                .map(|framing| retimed_framing(framing, retime));
            d.apply_clip_framing(framing);
        });
    }

    /// Whether `clip_id` is the clip the clip pane is showing — the guard for
    /// every "this clip changed" `UiEvent` that adjusts the clip view's
    /// framing (`follow_clip_retime`, `reframe_after_capture`).
    pub(in crate::view::display) fn is_open_clip(&self, clip_id: Uuid) -> bool {
        self.lead_clip_time
            .as_ref()
            .is_some_and(|lead| lead.clip_id == clip_id)
            && self.is_pane_visible(Pane::Clip)
    }

    /// `Z` in whichever timeline is showing — see `zoom_arranger_to_fit` /
    /// `zoom_clip_to_fit`.
    pub(in crate::view::display) fn zoom_timeline_to_fit(&mut self) {
        match self.focused_pane() {
            Pane::Arranger => self.in_pane(Pane::Arranger, Self::zoom_arranger_to_fit),
            Pane::Clip => self.in_pane(Pane::Clip, Self::zoom_clip_to_fit),
        }
    }

    /// `X` in whichever timeline is showing — see `zoom_arranger_back` /
    /// `zoom_clip_back`.
    pub(in crate::view::display) fn zoom_timeline_back(&mut self) {
        match self.focused_pane() {
            Pane::Arranger => self.in_pane(Pane::Arranger, Self::zoom_arranger_back),
            Pane::Clip => self.in_pane(Pane::Clip, Self::zoom_clip_back),
        }
    }
}

/// What clip-view `Z` frames: the hull of the selected notes' `[start, end)`
/// spans, widened symmetrically to at least one beat — a lone 1/32 hi-hat
/// would otherwise zoom to the cap and fill the screen with one note, losing
/// its neighbourhood. `None` with nothing selected: as in the arranger,
/// there is no fallback.
fn clip_zoom_to_fit_range(selected_spans: impl Iterator<Item = (i32, i32)>) -> Option<(i32, i32)> {
    let (start, end) = selected_spans.fold(None, |hull: Option<(i32, i32)>, (start, end)| {
        Some(hull.map_or((start, end), |(lo, hi)| (lo.min(start), hi.max(end))))
    })?;
    let min_span = beats_to_ticks(1.0);
    if end - start >= min_span {
        return Some((start, end));
    }
    let widened_start = (start + end) / 2 - min_span / 2;
    Some((widened_start, widened_start + min_span))
}

/// Clip-view `Z`'s framing of `[start, end)` in a clip reaching over
/// `region` (its window plus any notes outside it, `clip_reach`): the
/// arranger's fit maths (`fit_px_per_beat`, `centred_scroll_x`) with the fit
/// scale as the floor and the scroll clamped within that reach
/// (`clip_scroll_range`) — so a selection at the clip start sits flush left,
/// and one spanning the whole clip lands on the fit scale (`fit_scale`, in
/// pixels per beat). The floor is
/// capped at `MAX_PX_PER_BEAT`: a one-beat clip on a very wide screen fits
/// *above* the cap, and an inverted clamp range would panic.
fn clip_fit_framing(
    (start, end): (i32, i32),
    (region_start, region_end): (i32, i32),
    content_w: f32,
    fit_scale: f32,
) -> Framing {
    let floor = fit_scale.min(MAX_PX_PER_BEAT);
    let px_per_beat = fit_px_per_beat(end - start, content_w, floor);
    let ppt = px_per_beat_to_ppt(px_per_beat);
    let (min_scroll_x, max_scroll_x) = clip_scroll_range(region_start, region_end, ppt, content_w);
    Framing {
        px_per_beat,
        scroll_x: centred_scroll_x(start, end, ppt, content_w).clamp(min_scroll_x, max_scroll_x),
    }
}

/// What `Z` frames: the time selection when it spans real time, else nothing.
/// There is deliberately no fallback — an earlier cut framed the whole
/// arrangement with no selection, which read as `Z` misfiring. A clip band
/// press marquees exactly its clip, so zooming to one clip is band press +
/// `Z`.
fn zoom_to_fit_range(time_selection: Option<TimeSelectionRect>) -> Option<(i32, i32)> {
    time_selection
        .filter(|rect| rect.has_tick_range())
        .map(|rect| (rect.start, rect.end))
}

/// `Z`'s complete framing of `[start, end)` in a `content_w`-wide arranger
/// whose material ends at `last_content_tick` (which includes the selection
/// itself — see `arranger_last_content_tick`): the fitted scale, and the
/// scroll that centres the range, clamped like every arranger scroll. Only
/// the *hard* zoom limits apply — the content-relative floor exists to stop
/// `-`/wheel zooming out into nothing, not to stop `Z` fitting a long
/// selection in a short project. The view can't scroll before bar 1, so a
/// selection starting within one margin of it can't be centred: it keeps the
/// same scale (the same width on screen) but sits flush left. A lead-in
/// before bar 1 would centre it, and was tried — but it let every scroll
/// and zoom park the view in an empty gutter, which wasn't worth it for
/// that one edge case.
fn fit_framing(start: i32, end: i32, content_w: f32, last_content_tick: i32) -> Framing {
    let px_per_beat = fit_px_per_beat(end - start, content_w, ARRANGER_MIN_PX_PER_BEAT);
    let ppt = px_per_beat_to_ppt(px_per_beat);
    let scroll_x = centred_scroll_x(start, end, ppt, content_w)
        .clamp(0.0, arranger_max_scroll_x(last_content_tick, ppt));
    Framing {
        px_per_beat,
        scroll_x,
    }
}

/// The scale that fits `span_ticks` into the middle of `content_w`, leaving
/// `ZOOM_FIT_MARGIN` of the width empty on each side — a margin
/// fixed on *screen*, so every selection length frames identically. Clamped
/// to the zoom range (`floor` is the content-relative zoom-out limit): a span
/// too short to fill it at `MAX_PX_PER_BEAT` (under ~3 beats on a
/// 2900pt-wide view) sits centred at max zoom with wider margins; one too
/// long for `floor` overflows, centred.
fn fit_px_per_beat(span_ticks: i32, content_w: f32, floor: f32) -> f32 {
    let span_beats = span_ticks.max(1) as f32 / beats_to_ticks(1.0) as f32;
    let framed_w = content_w * (1.0 - 2.0 * ZOOM_FIT_MARGIN);
    (framed_w / span_beats).clamp(floor, MAX_PX_PER_BEAT)
}

/// The scroll offset that centres `[start, end)` in a `content_w`-wide view
/// at `ppt`. Unclamped — the caller clamps (a range near bar 1 pins left).
fn centred_scroll_x(start: i32, end: i32, ppt: f32, content_w: f32) -> f32 {
    (start + end) as f32 / 2.0 * ppt - content_w / 2.0
}

/// The default arranger scale for a `content_w`-wide content area:
/// `BARS_IN_VIEWPORT` bars across it. `pixels_per_tick` built from this is
/// bit-identical to the old fixed `content_w / BARS_IN_VIEWPORT /
/// bars_to_ticks(1)` — the two differ only by power-of-two factors.
fn default_arranger_px_per_beat(content_w: f32) -> f32 {
    content_w / (BARS_IN_VIEWPORT * bars_to_beats(1)) as f32
}

/// The zoom-out limit for a `content_w`-wide view of an arrangement ending at
/// `last_content_tick`: the scale that fits the arrangement plus
/// `ARRANGER_ZOOM_OUT_HEADROOM` across the width — but never tighter than the
/// default `BARS_IN_VIEWPORT` span (so a short or empty project can still zoom
/// out to the default) and never below the hard `ARRANGER_MIN_PX_PER_BEAT`.
/// Capped at `MAX_PX_PER_BEAT` so the range is never inverted.
fn min_arranger_px_per_beat(content_w: f32, last_content_tick: i32) -> f32 {
    let content_beats = last_content_tick.max(0) as f32 / beats_to_ticks(1.0) as f32;
    let default_beats = (BARS_IN_VIEWPORT * bars_to_beats(1)) as f32;
    let span_beats = (content_beats * ARRANGER_ZOOM_OUT_HEADROOM).max(default_beats);
    (content_w / span_beats).clamp(ARRANGER_MIN_PX_PER_BEAT, MAX_PX_PER_BEAT)
}

/// `px_per_beat` scaled by `factor`, clamped to `floor ..= MAX_PX_PER_BEAT`
/// — except that a scale already below `floor` (clips deleted while zoomed
/// out, a window narrowed) is never pushed *up* by a zoom-out: the floor only
/// stops further zooming out, it doesn't jolt the view.
fn zoomed_px_per_beat(px_per_beat: f32, factor: f32, floor: f32) -> f32 {
    (px_per_beat * factor).clamp(floor.min(px_per_beat), MAX_PX_PER_BEAT)
}

/// Relative slack under which a clip-view scale counts as the home scale, so
/// float drift (`× 1.25` then `÷ 1.25`) can't strand the view a hair off it.
const CLIP_HOME_TOLERANCE: f32 = 1e-4;

/// The clip view's zoom state for a scale of `px_per_beat`: `None` — home —
/// when it is the `home` scale; otherwise the scale, floored at `whole` (the
/// whole clip across the width — the view never shows more than the clip).
/// A clip that shrank under a zoom, so that the zoomed scale would now show
/// more than all of it, is floored to the whole clip, which for a short clip
/// is home.
pub(super) fn clip_zoom_state(px_per_beat: f32, whole: f32, home: f32) -> Option<f32> {
    let px_per_beat = px_per_beat.max(whole);
    ((px_per_beat - home).abs() > home * CLIP_HOME_TOLERANCE).then_some(px_per_beat)
}

/// The clip view's zoom state after zooming `px_per_beat` by `factor`:
/// clamped to `home ..= MAX_PX_PER_BEAT` (`zoomed_px_per_beat`; home is also
/// the zoom-out floor), and back home (`None`) on landing on the `home` scale
/// or stepping across it — so a zoom left below home by a widened window
/// (`sync_clip_scroll` floors at nothing) comes home on the way back in,
/// rather than stepping past.
fn zoomed_clip_state(px_per_beat: f32, factor: f32, home: f32) -> Option<f32> {
    let zoomed = zoomed_px_per_beat(px_per_beat, factor, home);
    if (px_per_beat - home) * (zoomed - home) < 0.0 {
        return None;
    }
    clip_zoom_state(zoomed, home, home)
}

/// `framing` after `retime`: the scale divided by the retime's, so each tick's
/// pixel distance from tick 0 is unchanged, and the scroll moved by the
/// retime's shift at the new scale, so every note keeps its on-screen x.
fn retimed_framing(framing: Framing, retime: EventSpaceRetime) -> Framing {
    let px_per_beat = framing.px_per_beat / retime.scale as f32;
    Framing {
        px_per_beat,
        scroll_x: framing.scroll_x + retime.offset as f32 * px_per_beat_to_ppt(px_per_beat),
    }
}

/// The scroll offset that keeps `anchor_tick` on the same screen pixel when
/// the scale changes from `old_ppt` to `new_ppt`. Unclamped — the caller
/// clamps with the *new* scale's bounds.
pub(super) fn zoomed_scroll_x(scroll_x: f32, anchor_tick: f32, old_ppt: f32, new_ppt: f32) -> f32 {
    anchor_tick * new_ppt - (anchor_tick * old_ppt - scroll_x)
}

/// The (fractional) tick a zoom pivots on. `pointer_px` is the pointer's x
/// relative to the content origin, used when it lies within
/// `0..=viewport_w`; otherwise the cursor, when it is on screen; otherwise
/// the viewport centre, so a key zoom with the cursor scrolled away doesn't
/// lurch the view toward it.
fn zoom_anchor_tick(
    pointer_px: Option<f32>,
    cursor_tick: f32,
    scroll_x: f32,
    ppt: f32,
    viewport_w: f32,
) -> f32 {
    let on_screen = |px: f32| (0.0..=viewport_w).contains(&px);
    if let Some(px) = pointer_px.filter(|&px| on_screen(px)) {
        return (px + scroll_x) / ppt;
    }
    if on_screen(cursor_tick * ppt - scroll_x) {
        return cursor_tick;
    }
    (viewport_w / 2.0 + scroll_x) / ppt
}

#[cfg(test)]
mod tests {
    use super::{
        Framing, centred_scroll_x, clip_fit_framing, clip_zoom_state, clip_zoom_to_fit_range,
        default_arranger_px_per_beat, fit_framing, fit_px_per_beat, min_arranger_px_per_beat,
        retimed_framing, zoom_anchor_tick, zoom_to_fit_range, zoomed_clip_state,
        zoomed_px_per_beat, zoomed_scroll_x,
    };
    use crate::core::config::{
        ARRANGER_MIN_PX_PER_BEAT, ARRANGER_ZOOM_OUT_HEADROOM, BARS_IN_VIEWPORT, MAX_PX_PER_BEAT,
        ZOOM_FIT_MARGIN, ZOOM_KEY_STEP,
    };
    use crate::core::input_event::TimeSelectionRect;
    use crate::core::time::{bars_to_ticks, beats_to_ticks, px_per_beat_to_ppt};
    use crate::models::clip::EventSpaceRetime;

    #[test]
    fn default_scale_is_bit_identical_to_the_old_fixed_viewport() {
        for content_w in [1.0, 640.0, 1274.0, 1382.0, 1917.5, 3000.25] {
            let old_ppt = content_w / BARS_IN_VIEWPORT as f32 / bars_to_ticks(1) as f32;
            let new_ppt = px_per_beat_to_ppt(default_arranger_px_per_beat(content_w));
            assert_eq!(
                old_ppt.to_bits(),
                new_ppt.to_bits(),
                "content_w {content_w}"
            );
        }
    }

    #[test]
    fn zoom_keeps_the_anchor_on_its_pixel() {
        let old_ppt = 0.01;
        let scroll_x = 350.0;
        let anchor_tick = 61_234.5;
        let anchor_px = anchor_tick * old_ppt - scroll_x;
        for new_ppt in [0.002, 0.0125, 0.04, 0.1] {
            let new_scroll_x = zoomed_scroll_x(scroll_x, anchor_tick, old_ppt, new_ppt);
            let new_px = anchor_tick * new_ppt - new_scroll_x;
            assert!(
                (new_px - anchor_px).abs() < 1e-3,
                "ppt {new_ppt}: {new_px} vs {anchor_px}"
            );
        }
    }

    #[test]
    fn zooming_out_from_bar_one_goes_negative_so_the_clamp_pins_it_left() {
        // Anchor mid-screen with the view at bar 1: zooming out wants a
        // negative scroll, which the caller's clamp turns into 0 — the view
        // grows to the right, like every DAW.
        assert!(zoomed_scroll_x(0.0, 20_000.0, 0.01, 0.005) < 0.0);
    }

    #[test]
    fn zoom_in_then_out_by_the_same_factor_round_trips() {
        let (scroll_x, anchor, ppt) = (512.0, 40_000.0, 0.01);
        let zoomed_in = zoomed_scroll_x(scroll_x, anchor, ppt, ppt * 1.25);
        let back = zoomed_scroll_x(zoomed_in, anchor, ppt * 1.25, ppt);
        assert!((back - scroll_x).abs() < 1e-2);
    }

    #[test]
    fn scale_is_clamped_to_the_zoom_range() {
        assert_eq!(zoomed_px_per_beat(10.0, 1.25, 2.0), 12.5);
        assert_eq!(
            zoomed_px_per_beat(MAX_PX_PER_BEAT * 0.9, 2.0, 2.0),
            MAX_PX_PER_BEAT
        );
        assert_eq!(zoomed_px_per_beat(4.0, 0.25, 3.0), 3.0);
    }

    #[test]
    fn a_scale_already_below_the_floor_is_not_pushed_up_by_zooming_out() {
        // Floor rose past the current scale (clips deleted while zoomed out):
        // zooming out holds still rather than jumping in...
        assert_eq!(zoomed_px_per_beat(3.0, 0.8, 5.0), 3.0);
        // ...and zooming in still works from where it is.
        assert_eq!(zoomed_px_per_beat(3.0, 1.25, 5.0), 3.75);
    }

    #[test]
    fn floor_fits_the_arrangement_plus_headroom() {
        // 200 bars of material: 200 * 4 * 1.25 = 1000 beats across the width.
        let content_w = 3000.0;
        let floor = min_arranger_px_per_beat(content_w, bars_to_ticks(200));
        assert_eq!(
            floor,
            content_w / (200.0 * 4.0 * ARRANGER_ZOOM_OUT_HEADROOM)
        );
    }

    #[test]
    fn floor_never_tighter_than_the_default_view() {
        // Empty and short projects can still zoom out to the default 32 bars.
        for last_tick in [0, bars_to_ticks(2), bars_to_ticks(20)] {
            assert_eq!(
                min_arranger_px_per_beat(1274.0, last_tick),
                default_arranger_px_per_beat(1274.0)
            );
        }
    }

    #[test]
    fn floor_bottoms_out_at_the_hard_minimum() {
        assert_eq!(
            min_arranger_px_per_beat(1274.0, bars_to_ticks(5_000)),
            ARRANGER_MIN_PX_PER_BEAT
        );
    }

    #[test]
    fn anchor_prefers_pointer_then_visible_cursor_then_centre() {
        let (scroll_x, ppt, viewport_w) = (100.0, 0.5, 1000.0);
        // Pointer over the content: the tick under it.
        assert_eq!(
            zoom_anchor_tick(Some(300.0), 50.0, scroll_x, ppt, viewport_w),
            800.0
        );
        // Pointer off the content (track header, or a key zoom): the cursor,
        // which sits at px 400 — on screen.
        assert_eq!(
            zoom_anchor_tick(Some(-20.0), 1000.0, scroll_x, ppt, viewport_w),
            1000.0
        );
        assert_eq!(
            zoom_anchor_tick(None, 1000.0, scroll_x, ppt, viewport_w),
            1000.0
        );
        // Cursor scrolled away (px -50): the viewport centre.
        assert_eq!(
            zoom_anchor_tick(None, 100.0, scroll_x, ppt, viewport_w),
            1200.0
        );
    }

    #[test]
    fn fit_frames_the_span_centred_with_equal_margins() {
        let content_w = 1200.0;
        let px_per_beat = fit_px_per_beat(bars_to_ticks(8), content_w, ARRANGER_MIN_PX_PER_BEAT);
        let ppt = px_per_beat_to_ppt(px_per_beat);
        let (start, end) = (bars_to_ticks(20), bars_to_ticks(28));
        let scroll_x = centred_scroll_x(start, end, ppt, content_w);
        let left_px = start as f32 * ppt - scroll_x;
        let right_px = end as f32 * ppt - scroll_x;
        // Equal margins, each exactly `ZOOM_FIT_MARGIN` of the width.
        let margin_px = content_w * ZOOM_FIT_MARGIN;
        assert!(
            (left_px - margin_px).abs() < 1e-2,
            "{left_px} vs {margin_px}"
        );
        assert!(
            ((content_w - right_px) - margin_px).abs() < 1e-2,
            "{right_px} vs {margin_px}"
        );
    }

    #[test]
    fn fit_is_clamped_to_the_zoom_range() {
        // A single beat on a 6000pt-wide view would want ~5520 px/beat:
        // capped at the max.
        assert_eq!(
            fit_px_per_beat(beats_to_ticks(1.0), 6000.0, 2.0),
            MAX_PX_PER_BEAT
        );
        // 5000 bars would want far below the floor: held at the floor.
        assert_eq!(fit_px_per_beat(bars_to_ticks(5_000), 1200.0, 3.0), 3.0);
        // A degenerate span never divides by zero.
        assert!(fit_px_per_beat(0, 1200.0, 2.0).is_finite());
    }

    #[test]
    fn z_frames_only_a_real_time_selection() {
        let rect = |start, end| TimeSelectionRect {
            start,
            end,
            track_start: 0,
            track_end: 2,
        };
        assert_eq!(zoom_to_fit_range(Some(rect(960, 7680))), Some((960, 7680)));
        // Regression: no selection used to fall back to the whole arrangement.
        assert_eq!(zoom_to_fit_range(None), None);
        // A purely vertical marquee spans tracks but no time.
        assert_eq!(zoom_to_fit_range(Some(rect(960, 960))), None);
    }

    #[test]
    fn every_selection_length_frames_the_same_screen_span() {
        // Regression, three times over: at the old 160 px/beat max a one-bar
        // selection filled only 30–50% of the width; then a fixed one-*beat*
        // margin made the framed width swing with the selection length; then
        // the 960 px/beat max still capped 1–2 beat selections on wide
        // displays (this test started at 4 beats, which is how that slipped
        // through). Every length from one beat must land on the same middle
        // span of screen, on content widths up to 4000pt.
        let expected_w = |content_w: f32| content_w * (1.0 - 2.0 * ZOOM_FIT_MARGIN);
        for content_w in [
            1000.0, 1274.0, 1700.0, 2100.0, 2400.0, 2880.0, 3400.0, 4000.0,
        ] {
            for beats in [1.0, 2.0, 3.0, 4.0, 8.0, 16.0, 64.0, 256.0] {
                let span_ticks = (beats * beats_to_ticks(1.0) as f32) as i32;
                let framed_px = beats * fit_px_per_beat(span_ticks, content_w, 2.0);
                assert!(
                    (framed_px - expected_w(content_w)).abs() < 1e-2,
                    "content_w {content_w}, {beats} beats: framed {framed_px}"
                );
            }
        }
    }

    /// Left and right screen margins of `[start, end)` under a framing.
    fn margins(start: i32, end: i32, content_w: f32, framing: Framing) -> (f32, f32) {
        let ppt = px_per_beat_to_ppt(framing.px_per_beat);
        let left = start as f32 * ppt - framing.scroll_x;
        let right = content_w - (end as f32 * ppt - framing.scroll_x);
        (left, right)
    }

    #[test]
    fn z_frames_the_same_width_wherever_the_selection_sits() {
        // Regression: the fit maths was uniform, but the clamps after it
        // skewed the framing — a long selection in a short project was
        // squeezed by the content-relative zoom-out floor, and one past the
        // last clip was clamped off-centre by the right-hand limit. Every
        // selection gets the same on-screen width; away from bar 1 it is also
        // centred with equal margins, while at bar 1 it sits flush left (the
        // view can't scroll before bar 1 — by decision, no lead-in).
        let content_w = 1700.0;
        let bar = bars_to_ticks(1);
        let expected = content_w * ZOOM_FIT_MARGIN;
        let framed_w = content_w - 2.0 * expected;
        let clips_end = 16 * bar;
        for (start, end, at_bar_one) in [
            (0, 4 * bar, true),          // at bar 1
            (0, bar, true),              // a single bar at bar 1
            (8 * bar, 12 * bar, false),  // mid-arrangement
            (0, 32 * bar, true),         // longer than the (short) arrangement
            (20 * bar, 28 * bar, false), // entirely past the last clip
        ] {
            // The selection counts toward the content extent, as in
            // `arranger_last_content_tick`.
            let last_content_tick = clips_end.max(end);
            let framing = fit_framing(start, end, content_w, last_content_tick);
            let (left, right) = margins(start, end, content_w, framing);
            let width = content_w - left - right;
            assert!(
                (width - framed_w).abs() < 0.05,
                "[{start}, {end}): framed width {width}, expected {framed_w}"
            );
            if at_bar_one {
                assert!(
                    left.abs() < 0.05,
                    "[{start}, {end}): not flush left ({left})"
                );
            } else {
                assert!(
                    (left - expected).abs() < 0.05 && (right - expected).abs() < 0.05,
                    "[{start}, {end}): margins {left} / {right}, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn clip_zoom_in_leaves_home_and_is_capped() {
        let home = 40.0;
        assert_eq!(zoomed_clip_state(home, 1.25, home), Some(50.0));
        assert_eq!(
            zoomed_clip_state(MAX_PX_PER_BEAT * 0.9, 2.0, home),
            Some(MAX_PX_PER_BEAT)
        );
    }

    #[test]
    fn a_short_clip_never_zooms_out_past_home() {
        // Home is the whole clip: zooming out from it stays home, and a big
        // zoom-out from a zoom lands home rather than below it.
        let home = 40.0;
        assert_eq!(zoomed_clip_state(home, 0.8, home), None);
        assert_eq!(zoomed_clip_state(100.0, 0.1, home), None);
        assert_eq!(zoomed_clip_state(100.0, 0.8, home), Some(80.0));
    }

    #[test]
    fn a_zoom_below_home_comes_home_on_any_key_zoom() {
        // A window widened while zoomed leaves the scale below home
        // (`sync_clip_scroll` floors at nothing). Zooming in across home
        // lands on it rather than stepping past; zooming out stays home.
        let home = 40.0;
        assert_eq!(zoomed_clip_state(30.0, 2.0, home), None);
        assert_eq!(zoomed_clip_state(30.0, 1.25, home), None);
        assert_eq!(zoomed_clip_state(30.0, 0.8, home), None);
    }

    #[test]
    fn clip_zoom_key_round_trip_returns_home_despite_float_drift() {
        // Regression guard: `× 1.25` then `÷ 1.25` must land home again, not
        // strand the view a hair off it.
        for home in [7.3, 23.437_5, 40.0] {
            let mut state = zoomed_clip_state(home, ZOOM_KEY_STEP, home);
            for _ in 0..5 {
                state = state.and_then(|ppb| zoomed_clip_state(ppb, ZOOM_KEY_STEP, home));
            }
            for _ in 0..6 {
                state = state.and_then(|ppb| zoomed_clip_state(ppb, 1.0 / ZOOM_KEY_STEP, home));
            }
            assert_eq!(state, None, "home {home}");
        }
    }

    #[test]
    fn a_clip_that_shrank_under_the_zoom_is_floored_to_the_whole_clip() {
        // The clip got shorter (⌥-) while zoomed in: its whole-clip scale rose
        // to or past the zoomed one, and the view comes home.
        assert_eq!(clip_zoom_state(40.0, 50.0, 50.0), None);
        assert_eq!(clip_zoom_state(50.0, 50.0, 50.0), None);
        assert_eq!(clip_zoom_state(60.0, 50.0, 50.0), Some(60.0));
        // With no floor (`sync_clip_scroll`), a scale below home is kept.
        assert_eq!(clip_zoom_state(10.0, 0.0, 50.0), Some(10.0));
    }

    #[test]
    fn clip_z_frames_only_a_real_note_selection() {
        // Regression guard, as in the arranger: no selection, no fallback.
        assert_eq!(clip_zoom_to_fit_range(std::iter::empty()), None);
    }

    #[test]
    fn clip_z_frames_the_hull_of_the_selected_notes() {
        let beat = beats_to_ticks(1.0);
        let notes = [(4 * beat, 5 * beat), (beat, 2 * beat), (2 * beat, 7 * beat)];
        assert_eq!(
            clip_zoom_to_fit_range(notes.into_iter()),
            Some((beat, 7 * beat))
        );
    }

    #[test]
    fn a_short_selection_is_widened_to_one_beat_around_its_centre() {
        let beat = beats_to_ticks(1.0);
        let hi_hat = (2 * beat, 2 * beat + beat / 8);
        let (start, end) = clip_zoom_to_fit_range(std::iter::once(hi_hat)).unwrap();
        assert_eq!(end - start, beat);
        // Centred on the note, to within integer rounding.
        assert!((start + end - (hi_hat.0 + hi_hat.1)).abs() <= 1);
    }

    #[test]
    fn clip_z_centres_the_selection_with_the_arranger_margins() {
        // An 8-bar clip at bar 5 on a 2000pt view, one bar selected mid-clip.
        let content_w = 2000.0;
        let bar = bars_to_ticks(1);
        let region = (4 * bar, 12 * bar);
        let fit = content_w / (8.0 * 4.0);
        let (start, end) = (7 * bar, 8 * bar);
        let framing = clip_fit_framing((start, end), region, content_w, fit);
        let (left, right) = margins(start, end, content_w, framing);
        let expected = content_w * ZOOM_FIT_MARGIN;
        assert!(
            (left - expected).abs() < 0.05 && (right - expected).abs() < 0.05,
            "margins {left} / {right}, expected {expected}"
        );
    }

    #[test]
    fn clip_z_stays_inside_the_clip() {
        // A selection at the clip start can't be centred without showing
        // time before the clip: it sits flush left instead.
        let content_w = 2000.0;
        let bar = bars_to_ticks(1);
        let region = (4 * bar, 12 * bar);
        let fit = content_w / (8.0 * 4.0);
        let framing = clip_fit_framing((4 * bar, 5 * bar), region, content_w, fit);
        let (left, _) = margins(4 * bar, 5 * bar, content_w, framing);
        assert!(left.abs() < 0.05, "not flush left: {left}");
    }

    #[test]
    fn clip_z_on_the_whole_clip_lands_on_the_whole_clip() {
        let content_w = 2000.0;
        let bar = bars_to_ticks(1);
        let region = (0, 8 * bar);
        let fit = content_w / (8.0 * 4.0);
        let framing = clip_fit_framing(region, region, content_w, fit);
        assert_eq!(
            clip_zoom_state(framing.px_per_beat, fit, fit),
            None,
            "{framing:?}"
        );
    }

    #[test]
    fn clip_z_survives_a_fit_scale_above_the_cap() {
        // Regression guard: a one-beat clip on a 4000pt view fits at 4000
        // px/beat, above `MAX_PX_PER_BEAT`; an uncapped floor made
        // `f32::clamp`'s range inverted, which panics.
        let beat = beats_to_ticks(1.0);
        let framing = clip_fit_framing((0, beat), (0, beat), 4000.0, 4000.0);
        assert_eq!(framing.px_per_beat, MAX_PX_PER_BEAT);
    }

    /// Regression: Enter's tempo fit on a long capture rescaled the clip's
    /// event ticks while the clip view kept its old framing, leaving the
    /// window far off screen. Mapped through the retime, every note keeps
    /// its on-screen x.
    #[test]
    fn a_retimed_framing_keeps_every_note_on_its_pixel() {
        let retime = EventSpaceRetime {
            scale: 1.125,
            offset: 1234.0,
        };
        let before = Framing {
            px_per_beat: 40.0,
            scroll_x: bars_to_ticks(29) as f32 * px_per_beat_to_ppt(40.0),
        };
        let after = retimed_framing(before, retime);
        let (old_ppt, new_ppt) = (
            px_per_beat_to_ppt(before.px_per_beat),
            px_per_beat_to_ppt(after.px_per_beat),
        );

        for tick in [
            bars_to_ticks(29) + 7,
            bars_to_ticks(30) + 250,
            bars_to_ticks(31),
        ] {
            let old_x = tick as f32 * old_ppt - before.scroll_x;
            let new_x = retime.map_tick(tick) as f32 * new_ppt - after.scroll_x;
            assert!((old_x - new_x).abs() < 0.05, "{tick}: {old_x} vs {new_x}");
        }
    }
}
