# Docked clip panel (design brief, phases 1–4 implemented)

This is the design brief for turning the clip view (piano roll) from a
full-window view you enter and leave into a **panel** that can dock below
the arranger, Ableton-style (Live's detail view). The panel always shows the
clip under the arranger cursor on the selected track. The clip view as it
stands today is documented in `020-views-and-state.md` § Views,
`030-ui-design.md` and `200-clip-view-zoom.md`. Read those first.

**Status:** brief written 2026-09-24, with every decision below settled with
the user the same day. **Phase 1 implemented 2026-09-24** on
`feature/docked-clip-panel` (`view/display/pane.rs`, the split scroll in
`render_state.rs`, the active-pane helpers in `view/display/mod.rs`, and
`draw_pane` in `rendering/mod.rs`); as-built in `030-ui-design.md` § Panes.
**Phase 2 implemented 2026-09-24** on the same branch; as-built in
`020-views-and-state.md` § Views (`Clip`), `040` and `100`. **Phase 3
implemented 2026-09-24** on the same branch; as-built in `030` § Panes, `010`
and `020` § Views. **Phase 4 implemented 2026-09-24** on the same
branch; as-built in `030` § Clip-view zoom and `010`. All four phases are
done.

Phase-1 notes:

- **`ClipExited` still zeroes the arranger scroll.** Before the split, the
  shared offset was reset on exit and `sync_arranger_scroll` paged it back
  out to the restored cursor. Phase 1 keeps that, so nothing moves. Phase 2
  (no stash) or phase 3 (docked) should drop it, because a docked arranger
  must not jump when the panel hides.
- **The per-frame syncs and the zoom/scroll dispatchers run inside
  `in_pane`**, so they use their own pane's geometry whatever has the focus.
  The other clip-only entry points (`scroll_to_fit_clip_region`, now
  `scroll_clip_to_start`,
  `init_pending_view_anchor`, called from `ClipEntered`/`EventsUpdated`)
  still rely on the focus being the clip view. Phase 3 has to scope them too.
- **Input and key routing are untouched.** Hit-tests still gate on
  `view_state()`, which is phase 3's `pane_at`.

Phase-2 notes:

- **The clip view's playhead is the transport's, mapped into the clip**
  (`Display::lead_clip_playback_tick`, pure `pane::clip_event_tick_at`), and
  it is hidden while playback is outside the clip. The clip's own
  `playback_tick` counter only advances while the transport plays *through*
  the clip. With the loop no longer on the clip it stalled at the clip end.
  `PendingClip` still reads the clip's counter, since the transport loops on
  the pending clip there.
- **One capture commit.** The clip-view variant (anchored on the cursor's
  phase within the looping clip) and its `InsertFromClipView` target were
  deleted. Both views use the arranger's, renamed from
  `commit_events_to_clip` to plain `commit_events_to_clip`. The
  variant's commit-never-changes-the-selection test pair moved over.
- **Two commit bugs surfaced in testing, both fixed in this phase.**
  - *A left-trimmed clip took no notes.* The insert placed them at the
    cursor's offset from the clip start, forgetting the clip's region
    start, so they landed before the region. This predates the phase.
  - *A clip outside the loop took no notes.* The capture clock agrees with
    the cursor only in phase, and a linear take's window starts at the raw
    cursor. The commit now moves the cursor into the clock's numbering
    first (`capture_anchor_tick`, from an offset recorded with each input
    note). The flaw was old, but it was hard to reach while the clip view
    looped the transport on its clip.
  - See `100-running-capture.md`.
- **The phase-preserving region update went with the rescale's side
  effect.** `RescaleSelectedClipTempo` was its only user
  (`TransportCommand::SetRegionPreserveNormalizedPlaybackPhase`,
  `Transport::set_region_preserve_normalized_playback_phase`, deleted).
- **The pending phrase stashes and restores from both views** now, since
  the clip view no longer has a clip region to hand back.
- **The lead-change rule lives in the two shared selection workflows**
  (`select_clip_workflow`, `clear_clip_selection_workflow` →
  `change_lead_clip`), so every path that re-selects the lead clip (cursor
  moves, track changes, split/delete/move edits, pending, uncommit) announces
  it. `ClipExited` no longer zeroes the arranger scroll.
- **Handler tests.** `core/event_handlers/selection.rs` gained the first
  `EventHandlers` test harness (unbounded channels, a bare `Sequencer`), which
  tests a lead change, a move within the lead clip, and moving off every
  clip.

Phase-3 notes:

- **Clicks carry their pane.** `MouseClickedTicks` gained a `pane`, so the
  handler routes a click by where it landed. A click in the unfocused pane
  sends `FocusPane` first, on the same channel, but the handler's
  `view_state()` read can't count on the sequencer having applied it yet.
- **Docked is the startup layout** (user's choice, 2026-09-24,
  `ClipPanel::new`): shown and docked, with the arranger holding the
  keyboard. That made a gap visible. `ProjectLoaded` drops the view's lead
  clip, and a load or new project didn't always re-announce one (the track-0
  select is a no-op when track 0 is already selected), so the panel said
  "no clip" until the cursor moved. `announce_lead_clip_after_load`
  (`project.rs`) now always announces it.
- **The frozen per-frame values are the clip pane's alone**
  (`pane::clip_pane_frame_value`). Docked, the arranger drew the clip pane's
  region snapshot (the lead clip's region, in its event ticks) as the loop
  region. `⌘L` set the loop, but the arranger kept showing the clip's region
  wherever the cursor sat on a clip (found in testing). Scroll, cursor and
  playback were already pane-guarded; the region wasn't.
- **The handler doesn't re-check the focus for arranger gestures.** A
  clip-header press with the docked clip view focused sends `FocusPane`,
  then `SelectClipSpan`, and the handler's `view_state() == Arranger` guard
  could read the old focus and drop the press. The guards on
  `SetCursorTick`, `SelectClipSpan`, `MoveClip`, `MoveRange` and the track
  mix/mute/solo events are gone. The view only produces them from the
  arranger pane, or from a key with the arranger focused.
- **`Shift+Tab` with a docked arranger focused is the view's.** The panel
  is already showing, so it just hides. Every other `Shift+Tab` still goes
  through `handle_shift_tab` (`EnterClip`/`ExitClip`), and
  `ClipEntered`/`ClipExited` show or hide the panel.
- **The per-frame syncs and the scroll/zoom gates key off visibility,** not
  focus: the arranger keeps paging with the cursor while the clip view has
  the keyboard, and the clip pane keeps its fit while the arranger has it.
- **`EventsUpdated`'s refit gate became layout-based**
  (`events_updated_refits_clip_canvas(layout, pending, zoomed)`), since its
  old "never in the arranger" case guarded the shared scroll offset that
  phase 1 split. Phase 4 then removed the refit altogether (see below).
- **Not done:** a draggable divider, per-clip zoom kept across lead
  changes, and anything for arranger lanes too short for the S/M buttons
  when docked (`track_header_rects` hides them below about 51px per lane).

**Superseded 2026-09-26 by `220-capture-without-pending-view.md`:** the clip view no longer caps its home framing at 8 bars, no longer pages to follow the clip cursor, and no longer re-fits: home is the clip's whole reach, frozen when the clip is opened, and the app never zooms or scrolls the clip view on the user's behalf. The phase-4 notes below describe what was built on 2026-09-24 and have been replaced; `030-ui-design.md` § Clip-view zoom has the current rules.

Phase-4 notes (decided with the user 2026-09-24: 8 bars on screen, follow the
cursor only, both panel sizes):

- **"Fit" became "home".** `clip_px_per_beat == None` is now the home scale
  (`clip_home_scale`): the whole clip across the width, or
  `CLIP_HOME_MAX_BARS` (8) bars for a longer clip. (Since 2026-09-26 "the
  whole clip" includes the bar of headroom after its end,
  `clip_view_pixels_per_tick`, so the end is always on screen at home; see
  `220-capture-without-pending-view.md`.) The zoom-out floor stays
  the whole clip (`clip_whole_px_per_beat`, formerly `clip_fit_px_per_beat`),
  so a long clip can still be seen whole with `-`; zooming back in lands home
  (`zoomed_clip_state` snaps a step that crosses it). `is_at_clip_fit` and
  `zoomed_clip_px_per_beat` gave way to `clip_zoom_state` and
  `zoomed_clip_state`.
- **One scroll rule at every scale.** `sync_clip_scroll` clamps to the clip
  and pages toward the cursor at home too, not only when zoomed. A clip that
  fits whole has a single offset in range, so short clips behave exactly as
  before. Panning works whenever the clip doesn't fit whole.
- **No refit on `EventsUpdated`.** With the per-frame sync keeping the view
  on the clip, the refit only did harm: it would jump a long clip back to its
  start on every note edit. `events_updated_refits_clip_canvas` and its tests
  are gone. `scroll_to_fit_clip_region` became `scroll_clip_to_start`, run on
  entry and on a lead-clip change.
- **Follows cursor edits only,** as in the arranger (`200`'s phase-1
  deviation): the playhead can run off-screen during playback.

---

## What changes, in one paragraph

Today `Arranger` and `Clip` are mutually exclusive. `Shift+Tab` enters the
selected clip. Entering loops the transport on that clip, stashes the
arranger cursor and region, and swaps the Display's cursor, region and
playhead atomics for the clip's own. Exiting restores all of it
(`enter_clip_workflow` / `exit_clip_workflow`, `ClipEntered` / `ClipExited`).

Under this brief, the clip view is a panel with two sizes:

- **maximized**, which is today's full-window look;
- **docked**, which is a band below the arranger, with both visible at once.

`⌘⌥E` toggles the size. `Shift+Tab` shows and hides the panel at whatever size
it has. The panel is only a view: it never touches the loop region. The
clip cursor is its own since 2026-09-25 (see "Two cursors" below).

## Decisions (2026-09-24)

- **`ViewState` means keyboard focus.** `Arranger` means the arranger has the
  keys. `Clip` means the clip pane has them. It stays the shared atomic that
  `input_handler.rs`'s `ClipContext` reads, so the key routing does not
  change. Visibility and size are view-local layout state (`ClipPanel`) that
  the sequencer never needs.
- **Focus follows the click.** A press in the other pane focuses it first,
  then the click is handled there. `Shift+Tab` showing the panel also focuses
  it; hiding it returns focus to the arranger. The focused pane gets a subtle
  accent edge (docked only, since maximized has one pane).
- **The loop region is never touched.** Showing and hiding no longer
  stashes and restores the region and cursor. The region side effects of clip
  edits (`NudgeClipRegion*`, `RescaleSelectedClipTempo`) go too. To loop a
  clip, you do what you do in the arranger: `⌥→` to select it, then `⌘L`.
- **Two cursors, mirrored one way** (2026-09-25; phase 2 shipped this as one
  cursor). An arranger cursor move resets the lead clip's cursor
  (`sync_clip_selection_to_cursor_workflow`, and `select_clip_span_workflow`
  for the header press), but a clip-view cursor move stays in the clip:
  `set_clip_cursor_workflow` pushes to the transport cursor only in
  `PendingClip`, which borrows the transport cursor. Entering the clip view
  and focusing its pane leave the clip cursor alone. The point is auditing an
  edit in context, as in Ableton: park the arranger cursor a few bars early,
  edit in the clip view, and `Space` replays from the arranger cursor while
  `⌥Space` (`SequencerCommand::PlayFromClipCursor` →
  `TransportCommand::PlayFromTick`) restarts from the clip cursor without
  moving it. A clip spans exactly one region length in the arrangement, so
  a clip cursor inside the window maps to a single arrangement tick. A
  cursor on material kept outside the window (it can go there since
  `220-capture-without-pending-view.md` phase 2) has no arrangement tick,
  so `⌥Space` plays from the clip's start instead
  (`selected_clip_play_from_tick`, `Clip::play_from_arrangement_tick`). The running capture commit still
  anchors on the arranger cursor, and the lead clip is still the clip under
  it.
- **The lead clip drives the panel.** The panel shows the clip under the
  cursor on the selected track (`sequencer.selected_clip()`, already kept in
  sync by `sync_clip_selection_to_cursor_workflow` / `apply_track_selection`).
  A new `UiEvent::LeadClipChanged { clip_view: Option<ClipView> }` tells the
  view. With no clip, the docked panel shows a quiet placeholder.
- **Changing the lead clip clears the event selection** (and republishes it,
  because today it goes stale). This is not a capture commit, so the
  commit-never-changes-selection rule is untouched.
- **Scale: fit, capped.** *(Superseded 2026-09-26 by `220`: no cap, no
  follow.)* A short clip fits the width, as today. A long one
  holds a minimum px/beat (about 8 bars on screen) and pages to follow the
  cursor. `+`/`-`/`Z`/`X` zoom as today (`200`), and the zoom resets per lead
  clip.
- **Docked height: a fixed fraction** of the lane area, about 40%. A
  draggable divider comes later.
- **Out of scope:** `PendingClip` keeps its full-window takeover and its own
  stash. Multi-clip editing driven by the arranger's time selection or
  marquee is also out of scope.

## Architecture

- **Panes.** `enum Pane { Arranger, Clip }`, with one rect per visible pane
  computed at the top of `ui()` (`PaneRects`). The coordinate helpers
  (`pixels_per_tick`, `content_origin_x`, `tick_to_screen_x`, `grid_tiers`,
  `track_area_top`/`_bottom`, `render_scroll_x`, …) read the **active pane**,
  `render.active_pane`, instead of branching on `view_state()`. `ui()` sets
  the active pane before each pane's draw pass. Pointer input sets it from the
  pane under the pointer, and keys from the focus.
  - An active-pane context rather than a `Pane` argument threaded through
    every helper: the helpers are called from hundreds of sites that all
    already sit inside one pane's pass, and the immediate-mode "current
    target" is the smaller, safer diff. `Display::in_pane(pane, f)` scopes a
    temporary switch.
- **Two scroll offsets.** `render.scroll_x` splits into `arranger_scroll_x`
  and `clip_scroll_x`. Each view's scroll code writes only its own.
- **Two sets of time atomics.** The Display keeps the transport
  cursor/region/playhead and, separately, the lead clip's, from its
  `ClipView`. The clip pane reads the latter. There is no more swapping on
  enter/exit.

## Phased plan

1. **Pane-parameterised geometry, no visible change.** `Pane`, `PaneRects`
   (one full-lane-area pane, chosen by `view_state()`), the active-pane
   context, split `scroll_x`, pane-based `track_area_*` and
   `content_origin_x`. Verify: nothing moves by a pixel.
2. **One cursor, no region stash, lead-clip tracking.** `LeadClipChanged`,
   the panel atomic set, drop the Display swap and the region stash/side
   effects, continuous clip-cursor sync, clear the selection on a lead change,
   and switch the capture commit to `commit_events_to_clip`.
   Still full-window.
3. **Docked layout + focus.** `ClipPanel`, `⌘⌥E` (tighten `⌘E` split's guard
   to `!alt` — today `⌘⌥E` splits), `Shift+Tab` preserving size, both panes
   drawn, `pane_at` pointer routing (`TimelineScroll`/`TimelineZoom` gain the
   pointer y), `FocusPane`, focus edge, empty placeholder.
4. **Capped fit + follow** for long clips in the panel.

## Must not regress

- The pixel-rounding invariant: one formula for every time-anchored x,
  **per pane**.
- `scroll_x` is written only by the scroll code of the pane it belongs to.
- `PendingClip`'s frozen canvas.
- Hit-testing uses the drawn geometry of the pane under the pointer.
- No locks on the render path. Panel state never leaves the view.
- Arranger lanes shrink when docked. `track_header_rects` hides S/M below
  about 51px per lane, so check at typical window heights.
