# Clip view zoom (design brief, phases 1–3 implemented)

This is the design brief for horizontal zoom in the piano roll (`Clip` /
`ClipEdit`). It carries `190-arranger-zoom.md` phase 4 into the clip views.
Phases 1–3 are implemented (see Status); the rest of this brief is the
plan they were built from. Read `190` first: this brief reuses its
mechanisms (anchored zoom, `grid_tiers`, the `Z`/`X` fit and history) and
lists only what differs. The clip view as it stands today is documented in
`030-ui-design.md` (§ Grid Hierarchy, and the clip-view scale-anchor bullet)
and `020-views-and-state.md` (§ Views, § Event Marquee Selection).

**Status:** brief written 2026-09-24; all decisions settled the same day.
**Phase 1 implemented 2026-09-24** on `feature/clip-view-zoom` (user-tested
the same day); as-built summary in `030-ui-design.md` § Clip-view zoom.
**Phase 2 implemented 2026-09-24** on the same branch (user-tested, committed
with phase 1 as `8e11bff`); as-built in `030-ui-design.md` § Grid Hierarchy
and `020` § cursor placement. **Phase 3 implemented 2026-09-24** on the same
branch (user-tested the same day); as-built in `030-ui-design.md` §
Clip-view zoom. All three merged to `main` 2026-09-24. Phase 4 (optional
extras) not started.

*Later, 2026-09-24:* `ViewState::ClipEdit` has since been collapsed into
`Clip` plus the `SharedAtomics::has_event_selection` flag (`020` § Views).
Where this brief says `Clip`/`ClipEdit`, read `Clip`; where it says
`ClipEdit`, read "`Clip` with notes selected".

Phase-1 deviations from this brief, deliberate:

- **Follow tracks cursor edits, not the running playhead.** The brief said
  "the playhead while playing". But the arranger follows only the cursor (the
  cursor is never written during playback), and the two views should behave
  alike. A zoomed clip view therefore lets the playhead run off-screen, as the
  arranger does. Revisit together for both views if it annoys.
- **Fit mode re-fits every frame**, in `sync_clip_scroll`, not only on
  `ClipEntered` / `EventsUpdated`. The scale already followed the region every
  frame, but `scroll_x` was only re-fitted on those events, so a window resize
  or a region change between them could leave the view out of step. The
  `EventsUpdated` gate is kept anyway (fit mode only), as the brief asked.
- **A clip that shrinks under the zoom drops back to fit.** When `⌥-` makes
  the clip short enough that the zoomed scale would show all of it, the view
  re-enters fit mode rather than holding a scale that shows past the clip end.
  *Superseded 2026-09-26* (`220`): the per-frame re-judge no longer floors a
  zoom at the whole clip, so a scale an edit pinned stays put; `-` and pinch
  still floor.
- **The rebinding shares the branch.** Deleting Shift+`-` and moving the
  stretch to `⌥=`/`⌥-` touch `input_handler.rs`, `commands.rs` (a stale doc
  comment: the command never doubled or halved anything, it is ±1 bar),
  `input_poller.rs` (a comment), `010`, `020`, `040` and `050`. Stage those
  on their own if they should be a separate commit.
- **Renames:** `ARRANGER_MAX_PX_PER_BEAT` → `MAX_PX_PER_BEAT`,
  `ARRANGER_ZOOM_KEY_STEP` → `ZOOM_KEY_STEP`, `InputEvent::ArrangerScroll` /
  `ArrangerZoom` → `TimelineScroll` / `TimelineZoom`, and `followed_arranger_x` →
  `followed_scroll_x`, which now takes a `(min, max)` range. `ArrangerFraming`
  and `ARRANGER_ZOOM_FIT_MARGIN` wait for phase 3, where the clip views first
  use them.

Phase-2 deviations / findings:

- **The piano roll gets its own sub-beat threshold** (`GridSurface`). The
  brief assumed 16ths drop out at `MIN_SNAP_PX` (8 px). But the arranger's
  phase-2 tuning had since added a stricter `MIN_SUB_BEAT_PX` (16 px) for
  rungs finer than a beat, to keep the arranger's default view calm. Applied
  here, it would have dropped the 16ths from an 8-bar clip on a 1300pt window
  at the default view. So `grid_tiers` takes a surface: `Arranger` keeps 16
  px (renamed `ARRANGER_MIN_SUB_BEAT_PX`), and `PianoRoll` uses the ordinary 8
  px, which gives exactly the brief's `content_w / 128` bars. `PendingClip`
  uses the piano-roll surface too, so every piano-roll view runs on one rule
  and `GridTiers::clip_view()` is gone.
- **The keyboard step reuses `MoveCursorByGrid`** rather than a new input
  event. In `Clip`, plain `←`/`→` is resolved view-side to the snap, exactly
  as in the arranger, and the handler routes it to
  `NudgeClipCursorByGrid { step_ticks }` (formerly `(direction)`, which was a
  fixed 16th in the model). `Shift+←/→` in `Clip` stays a raw 16th, which is
  now the fine step on long clips where the snap is an 8th. `⌘←/→` in `Clip`
  falls through to the handler's 16th, as before.
- **Clip-cursor grid steps are relative to the clip start**, as they always
  were (`step_to_grid` on the clip-relative cursor), not to absolute bars.
  This only matters for a clip that doesn't start on the snap grid.

Phase-3 notes:

- **As briefed.** `Z` frames the selected notes' hull (at least one beat),
  `X` pops a second `ZoomHistory`, and a manual zoom clears it. Renamed with
  it: `ArrangerFraming` → `Framing` and `ARRANGER_ZOOM_FIT_MARGIN` →
  `ZOOM_FIT_MARGIN`, now shared by both views.
- **One trap the brief missed:** the arranger's `fit_px_per_beat` clamps to
  `floor..=MAX_PX_PER_BEAT`, and in the clip view the floor is the fit scale.
  A one-beat clip on a ≈ 4000pt screen fits *above* the cap, so an uncapped
  floor gives an inverted range, and `f32::clamp` panics on that.
  `clip_fit_framing` caps the floor, and a regression test covers it.
- **A restored framing re-clamps to the clip as it is now.** If the clip has
  grown since the framing was saved so that its scale is now at or below the
  fit scale, `X` lands on fit mode.

---

## The question that shaped it: does `Z` need a time selection?

No. The arranger's `Z` frames a tick span, and there that span comes from the
marquee (`time_selection`). The clip view has no time selection, but it has
the **event selection**: `Clip::event_selection`, set by click, marquee drag
and `⌘/Ctrl+A`, and model state already mirrored into the view. In the clip
view, `Z` frames the hull of the selected notes, from the earliest selected
note-on to the latest note-off. Logic's piano roll and Ableton's MIDI editor
both zoom to the selected notes this way.

It also gives "fit to content" for free: `⌘/Ctrl+A`, then `Z`. So, as in the
arranger, `Z` with nothing selected stays a no-op, and there is no fallback to
misfire (`190` phase-3 decisions). It also follows the gesture-consistency
rule: the same selection frames the same span, however the notes got selected.

A clip-view time selection (delete time, insert silence, loop to a range inside
a clip) is a separate feature. It is **not** a prerequisite for zoom. Its open
design question is how a time range sits alongside the note marquee, which
already owns press-drag in the note lanes. `010` notes that `Shift+L/R` with
no event selection is earmarked for it.

---

## Current architecture (what exists today)

- **Scale:** `clip_view_pixels_per_tick()` (`view/display/mod.rs`) is
  `content_w() / region_ticks`. Entering a clip sets the loop region to the
  clip (`enter_clip_workflow` → `send_set_region_transport_command`), so the
  whole clip always fills the content width. There is no scale state; the
  scale is a pure function of the region and the window width.
- **Scroll:** `scroll_to_fit_clip_region()` (`state/scroll.rs`) sets
  `render.scroll_x = region_start * ppt`. It runs on `ClipEntered` and on
  **every `EventsUpdated` in `Clip`/`ClipEdit`**
  (`events_updated_refits_clip_canvas`, `state/ui_events.rs`), and every note
  edit emits `EventsUpdated`. Nothing else writes `scroll_x` in the clip
  views: there is no follow, no wheel scroll, and no zoom.
- **Grid:** `GridTiers::clip_view()` (`view/display/grid.rs`) is a fixed
  bar / beat / 16th grid with a label on every bar, walked by the same
  role-based `draw_timeline` loop as the arranger.
- **Snap:** `cursor_grid_ticks()` returns a fixed 16th in `Clip`/`ClipEdit`.
  Keyboard steps are resolved in the model: `NudgeClipCursorByGrid` →
  `grid_snapped_selected_clip_cursor_tick`, the note nudge is a fixed 64th
  (`GRID_NUDGE_TICKS`), and fine nudge is `FINE_NUDGE_TICKS`.
- **Input:** `InputEvent::ArrangerScroll` / `ArrangerZoom` are consumed in
  every view and are no-ops outside the arranger (`scroll_arranger_by` /
  `zoom_arranger_by` guard on `ViewState::Arranger`). Plain `+`/`-` fall
  through to `handle_plus_minus_keys`, where they are **spare** in `Clip` and
  `ClipEdit`. **Shift is taken today** (phase 1 frees it, see *Decisions*):
  Shift+`+`/`-` is tempo rescale in `Clip`, and Shift+`-` clears the
  selection in `ClipEdit`. Bare `Z`/`X` are unbound
  outside the arranger.
- **`PendingClip`** has its own anchored canvas (`init_pending_view_anchor`,
  `scroll_to_pending_clip_region`, a guard-window cursor follow). It is frozen
  on purpose so captured events never move while the user adjusts the region.
- **Vertical:** `piano_roll_note_range` / `note_area_geom` fit the clip's
  notes plus half an octave, shrinking row height to avoid overflow. There is
  no vertical zoom or scroll.

---

## Design

### State: "fit" is a mode, not a number

```text
Display.render.clip_px_per_beat: Option<f32>
```

- `None` means **fit**: today's behaviour, bit-identical. The scale follows the
  region (`clip_view_pixels_per_tick`) and `scroll_to_fit_clip_region` keeps
  running on `EventsUpdated`. The name matches `arranger_px_per_beat`, but the
  meaning of `None` differs. In the arranger `None` means "not latched yet". Here it
  is a real, persistent state, so the doc comment must say so.
- `Some(ppb)` means **zoomed**: set by the first `+`/`-`/wheel/pinch/`Z`. From
  then on `EventsUpdated` must **not** refit. Otherwise every nudge,
  transpose or delete snaps the view back to the whole clip. This is the one
  real trap in this phase. Gate `events_updated_refits_clip_canvas` on the fit
  mode as well as the view, and extend its test.
- **Reset to `None` on every `ClipEntered` and `ClipExited`.** Unlike the
  arranger, where zoom is a preference kept across visits, the natural "home" of
  the piano roll is the whole clip, and clips differ wildly in length. A
  1-bar zoom that suits a drum loop is wrong for the 16-bar clip entered
  next. (DAWs vary here. Ableton keeps per-clip zoom, which would mean
  persisting per-clip view state; not worth it now.)
- Zooming back out to the fit scale returns to `None`, so fit is sticky again
  and later edits refit. Without this, "zoom in, zoom back out" would leave the
  view unexpectedly pinned.

`pixels_per_tick()` gains the `Some` branch for `is_clip_view()`. Everything
downstream (note shapes, velocity lane, hit-tests, `region_bound_x`,
`tick_to_screen_x`) already routes through it, so the one-formula
pixel-rounding invariant holds with no new formula.

### Limits and clamps

- **Zoom-out floor = the fit scale.** Zooming out past the whole clip would
  mostly add empty space. `-` at the floor lands exactly on fit (`None`).
  Notes kept outside the window (`220-capture-without-pending-view.md`) are
  reached by scrolling, not by zooming out; the floor stays the window so a
  clip opens the same with or without them.
- **Zoom-in cap = the arranger's max** (`ARRANGER_MAX_PX_PER_BEAT`, 3840
  px/beat, 4 px per tick). One maximum everywhere; rename the constant to
  `MAX_PX_PER_BEAT` when it becomes shared.
- **Scroll range = the clip's reach.** `scroll_x ∈ [reach_start·ppt,
  reach_end·ppt − content_w]`, where the reach is the window plus one bar of
  headroom after it (`CLIP_END_HEADROOM_BARS`), widened to any note further
  out (`reach_over` in `models/clip/cursor_region.rs`, the one rule the clip cursor's `Clip::reach` and the view's `clip_reach_of` share; since 2026-09-25/26, `220` — it was the
  window alone). The clip cursor is limited to the same span (`Clip::reach`). Re-clamp every frame against the *current*
  reach, so a clip that shrinks under a zoomed view never leaves it past the
  end. `Z` framing clamps within the reach too, so selected notes outside the
  window can be framed.

### Shared code: generalise, don't copy

`state/zoom.rs`'s pure helpers are already view-neutral maths. Reuse them from
both views instead of forking a clip copy:

- `zoomed_scroll_x`, `zoomed_px_per_beat` (the floor is a parameter),
  `zoom_anchor_tick` (pointer → visible cursor → centre): as-is.
- `fit_framing` / `fit_px_per_beat` / `centred_scroll_x`: parameterise the
  clamp bounds (the arranger's `last_content_tick` vs the clip's region).
- `ArrangerFraming` → `Framing`; `ZoomHistory` stays one type with **two
  instances** (`arranger_zoom_history`, `clip_zoom_history`), because an
  arranger framing means nothing inside a clip.
- The `+`/`-` step constant (`ARRANGER_ZOOM_KEY_STEP`, 1.25) and the fit
  margin (`ARRANGER_ZOOM_FIT_MARGIN`, 4% of content width a side) become
  shared too, so the two views behave alike.

The view-local entry points become view-dispatching:
`zoom_by(factor, pointer_x)`, `zoom_to_fit()`, `zoom_back()`,
`scroll_by(delta_x)`. Each branches on `Arranger` vs `Clip | ClipEdit` and
stays a no-op in `PendingClip` and the modals. Rename
`InputEvent::ArrangerScroll` / `ArrangerZoom` to `TimelineScroll` /
`TimelineZoom` in the same change, since the old names would lie.

### Scrolling and follow

- **Wheel / trackpad horizontal scroll pans** when zoomed and does nothing in
  fit mode (nothing to scroll to). It suspends follow, like
  `scroll_arranger_by`.
- **Follow pages toward the cursor** *(superseded 2026-09-26 by `220`: the
  clip view no longer follows the cursor or zooms or scrolls on its own)*
  (as built: cursor edits only, like the
  arranger — see the phase-1 deviations). Generalise `followed_arranger_x` (it
  takes the page step as a parameter already). Page by two structural-tier
  units, as in the arranger.
- A manual zoom or scroll suspends follow; the cursor moving re-latches it:
  the arranger's `arranger_follow_suspended` rule, as a second flag (or one
  flag reset on view change).

### Adaptive grid (the original phase 4)

- `GridTiers::clip_view()` is deleted; the clip views call
  `grid_tiers(ppt, sixteenth_straight_ticks())`. The 1/16 floor is Logic's
  division floor: zooming in never subdivides below a 16th. (1/32 is an easy
  later switch, and `grid_tiers` already supports it.)
- **Deliberate change at the default view:** in fit mode, a long clip
  currently draws every 16th regardless of spacing. 16ths drop out, and the
  snap coarsens to 8ths, once they are closer than `MIN_SNAP_PX` (8 px)
  (superseded 2026-09-26: the piano roll now runs at Ableton-Narrowest
  density, 6 px down to a 256th floor — see `030-ui-design.md` § Grid
  Hierarchy),
  i.e. for clips longer than `content_w / 128` bars: ≈ 10 bars at a 1300 pt
  content width, ≈ 16–24 bars at the 2100–3100 pt widths seen in practice.
  That is the adaptive-grid rule working, and every DAW does it, but it is
  visible on long clips, so call it out when this phase is tested. Shorter
  clips don't change.
- **Snap follows the grid:** `cursor_grid_ticks()` returns
  `grid_tiers().snap_ticks` for `Clip | ClipEdit`. Mouse placement inherits
  it. The keyboard cursor step (`NudgeClipCursorByGrid`) resolves its step in
  the model today, so it moves to the "view resolves the operand" pattern the
  arranger adopted in its phase 2 (`MoveCursorByGrid { step_ticks }`).
- **Note nudge stays a fixed 64th, and fine nudge stays
  `FINE_NUDGE_TICKS`.** These are editing resolutions, not navigation, and a
  nudge that changed size with the zoom would make `←` on a selected note
  jump an unpredictable distance. Revisit only if it annoys.

### Bindings

| key | `Clip` / `ClipEdit` | notes |
|---|---|---|
| `+` / `=`, `-` (plain or Shift, never ⌘/Ctrl) | zoom in / out ×1.25 around the cursor (centre if off-screen) | exactly the arranger's binding; Shift is free once the rebinding below lands |
| `⌥=` / `⌥-` | stretch the clip one bar longer / shorter (the old Shift+`+`/`-` tempo rescale) | **both** `Clip` and `ClipEdit` — a whole-clip op, the event selection is irrelevant. Logic's Option-drag = time-stretch. On macOS `⌥=` types `≠`; egui-winit falls back to the physical key (`logical_key.or(physical_key)`), so it arrives as `Key::Equals` + alt |
| ⌘/Ctrl+wheel, pinch | zoom around the pointer | same `zoom_delta()` path as the arranger |
| wheel / two-finger horizontal | pan when zoomed | no-op in fit mode |
| bare `Z` | frame the selected notes' span | no-op with no selection, so fit-all is `⌘/Ctrl+A`, then `Z` |
| bare `X` | step back through the `Z` history | cleared by any manual zoom, as in the arranger, and on `ClipEntered`/`ClipExited` |

**`Z` specifics:**

- The span is the hull of the selected `NoteOn`s' `[start, end)`, read from
  `event_shapes` with the selection the view already mirrors. No model
  round-trip is needed.
- **Minimum framed span: one beat.** A single 1/32 hi-hat would otherwise zoom
  to the cap and fill the screen with one note. One beat keeps the note's
  neighbourhood in view.
- Horizontal only. It uses the same 4%-of-width margin, centring and
  `fit_framing` clamps as the arranger, clamped to the clip instead of the
  arrangement.
- If the fitted scale is at or below the fit scale (the selection spans the
  whole clip), `Z` lands on fit (`None`). It still pushes to the history, so
  `X` works.

---

## Phased plan

1. **Variable scale, same look.** First the rebinding, as its own commit:
   delete Shift+`-` (clear selection), move the clip stretch from
   Shift+`+`/`-` in `Clip` to `⌥=`/`⌥-` in `Clip | ClipEdit`
   (`handle_plus_minus_keys`), and update `010` (the ClipEdit bullets and the
   `Shift+-` mentions) and `020`. Then `clip_px_per_beat`, the fit mode, the
   `EventsUpdated` refit gate, clamps, `+`/`-` and ⌘/Ctrl+wheel/pinch, pan,
   and follow. The shared-helper extraction from `zoom.rs` happens here.
   Verify: in fit mode nothing moves by a pixel (with the same `region_bound_x`
   sweep as `190` phase 1); a nudge while zoomed keeps the framing; `-`
   back to the floor re-enters fit.
2. **Adaptive grid.** `grid_tiers` in the clip views, `cursor_grid_ticks`
   follows the snap, and the clip cursor step is resolved in the view. Rewrite
   `030` § Grid Hierarchy (drop "the clip views' fixed grid"), `010`'s
   `+`/`-` line and the ClipEdit bullets.
3. **`Z` / `X` on the event selection**, with the history per visit.
4. **Optional, later:** beat sub-labels (`5.3`) for both views; a fixed-grid
   toggle (Ableton ⌘5); vertical zoom/scroll for the note lanes (this is where
   a vertical `Z`, fitting the selected pitch range, would belong);
   `PendingClip` zoom, which would have to respect its frozen-canvas rule.

## Decisions

- **Shift+`+`/`-` is freed (decided 2026-09-24).** Clearing the event
  selection becomes Esc-only (the Shift+`-` binding is deleted), and tempo
  rescale, which is really "stretch the clip ±1 bar"
  (`rescale_selected_clip_tempo`), moves to `⌥=` / `⌥-` in both `Clip` and
  `ClipEdit`. Clip-view zoom then takes `+`/`=`/`-` plain or with Shift,
  exactly like the arranger. `⌥←/→` stays free in the clip views, kept for a
  future jump to note edges that mirrors the arranger's jump to clip edges.
- **Reset zoom per clip visit** rather than keeping it as a preference.
  *Decided 2026-09-24:* reset.
- **Floor = whole clip**, with no zoom-out past the clip bounds.
  *Decided 2026-09-24:* yes.

## Must not regress

- The pixel-rounding invariant: one formula for every time-anchored x, in both
  views.
- `scroll_x` is written only by the scroll code of the view that is showing.
  The arranger's `EventsUpdated` guard stays, and the clip view gets the fit-mode
  guard next to it.
- `PendingClip`'s frozen canvas: zoom/scroll input is a no-op there.
- Hit-testing (`event_id_at`, velocity drag, event marquee) keeps using the
  drawn geometry, so it follows the zoom with no separate change.
- No locks or atomics: clip zoom never leaves the view.
- Build gate as always: check, clippy, fmt, test, doc, with zero warnings.
