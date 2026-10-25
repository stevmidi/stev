# Porting to Linux and Windows

macOS is the supported platform (`280` § Platforms). Linux and Windows build and pass the tests in CI on every push to `main`, but nobody has checked that they *play* correctly, and Stev is a timing-critical app. This file is for anyone who wants to change that: what is macOS-only today, what matters most off macOS, and how to measure it. Ports are welcome (`CONTRIBUTING.md`). The roadmap carries full support as a staged candidate (`280` § Linux and Windows support, staged); its stages follow § What matters most below.

## What differs off macOS today

| Area | macOS | Linux / Windows | Where |
|---|---|---|---|
| **Sound** | a plugin host (CLAP, VST3) per track, plus MIDI Out | **MIDI Out only**: the whole `plugin_host` module is compiled out | `main.rs`, `core/plugin_host/`, `130` |
| **The `"clock"` thread** | sleeps to an absolute `Instant` deadline one period after the last, spinning the last 100 µs | the same code, unmeasured | `core/timer.rs` |
| **The `"midiout"` wait** | `select!` timeout until 2 ms before a deadline, then 250 µs sleeps and a 500 µs spin | the same code, unmeasured; see § Today for Windows | `core/threads/midi_output.rs` |
| **Thread priority** (`"clock"`, `"midiout"`) | time-constraint policy | Linux `SCHED_FIFO` 10 when `RLIMIT_RTPRIO` allows (no rtkit), Windows MMCSS "Pro Audio"; both unmeasured | `core/thread_priority.rs` |
| **Virtual MIDI port** (`Virtual: Stev`) | yes | Linux yes (`cfg(unix)`), Windows none: midir can't create one there | `core/midi/port.rs`, `output.rs`, `input.rs` |
| **MIDI hot-plug** | CoreMIDI client anchored to the main thread so new devices appear | never checked | `core/midi/port.rs`, `000` § Threading Model |
| **Audio worker pool** | per-plugin render across cores | empty: nothing to render without the plugin host | `core/audio/engine.rs`, `170` |
| **Quit with unsaved changes** | ⌘Q rerouted through the window's close request, so the prompt runs | the window's close request; check that the platform's quit reaches the prompt too | `view/appkit.rs`, `060` § Unsaved changes |
| **Dragging a `.mid` in from the file manager** | the pointer is read from AppKit during the OS drag, so the ghost clip follows it | winit reports no pointer during a file drag: the ghost probably stays put; check the drop still lands | `view/display/input/midi_drag.rs` |
| **Files** | library in `~/Documents/Stev`, overrun log in `~/Library/Logs/stev` | `dirs`' documents and local-data folders; check they're sensible | `core/paths.rs`, `core/audio/journal.rs` |

Everything not in the table is shared code and behaves the same everywhere: the sequencer, capture, undo, persistence, the UI. The metronome click goes through `cpal`, which supports ALSA and WASAPI.

## What matters most

1. **MIDI Out timing.** Off macOS there is no plugin host, so MIDI Out is the only way Stev makes a sound, and its timing is the first thing a user judges. See § Timing; measure before fixing.
2. **A smoke test** (§ Smoke test), to find out what else is broken. It has never been run.
3. **Plugin hosting.** The host's core is format-agnostic (`InstrumentMixer`, `InstrumentVoice`, the merged catalog; `130`). What's macOS-only is the platform glue: where plugins are found (CLAP's search paths differ per OS; VST3 bundles are macOS-shaped, `180`), how an editor window is parented (CLAP's GUI extension has X11 and Win32 APIs alongside Cocoa), and the audio-thread work (`170`). This is the large one: open an issue before starting.
4. **The rest of the table**, as the smoke test finds them.

## Timing

### Today

Measured on macOS with the probe below; Linux and Windows still traced from the code only:

- **The `"midiout"` wait was the bigger gap, and is fixed** (the first fix below). Every clip message used to wait for its intended instant plus the MIDI-output offset (`160`) in crossbeam's `select! … default(timeout)`, a condvar timeout: on an idle Intel Mac it was late by p99 1.26 ms, with 15% of messages over 1 ms. On Windows that timeout follows the system timer tick (15.6 ms by default), but it no longer times the final approach. Rust's `thread::sleep`, which now does, uses a high-resolution waitable timer on Windows 10 1803 and later, so the sleeps should already be sub-millisecond there without `timeBeginPeriod(1)`. The probe on Windows decides whether `timeBeginPeriod(1)` is needed at all; the third fix left it out.
- **The `"clock"` thread** (`core/timer.rs`) is now one absolute-deadline loop on every platform (the second fix below). Linux and Windows used to sleep *relative* to the end of each callback, so periods drifted and jittered. Tempo was never affected, because `Clock` credits musical time from real elapsed nanoseconds; tick granularity and jitter were.
- **Thread priority** is now requested for the two timing threads (the third fix below). Before it, no thread had real-time priority on any platform, and under load both timing threads were simply not scheduled for milliseconds at a time. The audio workers still take none: a time-constraint policy made their overruns several times worse (`170` § Audio workgroup).

### Measure first: the timing probe

Built 2026-10-14 (`src/core/timing_probe.rs`), to the design settled 2026-10-07. It gives each fix "before" numbers to beat, and the same run afterwards shows whether the fix worked.

- **An ignored test, not a mode of the app**: `#[test] #[ignore] timing_probe` in a new `src/core/timing_probe.rs` (`#[cfg(test)]`), after the `extract_picked_starts` precedent. Run it with `cargo test --release timing_probe -- --ignored --nocapture`. It never ships and never runs in CI, but can't rot, because CI's `clippy --all-targets` compiles it. Stev is a binary crate with no library, so an `examples/` program couldn't reach the real clock or `"midiout"` code. It needs no window, audio device or MIDI port, so it also runs over SSH. Release builds, because that's what users run; the report therefore prints with `println!`, the one sanctioned exception to the logging rule (a test installs no logger; `080` § Logging), noted at the call site.
- **Settings by environment variable** (the `STEV_CAPTURE_DIR` precedent): `STEV_PROBE_SECS` (default 60); `STEV_PROBE_LOAD=1` adds one busy thread per logical core at normal priority, standing in for a heavy session (raised priority only shows under contention, so every machine is measured idle and loaded); `STEV_PROBE_OUT` / `STEV_PROBE_IN` name the loopback ports. They match as a case-insensitive substring, unlike the app's exact-name selection, because the same interface has a different full name on each OS (ALSA appends client:port numbers). On macOS and Linux, `STEV_PROBE_OUT=virtual` loops through the probe's own virtual input instead, with no interface.
- **Clock jitter**: the real platform `Timer` at 1 ms records the gap between firings. The same timer feeds `"midiout"`: each firing sends every message whose intended instant has passed, as the sequencer does on a tick. Report the **mean** as well as p50 / p99 / p99.9 / max, because a relative sleep loop drifts the mean itself above 1 ms, and the timer skips periods it missed under load rather than catching up, which also lifts the mean.
- **`"midiout"` lateness, from the real loop**, not a copy. The thread's loop moves into a function generic over the port it writes to: the app passes `MidiOutputConnection`, unchanged in behaviour, and the probe passes a recorder or a real port, so the deadline-wait fix changes the very code the probe measures. The probe feeds it as the sequencer does: messages stamped with an intended instant, the default offset (`MIDI_OUT_OFFSET_DEFAULT_MS`), a sequence number carried in the data bytes so every arrival is matched to its own deadline. Report p50 / p99 / p99.9 / max of *arrival − deadline*, the count over 1 ms, and any messages lost.
  - **Recorder mode** (the default, no ports set): a recorder stamps each write. It measures Stev's scheduling alone, the thing the fixes below change. The pattern is a three-note chord every 16th at 120 BPM plus single notes at random sub-millisecond phases, so deadlines don't all fall on whole milliseconds.
  - **Loopback mode** (both port variables set): writes to a real output port and listens on a real input, stamping arrivals with `Instant::now()` in midir's callback. That adds the OS MIDI stack and the driver both ways. With a USB MIDI interface, plug its Out into its own In, so the cable and USB are measured too. Send single notes at least 5 ms apart: a 3-byte message takes ~1 ms on a 31.25 kbaud DIN wire, and a chord would queue on the wire and measure the cable instead. That serialization is a constant, so compare across platforms and before/after rather than reading the absolute figure. Without an interface: on macOS and Linux, loop through the probe's own midir virtual port (`STEV_PROBE_OUT=virtual`); on Windows, loopMIDI or Windows MIDI Services' loopback provides the pair.
- **Output**: one table per run, headed with OS, architecture, core count, duration, load and mode. The percentile and summary maths is a pure function with its own `mod tests`.
- **On Windows, measure Windows 11.** Unlike 10, it stops honouring a process's timer-resolution request while that process's window is minimised or hidden, so if `timeBeginPeriod` turns out to be needed (see § Today), it must be checked on 11, in the app, with its window minimised.

### The fixes

In this order, each measured with the probe:

- **A precise deadline wait in `"midiout"`** (done 2026-10-14): inside 2 ms of the next deadline, poll the channels in 250 µs sleeps, then spin the last 500 µs, instead of trusting the `select!` timeout (`160` § The thread). On the Intel Mac, 30 s runs, lateness p50 / p99 / max:

  | Run | Before | After |
  |---|---|---|
  | recorder, idle | 284 / 1261 / 1293 µs | 1 / 11 / 20 µs |
  | recorder, loaded | 944 / 3102 / 4371 µs | 1 / 1497 / 4157 µs |
  | virtual-port loopback, idle | 625 / 1315 / 1407 µs | 39 / 116 / 156 µs |

  Under load the remaining tail is the thread not being scheduled at all, which is what the priority fix is for.
- **One absolute-deadline timer for every platform** (done 2026-10-14): an `Instant`-based sleep-to-target with a 100 µs spin tail replaced both `SleepTimer` and the macOS `mach_absolute_time` loop. Each target is one period after the last, so late wake-ups don't drift the period; a target already past (a stall of a whole period) re-anchors to now and fires at once, where the mach loop fired every missed period back to back. Ticks come out the same either way, since `Clock` credits real elapsed time and spreads a firing's ticks across it. On the Intel Mac it matches the mach loop: clock gap p99 / max 1197 / 1327 µs idle (mach 1186 / 1727), and 3.4–3.6 / 7.1–8.5 ms loaded across two runs each (mach 3.1–3.3 / 7.5–8.5 ms), with a mean of ~1.05 ms loaded because missed periods are skipped. The Linux/Windows gain is unmeasured.
- **Raise the clock and `"midiout"` threads' priority** (done 2026-10-14), best effort and hand-rolled, with no new crate (`core/thread_priority.rs`; each thread builds its own request beside the constants it derives from): macOS's time-constraint policy (clock: period 1 ms, computation 300 µs, constraint 1 ms; `"midiout"`: no period, computation 1 ms, constraint 2 ms; preemptible, so a thread that overruns its computation is demoted until it blocks); Linux `SCHED_FIFO` at priority 10, which needs `RLIMIT_RTPRIO` (an `audio` group) and is otherwise refused; Windows MMCSS "Pro Audio" (`AvSetMmThreadCharacteristicsW`). A refusal is logged in debug builds and the thread runs on. rtkit (D-Bus) and `timeBeginPeriod(1)` were left out until a Linux or Windows probe run shows they're needed. A QoS class (user-interactive) was measured too and changed nothing on the Intel Mac. With every core busy, 30 s runs:

  | | Clock gap p99 / max | `midiout` lateness p99 / max | Loopback p99 / max |
  |---|---|---|---|
  | no priority | 3.4–3.5 / 6.9–7.5 ms | 1.9 / 4.5 ms | — |
  | time-constraint | 1000.1 µs / 1.005–1.017 ms | 4–7 / 11–18 µs | 62 / 107 µs |

  Idle, the clock gap is now p99 / max 1014 / 1038 µs (from 1186 / 1727), and recorder lateness p99 / max 27 / 31 µs. The audio-overrun run that ruled the policy out for the workers (`170` § Audio workgroup, the Serum 2 reference project under twelve `yes` processes) was rerun with it on and came out the same, so two more real-time threads, one spinning 10% of a core, don't compete with the audio callback measurably.
- **Same change**: `000` (the clock and `"midiout"` rows of the thread table), `160`'s jitter claim, and this file. All three fixes are in; what's left of stage 1 is measuring Linux and Windows (`280` § MIDI Out timing has the machines and the baseline table).

**Out of scope**: driver-timestamped MIDI output (CoreMIDI timestamps, Windows MIDI Services, ALSA sequencer queues). That's a per-platform rewrite of the output path, and belongs with external sync, which isn't planned (`280` § Not planned).

## Smoke test

What a port should check by hand, since CI has no screen, audio or MIDI:

- MIDI in: a keyboard plays through, and capture (`\`) makes a clip.
- MIDI out: a track plays on an external synth or DAW, on its own channel, in time with the metronome click (`K`).
- Save, quit, reopen: the project comes back, and quitting with unsaved changes asks first.
- First run with no MIDI gear: what does a new user hear? On Windows, the built-in *Microsoft GS Wavetable Synth* should show up as a MIDI output and play a MIDI Out track with no setup. Linux has no built-in synth: check that a running FluidSynth or qsynth shows up and plays.
- Unplugging and replugging the MIDI interface while the app runs.
- Dragging a `.mid` in from the file manager.

An Ubuntu 24.04 live USB stick on a Windows machine covers both platforms on the same hardware (the CI Linux runner is pinned to 24.04 too).

## Rules for a port

- **Keep the `cfg` boundary clean**: platform code lives behind `cfg(target_os = …)` in the platform modules, with no new platform assumptions in shared code, so a port stays a port and not an untangling job. CI on all three platforms enforces the compiling half.
- **Check the other platforms before pushing**: `check`, `clippy --all-targets` and `doc` take `--target` (`080` § Agentic Editing has the commands, including the Linux-from-macOS pkg-config stub). Removing a caller can leave items dead on one platform only.
- **Run the full build gate natively** on the platform you're porting to, tests included.
- **Update this file's table** as things change, along with the topic file for whatever you touched.
