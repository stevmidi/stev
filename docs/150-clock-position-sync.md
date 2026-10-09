# Clock ↔ playback position synchronization

Read alongside the `"clock"` and `"sequencer"` rows in `000-architecture.md` and
`100-running-capture.md` (the capture window depends on the invariant described
here). How this design was reached — the heuristic reconciler it replaced, the
absolute-assignment bug of the first cut, the Phase 2 audit and the rejected
alternatives — is in `archive/150-clock-position-sync-history.md`.

## The two counters

Two atomics carry a musical tick position, and the system requires them to stay
in a defined relationship:

| | Owner | Advances | Repositioned by |
|---|---|---|---|
| `SharedAtomics.clock_tick` | `Clock.ticks` (`src/core/clock.rs`), `"clock"` thread | **always**, credited from real elapsed nanos, running or not | `ClockCommand::AlignToPlayback` |
| `SharedAtomics.playback_tick` | `Transport.playback_tick` (`src/core/transport.rs`), `"sequencer"` thread | **only while running**, +1 per received `ClockTick` (`src/core/threads/sequencer_pump.rs`) | seeks, loop wrap (`Transport::tick`) |

A third, `SharedAtomics.elapsed_ticks`, is not a position at all — see *The
odometer* below.

### Why two counters, not one

- The **free-running** counter is required while the transport is stopped — the
  metronome clicks while stopped (`metronome.on_tick` is called outside the
  `is_running()` guard in `start_sequencer_thread`), and capture needs a live
  coordinate with the transport stopped.
- The **playback** counter must freeze while stopped and resume from the cursor —
  the playhead reads it (`Display::render_playback_tick`) and the plugin
  transport is built from it.

## The invariant

> `clock_tick ≡ playback_tick (mod region_length)`

**Not absolute equality.** While the transport runs the two counters advance in
lockstep, so their difference is constant. At a loop wrap playback jumps back by
the region length and the free-running clock does not — leaving the two at the
*same phase within the region*. Across a long loop the clock legitimately sits
whole loop lengths ahead of playback, and running capture depends on that
absolute progression surviving: its window math picks the loop containing the
last note (`src/core/sequencer/region/window.rs`, `100-running-capture.md`).

So **a loop wrap must not move the clock's loop index**. Only a genuine
discontinuity — a seek, play-from-cursor, or a region whose bounds moved under
unchanged playback — shifts the phase, and only that gets corrected.

### The two counters are never read at the same instant

`AlignToPlayback` carries a `playback_tick` **snapshot** taken on the
`"sequencer"` thread, while `clock_tick` is read **live** on the `"clock"` thread
when the command is drained — one timer firing later, and after however many
ticks were still in flight in `tick_rx`. The two are therefore a few ticks apart
on essentially *every* wrap, even though nothing is wrong. So:

- correcting the **phase** (move the clock by the least it can) costs those few
  ticks and keeps the loop index the clock free-ran to — correct;
- assigning **`playback_tick`** collapses the clock into the region's *first*
  iteration on every wrap — a silent, total loss of the free-run. (This shipped
  once and made a running-capture commit keep every pass of the take.)

**Never assign; always move by the smallest signed phase step.**

## `ClockCommand::AlignToPlayback { playback_tick, region_start, region_length }`

The seek site knows the target and the phase space and hands both over. The
clock's whole share of the work is one pure, unit-tested function:

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
shorter way round the circle. On a correction the clock stores the aligned
value, resets its fractional accumulator (only then — an uncorrected align keeps
its sub-tick credit), and logs `"Clock realigned with playback: … (+N ticks,
region phase A -> B)"`. A degenerate region (`region_length <= 0`) is a no-op.

Each wrap still pulls the clock back by the few ticks of command latency, so over
a very long take the clock's absolute progression runs slightly behind true
elapsed musical time. That is bounded (the phase is re-pinned every wrap) and is
why durations use the odometer instead.

> **A loop wrap re-anchors playback synchronously, not via `TransportEvent`.**
> `Transport::tick()` returns `TickOutcome::Wrapped(region_start)` on a wrap and
> the `"sequencer"` tick pump (`src/core/threads/sequencer_pump.rs`) immediately
> calls `EventHandlers::reanchor_playback` — the same body the
> `TransportEvent::PlaybackTickReset` arm runs (`reset_to_tick` +
> `align_clock_with_playback` + release the MIDI-out and plugin note safety nets).
> It must happen *before* the next `Sequencer::tick`: a single `select!` wakeup
> can drain a burst of ticks without ever reaching the `transport_event_rx` arm,
> so a deferred re-anchor let the track run past the region end and fire the
> *next* clip's opening note — a stuck note roughly every third loop at some
> tempos. Other position jumps (seek, play-from-cursor, normalized-phase region
> change) still route through `PlaybackTickReset`.

### The two send paths, and why the gating differs

Both live on `Transport`, which owns the playback tick, the region, the running
flag and the clock channel. Every clock command originates from `Transport`;
`EventHandlers` holds no clock sender.

- `Transport::align_clock_with_playback()` — **ungated**. Used for a playback
  discontinuity: `TransportEvent::PlaybackTickReset` /
  `PlaybackTickResetWithChase`, and the synchronous loop-wrap re-anchor. It must
  fire even while stopped: play-from-cursor stops, repositions, then starts, so
  gating it on `is_running()` would leave the clock in a stale phase for the
  whole of the next playback.
- `Transport::align_clock_with_playback_if_running()` — **gated**. Used for a
  region edit that leaves playback where it is. While stopped, `playback_tick` is
  parked at the cursor while the clock keeps free-running as the coordinate live
  capture and the metronome are counting in; aligning to a parked playback
  position would drag that coordinate backwards mid-phrase.

### Downstream compensators (still needed)

Alignment jumps still happen; they are merely well defined. Two consumers absorb
them and both stay:

1. `Metronome::on_tick` (`src/core/metronome.rs`) carries `prev_tick`/`last_beat`
   bookkeeping to re-arm the click after a non-unit step in the counter.
2. `last_inserted_event_tick` (`src/core/sequencer/region/window.rs`) anchors the
   capture window on insertion order rather than tick magnitude, so events left
   from a pre-snap clock context can't displace it (regression test
   `calculate_running_region_stale_high_tick_event_does_not_displace_window`).
   The odometer does **not** retire this: capture events stay in `clock_tick`
   space because the window math needs region-phase alignment with playback.

## The odometer: `elapsed_ticks`

Some consumers want elapsed musical time, not a position, and repositioning is
what corrupts them. `SharedAtomics.elapsed_ticks` is the transport odometer. The
`"clock"` thread credits it in the same place it credits `clock_tick`, so both
step at the same instant, with two differences that are the whole point:

- it advances **only while `running`** (it freezes while stopped, so a take
  armed before play doesn't count the stopped gap), and
- it is **never** touched by `AlignToPlayback` — the omission is the feature,
  and the arm carries a comment saying so.

`playback_tick` cannot serve as the duration base: it wraps at the region end.

Both coordinates are carried from the MIDI-input callback:

```rust
pub(crate) struct InputTicks {
    pub(crate) position: i32,  // clock_tick space — running capture
    pub(crate) elapsed: i32,   // odometer space — live recording
}
```

`midi_in_tx` carries `(Vec<u8>, InputTicks)`. `precise_input_tick` loads both
counters and hands them to the pure `input_ticks_at`, which applies the *same*
sub-tick correction to each base — valid because the two counters step together.
The two loads are not atomic with respect to each other; a firing between them
skews the bases by at most one tick, and a comment says so rather than reaching
for a lock on a MIDI callback.

`Sequencer::handle_midi_input_dispatch` routes `position` to
`add_midi_event_to_capture` and `elapsed` to `add_midi_event_to_live_rec`.
Live recording measures `elapsed_tick() - session.elapsed_start_tick` for note
placement, auto-end (`should_end_live_recording`) and the progress bar.

Structural results that show the split holds:

- **`Sequencer` holds no `clock_tick`** — only `elapsed_ticks`. Every position it
  needs arrives stamped on the input message; everything it measures itself is a
  duration.
- **`Display` holds no `clock_tick`** — the live-rec overlay measures a span.
- `clock_tick` is read in exactly two places: the MIDI-input position stamp and
  `ClockTick.tick` (metronome beat and discontinuity logic).

Ticks stay `i32` like every other tick in the codebase; they wrap after ~12.9
days of uptime at 120 BPM.

**If live recording misbehaves** — notes landing wrong *from some point partway
through* a take, a recording overrunning or never auto-ending, or the progress
bar jumping — suspect a duration consumer that has slipped back onto
`clock_tick`. A take wrong from its *first* note, or by a constant offset, is
latency calibration or the recording anchor instead.

## Tests

- `src/core/clock.rs` — `aligned_tick`: the loop-wrap no-op (the regression guard
  for the invariant, including many loops in), a small seek, a real seek, a
  degenerate region, positions before `region_start`.
- `src/core/transport.rs` — `make_transport_with_clock` keeps the clock receiver
  so the send paths are asserted: payload, ungated send while stopped, gated
  silence while stopped, gated send while running.
- `src/core/sequencer/capture.rs` — a three-pass and a twelve-pass running take
  stamped the way a free-running clock stamps them must commit exactly one cycle;
  `live_rec_note_placement_survives_a_clock_position_snap`,
  `running_capture_still_stamps_the_position_coordinate`,
  `should_end_live_recording_fires_on_odometer_distance`.
- `src/core/midi/input_tick.rs` — `input_ticks_at` moves both bases by the same
  correction, each anchored on its own counter.
- The odometer's freeze-while-stopped lives inside `Clock::start`'s timer
  closure and is a manual check. `Metronome`'s discontinuity tests cover the
  re-arm path.

**Manual check most likely to catch a broken invariant:** live recording and
running capture across several loop wraps — auto-end fires at the right length,
and a commit keeps only the last pass.
