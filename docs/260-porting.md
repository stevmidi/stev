# Porting to Linux and Windows

macOS is the supported platform (`240` § B). Linux and Windows build and pass the tests in CI on every push to `main`, but nobody has checked that they *play* correctly, and Stev is a timing-critical app. This file is for anyone who wants to change that: what is macOS-only today, what matters most off macOS, and how to measure it. Ports are welcome (`CONTRIBUTING.md`).

## What differs off macOS today

| Area | macOS | Linux / Windows | Where |
|---|---|---|---|
| **Sound** | a plugin host (CLAP, VST3) per track, plus MIDI Out | **MIDI Out only**: the whole `plugin_host` module is compiled out | `main.rs`, `core/plugin_host/`, `130` |
| **The `"clock"` thread** | sleeps to an absolute `mach_absolute_time` deadline, spinning the last ~100 µs | `SleepTimer`: callback, then `thread::sleep(1 ms)`, so each period is callback + 1 ms + wake-up delay | `core/timer.rs` |
| **The `"midiout"` wait** | crossbeam `select!` timeout | the same, but on Windows it follows the system timer tick (15.6 ms by default) | `core/threads/midi_output.rs` |
| **Thread priority** | none | none | — |
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

Traced from the code, not yet measured:

- **The `"midiout"` wait is probably the bigger gap.** Every clip message waits for its intended instant plus the MIDI-output offset (`160`) in crossbeam's `select! … default(timeout)`: a park/condvar timeout, not a precise timer, on every platform. On Windows that wait follows the system timer tick (15.6 ms by default), which could put up to ~15 ms of jitter on every note, far from the "few hundred µs" `160` assumes.
- **The `"clock"` thread** (`core/timer.rs`): on Linux and Windows, `SleepTimer` sleeps *relative* to the end of each callback, so periods drift and jitter. Tempo is unaffected, because `Clock` credits musical time from real elapsed nanoseconds; tick granularity and jitter are not.
- **No thread has real-time priority on any platform**, macOS included. The mach timer is only a spin tail.

### Measure first: the timing probe

Not built yet; this is the design (settled 2026-10-07). It gives each fix "before" numbers to beat, and the same run afterwards shows whether the fix worked.

- **An ignored test, not a mode of the app**: `#[test] #[ignore] timing_probe` in a new `src/core/timing_probe.rs` (`#[cfg(test)]`), after the `extract_picked_starts` precedent. Run it with `cargo test --release timing_probe -- --ignored --nocapture`. It never ships and never runs in CI, but can't rot, because CI's `clippy --all-targets` compiles it. Stev is a binary crate with no library, so an `examples/` program couldn't reach the real clock or `"midiout"` code. It needs no window, audio device or MIDI port, so it also runs over SSH. Release builds, because that's what users run; the report therefore prints with `println!`, the one sanctioned exception to the `dprintln!` rule (`dprintln!` compiles away in release), noted at the call site.
- **Settings by environment variable** (the `STEV_CAPTURE_DIR` precedent): `STEV_PROBE_SECS` (default 60); `STEV_PROBE_LOAD=1` adds one busy thread per logical core at normal priority, standing in for a heavy session (raised priority only shows under contention, so every machine is measured idle and loaded); `STEV_PROBE_OUT` / `STEV_PROBE_IN` name the loopback ports. They match as a case-insensitive substring, unlike the app's exact-name selection, because the same interface has a different full name on each OS (ALSA appends client:port numbers).
- **Clock jitter**: the real platform `Timer` at 1 ms records the gap between firings. Report the **mean** as well as p50 / p99 / p99.9 / max, because today's Linux/Windows loop drifts the mean itself above 1 ms.
- **`"midiout"` lateness, from the real loop**, not a copy. The thread's loop moves into a function generic over the port it writes to: the app passes `MidiOutputConnection`, unchanged in behaviour, and the probe passes a recorder or a real port, so the deadline-wait fix changes the very code the probe measures. The probe feeds it as the sequencer does: messages stamped with an intended instant, the default offset (`MIDI_OUT_OFFSET_DEFAULT_MS`), a sequence number carried in the data bytes so every arrival is matched to its own deadline. Report p50 / p99 / p99.9 / max of *arrival − deadline*, the count over 1 ms, and any messages lost.
  - **Recorder mode** (the default, no ports set): a recorder stamps each write. It measures Stev's scheduling alone, the thing the fixes below change. The pattern is a three-note chord every 16th at 120 BPM plus single notes at random sub-millisecond phases, so deadlines don't all fall on whole milliseconds.
  - **Loopback mode** (both port variables set): writes to a real output port and listens on a real input, stamping arrivals with `Instant::now()` in midir's callback. That adds the OS MIDI stack and the driver both ways. With a USB MIDI interface, plug its Out into its own In, so the cable and USB are measured too. Send single notes at least 5 ms apart: a 3-byte message takes ~1 ms on a 31.25 kbaud DIN wire, and a chord would queue on the wire and measure the cable instead. That serialization is a constant, so compare across platforms and before/after rather than reading the absolute figure. Without an interface: on Linux, loop through the probe's own midir virtual port; on Windows, loopMIDI or Windows MIDI Services' loopback provides the pair.
- **Output**: one table per run, headed with OS, architecture, core count, duration, load and mode. The percentile and summary maths is a pure function with its own `mod tests`.
- **On Windows, measure Windows 11.** Unlike 10, it stops honouring a process's timer-resolution request while that process's window is minimised or hidden, so the `timeBeginPeriod` fix below must be checked on 11, in the app, with its window minimised.

### The fixes

In this order, each measured with the probe:

- **A precise deadline wait in `"midiout"`**: when the next message is due within ~2 ms, sleep and spin to it instead of relying on the `select!` timeout's granularity. This helps macOS too.
- **One absolute-deadline timer for every platform**: an `Instant`-based sleep-to-target with a short spin tail replaces `SleepTimer` (and likely the mach-specific loop too). The pure deadline maths carries `mod tests`.
- **Raise the clock and `"midiout"` threads' priority**, best effort: Windows MMCSS "Pro Audio" (`AvSetMmThreadCharacteristics`) plus `timeBeginPeriod(1)` while the transport runs; Linux rtkit, with a silent fallback when refused; macOS's time-constraint policy. The `audio_thread_priority` crate covers all three (MPL-2.0, tolerated by `080` § Licensing), or hand-roll it and skip the dependency.
- **Same change**: `000` (the clock and `"midiout"` rows of the thread table), `160`'s jitter claim, and this file.

**Out of scope**: driver-timestamped MIDI output (CoreMIDI timestamps, Windows MIDI Services, ALSA sequencer queues). That's a per-platform rewrite of the output path, and belongs with external sync, which isn't planned (`240` § D).

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
