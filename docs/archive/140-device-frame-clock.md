# Device-frame-credited clock (deferred future direction)

This is the future-direction doc for the clock / musical-time subsystem — read
alongside the `"clock"` and `"audio-engine"` rows and the timing paragraph in
`000-architecture.md` and the sample-accurate scheduling section of
`130-plugin-host.md`. It describes a change that is **not implemented**; the
subsystem as it actually stands is documented in those two files.

**Status:** deferred, not started. Explored 2026-09-02 and consciously shelved:
current timing is subjectively fine with no observed drift or jitter, so building
this now would be a fix in search of a problem. Kept as a backup plan — pick it up
only if a symptom under *Revisit when you observe* actually shows up. The rest of
this file is a briefing for an agent tasked with turning it into a concrete
prototype plan; it is *not* a spec.

Re-assessed 2026-09-03, with two corrections recorded below: the trigger for this
work is **external-timeline sync**, not the internal drift symptoms originally
listed (see *Revisit when you observe*). The timing debt that is real today is a
different one — `150-clock-position-sync.md`, which should be done first.

**Goal of the prototype:** measure whether crediting musical time from the audio
device's frame count (instead of wall-clock nanoseconds) removes the long-session
drift between "where the sequencer thinks it is" and "what sample the card is
playing" — **without** a full sample-locked-transport rewrite and **without**
regressing the behaviours listed under *Must not regress*.

The agent should produce a phased plan (minimal measurable prototype first), not
attempt the whole thing in one pass.

---

## Current architecture (what exists today)

Two time sources:

1. **`Clock`** (`src/core/clock.rs`) runs on a dedicated software timer
   (`src/core/timer.rs` — `MachWaitUntilTimer` on macOS, `SleepTimer` on
   Linux/Windows, ~1 ms period). Spawned by `start_clock_thread` in
   `src/core/threads/mod.rs`, which calls `clock.start(|tick| tick_tx.send(tick))`.
   - Free-runs **always**, whether or not the transport is playing.
   - Each firing credits musical time from **real elapsed wall-clock
     nanoseconds** since the previous firing (`now.saturating_duration_since(prev)`),
     via `musical_credit_ppqn_us` → `tick_batch` (fractional accumulator) →
     `tick_offset_ns` (spreads a burst of ticks from one firing across its span).
   - Emits `ClockTick { is_beat, at: Instant, tick: i32 }` on `tick_tx`.
     `tick` is the absolute counter (`clock_tick.fetch_add(1)`); `at` is the
     *interpolated* intended `Instant` of that tick.
   - Publishes `SharedAtomics.clock_tick` (counter) and
     `SharedAtomics.clock_tick_instant_nanos` (wall-clock nanos when the counter
     last stepped) for the MIDI-input interpolation described below.
   - `ClockCommand::AlignToPlayback` (handled inside the timer closure via the
     pure `Clock::aligned_tick`) snaps the clock counter onto `playback_tick`
     when a seek or region change has moved their phases apart. A loop wrap
     leaves the phases equal and does not move the clock — see
     `150-clock-position-sync.md`.
   - `MAX_ELAPSED_NS` (100 ms) clamps a firing's credited span so a debugger
     pause can't dump a huge tick burst.

2. **`AudioClock`** (`src/core/audio/clock.rs`) lives in the audio callback
   (`Mixer::render` in `src/core/audio/engine.rs`). `observe(now, steady)` once
   per callback anchors an `Instant`→output-frame mapping, jitter-filtered
   (`ALPHA = 0.05`) and drift-tracked (`RESYNC_SECS = 0.05` hard re-anchor).
   `frame_for(at)` converts a tick's `Instant` to a target output frame.

The **bridge**: `ClockTick.at` (an `Instant`) → `AudioClock::frame_for(at)` →
target output sample. Used by `InstrumentMixer` (`src/core/plugin_host/mixer.rs`) for
CLAP clip events and by `MetronomeSource` (`src/core/audio/metronome_source.rs`)
for the click, both `+ SCHEDULE_DELAY_FRAMES` (one buffer, `src/core/audio/mod.rs`).

Consumers of the clock counter:
- `src/core/threads/sequencer_pump.rs` `start_sequencer_thread` select loop: per `ClockTick`,
  `transport.tick()` (advances `playback_tick` +1 with loop wrap, returning
  `TickOutcome::Wrapped` so the pump re-anchors the sequencer synchronously,
  `src/core/transport.rs`), `sequencer.tick(tick.at)` (tags clip events
  `EventTime::At(at)`, `src/core/sequencer/playback.rs`), `metronome.on_tick(&tick,
  running)` (`src/core/metronome.rs`, uses `tick.tick` for the beat, `tick.at`
  for the `ClickEvent`).
- `src/core/midi/input_tick.rs` `precise_input_tick` / `interpolate_input_tick`:
  extrapolates a sub-tick MIDI-input position from `clock_tick` +
  `clock_tick_instant_nanos` + `tempo`.

Relevant docs: `000-architecture.md` (thread model, the `"clock"` and
`"audio-engine"` rows, the timing paragraph), `130-plugin-host.md`
(sample-accurate scheduling section), `150-clock-position-sync.md`
(the `clock_tick` / `playback_tick` reconciliation this doc's `Clock` description
depends on — planned for removal, and to be done before this work).

---

## The problems this targets

1. **Musical time and audio output are measured by different clocks.** The clock
   is wall-clock-based; the audio device runs on its own crystal (ppm-level
   difference). `AudioClock`'s feedback nudge papers over this *for plugin event
   placement*, but the two domains stay independent and slowly diverge over a
   long session.
2. **Estimate-on-estimate.** `ClockTick.at` is interpolated (`tick_offset_ns`),
   then `AudioClock` re-estimates the `Instant`→sample mapping. Two layers of
   approximation between "tick N" and "sample M".
3. **Timer-period trust.** The Apple-Silicon 1.6 % tempo error was this class of
   bug (nominal 1 ms firing ≠ real 1.016 ms). Mitigated today by crediting real
   nanos, but the timer is still in the musical-time path.

Clock/playback realignment is *purely* discontinuity handling, **not** drift
correction — `150-clock-position-sync.md` phase 1 replaced the old heuristic
`sync_clock_with_playback` with an exact phase check, so phase 2's "re-measure
`sync_clock_with_playback`" step below is moot. Frame-crediting does not remove
the need to realign on a seek.

---

## Proposed approach

Keep the ~1 ms timer thread. Change only **what credits musical time**:

- The audio engine already keeps a monotonic frame counter (`Mixer.steady`,
  `u64`, in `src/core/audio/engine.rs`). Publish it — e.g.
  `SharedAtomics.device_frames: Arc<AtomicU64>`, stored once per callback.
- `Clock`, on each timer firing, reads `device_frames`, computes
  `frames_delta = now_frames - prev_frames`, converts to elapsed µs via the
  device sample rate, and feeds that into the **existing**
  `musical_credit_ppqn_us` / `tick_batch` path in place of the nanosecond delta.
  (`musical_credit_ppqn_us` currently takes `elapsed_ns`; it becomes
  `elapsed_us` from frames, same formula.)
- `Clock` needs the device sample rate — thread it in from
  `EngineHandle.sample_rate` (or a new atomic).

Net effect: musical time is now denominated in the audio device's own clock.
Tick N and output sample M are locked by construction.

### Minimal prototype (do this first, make it measurable)

- Swap the crediting source only. Keep `ClockTick.at` as an interpolated
  `Instant` (derive it from the frame count + sample rate + a fixed anchor).
  Keep `AudioClock`, `EventTime::At(Instant)`, `SCHEDULE_DELAY_FRAMES`, the
  sequencer loop, the metronome, `ClockCommand::AlignToPlayback` — all
  untouched.
- Instrument it: log/measure musical position vs `device_frames`-derived
  position over a 30+ min run; compare to a wall-clock-credited baseline on the
  same machine.
- Success = the frame-credited musical position stays locked to the audio output
  over the session, and every *Must not regress* item still holds.

### Possible phase 2 (only if phase 1 measures well)

- Put a **target frame** on `ClockTick` (and `EventTime::AtFrame(u64)` on
  `ClipInstrumentEvent`, `src/core/sequencer/instrument_event.rs`). `InstrumentMixer`
  and `MetronomeSource` place events by frame arithmetic; `AudioClock` shrinks
  to (at most) the narrow `Instant`→frame mapping the MIDI-input side may still
  want, or disappears. This is what removes problem #2.
- Clock/playback realignment needs no re-measurement: since
  `150-clock-position-sync.md` phase 1 it is pure discontinuity handling with no
  drift-correction role to relax. Keep it working.

---

## Must not regress

Design the prototype around these. Each has a concrete home to check against.

| Behaviour | Where it lives | Why frame-crediting is fine (verify) |
|---|---|---|
| Metronome clicks while transport stopped | `metronome.on_tick` called outside the `is_running` guard in `start_sequencer_thread` | `device_frames` advances whenever the audio stream runs (always), so the clock still free-runs while stopped |
| Pending phrases / capture while stopped | `Sequencer::handle_midi_input_dispatch` + `precise_input_tick` | Musical time still advances while stopped; input still gets a tick coordinate |
| MIDI-input sub-tick precision | `interpolate_input_tick` uses `clock_tick_instant_nanos` (wall-clock nanos of last counter step) | The counter still steps in the 1 ms timer thread; that atomic's semantics are unchanged — **the MIDI-input side needs no change** |
| Snappy transport start (~1 ms) | `Transport::start` sets `running`; sequencer loop polls it every timer firing | Timer still fires at ~1 ms; nothing about start latency changes |
| App runs with no audio output device | `AudioEngine::start` returns `Err` → `start_audio_engine` returns `None` | **New failure mode**: no engine → `device_frames` never moves → clock would freeze. Prototype MUST fall back to wall-clock crediting when `device_frames` is absent or stalls (e.g. hasn't moved in N ms) |
| Startup ordering | `start_clock_thread` and `start_audio_engine` both called from `main` | Clock may start before frames flow — must tolerate a zero/stalled counter at startup (fall back until frames move) |
| CLAP clip + click sample-accuracy | `InstrumentMixer::render_into`, `MetronomeSource::render_into` | Phase 1 keeps the `Instant` bridge intact; phase 2 must preserve equivalent placement |
| Loop-wrap / seek alignment of metronome & capture vs playback | `Clock::aligned_tick`, triggered via `TransportEvent::PlaybackTickReset` → `Transport::align_clock_with_playback` → `ClockCommand::AlignToPlayback` | Still needed (discontinuity, not drift) — keep it working |

---

## Open questions for the plan to resolve

1. How does `Clock` get the sample rate — constructor arg, atomic, or does
   `start_audio_engine` hand it back before `start_clock_thread` runs? (Ordering
   in `main` currently: clock is set up, then engine starts, then clock thread
   starts — check exact order in `src/main.rs`.)
2. Fallback detection: "device_frames stalled" threshold, and how to switch
   crediting mode without a musical-time glitch at the switch point.
3. Does `MAX_ELAPSED_NS` (the debugger-pause clamp) have a frame-count
   equivalent, or is it obsolete once crediting is frame-based (the frame count
   can't run ahead during a pause)?
4. Phase 2 only: `EventTime::AtFrame` vs keeping `At(Instant)` — scope, and
   whether `AudioClock` can be deleted or must stay for MIDI-input.
5. Measurement methodology — what exactly to log, over how long, on which
   machines (Apple Silicon is the known-interesting case).

---

## Revisit when you observe

**Why not now.** `AudioClock` (`src/core/audio/clock.rs`) already closes the loop
on device-vs-system clock drift *for event placement*: it nudges its
`Instant`→frame anchor by `ALPHA` (0.05) of each callback's observed error, hard
re-anchors past `RESYNC_SECS` (0.05 s), and is unit-tested against 200 ppm device
drift over 1000 callbacks (`audio_clock_follows_slow_device_clock_drift`).
Ordinary jitter and slow crystal mismatch are corrected where they are most
audible.

**What is left unfixed.** The musical position counter itself —
`SharedAtomics.clock_tick`, credited from wall-clock nanoseconds in
`Clock::start` — slowly diverging from the audio device's timeline over a long
session. This is an accumulating *offset*, not jitter.

**Symptoms that point here:**

1. **The real trigger: syncing to anything outside this process.** MIDI clock or
   MTC out, slaving to or from a DAW, or tracking against an external recorder.
   Musical time currently ticks at wall-clock rate while the audio leaves at
   device rate; the ppm difference is unobservable *inside* the app (nothing
   compares the two) but becomes a real, accumulating offset the moment a second
   timeline is involved. None of `midi-seq`'s current features do this — which is
   exactly why the work is deferred. **The likely route in for this project:**
   `120-daw-track-sync.md` already runs a virtual MCU port to Logic, but for
   *track selection only* — no transport, no MTC, no MIDI clock. Extend that to
   running the two transports together, or add MIDI clock out, and this brief
   becomes live on that day rather than on a symptom.
2. MIDI recorded late in a session landing progressively earlier or later against
   clips recorded early in the *same* session, with the error growing the longer
   the app has been open. (A *constant* offset is latency calibration, not this.
   Note this path never touches `AudioClock` — check `150-clock-position-sync.md`
   first, a reposition bug looks similar.)
3. The clock's realign debug log ("Clock realigned with playback: … (region
   phase N vs M)") showing the phase gap trending upward with uptime. It firing
   at all on seeks and region changes is normal discontinuity handling — the
   *growth* over a session is the signal. (A loop wrap does not log: the phases
   match and the clock is not moved. See `150-clock-position-sync.md`.)

**Corrected — not a symptom of this.** An earlier revision listed "metronome click
and a hosted CLAP instrument on the same beat drifting audibly apart". They
cannot: `MetronomeSource::render_into` and `InstrumentMixer::render_into` both place
events from the *same* `ClockTick.at` through the *same* `AudioClock::frame_for`
plus the same `SCHEDULE_DELAY_FRAMES`. Any device-vs-system skew moves both
identically. Click-vs-synth misalignment points at the plugin's own latency or
`SCHEDULE_DELAY_FRAMES`, never here.

**Rule of thumb.** A timing symptom that scales with session length and clears on
restart points at this brief. One that is constant, or that tracks audio buffer
size or CPU load, points at `SCHEDULE_DELAY_FRAMES`, the audio buffer, or the
plugin instead — not here.

---

## Verification (this project's norms)

- The full build gate must stay green — see the ground rule in
  `AGENTS.md` (it includes `cargo doc`, which the earlier version
  of this line predated).
- Logic in `core/time.rs` / `core/clock.rs` / pure helpers gets `mod tests` in
  the same change; the crediting-swap is unit-testable (frame delta → tick count).
- The maintainer tests audio behaviour manually — do **not** launch the app.
  Deliver a clear manual-test checklist covering every *Must not regress* row.
- Update `000-architecture.md` (and `130-plugin-host.md` if the bridge changes),
  and this file's *Status*, in the same change.
