> **Archived snapshot (2026-09-30).** The full `150-clock-position-sync.md` before its history was trimmed. The live doc is `../150-clock-position-sync.md`; this copy keeps the phase narratives, bug stories, superseded designs and dated decisions that were removed from it. Not maintained.

# Clock ↔ playback position synchronization

Read alongside the `"clock"` and `"sequencer"` rows in `000-architecture.md`,
`100-running-capture.md` (the capture window depends on the invariant described
here), and `archive/140-device-frame-clock.md` (a *different*, deferred timing concern —
see *Relationship to 140* below).

**Status:** **Both phases implemented.** Phase 1 landed 2026-09-03 and was
**corrected 2026-09-05** — the first cut assigned `playback_tick` absolutely,
which broke running capture across loop wraps; see *Follow-up fix*. **Phase 2
landed 2026-09-05**, splitting the odometer out of `clock_tick`.

---

## The two counters

Two atomics carry a musical tick position, and the system requires them to stay
in a defined relationship:

| | Owner | Advances | Repositioned by |
|---|---|---|---|
| `SharedAtomics.clock_tick` | `Clock.ticks` (`src/core/clock.rs`), `"clock"` thread | **always**, credited from real elapsed nanos, running or not | `ClockCommand::AlignToPlayback` |
| `SharedAtomics.playback_tick` | `Transport.playback_tick` (`src/core/transport.rs`), `"sequencer"` thread | **only while running**, +1 per received `ClockTick` (`src/core/threads/sequencer_pump.rs`) | seeks, loop wrap (`Transport::tick`) |

Both semantics are needed — see *Why two counters is fine*. What was wrong was
**how they were made to agree**.

## The invariant

> `clock_tick ≡ playback_tick (mod region_length)`

**Not absolute equality.** While the transport runs the two counters advance in
lockstep (one `+1` each per delivered `ClockTick`, `src/core/threads/sequencer_pump.rs`), so
their difference is constant. At a loop wrap playback jumps back by the region
length and the free-running clock does not — leaving the two at the *same phase
within the region*. Across a long loop the clock legitimately sits whole loop
lengths ahead of playback, and two consumers depend on that absolute progression
surviving:

- running capture, whose window math picks the loop containing the last note
  (`src/core/sequencer/region/window.rs`, `100-running-capture.md`);
- live recording, until Phase 2 moved it onto its own counter — it now measures
  `elapsed_tick() - session.elapsed_start_tick` instead, which is never
  repositioned at all.

So **a loop wrap must not move the clock's loop index**. Only a genuine
discontinuity — a seek, play-from-cursor, or a region whose bounds moved under
unchanged playback — shifts the phase, and only that gets corrected.

### The two counters are never read at the same instant

The wrap is only *theoretically* a no-op. `AlignToPlayback` carries a
`playback_tick` **snapshot** taken on the `"sequencer"` thread at the wrap, while
`clock_tick` is read **live** on the `"clock"` thread when the command is drained
— one timer firing later, and after however many ticks were still in flight in
`tick_rx`. The two are therefore a few ticks apart on essentially *every* wrap,
even though nothing is wrong.

That makes the shape of the correction the whole ball game:

- correcting the **phase** (move the clock by the least it can) costs those few
  ticks and keeps the loop index the clock free-ran to — correct;
- assigning **`playback_tick`** collapses the clock into the region's *first*
  iteration, and does so on every wrap — a silent, total loss of the free-run.

This is easy to get wrong twice. An early revision of this document proposed an
unconditional `RepositionTo(playback_tick)`; the first implementation of Phase 1
avoided that but still *assigned* `playback_tick` whenever the phases differed,
which the latency above makes true at every wrap. See *Follow-up fix*.

---

## What Phase 1 replaced

`Clock::sync_clock_with_playback` — ~92 lines, **zero tests**, running on the
timer thread, invoked from 7 sites via `ClockCommand::SynchronizeWithPlayback`.
It reached the right answer through three stacked heuristic layers:

1. a sub-beat phase nudge with a magic `SYNC_TOLERANCE: i32 = 10` — phase
   differences up to 10 ticks were left uncorrected or merely nudged;
2. a beat-within-bar check, `% 4` hardcoded;
3. a bar-within-region check.

All three compared *region phases*, and every branch that corrected ended at
`new_clock_val = playback_val`. The layers only decided *whether* to do that one
thing — expensively, untestably, and with slop.

Two downstream workarounds absorb the jumps it produces, and **both remain**
(jumps still happen; they are merely well defined now):

1. `Metronome::on_tick` (`src/core/metronome.rs`) carries `prev_tick`/`last_beat`
   bookkeeping to re-arm the click after a non-unit step in the counter.
2. `last_inserted_event_tick` (`src/core/sequencer/region/window.rs`) anchors the
   capture window on insertion order rather than tick magnitude, so events left
   from a pre-snap clock context can't displace it — with its regression test
   `calculate_running_region_stale_high_tick_event_does_not_displace_window`.

---

## Why two counters is fine

"Collapse them into one" is the wrong fix:

- The **free-running** counter is required while the transport is stopped — the
  metronome clicks while stopped (`metronome.on_tick` is called outside the
  `is_running()` guard in `start_sequencer_thread`), and pending-phrase / running
  capture needs a live coordinate with the transport stopped.
- The **playback** counter must freeze while stopped and resume from the cursor —
  the playhead reads it (`Display::render_playback_tick`) and the CLAP transport
  is built from it.

Both roles are real. The fix was to make the relationship an **assignment**
instead of a negotiation.

---

## Phase 1 — as implemented

### `ClockCommand::AlignToPlayback { playback_tick, region_start, region_length }`

The seek site knows the target and the phase space; it hands both over instead of
letting the timer thread re-derive them. The clock's whole share of the work is
one pure, unit-tested predicate:

```rust
fn aligned_tick(
    clock_tick: i32,
    playback_tick: i32,
    region_start: i32,
    region_length: i32,
) -> Option<i32> {
    if region_length <= 0 {
        return None;
    }
    let clock_phase = (clock_tick - region_start).rem_euclid(region_length);
    let playback_phase = (playback_tick - region_start).rem_euclid(region_length);
    let delta = Clock::signed_phase_diff(clock_phase, playback_phase, region_length);
    (delta != 0).then_some(clock_tick - delta)
}
```

`signed_phase_diff` reduces `a - b` into `[-modulo / 2, modulo / 2)` — the
shorter way round the circle — so the correction is always the **smallest signed
move that fixes the phase**, never an assignment. The clock keeps the loop index
it free-ran to; only its position inside the region changes. `TransportEvent`
needed no new variant. On a correction the clock stores the aligned value, resets
its fractional accumulator, and logs
`"Clock realigned with playback: … (+N ticks, region phase A -> B)"`.

> **A loop wrap re-anchors playback synchronously, not via `TransportEvent`.**
> `Transport::tick()` returns `TickOutcome::Wrapped(region_start)` on a wrap and
> the `"sequencer"` tick pump (`src/core/threads/sequencer_pump.rs`) immediately calls
> `EventHandlers::reanchor_playback` — the same body the
> `TransportEvent::PlaybackTickReset` arm runs (`reset_to_tick` +
> `align_clock_with_playback` + release the MIDI-out and CLAP note safety nets).
> It must happen *before* the next `Sequencer::tick`: a single `select!` wakeup
> can drain a burst of ticks (a >1-tick clock firing, or backed-up ticks),
> calling `transport.tick()` / `sequencer.tick()` in a loop without ever reaching
> the `transport_event_rx` arm, so a deferred re-anchor let the track run past
> the region end and fire the *next* clip's opening note into its CLAP
> instrument — a stuck note roughly every third loop at some tempos. Other
> position jumps (seek, play-from-cursor, normalized-phase region change) still
> route through `PlaybackTickReset`.

**Deleted**: `sync_clock_with_playback`, `SYNC_TOLERANCE`, and `Clock`'s
`playback_ticks` / `region_start` / `region_end` fields (their names as they were then) —
`Clock::new` went from 8 arguments to 5 and lost its
`#[allow(clippy::too_many_arguments)]`.

### The two send paths, and why the gating differs

Both live on `Transport`, which owns the playback tick, the region, the running
flag and the clock channel:

- `Transport::align_clock_with_playback()` — **ungated**. Used for a playback
  discontinuity: `TransportEvent::PlaybackTickReset` /
  `PlaybackTickResetWithChase`, and the synchronous loop-wrap re-anchor
  (`EventHandlers::reanchor_playback`). It must fire even while stopped:
  `TransportCommand::PlayFromCursor` does `stop(); restart_from_cursor(); start();`,
  so gating it on `is_running()` would leave the clock in a stale phase for the
  whole of the next playback.
- `Transport::align_clock_with_playback_if_running()` — **gated**. Used for a
  region edit that leaves playback where it is. While stopped, `playback_tick` is
  parked at the cursor while the clock keeps free-running as the coordinate live
  capture and the metronome are counting in; aligning to a parked playback
  position would drag that coordinate backwards mid-phrase.

The 7 old send sites became calls to these two, and the duplicated
`if transport.is_running()` wrappers in `transport_handler.rs` went away with
them. `handle_transport_event` now takes `&Transport`. `EventHandlers` no longer
holds `clock_command_tx` at all — every clock command now originates from
`Transport`.

### Behaviour changes (all improvements)

1. **Phase errors are always corrected.** A phase difference below the old
   `SYNC_TOLERANCE`, or one that happened to land on the same beat and bar, was
   previously left uncorrected. Every phase error is now corrected, by the least
   the clock has to move.
2. **Sub-tick credit survives an uncorrected align.** The old code ran
   `fractional_ticks.set(0)` on *every* sync, including the no-op loop wrap,
   discarding up to ~0.5 ms of accumulated musical credit per wrap. It now resets
   only when a correction is actually applied.
3. **Diagnostics.** "Clock out of sync by N ticks" / "Clock jitter N ticks" are
   replaced by one "Clock realigned with playback" line carrying both region
   phases. `archive/140-device-frame-clock.md`'s symptom 3 tracks this.

Preserved deliberately: the `region_length <= 0` early-out (a degenerate region
stays a no-op) and both downstream compensators.

### Tests

`src/core/clock.rs` — `aligned_tick` is pure, so what was untestable is now
covered directly: the loop-wrap no-op (the regression guard for the invariant,
including many loops in), a sub-tolerance seek that previously went uncorrected,
a real seek, a degenerate region, and positions before `region_start`
(`rem_euclid` on both sides).

`src/core/transport.rs` — `make_transport_with_clock` keeps the clock receiver so
the send paths can be asserted: the command payload, ungated send while stopped,
gated silence while stopped, gated send while running.

`src/core/sequencer/capture.rs` — two running-capture regression tests added
2026-09-05 with the follow-up fix below, because the layer the bug actually
*surfaced* in had none: a three-pass take and a twelve-pass take, both stamped
the way a correctly free-running clock stamps them, must commit exactly one
cycle. See `100-running-capture.md`.

`Metronome`'s existing discontinuity tests still cover the re-arm path unchanged.

---

## Follow-up fix (2026-09-05) — the correction must be relative

**Symptom.** Loop a region, improvise freely over several wraps, commit: instead
of the last cycle, *every note of the whole take* landed in the new clip, piled
on top of each other.

**Cause.** The first cut of `aligned_tick` returned `playback_tick` — an
**absolute assignment** — whenever the region phases differed. Per *The two
counters are never read at the same instant*, the phases differ by the command
latency at essentially every loop wrap, so the clock was dragged back to
`region_start` on each pass. Every cycle's MIDI input was then stamped into the
same `[region_start, region_end)` span, the crop window in
`Sequencer::calculate_running_region` legitimately contained all of them, and the
commit kept the lot.

The pre-Phase-1 `sync_clock_with_playback` survived this by accident: for a small
phase error it did `new_clock_val -= ticks_diff`, a *relative* nudge that
preserved the loop index, and only assigned `playback_tick` when the beat-in-bar
or bar-in-region differed — which a few ticks of latency never triggers. Phase 1
collapsed the two cases into the assignment and lost the distinction. Phase 1's
own `aligned_tick_is_none_when_a_loop_wrap_leaves_phases_equal` test did not
catch it because it modelled the wrap as the two counters being read
simultaneously.

**Fix.** `aligned_tick` now returns `clock_tick - signed_phase_diff(...)`: the
smallest signed move onto playback's phase, loop index preserved. Assigning is
never correct under an invariant that is stated modulo the region length.

**Cost that remains.** Each wrap still pulls the clock back by the few ticks of
command latency, so over a very long take the clock's absolute progression runs
slightly behind the true elapsed musical time. This is pre-existing behaviour
(the old nudge did the same), is bounded by the phase — which is re-pinned every
wrap — and is exactly what Phase 2's odometer split would remove. It is not worth
its own change.

---

## Phase 2 — separate *position* from *odometer*

**Status:** audited and planned 2026-09-03, deferred, then **implemented
2026-09-05** — brought forward because Phase 1's follow-up fix made the cost of
conflating the two roles concrete and left the position consumers with
regression tests, so the shared input plumbing this touches was covered while the
distinction was fresh.

Some `clock_tick` consumers do not want a position at all; they want elapsed
musical time. Repositioning that is what corrupts them.

### Audit result

Ran the discovery step this phase was gated on. It found **three** duration
consumers, not the two originally listed — and the one that was missed is the
consequential one:

| # | Site | Uses | Effect of a mid-recording clock snap |
|---|---|---|---|
| 1 | `add_midi_event_to_live_rec` (`src/core/sequencer/capture.rs`) | `local_tick = tick - clock_start` | **Every subsequently recorded note lands at the wrong position inside the clip.** Corrupts the take itself, not just its bounds |
| 2 | `should_end_live_recording` (`src/core/sequencer/playback.rs`) | `clock_tick() - session.clock_start_tick >= length` | Recording overruns its length or never auto-ends |
| 3 | Live-rec thumbnails + arranger progress bar (`capture.rs`, `src/view/display/rendering/arranger.rs`) | `clock_tick() - clock_start` as the span | Progress bar jumps or rewinds |

Plus `start_live_recording_workflow`
(`src/core/event_handlers/live_recording.rs`), which stores
`if is_running() { clock_tick() } else { playback_tick() }` — two different
coordinate spaces depending on run state, made consistent afterwards only because
pressing play emits an ungated align that snaps the clock onto playback. It works
by coincidence of ordering.

Position consumers, which **stay** on `clock_tick`: `precise_input_tick` →
`add_midi_event_to_capture` (the running-capture window) and the metronome beat.

Two findings that change the shape of the work:

- **`playback_tick` cannot serve as the duration base.** It wraps at the region
  end, so it is not monotonic across a recording. The free-running counter was
  the right instinct; only its repositioning is wrong.
- **The MIDI-input path feeds one tick into two consumers that want different
  spaces** — capture wants position, live-rec wants elapsed. So the input path
  must carry *both* numbers. This is the bulk of the change, and the original
  "add an atomic and repoint two readers" sketch understated it.

Also noted: `rebuild_live_thumbnail_snapshot(current_tick)` takes a
position-space tick but walks local-space event ticks, so its
`.map_or(current_tick, …)` fallback mixes spaces. That branch looks unreachable —
an unmatched NoteOn is always in `held_notes` and filtered out above it — so this
is a tidy-up, not a live bug.

### Design — as implemented

`SharedAtomics.elapsed_ticks` is the transport odometer. The `"clock"` thread
credits it in the same place it credits `clock_tick`, so both step at the same
instant, but with two differences that are the whole point:

- it advances **only while `running`** (`Clock` now takes `running`), and
- it is **never** touched by `AlignToPlayback` — the omission is the feature, and
  the arm carries a comment saying so.

`clock_tick` keeps its position role unchanged.

Freezing while stopped, rather than free-running, is what lets the run-state
branch in `start_live_recording_workflow` be **deleted** instead of replaced. A
free-running odometer would have counted the stopped gap against a take armed
before play — a regression against the old behaviour, which only worked because
pressing play happened to emit an align afterwards.

Both coordinates are carried from the MIDI-input callback:

```rust
pub(crate) struct InputTicks {
    pub(crate) position: i32,  // clock_tick space — running capture
    pub(crate) elapsed: i32,   // odometer space — live recording
}
```

`midi_in_tx` carries `(Vec<u8>, InputTicks)`. `precise_input_tick` loads both
counters and hands them to the pure `input_ticks_at`, which applies the *same*
sub-tick correction to each base via the existing `interpolate_input_tick` —
valid precisely because the two counters step together. The two loads are not
atomic with respect to each other; a firing landing between them skews the bases
by at most one tick, less than the correction being applied, and a comment says
so rather than reaching for a lock on a MIDI callback.

`Sequencer::handle_midi_input_dispatch` routes `position` to
`add_midi_event_to_capture` and `elapsed` to `add_midi_event_to_live_rec`.
`LiveRecSession.clock_start_tick` and `LiveRecState.clock_start_tick` became
`elapsed_start_tick`.

Two structural results worth noting, because they are how you can tell the split
took:

- **`Sequencer` no longer holds `clock_tick` at all** — its field became
  `elapsed_ticks` and `clock_tick()` became `elapsed_tick()`. Every position it
  needs now arrives stamped on the input message; everything it measures itself
  is a duration.
- **`Display` no longer holds `clock_tick` either** — the live-rec overlay was
  its only reader, and that measures a span.

`clock_tick` is now read in exactly two places: the MIDI-input position stamp and
`ClockTick.tick` (metronome beat and discontinuity logic).

Kept `i32` for consistency with every other tick in the codebase; it wraps after
~12.9 days of uptime at 120 BPM, the same bound `clock_tick` already has.

`rebuild_live_thumbnail_snapshot` now takes the clip-local tick rather than a
position-space one, which retires the space mix noted in the audit.

### Files

`shared_atomics.rs`, `setup.rs`, `clock.rs`, `midi/input.rs`,
`midi/input_tick.rs`, `shared_channels.rs`, `threads/sequencer_pump.rs`,
`sequencer/capture.rs`, `sequencer/state.rs`, `sequencer/playback.rs`,
`sequencer/live_recording.rs`,
`event_handlers/live_recording.rs`, `view/display/mod.rs`,
`view/display/rendering/arranger.rs`.

### Tests

- `midi/input_tick.rs` — `input_ticks_at` is pure: one test that both bases move by
  the same sub-tick correction, one that each is anchored on its own counter.
  (Extracting the helper was prompted by the first attempt, which drove
  `precise_input_tick` through the real monotonic clock and measured a zero
  correction because the lazy timeline origin is initialised by the test itself.)
- `sequencer/capture.rs` — **the point of the phase**:
  `live_rec_note_placement_survives_a_clock_position_snap` dispatches two notes
  whose `position` values are deliberately incoherent and whose `elapsed` values
  are 480 apart, and asserts the recorded clip-local ticks are exactly
  `[480, 960]`. Unrepresentable before the split, which is what makes it a guard.
  Alongside it, `running_capture_still_stamps_the_position_coordinate` proves the
  two recorders really do read different coordinates, and
  `should_end_live_recording_fires_on_odometer_distance` covers the auto-end,
  including that playback and the cursor moving under it change nothing.
- The odometer's freeze-while-stopped lives inside `Clock::start`'s timer
  closure and is not unit-testable; it is a manual check.

### What this does *not* buy

An earlier revision suggested this would let `last_inserted_event_tick`'s
stale-context workaround be retired. **It does not.** Capture events stay stamped
in `clock_tick` space, which still gets snapped, so pre-snap events with stale
high ticks remain possible. Moving capture onto the odometer would not help
either: the window math needs region-phase alignment with playback, so it would
have to track a snap offset — more machinery, not less. That workaround stays.

### The symptoms this removed

These were the *Revisit when you observe* list while the phase was deferred; they
are what to re-check if live recording ever misbehaves again.

All three symptoms need the same trigger — a **region edit or an arranger↔clip
view switch while live-recording**. The seek paths (`PlayFromCursor`,
`TogglePlayback`, `Stop`) all call `end_live_recording_workflow` first, so they
are already safe, which is why this is rare enough to defer.

1. Notes in a finished take landing at the wrong position *from some point
   partway through the recording onward*, with everything before that point
   correct. The snap is the dividing line. This is consumer 1 and the reason the
   phase is worth doing at all.
2. A live recording overrunning its region length, or never auto-ending.
   (Consumer 2.)
3. The arranger live-rec progress bar jumping or rewinding mid-take.
   (Consumer 3 — the visible early warning for the other two.)

A take that is wrong from its *first* note, or wrong by a constant offset
throughout, is not this — that is latency calibration or the recording anchor,
not a mid-take clock snap.

### Alternatives considered and rejected

Recorded because they are the tempting shortcuts if this ever needs revisiting.

Shifting `clock_start_tick` by the same delta whenever the clock snaps preserves
the difference in a few lines, but adds a third compensator patching a derived
value after the fact — the exact pattern Phase 1 removed. Cheapest of all, and
least principled: gate the four region-change commands on `is_recording()` the
way the seek commands already are, which closes the bug without touching the
counters but makes region edits silent no-ops mid-recording.

---

## Relationship to 140

Independent, and this came first.

`140` (device-frame crediting) is about *what unit credits musical time*. This
doc is about *who owns musical position*. With Phase 1 landed, `140`'s phase 1
touches a single function that produces credit, with no reconciler to reason
about.

---

## Verification (this project's norms)

The full build gate, clean — see the ground rule in `AGENTS.md`.
Both phases were verified against it, including `cargo doc`.

The maintainer tests audio behaviour manually — do **not** launch the app.
Checklist for the Phase 1 change:

1. Metronome clicks on the beat with the transport **stopped**.
2. Loop a 2-bar region for several minutes: click stays on the downbeat, tempo
   does not creep (covers behaviour change 2).
3. Restart from cursor while playing — click re-aligns immediately, no double or
   missed click.
4. Play from cursor after a stop (the ungated path) — playback and click aligned
   from the first bar.
5. Region edits while playing: `SetRegion`, clip-span toggle, region start/end to
   cursor.
6. Open and close a pending phrase (`restore_global_region`) while playing. (The clip
   view used to stash and restore the region too; since `archive/210-docked-clip-panel.md` it
   leaves the loop alone.)
7. Running capture: play a phrase against the loop, commit, window lands where it
   did before (`100-running-capture.md`).
8. **Live recording across several loop wraps** — auto-end still fires at the
   right length. This is what the wrap-no-op protects; it is the test most likely
   to catch a mistake in the invariant.
9. Performance-lane bar jump (`110-performance-lane.md`) still lands on the bar.
10. Degenerate region (set region start past its end while playing) — no crash,
    no clock jump.
