# Arranger Overview

**Status: drafted 2026-11-25, decisions 1–3 of the open points agreed with the user the same day; not built.** A thin strip at the top of the arranger shows the whole arrangement in miniature, with a box around the part on screen. Drag the box sideways to scroll and up or down to zoom; click elsewhere on the strip to jump the view there. It moves the view only, never the cursor. Today the arranger is navigated by the wheel, the zoom keys and cursor-follow paging, which gets tedious once an arrangement runs past five minutes, and the performance lane (`110`, `520`) makes long arrangements quick to produce. The interaction is Ableton's overview (also in Cubase and Bitwig, and in code editors' minimaps). Only its behaviour is taken; it is drawn in Stev's own theme.

## Background

- **The arranger's view state** is two numbers: `RenderState::arranger_scroll_x` and `arranger_px_per_beat` (`030` § Arranger Navigation). The scroll is clamped to `0 ..= arranger_last_content_tick() × ppt` (the latest clip end, the cursor, the region end or the marquee's end). Zoom goes through `zoom_arranger_by(factor, pointer_x)`, which keeps an anchor tick on its pixel, applies the content-relative zoom-out floor, clears the `Z` history and suspends cursor-follow. A scroll suspends follow too, and the next cursor *change* re-latches it and pages the view back to the cursor.
- **What the view already knows**: `render.clip_shapes` (track, start, end, muted) mirrors every clip; `track_color(track_idx)` gives its colour; the region and the playhead are read from the atomics every frame (`region_bounds_for_render`, the playhead in `overlays.rs`). The overview needs nothing new from the sequencer.
- **The arranger pane's vertical layout** (`arranger_layout`, `rendering/mod.rs`): the pane's top, then the `TIMELINE_H` (30px) strip, the `PERFORMANCE_LANE_H` lane, then the track lanes. The clip view's pane is its own `TIMELINE_H` strip and the piano roll.

## The design

**1. A strip of its own above the timeline, in the arranger pane only.** `OVERVIEW_H` (18px, tuned by hand) at the top of the arranger pane, above the timeline strip, across the content area (`content_x` to `content_right_x`, the timeline's width). The track-column panel to its left is filled up from the overview's top, so the column still reads as one panel. Its background is the timeline strip's, `BG_TIMELINE`, with a `grid_seam_color()` groove between the two. It is always shown in the arranger, docked or alone, also when the whole song fits on screen: a strip that appears and disappears would move every lane under the pointer. No toggle. The clip view has none (it has its home framing, `030` § Clip-view zoom). The lanes give up its 18px.

**2. It maps the whole arrangement across its width.** The span is `0 .. overview_end`, where `overview_end` is the larger of `arranger_last_content_tick() × ARRANGER_ZOOM_OUT_HEADROOM` (1.25, the zoom floor's headroom, so the last clip doesn't sit against the right edge) and the tick at the view's right edge (so the box always fits). A pure `overview_span_end(last_content_tick, view_end_tick)` decides it, and `overview_tick_at` / `overview_x_of` map between ticks and strip x. While a drag is held the span is **latched at the press** and re-fit on release, so the mapping never moves under the hand (the view's end grows while you zoom out or scroll right, and a live re-fit would make the box run from the pointer).

**3. What it draws.** Back to front:
- **Clips**: one filled rect per `ClipShape` in its track colour, dimmed when muted as in the lanes. No notes, no header. The strip's height is split evenly between the tracks (`overview_row` pure, never under 1px: with many tracks rows touch or share a pixel, which is fine at this size). A clip narrower than a pixel is drawn 1px wide, so a short clip far out doesn't vanish.
- **The loop band**: the region as a translucent `theme::region()` fill over the strip's full height, at `loop_dim` like the timeline's band (dimmed while looping is off or the performance lane is armed). No edge lines and no handles: it is moved in the timeline (`580`).
- **The playhead**: a 1px `accent()` line while the transport runs, as in the lanes.
- **The view box**: a 1px `fg_dim()` outline from the view's left tick to its right tick, with a faint `fg` wash inside so the visible part reads lighter. Plain corners, not Ableton's rounded bracket. At least `OVERVIEW_MIN_BOX_PX` (6px) wide, centred on the view, so it can still be grabbed zoomed far in.

**4. The cursor is not drawn** and never moved (agreed with the user 2026-11-25). The overview is a view control. A click or drag moves `arranger_scroll_x` and `arranger_px_per_beat` and nothing else: no cursor, no marquee, no selection, no playhead, no focus change. Like every scroll it suspends cursor-follow, so the view stays where it was put even with the cursor off screen; the next cursor move pages back to it as today.

**5. Press and horizontal drag: scroll.** A press anywhere on the strip starts `gesture.overview_drag` (an `OverviewDrag { grab_offset_ticks, last_y, span_end }`), taken before every other arranger press, like the loop band's.
- **Inside the box**: the press records the pointer's tick offset from the view's left tick (the clip move's `grab_offset`), so the box stays under the hand instead of jumping.
- **Outside the box**: the view jumps at once so the box is centred on the pointer (`overview_centred_scroll`), clamped as every scroll is, and the drag continues from there with the pointer at the box's centre. So a click elsewhere is "show me that part", and a press-and-drag outside the box scrubs the view without first aiming at the box.

Each move sets `arranger_scroll_x` from `(pointer_tick − grab_offset) × ppt`, through the existing clamp (`arranger_max_scroll_x`). No threshold: the box follows the hand from the first pixel. The view moves continuously, not by grid steps, as the wheel does.

**6. Vertical drag: zoom**, the same zoom as `+`/`-` and ⌘-wheel (agreed with the user 2026-11-25). Each move's vertical delta becomes a factor `2^(dy / OVERVIEW_ZOOM_PX_PER_DOUBLING)` (pure `overview_zoom_factor`, 60px a doubling as a starting value), **drag down zooms in**, up zooms out (Ableton's direction). It goes through the zoom path (`zoom_arranger_by`'s body, given an anchor *tick* instead of a pointer x: a small split into `zoom_arranger_about(factor, anchor_tick)`, which `zoom_arranger_by` then calls), so the limits, the content-relative floor, the cleared `Z` history and the suspended follow all come with it. The anchor is the tick under the pointer in the overview, kept on its pixel in the lanes, so the box narrows or widens around the hand. A diagonal drag does both in the same frame: the zoom first, then the scroll from the new scale, so the anchor stays under the pointer in both.

**7. Pointer and the rest of the input.** Over the strip the pointer is `Grab`, held `Grabbing` (`gesture.overview_hover`, set beside the other cursors in `update()`). No hover line over it (`update_hover_cursor_tick` gates on the lanes and the timeline). A wheel or trackpad scroll over it acts as over the timeline strip. A double-click is two clicks (no zoom-to-fit; `Z` does that over a marquee). No keys. The drag ends on release, or is cleared with the other drags on `ProjectLoaded`, `ClipEntered` and the other view changes.

**8. View state only.** Not saved, not undoable, not part of the project fingerprint (scroll and zoom aren't), so it never marks the project unsaved. An audio export doesn't block it: it moves nothing the transport owns.

## What it does not do

- **No vertical navigation of the tracks.** The strip squeezes every track into its height; the lanes keep their own vertical scroll. Clicking a track's row in the overview doesn't scroll the lanes to it (a possible follow-up if it is missed in use).
- **No editing** in the overview: clips can't be moved, the loop can't be dragged there.
- **No markers, cursor triangle or marquee** drawn in it.
- **No toggle or resizing** of the strip.
- **Nothing in the clip view.**

## Open questions (hand-test decides)

1. The height (18px) and whether many tracks still read at it.
2. The zoom sensitivity (60px a doubling) and the direction (down zooms in).
3. Whether the outside-the-box press should jump at once (as written) or only on release, so a press-and-drag outside could also be read as a new framing.
4. Whether the cursor *should* be drawn as a 1px `fg_dim` tick, to find your way back to it. Left out to keep the strip quiet.

## Docs and tests the build owes

- `030`: § Timeline gets the overview strip in the arranger's layout (and the panel corner), § Arranger Navigation a bullet on the overview (span, box, the drag, view-only), § Rendering Performance a line on its per-frame cost. `020`: the gesture with the other arranger presses. `010` and `FIXED` in `view/display/settings/keyboard_tab.rs`: the mouse row. `CHANGELOG.md` `[Unreleased]`: an Added line.
- The layout change is the risky part: the arranger's timeline strip no longer starts at the pane's top. Every `pane_rect().min.y + TIMELINE_H` and `track_area_top() + TIMELINE_H` in the arranger (timeline, overlays, input, the loop band's hit test) has to go through one `Display::timeline_top()` (the pane's top plus `OVERVIEW_H` in the arranger, plus 0 in the clip view), so they can't drift apart. `ArrangerLayout` gains `overview_top`.
- Tests: `overview_span_end` (content plus headroom, the view's end when it is past it, an empty project), `overview_tick_at` / `overview_x_of` round-trip, the box (its ticks, the minimum width centred), `overview_centred_scroll` (centred, clamped at bar 1 and at the right clamp), the drag scroll with a grab offset, `overview_zoom_factor` (0 px is 1×, down above 1, symmetric up and down), `zoom_arranger_about` keeping its anchor tick on its pixel, and the layout: the timeline strip and the lanes moved down by `OVERVIEW_H` in the arranger and not in the clip view.
- To check by hand: a 5-minute and a 15-minute project, zoomed in and out; the drag inside and outside the box, sideways, vertical and diagonal; the cursor left where it was, then an arrow key paging back to it; the loop band and the playhead running; many tracks; docked and arranger alone; the clip view unchanged.

## Phases

One phase: the layout split, the drawing, the drag, the docs and the tests.
