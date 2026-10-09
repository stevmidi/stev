//! The panes the lane area is split into: the arranger and the clip view
//! (piano roll), and the clip panel state that picks the split. Each visible
//! pane owns a rect, and the coordinate helpers on `Display`
//! (`pixels_per_tick`, `content_origin_x`, `track_area_top`, …) answer for
//! the **active** pane — the one being drawn, or hit-tested, or focused (see
//! `Display::active_pane`). Pure layout here, unit-tested; see
//! `archive/210-docked-clip-panel.md`.

use egui::Rect;

pub(crate) use crate::core::view_state::Pane;

/// Which view-local surface has the keyboard, on top of the pane focus
/// (`ViewState`): the focused pane itself, the arranger's track-header
/// column, or the browser panel. One owner, so focusing one takes the
/// keyboard from the other. See `020-views-and-state.md` § Views.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum KeyFocus {
    /// The focused pane — the arranger lanes or the clip view.
    #[default]
    Pane,
    /// The arranger's track-header column (a click on a header's empty
    /// area). Only effective in `Arranger`.
    TrackHeaders,
    /// The browser panel. Only while it is visible.
    Browser,
}

/// The clip panel's height: the whole lane area, or a band docked below the
/// arranger. `⌘⌥E` toggles it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PanelSize {
    /// The clip view fills the lane area — the full-window clip view the app
    /// always had.
    Maximized,
    /// The clip view is a band below the arranger, both visible at once.
    Docked,
}

/// The clip panel: whether it is shown (`Shift+Tab`) and at which size
/// (`⌘⌥E`). View-local layout state, never shared — the sequencer only needs
/// the keyboard focus (`ViewState`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct ClipPanel {
    /// Shown or hidden. Follows `ClipEntered` / `ClipExited`, and `Shift+Tab`
    /// in a docked arranger with the focus. Shown at startup.
    pub(super) visible: bool,
    /// The size it shows at — kept while hidden, so `Shift+Tab` brings it back
    /// the size it was. Docked at startup.
    pub(super) size: PanelSize,
}

impl ClipPanel {
    /// The startup layout: shown and docked below the arranger, with the
    /// arranger keeping the keyboard (the user's default, 2026-09-24).
    pub(super) fn new() -> Self {
        ClipPanel {
            visible: true,
            size: PanelSize::Docked,
        }
    }

    /// The layout this panel state asks for.
    pub(super) fn layout(self) -> PaneLayout {
        match (self.visible, self.size) {
            (false, _) => PaneLayout::ArrangerOnly,
            (true, PanelSize::Maximized) => PaneLayout::ClipOnly,
            (true, PanelSize::Docked) => PaneLayout::Docked,
        }
    }
}

/// Which panes the lane area shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PaneLayout {
    /// The arranger fills the lane area.
    ArrangerOnly,
    /// The clip view fills the lane area.
    ClipOnly,
    /// The arranger above, the clip view docked below it.
    Docked,
}

/// The share of the lane area the docked clip view takes.
const DOCKED_CLIP_FRACTION: f32 = 0.4;
/// The gap between the docked panes, where the split line is drawn.
pub(super) const PANE_GAP_Y: f32 = 6.0;

/// The rect of each visible pane, `None` for a hidden one.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct PaneRects {
    /// The arranger's rect, when shown.
    pub(super) arranger: Option<Rect>,
    /// The clip view's rect, when shown.
    pub(super) clip: Option<Rect>,
}

impl PaneRects {
    /// `pane`'s rect, `None` when it is hidden.
    pub(super) fn get(self, pane: Pane) -> Option<Rect> {
        match pane {
            Pane::Arranger => self.arranger,
            Pane::Clip => self.clip,
        }
    }

    /// The visible pane holding screen y `y`. A y in the gap between docked
    /// panes counts as the pane below; outside every pane (the header, the
    /// status bar), `None`.
    pub(super) fn pane_at_y(self, y: f32) -> Option<Pane> {
        if let Some(arranger) = self.arranger
            && y >= arranger.min.y
            && y < arranger.max.y
        {
            return Some(Pane::Arranger);
        }
        let clip = self.clip?;
        let top = self.arranger.map_or(clip.min.y, |arranger| arranger.max.y);
        (y >= top && y <= clip.max.y).then_some(Pane::Clip)
    }
}

/// Splits `lane_area` into pane rects for `layout`.
pub(super) fn pane_rects(lane_area: Rect, layout: PaneLayout) -> PaneRects {
    match layout {
        PaneLayout::ArrangerOnly => PaneRects {
            arranger: Some(lane_area),
            clip: None,
        },
        PaneLayout::ClipOnly => PaneRects {
            arranger: None,
            clip: Some(lane_area),
        },
        PaneLayout::Docked => {
            let clip_h = (lane_area.height() * DOCKED_CLIP_FRACTION).round();
            let clip_top = lane_area.max.y - clip_h;
            let mut arranger = lane_area;
            arranger.max.y = clip_top - PANE_GAP_Y;
            let mut clip = lane_area;
            clip.min.y = clip_top;
            PaneRects {
                arranger: Some(arranger),
                clip: Some(clip),
            }
        }
    }
}

/// A frozen per-frame value (`RenderState::clip_frame_*`) as `pane` may
/// use it: the snapshots are the clip pane's — taken in its own coordinates,
/// the lead clip's region and cursor in its event ticks — so the arranger
/// never gets one, even while it is drawn in the same frame beside the docked
/// clip view. The region snapshot once leaked into the arranger, which then
/// drew the lead clip's region in place of the loop region.
pub(super) fn clip_pane_frame_value<T>(pane: Pane, snapshot: Option<T>) -> Option<T> {
    snapshot.filter(|_| pane == Pane::Clip)
}

/// Maps the arrangement tick `tick` into a clip's event-tick space — the
/// clip spanning `clip_span` (arrangement ticks, end exclusive) and looping
/// its region `region` (event ticks) across it. `None` when `tick` is outside
/// the span or the region is empty. The view-side twin of
/// `Clip::event_tick_from_arrangement_tick`, for the clip view's playhead.
pub(super) fn clip_event_tick_at(
    tick: i32,
    clip_span: (i32, i32),
    region: (i32, i32),
) -> Option<i32> {
    let (clip_start, clip_end) = clip_span;
    let (region_start, region_end) = region;
    let region_len = region_end - region_start;
    if !(clip_start..clip_end).contains(&tick) || region_len <= 0 {
        return None;
    }
    Some(region_start + (tick - clip_start).rem_euclid(region_len))
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    use super::{
        ClipPanel, PANE_GAP_Y, Pane, PaneLayout, PanelSize, clip_event_tick_at,
        clip_pane_frame_value, pane_rects,
    };

    /// Regression: docked, the arranger drew the lead clip's region (its
    /// event-tick snapshot) as the loop region, so `⌘L` seemed to leave the
    /// loop where it was.
    #[test]
    fn only_the_clip_pane_gets_the_frozen_frame_values() {
        let snapshot = Some((0, 3840));
        assert_eq!(clip_pane_frame_value(Pane::Clip, snapshot), snapshot);
        assert_eq!(clip_pane_frame_value(Pane::Arranger, snapshot), None);
        assert_eq!(clip_pane_frame_value::<i32>(Pane::Clip, None), None);
    }

    #[test]
    fn docked_splits_the_lane_area_with_the_clip_view_below() {
        let rects = pane_rects(lane_area(), PaneLayout::Docked);
        let arranger = rects.get(Pane::Arranger).unwrap();
        let clip = rects.get(Pane::Clip).unwrap();

        assert_eq!(arranger.min, lane_area().min);
        assert_eq!(clip.max, lane_area().max);
        assert_eq!(clip.height(), (lane_area().height() * 0.4).round());
        assert_eq!(clip.min.y - arranger.max.y, PANE_GAP_Y);
        assert_eq!((arranger.width(), clip.width()), (1300.0, 1300.0));
    }

    #[test]
    fn pane_at_y_picks_the_pane_and_gives_the_gap_to_the_one_below() {
        let rects = pane_rects(lane_area(), PaneLayout::Docked);
        let arranger = rects.get(Pane::Arranger).unwrap();
        let clip = rects.get(Pane::Clip).unwrap();

        assert_eq!(rects.pane_at_y(arranger.min.y), Some(Pane::Arranger));
        assert_eq!(rects.pane_at_y(arranger.max.y + 1.0), Some(Pane::Clip));
        assert_eq!(rects.pane_at_y(clip.center().y), Some(Pane::Clip));
        assert_eq!(rects.pane_at_y(10.0), None, "the header");
        assert_eq!(rects.pane_at_y(clip.max.y + 5.0), None, "the status bar");

        let single = pane_rects(lane_area(), PaneLayout::ClipOnly);
        assert_eq!(single.pane_at_y(100.0), Some(Pane::Clip));
    }

    #[test]
    fn the_app_starts_with_the_panel_docked() {
        assert_eq!(ClipPanel::new().layout(), PaneLayout::Docked);
    }

    #[test]
    fn the_panel_keeps_its_size_while_hidden() {
        let mut panel = ClipPanel::new();
        panel.size = PanelSize::Maximized;
        assert_eq!(panel.layout(), PaneLayout::ClipOnly);
        panel.visible = false;
        assert_eq!(panel.layout(), PaneLayout::ArrangerOnly);
        panel.visible = true;
        assert_eq!(panel.layout(), PaneLayout::ClipOnly);
        panel.size = PanelSize::Docked;
        assert_eq!(panel.layout(), PaneLayout::Docked);
    }

    #[test]
    fn the_playhead_maps_into_the_clip_only_inside_its_span() {
        // A clip at 1000..3000 whose region is 200..1200 (looped twice).
        let span = (1000, 3000);
        let region = (200, 1200);
        assert_eq!(clip_event_tick_at(1000, span, region), Some(200));
        assert_eq!(clip_event_tick_at(1500, span, region), Some(700));
        // The second pass wraps back onto the region start.
        assert_eq!(clip_event_tick_at(2000, span, region), Some(200));
        assert_eq!(clip_event_tick_at(2999, span, region), Some(1199));
        // Outside the span: no playhead in the clip view.
        assert_eq!(clip_event_tick_at(999, span, region), None);
        assert_eq!(clip_event_tick_at(3000, span, region), None);
        // A degenerate region never divides by zero.
        assert_eq!(clip_event_tick_at(1500, span, (200, 200)), None);
    }

    fn lane_area() -> Rect {
        Rect::from_min_max(pos2(0.0, 46.0), pos2(1300.0, 800.0))
    }

    #[test]
    fn a_single_pane_takes_the_whole_lane_area() {
        let arranger = pane_rects(lane_area(), PaneLayout::ArrangerOnly);
        assert_eq!(arranger.get(Pane::Arranger), Some(lane_area()));
        assert_eq!(arranger.get(Pane::Clip), None);

        let clip = pane_rects(lane_area(), PaneLayout::ClipOnly);
        assert_eq!(clip.get(Pane::Clip), Some(lane_area()));
        assert_eq!(clip.get(Pane::Arranger), None);
    }
}
