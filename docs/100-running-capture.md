## Running Capture Windowing

Running capture operates on **continuous clock ticks** while transport playback is looped.
Window calculations therefore must account for both absolute tick history and loop-relative
placement.

Key files: `src/core/sequencer/capture.rs`, `src/core/sequencer/region/window.rs`
(the `calculate_*` window maths; `region/mod.rs` keeps the cursor/region mutators).

### Core model

A **new clip** (`build_committed_capture_clip`, placed by `CommitClipEdit`) keeps the whole
last pass of the take and uses its *region* to say which slice plays — see *What a new clip
retains* below. The crop **window** described here is what the two
*insert-into-existing-clip* paths use.

- `calculate_running_region` computes the crop window for running capture commits.
- Inputs:
  - `anchor_tick`: where the committed material begins, in capture-event coordinate space
    (see Anchor contract below). The window is **pinned** here — at this phase of the loop
    pass holding the last note-on — and never slides toward the last note: the window is
    the loop remainder from the cursor and the clip is placed *at* the cursor, so any slide
    moves every event off the region phase it was played at.
  - `region_length`: requested capture window length.
  - `transport_loop: Option<i32>`: `Some(len)` when playback is actually wrapping — the
    window is `[anchor + k·len, +region_length)` for the pass `k` holding the last note-on;
    `None` for a **linear** take — `[anchor, +region_length)`, `k = 0`. Derived by
    `normalize_running_capture` from `Sequencer::playback_is_looping()` (see *Looping vs
    linear* below), not passed by the commit paths.
- Output: `(region_start_tick, region_end_tick)` used to bound and crop capture events.
- Window anchor note: uses **`last_inserted_event_tick`** (last NoteOn by insertion order),
  not `max_event_tick`. This is intentional — capture events are stamped with `clock_tick`,
  which is moved onto `playback_tick`'s phase on seeks and region restores
  (`ClockCommand::AlignToPlayback`; a plain loop wrap only nudges the phase and never
  moves the clock out of the loop it has free-run to — see `150-clock-position-sync.md`).
  Old events from before a correction carry high absolute ticks; `max` would pick those
  and shift the window to the wrong loop.

### Anchor contract

`clock_tick` tracks `playback_tick` relative to `region_start` — precisely,
`clock_tick ≡ playback_tick (mod region_length)`, not absolute equality: the clock free-runs
past the region end while playback wraps, so it can sit whole loop lengths ahead. For correct
window placement, the anchor must be in the same coordinate space as the capture event ticks.

**The anchor is the cursor moved into the clock's numbering** (`capture_anchor_tick`). `handle_midi_input_dispatch` records `clock_tick − playback_tick` at every
note-on captured while running (`Sequencer::capture_clock_offset`, cleared with the buffer).
The commit adds that offset, rounded to the nearest whole loop length (dropping the few ticks
of input latency), to the cursor. A looping take doesn't need it (its pass index absorbs whole
loops); a **linear** take does: its window starts at the anchor itself, so a raw cursor tick
would miss a take the clock had stamped loops away, and a take outside the loop region would
commit nothing (or, for a new clip, size itself from the wrong span). The sequencer still doesn't hold `clock_tick`; the offset arrives
with the input, like every other position it uses.

Running capture is a **position** consumer and stays on `clock_tick`. Live recording
measures a duration on `SharedAtomics.elapsed_ticks` instead — the
two coordinates arrive together on each input message as `InputTicks`
(`src/core/midi/input.rs`), and `handle_midi_input_dispatch` routes one to each. See
`090-live-recording.md`.

**The free-run is what separates one pass of a take from the next.** Each cycle of an
improvisation is stamped a whole `region_length` further along, so the crop window can pick
the loop containing the last note and drop the earlier passes. Anything that resets
`clock_tick` to `region_start` on a loop wrap — an absolute rather than relative
`AlignToPlayback` correction — piles every cycle into one span, and the commit then keeps the
entire take. See *The two counters are never read at the same instant* in
`150-clock-position-sync.md`.

**Why the cursor, and why "region phase" is the model.** The clock's phase is held
relative to `region_start`, so in clock space the loop grid is `region_start + k·L`, and
`cursor + k·L` is exactly "the cursor's phase in pass `k`". With the window pinned there
and the clip placed at the cursor, `window_start` and `clip.start` are the *same tick in
the region grid*, so `clip_local_tick + clip.start` is always the region phase the note was
played at — whatever the cursor's offset into the loop, and whether or not the cursor moved
between play-start and commit. The cursor decides *which slice* of the loop becomes the
clip and where it sits; the events phase themselves. "Anchor on where playback started" is
the same statement (the playback start is itself a point on the region grid) *until* the
cursor moves mid-take — and then it would shift every note by `cursor − playback_start`,
off the backing it was played against. So the cursor is the anchor, not the playback start.

Any start position works: inside the region the phase is `(cursor − region_start) mod L`;
before the region playback runs in and then loops, so at commit the take is loop-shaped and
the clip at the cursor holds the last pass at its region phase; after the region playback
never wraps and the take is linear (see *Looping vs linear*).

- **`NewClip`** and **`InsertFromArranger`**: use `cursor_tick` as anchor.
  `cursor_tick` shares coordinate space with `clock_tick`. This is the stable anchor for
  all arranger-context commits. (For `NewClip` the anchor sets the region slice and the
  clip's placement; the *pass* it keeps is chosen on the region grid — see *What a new
  clip retains*.)
- The insert into an existing clip uses the same arranger-cursor anchor from **both**
  views: the clip view never touches the loop region, so the loop is always the arranger's.
  Anchoring on the cursor rather than the clip start is what skips notes from before the
  playback start position (see *Notes before the playback start* below).

**Do not use `clip.start_tick()` as anchor from the arranger.** When the transport region
is not aligned with the clip (the normal arranger case), `clock_tick` is anchored to
`region_start` while `clip.start_tick()` can be anywhere. This causes `loop_idx = -1` when
notes precede the clip's arrangement position, producing a systematic bar-shift error on
every other loop wrap. Using `region_start` as anchor instead causes negative event ticks
when `region_start < clip.start_tick()`, silently dropping events. The correct solution is
`cursor_tick` as implemented in `build_running_capture_insert`.

### Commit paths

There are two running-capture commit paths in `capture.rs`:

#### `build_committed_capture_clip` + `CommitClipEdit` — arranger, new clip

The new-clip commit is undoable, and is split in two for it:
`Sequencer::build_committed_capture_clip(&self) -> Option<(track_idx, Clip)>` is the pure
half — everything below, ending with the positioned clip and a `Track::fits` check, touching
neither the track nor the buffer — and `CommitClipEdit::from_running_capture` freezes that
clip; its `edit()` does the `add_clip` and clears the capture buffer (once — a redo never
wipes a take started since), its `undo()` lifts the clip by id. `None` from the builder
(empty capture, or a gap under one grid step) means nothing enters the undo record. `commit_clip_to_track` is a `#[cfg(test)]` shim in `capture.rs`'s
tests that runs the real edit. Undo leaves
the buffer cleared by decision — redo is the way back. See `050-undo-redo.md`.
- Anchor: `cursor_tick`
- Window size:
  - **playback wrapping** (`playback_is_looping()`) → the *remainder of the current loop
    cycle from the cursor*: `loop_len - (cursor_tick - region_start).rem_euclid(loop_len)`.
    Cursor at the loop start (phase 0) gives a full cycle; a bar in gives one bar less — so a
    clip committed mid-loop fills only the space it can occupy, matching what the user sees
    between the cursor and the loop end.
  - **linear** (loop off, or a start after the region end — the flag alone is not enough,
    see *Looping vs linear*) → content-sized: the last note-on (`last_inserted_event_tick`,
    cursor-relative) rounded up to the next bar via `time::next_bar_boundary_after`.
  - Both are then clamped to **`available`** (`next_clip_start_after(cursor) - cursor`, or
    `i32::MAX`) so the committed clip never overlaps the next clip on the track and
    `add_clip` doesn't reject it — and floored to **the
    minimum clip length** (a beat, `time::min_clip_length_ticks`; the same floor the edge drags enforce, so a committed clip can be as short as a clip can be trimmed; a
    collapsed loop region can be zero-width, and `crop(0)` divides by zero in
    `Region::snap_to_grid`). A gap smaller than that no-ops. The *linear* content is still rounded up to the next bar;
    only the clamp/floor is finer.
- Target: `RunningCaptureTarget::NewClip`
- Result: a new clip placed at `cursor_tick`, `end_tick = cursor + region_length`; the
  region is `[phase, phase + region_length)` in the clip's event space, and the rest of
  the loop is retained around it (below).

#### What a new clip retains

The commit keeps the take **as played** and lets the region do the hiding — the same
non-destructive shape as a clip split or an edge trim (`020-views-and-state.md`, *Clip
Edge Drag-Resize*): the event list stays whole, only `start_tick`/`region` say what plays.

- **Looping take** (`playback_is_looping()`): the **last pass only** — the **region-grid**
  pass holding the last note-on, `[region_start + j·L, region_start + (j+1)·L)`
  (`calculate_running_region` anchored on `region_start`), i.e. "the last wrap" exactly as
  the loop shows it, **whatever the cursor** — kept whole by `Clip::fold_into_loop`
  (`tick' = (tick − region_start).rem_euclid(L)`; a note held across the wrap is closed at
  `L − 1`, as `crop` would). Phases the pass has not reached yet stay **empty** — never
  topped up from the pass before (see the notes at the end of this section). The region is
  the slice from the cursor,
  `[phase, phase + region_length)` with `phase = (cursor − region_start) mod L`, and
  `start_tick = cursor` — so `start_tick + (event_tick − region.start)` is the region phase
  the note was played at. Phases before the cursor (played after the wrap, "premature")
  sit **before `region.start`**; material past an `available`-clamped window sits **after
  `region.end`**. Both are there for the header-band edge drags: the left edge's clamp
  (`Sequencer::clip_start_trimmed_to`, `start_tick − region.start`) is exactly the
  pre-roll, the right edge reveals the rest. Nothing from the last wrap is dropped at
  commit: a cursor accidentally left on bar 2 of a 2-bar loop commits bar 2 as the region
  with bar 1 as pre-roll, and one left-edge drag restores the whole take.
- **Linear take**: everything from the earliest note-on's bar (relative to the cursor)
  through the content end (`linear_content_length`, unclamped); the region starts at the
  cursor. Pre-roll here only arises when the cursor was moved later mid-take.
- **A silent region still commits.** A take whose every note is at a phase before the
  cursor produces a clip whose region is empty and whose pre-roll holds the notes — drag the left edge to hear them.
- **Late-note relocation** runs on the folded clip: a note within
  `LATE_NOTE_TOLERANCE_TICKS` of the loop end anticipates the wrap's downbeat and moves to
  phase 0 — inside the region when the cursor is at phase 0, retained pre-roll otherwise.
  Never teleported to a mid-loop cursor.

_Why the last pass and not "the most recent note at every phase"._ Folding the last `L`
ticks ending at the last note-on would give a mid-pass commit the previous pass's bars for
the phases not yet reached. The workflow is *try an idea on every wrap, commit when the last
one is right*, so those leftovers from the second-last noodle would linger in the tail of
the clip. The pass-based window leaves those phases empty; do not introduce the "most recent
at each phase" fold.

_Why the region-grid pass and not the cursor-grid one._ The insert paths window the
cursor-grid pass `[cursor + k·L, …)` because what they insert is pinned at the cursor. For a
new clip, with the cursor a bar in, that pass is "bar 2, then bar 1 of the *next* wrap":
a full-wrap take committed at the loop's end would have an empty bar-1 slot and lose the
bar 1 played *before* bar 2 — the cursor's placement would decide which notes survive.
The region-grid pass is the wrap as the user hears it; the cursor only chooses the slice
and where it sits. Consequence: a note
played after the wrap belongs to the *next* pass, so committing right after it keeps that
pass alone — same "last pass only" rule, no exception for the cursor.

#### `build_running_capture_insert` — existing clip, from either view
- Undoable: `InsertCaptureEdit::from_running_capture` records it
  (`220-capture-without-pending-view.md`). The builder is pure and returns
  the notes to add in the clip's event ticks; the edit adds them, clears the buffer on
  its first `edit()` only, and undo restores the clip's events. `None` (nothing
  recorded, buffer kept) when nothing would land in the clip
- Anchor: `cursor_tick` (same coordinate space as `clock_tick`)
- Window size: `clip.region_length()`, pinned at the cursor's phase in the arranger loop
  (which is independent of the clip)
- Target: `RunningCaptureTarget::InsertFromArranger`
- `event_offset_ticks = cursor_tick - clip.start_tick()` shifts crop-relative ticks into
  clip-local space
- Guard: returns `None` if `cursor_tick` is outside `[clip.start_tick(), clip.end_tick())`
- What lands: every note whose onset fits the room left in the clip, and every wheel move
  (pitch bend, mod wheel — `090`) in it, the one after the last note-off included — a
  bend easing back to centre must come along or the clip is left bent
  (`Clip::cloned_events_in_range`)
- **User must have the cursor positioned on the selected clip** before committing
- A mid-clip cursor inserts at the correct phase; notes at a phase before the cursor
  (played after the wrap) and notes past the clip's span inside a longer loop are
  dropped, never shifted into the clip

The `Commit` arm of `sequencer_handler.rs` uses it from the arranger and the clip view
alike: the loop is always the arranger's, and the anchor is the transport cursor.

#### Notes before the playback start

The existing-clip path anchors the window on the cursor — where playback started, unless
it was moved since — so a mid-clip start commits only what was played *from* that
position. A cursor a bar into a 2-bar clip makes the window
`[cursor + k·L, cursor + (k+1)·L)` for the cursor-loop `k` holding the last note-on. Two
kinds of note fall out of that:

- **counted in before the start** (played while stopped, or in the earlier part of the
  same pass): they sit in cursor-loop `k-1`, outside the window — dropped by the crop;
- **played after the loop wrapped** (bar 1 of the next pass): they are in cursor-loop
  `k`, but land past `region_length` after the `clip_offset` shift. The insert range is
  clamped to `region_length - clip_offset` so these are **discarded**, never
  inserted out of region where they would resurface on a later region extend.


### `normalize_running_capture` parameters

- `anchor_tick` — where the region starts (`NewClip`) / where the window is pinned (the
  insert targets); see *Anchor contract*.
- `region_length` — the region size (`NewClip`) / the crop-window size, also
  `calculate_running_region`'s `region_len` (inserts).
- `target`:
  - `RunningCaptureTarget::NewClip` — the fold-and-retain path of *What a new clip retains*.
    No crop.
  - `RunningCaptureTarget::InsertFromArranger` — the pinned window, then
    `relocate_late_notes_to_region_start` before crop, **keyed on the loop pass, not the
    window**: a note within `LATE_NOTE_TOLERANCE_TICKS` of the *loop* end anticipates the
    wrap's downbeat and is relocated to the *loop* start. The crop then keeps it exactly when
    that downbeat is the anchor (cursor phase 0) and drops it otherwise. Keying on the window
    instead would teleport a wrap-anticipating note to a mid-loop cursor (a phase error). A
    linear take (no wrap to anticipate) gets no relocation. Used by
    `build_running_capture_insert`.

The insert paths still drop phases before the cursor (see *Notes before the playback
start*): inserting them would overwrite the target clip's own bar 0, a different decision
from retaining them in a fresh clip.

The transport loop is **not** a parameter: `normalize_running_capture` reads it from the
sequencer (`playback_is_looping().then(|| region_end() - region_start())`).

### Invariants

**A take is its notes.** The capture buffer also holds wheel moves (`090`), but every
gate and size reads notes: `Clip::has_notes` gates both running builders and the
stopped commit (a wheel wiggled alone commits nothing, `a_capture_of_wheel_moves_alone_commits_nothing`),
windows come from note-ons and note-offs (`calculate_running_region`,
`linear_content_length`, `note_tick_bounds`), and the stopped capture source is trimmed
back from its last note edge (`040`). Wheel moves ride along inside whatever window the
notes chose.

`calculate_running_region` must preserve:

1. **Caller-set window length** (at least one arranger grid step, a beat) — `calculate_running_region` takes
   `region_length` as given. `build_committed_capture_clip` decides it: the loop remainder from the
   cursor when playback is wrapping, else the content span (last note-on → next bar). The
   insert passes the target clip's `region_length()`.
2. **Pinned start**: the window starts at the anchor's phase — `(start − anchor) % L == 0`
   when looping, `start == anchor` when linear. It never slides toward the last note.
3. **Loop-stable placement**: same phrase timing on successive loops should keep the same
   loop-relative window phase.
4. **Last pass only**: the pass holding the latest note-on, and phases it has not reached
   stay empty. For a **new clip** that is the *region-grid* pass — the wrap as displayed,
   independent of the cursor (*What a new clip retains*). For an **insert** it is the
   *cursor-grid* pass (the crop is pinned at the cursor): a note at a phase before the
   anchor (played after the wrap) selects the pass it *followed* rather than pulling the
   window forward onto itself.
5. **A window longer than the loop** (an existing clip longer than the arranger loop) starts
   at the pass holding the last note and simply extends past that pass's end — nothing later
   than the last *inserted* note-on exists there to sweep in. So a 4-bar clip over a 2-bar
   loop gets the last pass's notes in bars 0–1 (what the user heard), not pass 1 in bars 0–1
   and pass 2 in bars 2–3.
6. **Nothing is dropped from a new clip**: pre-cursor phases and post-window material are
   retained outside the region for the edge drags (*What a new clip retains*).

### Looping vs linear

`Transport::tick` wraps only while the loop flag is on **and** `playback_tick` is inside
the region. `Sequencer::playback_is_looping()` is that same condition, and it — not
`is_loop_enabled()` — decides both the commit sizing in `build_committed_capture_clip` and whether
`calculate_running_region` gets a `transport_loop`. A start after the region end with the
flag still on is a linear take: content-sized, windowed straight from the anchor. Sizing it
as a loop remainder would, once more than `L` was played, jump the pass index forward and
shift the notes back by whole loops.

Looping takes need the loop length because capture events are stamped with the free-running
`clock_tick` and each pass sits a whole `L` further along; the pass index is what separates
"the last cycle" from the earlier ones. A linear take has no passes: `k = 0`.

### Regression coverage

Tests for `calculate_running_region` in `src/core/sequencer/region/window.rs`:

- first-bar note with 3-bar loop / 2-bar capture keeps window at loop start,
- **a note past the window's end does not pull it forward (pinned start)**,
- **loop shorter than the window: the window starts at the pass holding the last note**,
- **mid-loop anchor, last note at a phase before it (post-wrap): the previous remainder**,
- **linear (`None`): the window starts at the anchor however far the last note is**,
- same phrase on successive loops preserves loop-relative window phase,
- stale high-tick event (post clock-reset) does not displace window.

Test for `playback_is_looping` in `src/core/sequencer/state.rs`: the flag alone is not
enough — playback at/after `region_end` or before `region_start` is not looping.

Tests for `normalize_running_capture` in `src/core/sequencer/capture.rs`:

- bar-1 note in buffer lands in bar 1 of normalized clip,
- bar-2 note in buffer lands in bar 2 of normalized clip,
- empty buffer returns `None`,
- all events after crop are within `[0, region_length)`.

Tests for `commit_clip_to_track` (the test shim over `build_committed_capture_clip` +
`CommitClipEdit`) in `src/core/sequencer/capture.rs`, plus
`build_committed_capture_clip_does_not_touch_the_track_or_buffer`:

- new clip is placed at `cursor_tick`,
- all events in committed clip are 0-based (within region length),
- **a three-pass take commits only the last cycle** (guards against an absolute
  `AlignToPlayback` correction),
- **a twelve-pass take with two notes per pass still commits a single cycle**,
- capture buffer is cleared after commit,
- returns `None` on empty capture,
- **loop enabled, cursor at the loop start → clip length equals the loop-region length**,
- **loop enabled, cursor a bar into a 2-bar loop → one-bar clip (loop remainder)**,
- **a next clip on the track → the commit shrinks to the gap instead of no-op'ing**,
- **loop disabled → clip is content-sized to the bar after the last note-on**,
- **loop disabled + a single short note → one-bar clip (content rounds to the bar)**,
- **looping, cursor on the last beat → a one-beat clip (the grid-step floor)**,
- **a one-grid-step gap before the next clip fits; half a step no-ops**,
- **a zero-width (collapsed) loop region → one-bar clip, no `crop(0)` panic**,
- **cursor a bar into a 2-bar loop, played through the wrap into bar 0 → the bar-1 notes
  at their region phase, the bar-0 note dropped**,
- **cursor on beat 2 → every note at `clip_local + clip.start == played tick`**,
- **loop off, three bars played, one bar free before the next clip → the *first* bar**,
- **a note a hair ahead of the wrap with the cursor a bar in → dropped, not moved to the
  cursor**,
- **same note with the cursor at the loop start and the window clamped by a next clip →
  relocated to the clip start**,
- **loop off, a note near the content end → not relocated to tick 0**,
- **loop flag on but cursor and playback after the region end → linear, content-sized
  past `L`**,
- **cursor before the region, playback looping → the clip at the cursor holds the last
  pass at its region phase; lead-in and earlier pass dropped**,
- **cursor left on bar 2, full-wrap take → bar 1 retained before the region and
  `resize_selected_clip_region_start_to_tick(0)` reveals it at its played tick**,
- **a post-wrap note selects the next region pass; the previous wrap is not carried
  along**,
- **every note at a phase before the cursor → a clip with a silent region and the notes
  as pre-roll, not `None`**,
- **mid-pass commit keeps the last pass only (new bar 0; the previous pass's bar 1 does
  not linger)**,
- **a note held across the wrap is closed at `L − 1`**,
- **loop off, cursor moved a bar after the take began → the earlier bar is pre-roll and
  the left edge reveals it**,
- **events are within one loop `[0, L)`**; the mid-loop/mid-bar/late-note tests assert
  on the region slice and on the retained material separately.

Tests for `fold_into_loop` in `src/models/clip/pairing.rs`: only onsets inside the window
survive (the same phase one loop older is out), ticks fold by `phase_origin` and are
re-sorted, a straddling and a still-held note both close at `loop_len − 1`, orphan offs
are dropped, non-note events fold like any tick.

The test helper `make_sequencer` parks `playback_tick` at `region_start`, so the loop flag
means playback *is* wrapping; a linear-take test flips the flag or parks playback outside.

Tests for the running insert in `src/core/sequencer/capture.rs`, named
`insert_running_capture_*` and driven through the real `InsertCaptureEdit` by a
test-module helper:

- bar-1 note with cursor at clip start inserted in bar 1 of non-zero-start clip,
- bar-2 note with cursor at clip start inserted in bar 2 of 2-bar clip,
- returns `None` when cursor is before clip start,
- returns `None` when cursor is at or after clip end (half-open),
- returns `None` on empty capture,
- capture buffer is cleared after successful commit,
- one undo takes the notes back out and redo puts the same ones back
  (`insert_running_capture_is_undoable`),
- **cursor a bar into the clip, loop wrapped → the post-wrap bar-1 note is dropped, not
  parked past the clip end**,
- **2-bar clip in a 4-bar loop, notes played past the clip's span → dropped, not slid
  back into the clip**,
- **a left-trimmed clip (region start past 0) gets the notes inside its region, not before
  it** (`insert_running_capture_lands_inside_a_left_trimmed_region` — the shift into the
  clip's event ticks must include `region.start`),
- **a linear take outside the loop, with the clock loops ahead, lands at the cursor** — for
  an insert and for a new clip (`…_outside_the_loop_finds_the_take`), plus
  `reset_capture_forgets_the_clock_offset` and the pure `is_note_on` (the clock shift's rounding is
  `core::time::snap_to_grid`, tested in `time.rs`),
- a commit selects nothing when nothing was selected, and keeps an existing selection as it
  was (the commit-never-changes-the-selection pair).
