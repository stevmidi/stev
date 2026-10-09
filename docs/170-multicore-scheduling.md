# Multicore Scheduling for the Audio Callback

Spreading the per-block plugin render across cores instead of running it all on
the one `cpal` callback thread. `src/core/audio/worker_pool.rs` +
`InstrumentMixer`'s render pass.

## Why: deadline, not throughput

The audio callback has `frames / sample_rate` seconds of wall clock to produce a
block — **5.33 ms** at 256 frames / 48 kHz — and it gets one thread. So the
constraint is

```
Σ (per-voice process time)  <  5.33 ms     on ONE core
```

You can have every other core idle and still drop out. This is why process CPU
percent is the wrong instrument: it is a throughput figure averaged over all
cores. A measured example (5 instrument tracks playing, 2026-09-04): the
`DSP` readout showed **67%** of the block deadline while Activity Monitor showed
**75%** for the whole process — i.e. the callback thread *was* nearly the entire
app, pinned to one core, two-thirds of the way to a dropout while using well
under 10% of the machine. `AudioLoad` (see `000-architecture.md`) measures the
number that actually matters.

Two things to know before reading the numbers. **`DSP` at 100% is a dropout,
not a warning** — CoreAudio calls the IOProc once per buffer period and has no
queue, so a callback that takes longer than its period leaves the device with
nothing to play. Treat ~80% as the working ceiling, since the peak hold is a
lagging indicator and ordinary jitter closes the rest. And a deadline overrun
always clicks, but **not every click is an overrun** — voice stealing inside a
plugin, an abrupt gain change or a note-reset burst all click with the meter at
30%, so the meter rules the deadline in or out rather than detecting glitches in
general. The latched `OVR` chip is what actually catches a rare one — and the
`XRUN` chip next to it is the *device's* verdict (CoreAudio's
`kAudioDeviceProcessorOverload`, via cpal's `ErrorKind::Xrun`), which counts
the whole IO cycle, not just this render. Read them as a pair: `OVR` alone means
the render ran past its (deliberately strict) budget but the safety offset
absorbed it; `XRUN` alone means the glitch came from outside the render — the IO
thread pre-empted, device contention — which the render-side timer cannot see.

**When it was the code, the journal says which code.** Every overrun writes one
line to `~/Library/Logs/stev/audio-overruns.log` (path printed at engine
start; see `000-architecture.md` § audio engine):

```
2026-09-21 14:03:11.482  OVERRUN  256f  used 6120/5333 µs (114%)  | click 3 µs  | instruments 6050 µs [t3 5900, t1 120]  | other 67 µs
```

Read it left to right: how far over (`used/budget`), then which source, then —
inside `instruments` — which track, heaviest first, 1-based as the arranger
numbers them. `other` is what no source accounts for: the mix-down, format
conversion and the worker-pool hand-off. Three shapes to recognise:

- **one track dominates** (`t3 5900` of 6050) — that plugin, on that block;
  next stop is Instruments' Time Profiler on the IO thread, or the plugin's own
  settings (oversampling, voice count);
- **`instruments` high but every track small** — the render pass was cheap and
  the time went between the voices: the pool hand-off. This is the
  audio-workgroup symptom, and the reason that work is still on this file's
  list;
- **`other` high** — outside every source: mix-down or conversion, which
  should be constant and tiny, so look for an outside cause (denormals not
  flushed, a debug build, the machine throttling).

A sustained overload writes the first eight lines of each quarter-second drain
and a `… N more overrun(s) in this burst` summary, so the file stays readable;
it rotates to `.1` at 4 MB.

Note the corollary when reading results: **parallelising makes process CPU% go
up and the `DSP` readout go down.** More cores busy, less wall clock per block.
The `DSP` figure is the one to compare before and after.

Measured on one project, 5 instrument tracks playing, i7-8700B (6 physical /
12 logical):

| | DSP (deadline) | process CPU |
|---|---|---|
| serial | 67% | 75% |
| pool, first cut | 25% | 370% |
| pool, idle spin fixed | 25–27% | 97% |

And the same project grown to **8 instrument tracks** (several playing chords),
6 runners: **56% average / peak crossing 70%**, 226% process CPU. That is the
queueing model, not overhead — 8 items over 6 runners means two runners take two
items each, so the makespan is ~2× the per-item cost. The serial equivalent would
have been ~107%, i.e. dropouts; it is clean at 56%.

> **"Voice" here means one hosted plugin instance — one per track — not a synth
> voice.** `ClapVoice` is an unfortunate name for it. Notes played into a track
> do not add items to the pool; they make that track's item **more expensive**,
> and chords make per-item cost vary a lot between tracks. That variance is what
> makes cost-ordered claiming worth more than it first appears.

The middle row is the cautionary one — see *The cost that is left*. The last row
is ~4.3 ms of CPU work per 5.33 ms block finishing in a 1.4 ms window, i.e. about
3.2 cores' worth of concurrency out of 5 runners. The gap from 5 is load
imbalance: makespan is bounded below by the single most expensive voice, which is
what cost-ordered claiming would chip at. Total work rose ~30% (75% → 97%), the
ordinary price of parallelising — syscalls, the caller's barrier spin, and voices
no longer sharing cache.

Useful corollary of being imbalance-bound rather than throughput-bound: **voices
added up to the core count are nearly free.** Filling all 8 tracks should stay in
the 25–35% range, against the ~107% the serial version would have needed.

## Why it is tractable here

A real DAW's hard problem is the graph: track → bus → sidechain → master imposes
an ordering, and latency compensation makes it worse. This app has none of that.
`TrackOutput::Instrument` tracks are independent, fanning into one stereo sum, so
the dependency graph is a single fan-in and needs no analysis at all.

That shape is what `InstrumentMixer::render_into`'s two passes express (see
`130-plugin-host.md`):

- **render pass** — one `process()` per voice into its own `out_bufs`. All the
  cost, fully independent, parallel.
- **summing pass** — gain ramp accumulated into the shared mix, in track order.
  Order-dependent and shared, so it stays on the callback thread. It is a couple
  of multiply-adds per frame per track; parallelising it would cost more than it
  saves.

Calling `process()` from a worker thread is legal CLAP. `[audio-thread]` is a
**per-instance** constraint — one instance's audio-thread calls must not overlap
each other — not a promise that the host uses one thread for everything. Hosts
routinely use a different thread per instance, and per block.

## `WorkerPool`

`WorkerPool::for_each(items, runners, f)` is rayon-shaped
(`T: Send`, `F: Fn(usize, &mut T) + Sync`) but allocates nothing, locks nothing,
and steals nothing across sections — all forbidden on the audio thread.

`runners` is how many threads should take part *including the caller*; pass the
count of items that will really do work. `InstrumentMixer` passes its awake-voice
count, so a block with one busy voice runs inline and never pays to wake anyone,
and a block with three doesn't wake a worker per empty track slot.

### The handshake, and why it is sound

One `for_each` call is a section. Per participating worker it is a strict
ping-pong:

1. the caller writes the type-erased `Job` and resets the shared claim cursor;
2. it bumps that worker's `seq` (release) and unparks it;
3. every runner — workers *and the caller* — `fetch_add`s indices off the shared
   cursor and runs them until exhausted;
4. each worker stores its `seq` into its `ack` (release);
5. the caller spins (never parks — it is the real-time thread) until every
   participating worker has acked.

Step 5 is what makes step 1 sound. A worker touches `job` only between observing
its own `seq` change and storing its `ack`, and the caller never rewrites `job`
until every one of those windows has closed. **There is therefore no such thing
as a straggler from a previous section** — the failure mode that makes the
obvious "publish a generation counter and let workers pick it up" design unsound,
because a late worker can otherwise mix one section's job pointer with the next
section's cursor. It also means the `JobCtx` on the caller's stack outlives every
dereference of it, which is what licenses the raw-pointer payload.

The payload is a `*mut T`, never a `&mut [T]`: runners hold `&mut` to disjoint
indices concurrently, so no reference spanning the whole slice may exist while
they run. The claim cursor handing each index out exactly once is what makes each
of those `&mut` unique.

An item that panics is contained (`catch_unwind` in `run_claimed`) — otherwise a
worker's `ack` would never land and the audio thread would spin forever.

### Invariants to keep

- **`for_each` must never return before every runner is done.** `InstrumentMixer`
  relies on this: `HostShutdown.in_process` is set around the whole of
  `render_into`, so it still means "no `process()` call is in flight" only
  because the barrier has closed. App-exit plugin teardown depends on it.
- **`f` must not block.** Every runner is a real-time thread and the caller waits
  for all of them.
- **Worker count** is `min(cores - 1, MAX_TRACKS - 1)`, and **0 off macOS** —
  the CLAP mixer is the only source with per-item independent work, so other
  platforms get an empty pool rather than parked threads that would never run.
  `cores` is `available_parallelism()` today, which is a placeholder: it counts
  SMT siblings on Intel and E-cores on Apple Silicon, both of which
  over-provision (see sizing under *Not done yet*). It only bites at high awake
  counts, because `runners` already clamps to the work actually available.

### The cost that is left

The caller waits for the slowest worker to *wake*. A parked worker costs ~10–50
µs to wake and the scheduler may deschedule one at any time. That is the residual
risk of this design, and it is what audio workgroups exist to bound.

**Idle workers must not spin.** A worker spins (`SPIN_ROUNDS`, single-digit µs)
only in the moment *after finishing a section*, when another might already be on
its way; a worker that is merely idle parks with no spin at all, on a long
`PARK_TIMEOUT` that exists only as a lost-wakeup backstop. Getting this wrong is
expensive and invisible from the `DSP` readout, which measures only the callback:
an earlier version re-spun 20 000 `pause` iterations after every 1 ms park
timeout, which is a ~50% duty cycle per worker *forever*, playing or not. On a
thermally constrained machine that is not merely wasted CPU — it costs turbo
headroom and so makes the number you are trying to improve worse. Note
`spin_loop()` is `pause`, ~140 cycles on modern x86; spin counts that look small
are not.

## Not done yet

**Worker threads have no real-time priority, and that is a priority inversion.**
CoreAudio gives its IO thread — the one `cpal` calls back on — time-constraint
scheduling automatically. The `WorkerPool`'s threads are ordinary threads. So
while the transport is running, UI work can preempt a worker while the real-time
callback thread sits *spinning at the barrier* waiting for it. Symptom to look
for: the `DSP` readout being noticeably more sensitive to UI activity (dragging
the mouse, resizing) while playing than while stopped, since a stopped transport
sleeps every voice and runs the callback inline with no workers engaged at all.

The minimal fix is `thread_policy_set(THREAD_TIME_CONSTRAINT_POLICY)` on each
worker via the `mach` crate (already a dependency) — smaller than the workgroup
join below, and on Intel it is most of the benefit, since there is no core-type
placement problem to solve there.

**Audio workgroups (macOS).** How much this matters depends on the machine.

On **Apple Silicon it is make-or-break**: without joining the device's workgroup
the scheduler does not know the worker threads share the callback's deadline and
will happily place them on E-cores, and the result can be *worse* than serial. On
**Intel there are no efficiency cores** — every core is identical, so a
phase-2-only pool already schedules sensibly and its measurements are meaningful.
Joining is still worth doing there for deadline-aware scheduling and to keep the
workers from being preempted by lower-QoS work, but it is an improvement rather
than a precondition.

The sequence:

1. `kAudioObjectSystemObject` + `kAudioHardwarePropertyDefaultOutputDevice` →
   `AudioDeviceID`;
2. that device + `kAudioDevicePropertyIOThreadOSWorkgroup` (`'oswg'`) →
   `os_workgroup_t`, retained for the pool's life;
3. each worker calls `os_workgroup_join` **on itself**, with its **own** token,
   and `os_workgroup_leave` on exit.

Two caveats. cpal does not expose the raw `AudioDeviceID` it opened, but since
0.18 `Device::id()` returns the device's CoreAudio UID
(`kAudioDevicePropertyDeviceUID`), which
`kAudioHardwarePropertyTranslateUIDToDevice` turns back into the
`AudioDeviceID` — so step 1 can address the *opened* device rather than
re-querying the default, and stays correct the day device selection is added.
And the property is
macOS 11+; older systems return "property not found", where the fallback is
`thread_policy_set(THREAD_TIME_CONSTRAINT_POLICY)` via the `mach` crate (already
a dependency).

**Cost-ordered claiming** is the other outstanding item — see below.

**Cost-ordered claiming.** Keep an EWMA of each voice's last block time and claim
expensive voices first (longest-processing-time-first). Materially better
makespan when one track is a monster and three are cheap; the claim cursor would
walk an order array instead of `0..len`.

## Considered, deferred: look-ahead rendering for non-armed tracks

The idea: keep the armed/live track on the device block size for responsiveness,
and let the other tracks render in **larger blocks, ahead of time**, so their
cost stops competing for the callback's 5.33 ms.

It is standard practice and every major DAW ships a version of it — Reaper's
*anticipative FX processing* (which automatically excludes record-armed /
input-monitored tracks), Cubase's *ASIO-Guard*, Studio One's *Dropout
Protection*, Logic's *Process Buffer Range* (global rather than per-track) and
its *Low Latency Mode*, which instead bypasses high-latency plugins on the live
path. `live_instrument_target` already identifies the armed track here, so the
two-tier split has an obvious seam.

**What it is not:** a per-track *device* buffer. There is one hardware callback
at one size — CoreAudio calls the IOProc every `frames` and has no queue (see
*Why: deadline, not throughput*). What these features decouple is the
**processing** block from the **device** block: non-live tracks are rendered
ahead into per-track ring buffers on worker threads, and the callback drains
those (a memcpy) while synchronously rendering only the live track.

**What it buys is deadline slack, not less CPU.** The hard per-block deadline
becomes a soft one the size of the look-ahead ring, so a transient spike — a
chord landing on three tracks at once, a plugin's voice allocator, a page fault —
is absorbed instead of clicking. Amortizing per-block fixed costs (plugin
`process()` entry, the barrier handshake, cache warm-up) is a real but secondary
win, and it is largest with many cheap plugins at small buffers.

**What it costs is staleness.** Anything rendered ahead cannot react to anything
that happens after it was rendered: fader moves, automation, a knob turned in a
plugin editor, live MIDI. That is exactly why every DAW above excludes the armed
track, and the flush-on-change logic (seek, loop wrap, tempo change, clip edit,
gain change) is where the complexity and the bugs would live.

CLAP-wise the setup is cheap: `engine::load` already activates with
`min_frames_count: 1, max_frames_count: max_frames`, and `process()` may be
called with any count up to that maximum. Voices rendering at *different* block
sizes therefore needs nothing but activating every voice at the **largest** size
any voice might see — a track moving between the live tier and the anticipated
tier needs no reactivation.

### Why it is deferred

- **This pool is imbalance-bound, not overhead-bound.** The measurements above
  say makespan is bounded below by the single most expensive voice, and that
  voices added up to the core count are nearly free (8 tracks at 56%). Larger
  blocks do not touch imbalance; **cost-ordered claiming** does, and the
  **audio workgroup** is what makes the existing numbers trustworthy. Both are
  far smaller changes and both come first.
- **It cuts across the timebase work.** Sample-accurate scheduling, `pending` +
  `SCHEDULE_DELAY_FRAMES`, `150-clock-position-sync.md`'s
  `clock_tick ≡ playback_tick (mod region_length)` invariant and
  `160-midi-out-offset.md` all exist to land clip MIDI, the click and CLAP audio
  on one instant. Pre-rendered audio still *plays out* at the correct sample, so
  that alignment survives; what does not come free is the requirement that clip
  events be **known a window ahead**. Loop wraps, live recording into the region
  currently playing, and a quantization change all invalidate precisely that
  window.
- **The app's maximum size already fits.** With 8 tracks (then the fixed count) landing in
  the 25–35% band after the pool, there is no problem left to solve at the
  sizes this app can actually reach.

### What would justify picking it up

Any one of these, not the general wish for headroom:

1. Plugins get much heavier than the current set — large sampled instruments,
   convolution reverb — and the `DSP` peak sits near the ~80% working ceiling
   with the workgroup and cost-ordered claiming already in.
2. Per-track effect *chains* arrive. That is also when the fan-in shape this
   pool relies on (*Why it is tractable here*) stops holding, so the two changes
   want designing together.
3. The single-most-expensive-voice floor is the thing being hit, and freeze /
   bounce-in-place has been rejected as the answer for it.

If it is picked up, the tiers should be a property of the *voice*, not a global
mode, and the first cut should anticipate exactly one block — enough to prove the
flush paths against loop wraps and running capture before the ring gets any
deeper.

## Thread-count sizing (done)

`core::audio::topology::max_dsp_threads` is the ceiling on runner threads; the
`runners` argument to `for_each` is what actually keeps a quiet block from
engaging anyone. **The ceiling is architecture-dependent**, and getting there
took two wrong turns worth recording, because both look reasonable in isolation:

1. `available_parallelism()` — wrong on Apple Silicon, where it counts E-cores.
2. Physical cores only — wrong on x86, and for a subtler reason: the alternative
   to putting an item on an SMT sibling is not "a free core", it is *running two
   items back to back on one core*. Two independent DSP threads sharing a core is
   never slower than that, and usually 10–30% faster. Capping at physical cores
   forced 8 tracks through 6 runners, i.e. a full second round for two of them.

The distinction is what the extra logical CPUs *are*. On x86 they are siblings of
the same fast cores, so using them beats queueing. On Apple Silicon they are
efficiency cores — several times slower, and an item placed on one holds the
barrier up for everyone, which is worse than queueing behind a P-core. So:
logical count on x86, performance cluster on aarch64.

`performance_core_count` reads it via `sysctlbyname("hw.perflevel0.physicalcpu")`
— the physical core count of the highest-performance cluster, which is the
P-cores on Apple Silicon and simply all the cores on a homogeneous Intel Mac,
which does publish the perf-level keys (an i7-8700B reports 6 for it and for
`hw.physicalcpu` alike). `hw.physicalcpu` is the fallback for kernels predating
perf levels, and `available_parallelism()` the last resort off macOS, where the
pool is empty anyway.

**Measured on x86 (i7-8700B, 8 tracks).** 6 runners → 8 runners took the busiest
sections from ~56% average / peak over 70% to **53/73%**, with quieter sections at
**44/54%**. So SMT bought a little on the average and did *not* worsen the peak —
worth keeping, but the effective gain is closer to 1.05× per shared core than the
1.2× the textbook figure suggests. Audio stayed clean throughout.

The consequence worth noting: with 8 items and 8 runners, **every runner takes
exactly one item**, so there is no packing decision left and cost-ordered claiming
buys nothing at full track count. It matters again now that a project can hold
`MAX_TRACKS` (16) tracks, more than the thread ceiling on most machines — not
measured yet. The makespan is now bounded by the single most
expensive track, which is the floor described under *What this cannot fix*.

## Denormal flush (done)

`core::audio::denormals::flush_denormals_to_zero` sets `MXCSR` bits 15 (`FTZ`)
and 6 (`DAZ`), removing the microcode assist a decaying reverb / delay / filter
tail otherwise triggers on x86 — on the order of a hundred cycles per operation,
and the reason a meter can climb *after* you stop playing. `MXCSR` is per-thread,
so it is set on entry to each `cpal` callback (that thread isn't ours to hook)
and once per `WorkerPool` worker, both through `thread_role::enter_render_thread`. No-op on aarch64, where subnormals already run
at close to full speed. `_mm_setcsr` is deprecated in Rust, so it is
`stmxcsr`/`ldmxcsr` via `asm!`.

It made **no measurable difference** on the project measured above (25% before
and after). The likely reason is that most modern plugins already do this
themselves — JUCE's `ScopedNoDenormals` is in every JUCE `processBlock` — so the
host-side flush is redundant for them. It is kept as cheap insurance for plugins
that don't, and because the cost is a few dozen cycles per callback. Do not
expect it to show up in a benchmark; expect it to stop one badly-behaved plugin
from falling off a cliff during its release tail.

## What this cannot fix

Parallelism redistributes; it does not reduce total work. The floor is the
**single most expensive voice** — one plugin that alone exceeds the block
deadline is still a dropout with any number of cores. The levers for that are
elsewhere and none of them are implemented: a larger buffer, look-ahead
processing for non-armed tracks (*Considered, deferred* above), freeze /
bounce-in-place, or a track *disable* that unloads the plugin outright as
distinct from mute (which deliberately keeps processing so tails ring out).
