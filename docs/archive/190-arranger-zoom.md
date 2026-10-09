# Arranger zoom + adaptive grid (design brief, phases 1–3 implemented)

This is the design brief for horizontal zoom in the arranger and the
zoom-adaptive grid that has to come with it. It describes a change that is
**not implemented**; the arranger as it actually stands — a fixed 32-bar
viewport, a fixed bar + beat grid, one-beat snap — is documented in
`030-ui-design.md` § Arranger Navigation / § Grid Hierarchy and
`020-views-and-state.md`. Read those first.

**Status:** brief written 2026-09-22 from a survey of how Ableton Live, Logic
Pro, Cubase, Bitwig, Studio One and REAPER do it. **Phase 1 landed 2026-09-23**
on `feature/arranger-zoom` (`view/display/state/zoom.rs`; the as-built summary
is in `030-ui-design.md` § Arranger Navigation). **Phase 2 landed 2026-09-23** too
(`view/display/grid.rs`; as-built in `030-ui-design.md` § Grid Hierarchy and §
Arranger Navigation). **Phase 3 landed 2026-09-23** (bare `Z` / `X`,
`zoom_arranger_to_fit` / `zoom_arranger_back` in `state/zoom.rs`, `ZoomHistory`
in `render_state.rs`). Phase 4 not started; its clip-view half (adaptive
grid, plus zoom and `Z`/`X` in the piano roll) is designed in
`200-clip-view-zoom.md`. The *Current architecture*
section below describes the pre-phase-1 code; read it as history.

Phase-1 deviations from this brief, deliberate:

- **Scale field is `Option<f32>`**, `None` until `sync_arranger_scroll` latches
  the default on the first arranger frame (after `ui` has set `canvas_rect`);
  `Display::arranger_px_per_beat()` falls back to the default until then.
- **Key repeat rides the OS repeat, not `REPEATABLE_KEYS`.** egui's `key_down`
  is keyed on the *logical* key: `+` held as Shift+`=` and released after
  Shift reports its release as `=`, leaving `+` "down" for ever — a
  state-driven repeat would zoom without end. `InputPoller` passes through
  egui's `repeat: true` events for unmodified `+`/`=`/`-` only, so Shift+`+`
  (US layouts) doesn't repeat but `=` does; Shift-held `+`/`-` in the clip
  views (tempo rescale, pending cursor nudge) still fire once per press.
- **The zoom-out limit is content-relative, not a fixed 0.5 px/beat.** The
  fixed floor (≈ 640 bars on screen) let a short song shrink to a sliver in a
  sea of empty bars. Zoom-out now stops where the arrangement
  (`arranger_last_content_tick()`) × `ARRANGER_ZOOM_OUT_HEADROOM` (1.25) fills
  the width, never tighter than the default 32 bars, with a hard floor of
  `ARRANGER_MIN_PX_PER_BEAT = 2.0` (≈ 160 bars) for very long songs. Only
  zoom-*out* is blocked, so a floor that rises under the current scale never
  jolts the view. Phase 2's invariant test (5) now runs at 2.0, not 0.5.
- **The grid loop now shares `region_bound_x`'s formula** (`grid_line_x`). It
  used `pixels_per_step * step`, which agrees with `tick * ppt` at the old
  fixed scale but rounds 1px differently at arbitrary zooms (3.3 px/beat,
  bar 31) — exactly the "one formula" invariant under *Must not regress*. The
  zoom-sweep test proves the old form drifts. Side effect: clip-view 16th
  lines use it too, so they may move ≤1px to agree with note/region x.

Phase-2 deviations / findings:

- **Plain `←`/`→` did not inherit the snap on its own.** The brief assumed it
  did, but the arranger step lived in `input_handler.rs`
  (`handle_move_transport_cursor`, a fixed beat) where the view-local zoom
  isn't visible. It is now resolved view-side into
  `InputEvent::MoveCursorByGrid { step_ticks }` → `TransportCommand::MoveCursor`
  (the "view resolves the operand" pattern), and the handler's Arranger arrow
  arm is a no-op.
- **`⇧←/→` was never "1 bar"** — it stepped the marquee edge by
  `cursor_grid_ticks()` (a beat) all along; `010` was stale. It now follows the
  zoom with everything else.
- **The clip views' fixed grid is a `GridTiers` value** (`GridTiers::clip_view()`),
  so `draw_timeline` has a single role-based loop for every view. Phase 4 just
  swaps that constant for `grid_tiers(ppt, 1/16)`.
- **Line tones: one groove, falling opacity — not the brief's three tones.**
  The brief's `grid_seam_color()` / `in_lane_beat_color` / `in_lane_sub_color`
  mixed two bases (the bar groove, darker than the lane; beat / 16th from
  `grid_major`, *lighter* than it in all 30 themes), which read as two
  competing grids once zoom put many fine lines on screen — and inverted the
  hierarchy in the near-black themes. Every tier is now `grid_seam_color()`
  at `GridTiers::line_strength` (bar 1.0, beat 0.5, finest 0.28); beats take
  the snap strength when nothing finer sits under them
  (`GridTiers::has_tier_below_beat`). The `in_lane_*` names below are
  historical. See `030-ui-design.md` § Grid Hierarchy.
- **Model side:** `arranger_grid_ticks()` → `min_clip_length_ticks()`, as
  planned. Every sequencer caller was a minimum-length floor, none a snap.
- **Odd bars zoomed out** (structural tier ≥ 2 bars, snap = 1 bar) draw as
  snap-tier lines with a short ruler tick and no label, per the role rule.

Phase-3 deviations / decisions:

- **Bare `Z` / `X`, per the bindings table** — the phased plan's `Shift+Z` /
  `Shift+X` was the inconsistency; the user chose Ableton's bare keys.
- **Selection-only `Z`.** The brief's order was marquee → selected clip →
  whole arrangement. The view holds no selected clip (marquee-only model,
  `020`; a band press marquees the clip anyway), and the whole-arrangement
  fallback was cut after user testing: `Z` with nothing selected zooming
  anywhere read as a misfire. No marquee ⇒ no-op.
- **Only `Z` feeds the history, and any manual zoom clears it.** `X` is "undo
  my last zoom-to-fit", not a record of every continuous adjustment. The
  first cut left the history alone on `+`/`-`/wheel/pinch; then `X` from a
  manual zoom-out jumped *in* to a stale pre-`Z` framing with no way back,
  intermittently (it depended on old `Z`s silently stacked). Bounded at 16,
  cleared on project load, a no-op `Z` (already framed) doesn't push a
  duplicate; scrolling leaves it.
- **Fit margin** is `ARRANGER_ZOOM_FIT_MARGIN` = 4% of the *content width*
  each side — fixed on screen, so every selection length frames the same.
  History: 5% of the span (equivalent, but hidden behind the 160 px/beat
  cap below), then one *beat* a side to match Ableton by eye — a time
  margin, which swings on screen with the zoom (wide around short
  selections, nearly none around long ones), so it read as inconsistent.
- **Same framed width wherever the selection sits** (`fit_framing`).
  Uniform fit maths wasn't enough — the clamps after it skewed the framing:
  a long selection in a short project was squeezed by the content-relative
  zoom-out floor (`Z` now uses only the hard limits); a marquee past the
  last clip was clamped off-centre (its end now counts in
  `arranger_last_content_tick`). A selection at bar 1 sits flush left: a
  lead-in before bar 1 (`arranger_min_scroll_x`) was tried to centre it, but
  it let every manual scroll/zoom park the view in an empty gutter; the
  user preferred dropping it over a Z-only special case.
- **Max zoom raised 160 → 960 → 3840 px/beat** (now 4 px per tick). 960
  still capped 1–2 beat selections on a ≈ 2100pt+ content width (margins
  grew as the selection shrank; 3 and 4 beats matched); 3840 fits one beat
  up to ≈ 4200pt, and `f32` scroll keeps sub-pixel precision to 200+ bars.
  Earlier: The brief's 160
  ("≈ 2 bars across the screen — tick-level editing lives in the piano roll")
  capped `Z`: a one-bar selection filled only 30–50% of a real window, so
  Ableton's "selection fills the width" framing was impossible for anything
  under ~2 bars. The deeper limit applies to `+`/`-`/wheel too — one max, or
  a manual zoom from a `Z` framing would jump;
  the fit is clamped to the content-relative floor and the max, and centred.

---

## What the DAWs agree on (the survey, condensed)

1. **The grid adapts to zoom, and the visible grid is the snap grid.** Every
   DAW ships this on by default — Ableton "Adaptive Grid", Logic "Smart"/"Auto"
   snap, Cubase "Adapt to Zoom", Bitwig "Adaptive" beat grid, Studio One
   "Adaptive" snap, REAPER "snap follows grid visibility" — with a fixed-grid
   mode as the opt-out. Zooming in subdivides (bar → beat → 8th → 16th …);
   zooming out coarsens, and past "one line per bar" it keeps going the other
   way (every 2, 4, 8 bars).
2. **The subdivision is chosen by pixel spacing, not by "zoom level".** REAPER
   exposes the mechanism verbatim: a finest-ever grid plus a *minimum pixels
   between grid lines* (default 20); the grid drops to the next coarser rung
   whenever lines would be closer than that. Ableton's Widest…Narrowest and
   Logic's Auto ladder are the same knob with presets. The ladder is powers of
   two (…, 4 bars, 2 bars, bar, 1/2, 1/4, 1/8, 1/16, 1/32). No hysteresis —
   each rung is exactly 2× its neighbour, so a switch reads as half the lines
   appearing or vanishing, never as a jump.
3. **Logic's refinement:** the user's division value (1/16 by default) is the
   *floor* — the adaptive grid never subdivides below it, however far you zoom.
4. **Rendering hierarchy is fixed to *roles*, not rungs.** Bar lines always
   look like bar lines; the finest visible tier is always the dimmest. When the
   tiers shift, the tones shift with them.
5. **Ruler labels thin with the same rule** — every bar, then every 2/4/8/16
   bars, chosen by text width. Zoomed in past a bar, Bitwig/Logic/Cubase add
   beat sub-labels (`5`, `5.3`, `5.3.2`).
6. **Zoom anchor is the one real disagreement.** Pro Tools, Bitwig, REAPER, FL,
   Studio One zoom around the *mouse pointer* on ⌘/Ctrl+wheel; Ableton zooms
   around the *selection/cursor* (`+`/`-`, ⌘+wheel); Cubase around the playhead
   if visible else screen centre — and its forum is full of complaints. The
   sane split everyone converges on: **wheel/pinch → pointer, keys → cursor.**
7. **Gestures:** ⌘/Ctrl+wheel and trackpad pinch for zoom; `+`/`-` keys
   (Ableton, Bitwig); zoom-to-selection (`Z`) with a step-back (`X`, Ableton
   keeps a zoom history); vertical drag in the ruler. Range: whole project on
   screen out to roughly a beat across the screen — tick-level editing lives in
   the piano roll, not the arranger.

---

## Current architecture (what exists today)

- **Scale:** `Display::pixels_per_tick()` (`view/display/mod.rs`) is a pure
  function of `content_w()` and `core::config::BARS_IN_VIEWPORT` (32) in the
  arranger. The clip views compute their own fit-to-region / anchored scale
  (`clip_view_pixels_per_tick`, `pending_clip_pixels_per_tick`) — leave those
  alone in phases 1–3.
- **Scroll:** `Display.render.scroll_x` in pixels, the only horizontal-offset
  state. `state/scroll.rs`: `sync_arranger_scroll` pages toward the cursor in
  hard-coded **2-bar** steps (`followed_arranger_x`), `scroll_arranger_by`
  applies wheel deltas and suspends follow, `arranger_max_scroll_x` clamps to
  `arranger_last_content_tick() * ppt`.
- **Grid drawing:** `draw_timeline` (`rendering/timeline.rs`) walks
  `timeline_step_span` in one-beat steps; step `i % 4 == 0` is a bar (label +
  1px groove + full bar tick), else a beat (dim line + half tick); 16ths are
  drawn only when `is_clip_view() || is_pending_clip_view()`. Tones are chosen
  by *view*: the arranger's beat lines borrow the 16th tier's dimmer
  `in_lane_sub_color` because "the arranger packs beats ~8× tighter" — a
  statement about pixel spacing that stops being true the moment zoom exists.
- **Snap:** `Display::cursor_grid_ticks()` returns `time::arranger_grid_ticks()`
  (one beat) in the arranger, `sixteenth_straight_ticks()` in `Clip`/`ClipEdit`.
  Every gesture in `input/gestures.rs` (marquee, clip edge resize, band move,
  arrow-key cursor stepping, `⌘/Ctrl+←/→` nudge) snaps through it. **Note the
  sequencer side also uses `arranger_grid_ticks()`** as the *minimum clip /
  region length* (`capture.rs`, `region/mod.rs`, `region/window.rs`,
  `commit_clip.rs`) — that is a model invariant, unrelated to view snap, and
  must not follow the zoom.
- **Input:** `InputPoller` reads `smooth_scroll_delta.x` → `ArrangerScroll`.
  `+`/`-` reach `input_handler.rs::handle_plus_minus_keys`, which is a **no-op
  in `Arranger`** (`010-keybindings.md`: "Plain `+`/`-` … are no-ops in
  `Arranger`") — free to claim. **Bare `Z` and bare `X` are unbound
  everywhere**: undo/redo is `⌘/Ctrl+Z` / `⇧⌘/Ctrl+Z` exclusively
  (`input_handler.rs`'s `Key::Z` arm is guarded `if modifiers.command`), and
  the only other `Key::Z` in the tree is the parked hardware-keypad route
  (`threads/controller.rs`), which synthesizes `command: true` for exactly that
  reason — it has no physical command key — so it never emits a bare `Z`
  either. Both are free for zoom, as Ableton binds them.
- **Bar-tick pixel rounding** is a documented invariant: every time-anchored x
  is `round(tick * ppt + content_x - scroll_x)` (`region_bound_x`,
  `tick_to_screen_x`, the grid loop). Zoom must not introduce a second formula.

---

## Design

### State

One new field, view-local like `scroll_x`, never crossing a thread:

```text
Display.render.arranger_px_per_beat: f32
```

Pixels per **beat**, not bars-per-viewport, because (a) the grid ladder is a
function of pixels, (b) every DAW keeps pixel scale fixed under window resize —
a wider window shows *more bars*, it doesn't stretch them. That is a deliberate
change from today, where a resize keeps 32 bars and rescales; accept it. The
initial value reproduces today's look exactly: `content_w() / (32 * 4)` on the
first arranger frame (lazily, since `content_w` needs the window size). Clamp to
`[MIN_PX_PER_BEAT, MAX_PX_PER_BEAT]` — start with `0.5` and `160.0` (≈ 640 bars
and ≈ 2 bars across a 1300px content width). `BARS_IN_VIEWPORT` survives only as
the *default* zoom.

`pixels_per_tick()` in the arranger becomes `arranger_px_per_beat / PPQN`;
nothing downstream changes, because everything already routes through it.

### Anchored zoom (pure, tested)

```text
fn zoomed_scroll_x(scroll_x, anchor_tick, old_ppt, new_ppt) -> f32
    = anchor_tick * new_ppt - (anchor_tick * old_ppt - scroll_x)
```

i.e. the anchor tick stays on the same screen pixel. Then clamp through
`arranger_max_scroll_x` with the *new* ppt. Anchor source:

- wheel / pinch → tick under the pointer (`screen_x_to_tick(pointer_x)`), or
  the cursor when the pointer is outside the content area;
- `+` / `-` keys → `cursor_tick` (Ableton), falling back to the viewport centre
  when the cursor is off-screen so the view doesn't lurch;
- zoom-to-selection → n/a (it sets both scale and scroll directly).

A zoom, like a wheel scroll, **suspends cursor-follow**
(`arranger_follow_suspended = true`): otherwise `sync_arranger_scroll` pages the
view back to the cursor on the next frame and undoes the zoom's framing. The
existing "cursor moved → re-latch" rule already covers getting follow back.

Zoom steps are multiplicative. Keys: `ZOOM_KEY_STEP = 1.25` per press (held key
auto-repeats via `REPEATABLE_KEYS`). Wheel/pinch: egui gives this for free and
the two gestures can't collide — verified in the pinned egui 0.36.2
(`input_state/mod.rs:459-467`): when `wheel.modifiers` matches
`InputOptions::zoom_modifier` (default `Modifiers::COMMAND` — ⌘ on macOS, Ctrl
elsewhere, the same modifier every `⌘/Ctrl+X` binding here tests) the wheel
delta is folded into `zoom_factor_delta` as
`(scroll_zoom_speed * (dx + dy)).exp()` and **`smooth_scroll_delta` is left
`Vec2::ZERO`**; otherwise the reverse. So `InputPoller` reads
`i.zoom_delta()` (which also prefers a real multi-touch pinch measurement over
the ctrl-scroll approximation when one is present) beside the existing
`smooth_scroll_delta.x`, and a frame yields at most one of
`ArrangerZoom { factor, pointer_x }` / `ArrangerScroll { delta_x }` — no
modifier test of our own, and no risk of a ⌘-wheel gesture scrolling *and*
zooming. Note `Shift`/`Alt` are egui's horizontal/vertical scroll modifiers and
combine with the zoom modifier to constrain the axis; we use the scalar
`zoom_delta()` and ignore that.

### The grid ladder (pure, tested — this is the heart of it)

```text
/// The rungs, coarse → fine, in ticks. Straight only; triplets are a fixed
/// alternate ladder, not a phase-1 concern.
const GRID_LADDER_BARS: [i32; _] = [64, 32, 16, 8, 4, 2, 1];          // × bars_to_ticks(1)
const GRID_LADDER_SUB:  [i32; _] = [2, 4, 8, 16, 32];                // divisions of a bar

struct GridTiers {
    /// Structural tier: every N bars (N ≥ 1). Groove tone, bar tick, bar number.
    bar_ticks: i32,
    /// Beat tier, `None` when beats are too tight to draw. `in_lane_beat_color`.
    beat_ticks: Option<i32>,
    /// Finest visible tier == the snap grid. `in_lane_sub_color`. Equal to
    /// `beat_ticks` or `bar_ticks` when nothing finer fits (then not drawn twice).
    snap_ticks: i32,
    /// Bar-number label spacing: every N bars, N ≥ bar_ticks in bars.
    label_ticks: i32,
}

fn grid_tiers(px_per_tick: f32, snap_floor_ticks: i32) -> GridTiers
```

Rules, each a pixel threshold applied to the rung's on-screen spacing
`rung_ticks * px_per_tick`:

| tier | rule | constant | chosen so today's default is unchanged |
|---|---|---|---|
| `snap` | finest rung ≥ `MIN_SNAP_PX`, never finer than `snap_floor_ticks` | `MIN_SNAP_PX = 8` | beat is 9.95px at 32 bars → snap = beat, as today |
| `beat` | `Some(beat)` iff beat spacing ≥ `MIN_BEAT_PX` **and** bar tier is 1 bar | `MIN_BEAT_PX = 8` | drawn today at 9.95px |
| `bar` | coarsest-necessary: 1 bar if bar spacing ≥ `MIN_BAR_PX`, else the first `GRID_LADDER_BARS` rung that clears it | `MIN_BAR_PX = 16` | 39.8px → 1 bar |
| `label` | first rung ≥ `bar` whose spacing ≥ `MIN_LABEL_PX` | `MIN_LABEL_PX = 28` | 39.8px → every bar |

`snap_floor_ticks` is Logic's division floor — `sixteenth_straight_ticks()` for
the arranger (one could argue 1/32; start at 1/16 and see). Tiers are drawn
coarse-over-fine, each line once: a tick that lands on a coarser tier is drawn
as that tier only (the current `i % 4 == 0` test generalised to `tick %
bar_ticks == 0`, then `% beat_ticks`, then the rest).

Invariant tests to write with it: (1) at `content_w = 1274`, 32 bars,
`grid_tiers` returns exactly bar=1 bar, beat=Some(beat), snap=beat,
label=1 bar — i.e. today's arranger; (2) monotonic — zooming in never coarsens
any tier, zooming out never refines one; (3) every tier divides the next
coarser one (`bar % beat == 0`, `beat % snap == 0`, `label % bar == 0`); (4)
snap never goes below the floor; (5) at `MIN_PX_PER_BEAT` bar tier ≥ 2 bars
and labels still ≥ `MIN_LABEL_PX` apart.

### Rendering follows roles

`draw_timeline`'s hard-coded three cases (bar / beat / clip-view-16th) become a
walk over `GridTiers` at `snap_ticks` step (not one-beat step —
`timeline_step_span` takes the step size as a parameter already). Tone by
**role**: bar tier `grid_seam_color()`, beat tier `in_lane_beat_color`, snap
tier `in_lane_sub_color`. The arranger-specific "borrow the 16th tone for
beats" special case is deleted: at the default zoom the beat tier *is* the snap
tier, so it gets `in_lane_sub_color` from the role rule and looks identical;
zoomed in far enough for an 8th/16th tier to appear under it, the beat tier
lifts to its own tone exactly as the clip view's does today. Ruler ticks keep
the height graduation (bar 1.0, beat 0.5, snap 0.35 of `REGION_BAND_H`).

Bar numbers draw only on `label_ticks` multiples (`bar_num = tick /
bars_to_ticks(1) + 1`). Beat sub-labels (`5.3`) are a phase-4 nicety — the
label band budget in `030` § Strip band budget is already tight.

`030 § Grid Hierarchy`, `§ Arranger Navigation` and the `in_lane_beat_line_color`
comment block all state the 32-bar assumption; rewrite them in the same change
(ground rule: docs stay in sync).

### Snap follows the grid

`cursor_grid_ticks()` for `Arranger` returns `grid_tiers(ppt, floor).snap_ticks`.
Nothing else changes — marquee, resize, move, `⌘/Ctrl+←/→` and plain `←/→`
cursor stepping all inherit it, which is exactly the DAW behaviour (arrow keys
step by the visible grid). `Shift+←/→` (1 bar) and the sequencer-side minimum
length (`arranger_grid_ticks()`) are untouched. Because the snap grid always
divides the bar and `grid_tiers` is monotonic, a tick snapped at one zoom
stays on-grid at any coarser zoom — no "my clip is now off-grid" surprise.

### Paging and clamping become zoom-relative

- `followed_arranger_x` pages by **two `bar_ticks`** (two structural-tier
  units) instead of a literal 2 bars: identical today, and still lands on a
  visible bar line at every zoom. Pass the step in as a parameter; the tests
  already do.
- `arranger_max_scroll_x` is unchanged in form — only the ppt it's handed
  varies. Every zoom re-clamps.
- A zoom-out that would push `scroll_x` below 0 clamps to 0, so zooming out
  from bar 1 grows to the right, like every DAW.

### Bindings (all view-local, consumed in `Display::forward_input_event`
before forwarding, like the marquee keys and the theme chords)

| key | action | precedent |
|---|---|---|
| `+` / `=` , `-` | zoom in / out ×1.25 around the cursor | Ableton, Bitwig; and `010`'s "`+`/`-` are generic adjustment keys" — they are no-ops in `Arranger` today |
| ⌘/Ctrl + wheel, pinch | zoom around the pointer | Bitwig, REAPER, FL, Studio One |
| `Z` | zoom to the time selection (marquee), or to the selected clip's span, or to the whole arrangement, in that order of availability | Ableton `Z`, exactly |
| `X` | step back through a small zoom history (repeatable) | Ableton `X`, exactly |

Vertical ruler-drag zoom and the `Alt+wheel` vertical axis are out of scope
(the arranger has no vertical zoom — `arranger_layout()` always fits all
tracks).

### Persistence

None. Zoom is view state like `scroll_x`. `ProjectLoaded` / `ClipExited`
reset `scroll_x` to 0 today; **keep the zoom** across those (a user's zoom is
a preference, a scroll position is not) — but re-clamp `scroll_x` against it.

---

## Phased plan

1. **Variable scale, same look.** *(done 2026-09-23)* Add `arranger_px_per_beat`, route
   `pixels_per_tick()` through it, add `zoomed_scroll_x`, `ArrangerZoom`
   input, `+`/`-` and ⌘/Ctrl+wheel. Grid still bar + beat, snap still one beat.
   Verify: at the default value nothing moves by a pixel (`region_bound_x`
   test sweep still holds), zoom in/out keeps the anchor pixel fixed, follow
   suspension works. Ship.
2. **Adaptive grid.** *(done 2026-09-23)* `grid_tiers` + tests, `draw_timeline` walks tiers by
   role, labels thin, `cursor_grid_ticks` follows `snap_ticks`, paging by
   `bar_ticks`. Rewrite `030` § Grid Hierarchy / § Arranger Navigation, `020`'s
   snap mentions, `010`'s `+`/`-` line.
3. **Zoom to selection / history** *(done 2026-09-23 — bare `Z` / `X`, see
   Status)*.
4. **Optional, later** *(clip-view half now briefed in `200-clip-view-zoom.md`)*: run the clip views' fixed bar/beat/16th grid through
   the same `grid_tiers` with `snap_floor = 1/16` (or 1/32) so the two views
   share one grid rule instead of two hard-coded ones; beat sub-labels; a
   fixed-grid toggle (Ableton ⌘5) if adaptive ever annoys — every DAW offers
   the opt-out, none default to it.

## Must not regress

- The pixel-rounding invariant: one formula for every time-anchored x.
- `030 § Clip Anatomy`: clip edges pixel-snapped, thumbnails at any width (the
  1px `draw_thumb` floor already covers a 1-beat clip at 0.5 px/beat… it
  becomes a 1px sliver, which is correct).
- `scroll_x` is only written by arranger scroll code while the arranger is
  showing — the `events_updated_refits_clip_canvas` gate; zoom code gets the
  same `view_state() == Arranger` guard as `scroll_arranger_by`.
- No locks / atomics: zoom never leaves the view.
- `arranger_grid_ticks()` on the sequencer side keeps meaning "minimum clip
  length"; rename it `min_clip_length_ticks()` in phase 2 when the view stops
  using it, so the name stops implying a snap grid.
- Build gate as always: check, clippy, fmt, test, doc — zero warnings.
