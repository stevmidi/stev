# Offline Audio Export

**Status: drafted 2026-11-14, not agreed, nothing built; phase 1 measured the same day (§ Measured). Issue: #11 (reopened 2026-11-14 after being closed as not planned the same day).** The stem export (`⌘/Ctrl+⇧+R`, `130` § Audio export) stops being a playback run through the device and becomes an **offline render**: the audio device is let go, and a render loop steps the sequencer and the mixer through the selection block by block in *virtual time*, as fast as the plugins can compute. The files are the same files (a WAV per stem plus `Mix.wav`, the selection exactly), exact to the sample however slow the machine, and usually done well before real time. Nothing new renders audio and nothing new schedules events: it is the playback path with another caller.

## Background

The export today is "an ordinary playback run with a tap on the mixer" (`130` § Audio export). #11 was closed on two premises. Both were looked at again.

**"It needs a second way to drive the transport, and it must match playback exactly."** It doesn't. The two places time enters the playback path already take it as a value:

- `Sequencer::tick(at)` never reads a clock. The `"clock"` thread hands it each tick's intended `Instant` (`ClockTick::at`), and every clip event, export mark, chased note, wheel and parameter move is stamped from that.
- The mixer turns an `Instant` into a sample with arithmetic: `RenderCtx::scheduled_frame` is `AudioClock::frame_for(at)`, `(at − anchor) × rate`, plus one buffer of `schedule_delay`. The only wall-clock read is `AudioClock::observe`, once per device callback, which nudges the anchor.

So an offline run needs no second transport. A driver calls the same `tick` with instants it makes up (`t0 + n × tick length`), and renders blocks against a clock anchored at `t0` that is never observed again. The mixer's own tests already render this way (`render` in `plugin_host/mixer.rs` builds a `RenderCtx` by hand and calls `render_into` with no device). What is new is the stepping between two threads, not the timing.

**"Speed is the only gain: the files are already glitch-free."** They are click-free. They are not exact on a machine that overruns (§ Measured). The tap's timeline is `steady`, which counts only frames that were rendered (`Mixer::render`). If the device drops a cycle on an xrun, those frames are never rendered, the clock's anchor slides to absorb the gap, and from there on every event (and the end mark) lands one buffer early against the grid. The waveform stays continuous, so listening finds nothing, but the file comes out shorter than the selection and its later bars no longer sit on a DAW's grid at the project tempo. All stems shift together, so they still agree with each other and with `Mix.wav`. The real-time export is exact only on a machine that keeps up, and the machines that most need the export (an old Mac with a heavy project) are the ones that don't.

What offline rendering then gives:

- **Exact files on any machine.** No device, so no deadline and no dropped cycle. A project too heavy to play cleanly still exports cleanly; it just takes longer.
- **Speed where there is headroom.** The render runs at the project's DSP load turned over: a project that meters 25% renders about four times faster than real time. A project that overruns in playback renders *slower* than real time. The gain there is exactness, not speed.
- **Less to go wrong.** A device unplugged, a rate changed in Audio MIDI Setup, an overrun, a full ring: none of them can touch a run that has no device and can wait.

What it costs is in § Decisions 6 and 7: the plugins have to be told, and what they render offline is not always what they played.

## Measured

2026-11-14, the 2018 Mac mini, a Scarlett 4i4 at 48 kHz, `_debug/audio-workgroup-join.stev` (seven plugin tracks: Serum 2, Vital, Spire), bars 1 to 6 at 96 bpm: 12.5 s, 600,000 frames.

| Buffer | Overruns during the run (`audio-overruns.log`) | Frames in every file | Against 600,000 |
|---|---|---|---|
| 256 | 44 | 595,241 | 4,759 short (99.1 ms, 18.6 buffers) |
| 2048 | 0 | 600,026 | 26 over (0.5 ms) |

- **Overruns shorten the files.** 99 ms over five bars is about two thirds of a 16th note at this tempo by bar 6. All eight files are the same length, so the stems still agree with each other.
- **The loss follows the overruns through the run.** Three note attacks in one stem sat about 2,500 frames early by 5.6 s and about 3,700 by 12.0 s, against the first; the journal has the overruns spread the same way, with some twenty in the last second. Thin evidence (the material is mostly sustained), but it rules out a start mark that merely landed late.
- **Less than a buffer per overrun**, about 0.4 on average. The xrun count from the footer wasn't noted, so it isn't known how many of the 44 dropped a device cycle.
- **The clean run is 26 frames long, not exact.** 43 ppm. Either the interface's clock against the system's (musical time is credited from the system clock, the file is counted in device samples) or the smoothed anchor between the two marks; one run can't tell them apart. Negligible here, 13 ms over five minutes if it is clock skew. An offline render has neither.

So the premise #11 was closed on doesn't hold on this machine, and the result line's "overruns while playing" note understates what an overrun does to the files.

## Decisions

1. **Offline replaces the real-time run.** One export, not a choice between two. `⌘/Ctrl+⇧+R`, the plan (`Sequencer::audio_export_plan`), the folder, the file names, the selection as the length, the stems at unity and the mix at the sketch's levels, the click left out, no tails: all as built. What goes is in § What goes away. The real-time run has one thing offline rendering can't do, a plugin that needs real time (§ Open questions 1).
2. **Virtual time, not a new clock.** For the run, the engine owns a time that advances only as blocks are rendered:
   - `t0` is an `Instant` taken at the run's start. Block `k` covers `[t0 + k × N / rate, t0 + (k + 1) × N / rate)`, with `N` the block size.
   - The sequencer's tick `n` of the run carries `at = t0 + n × tick length`, the tick length worked out from the tempo in integers, so it never drifts. The tempo can't change mid-run (the run blocks the command, as today).
   - The mixer's `AudioClock` is anchored once, `t0` at the run's first frame, and not observed again until the stream is back.

   `Instant`s that never happened are fine: nothing on this path compares them with now. Everything that *does* read the wall clock is kept out of the run (decision 5).
3. **The engine thread renders, in step with the sequencer.** The `"audio-engine"` thread already takes the mixer back when a stream is dropped (`480` decision 6). For an export it drops the stream, keeps the mixer, and loops:
   1. ask the `"sequencer"` thread to advance to the block's first instant, and wait for its answer;
   2. render the block (every source, into a mix that is thrown away; the tap takes what it takes);
   3. stop when the mixer has ended the export (`ExportShared::has_ended`) or the run was given up.

   Then it opens the stream again, through the ordinary restart, with no footer line.

   Advancing to the block's *first* instant is what a callback sees on a perfect day: the sequencer has ticked up to now, and with `schedule_delay` at one block those events land inside the block being rendered. The plugins' `BlockTransport` reads the same playback position it would in playback. The clip ring (`INSTRUMENT_MIDI_RING_CAPACITY`, 512) never holds more than one block's events, and the sequencer is never ahead of the render.
4. **The sequencer is stepped, not clocked, during the run.** The `"sequencer"` thread's `select!` gets an arm for the engine's "advance to `until`": it runs `transport.tick()` and `sequencer.tick(at)` for each tick due before `until`, and answers. While the run is on, the `"clock"` thread's ticks no longer advance the transport (they still reach the recovery timer). The transport reads as running, the playhead sweeps the selection at the render's speed, and the footer's `Exporting… 0:42 / 2:10` counts written audio as it does now.
5. **What reads real time stays out of the run.**
   - **MIDI Out tracks** send nothing. Their clip events carry the tick's instant to the `"midiout"` queue, which waits for it in real time; fed virtual instants it would play external gear in bursts. They're already left out of the files and named in the footer. Today they play along while the export runs; offline they are silent.
   - **The metronome** gets no ticks during the run. It never reached the files.
   - **Esc** stamps its closing marks with the run's current virtual instant, not `Instant::now()`.
   - **The DSP meter and the overrun journal** record nothing: there is no deadline to miss.
6. **Plugins are switched to their offline mode for the run, and back after.** Rendering faster than real time while a plugin believes it is live is wrong for anything that works against the clock: a sampler streaming from disk drops the notes whose samples haven't arrived. Both formats have a mode for this:
   - **VST3:** `ProcessSetup::processMode` and `ProcessData::processMode` are `kRealtime` in both places today (`vst3/component.rs`, `vst3/voice.rs`). `kOffline` goes in through `setupProcessing`, which is only allowed while inactive: the `setActive(false)` → `setupProcessing` → `setActive(true)` sequence `480` built for a rate change (`component::reactivate`).
   - **CLAP:** the `render` extension (`CLAP_RENDER_OFFLINE`), set on the main thread. Not among the `clack-extensions` features enabled today.

   The switch reuses `480`'s path whole. A voice already carries the rate it was activated at, and the UI asks back and reactivates any voice whose rate isn't the stream's. That becomes rate *and mode*: the wanted mode is offline from the moment an export is asked for until it ends. So the order is: the voices come back and are reactivated offline, one per frame under the panel ("Preparing export", "Serum 2", "3 of 8"), and go in together; the run starts; at its end the stream reopens and the same path brings them back to real time ("Restarting audio"). The mixer holds a voice in the wrong mode silent, as it does one at the wrong rate, so the run can't start on a voice that hasn't switched.
7. **A plugin that won't switch.** One that refuses the offline setup is reloaded from its live state, as one that refuses a rate change is (`480` § Things to check). One that says it can't render offline at all (CLAP's `has_hard_realtime_requirement`: hardware-backed DSP) is § Open questions 1.
8. **The block size is the running stream's buffer.** The run renders in blocks of the size playback used, so a plugin whose smoothing or modulation steps per block renders as it played. `schedule_delay` stays that one buffer.
9. **The writer waits and is waited for.** The tap's rings drop a block they have no room for and flag it (`130` § The tap), because the audio thread can't wait. The engine thread can: before a block it checks that every ring has room and yields until the `"audio-export"` thread has caught up. Nothing is ever dropped, and the "samples dropped" warning goes. The writer's 3 s stall timeout stays, as the guard for a run that stops advancing.
10. **No sound while it runs.** The device is closed for the run. Live input already had no plugin target during an export (`LiveInputRouter::route`), so nothing that could be played is lost. The footer shows the progress; Esc cancels and removes the files, as now.
11. **Nothing about the device can end the run.** An outside change while the device is closed (unplugged, the rate changed) is met when the stream reopens, by the ordinary restart. The files are written at the rate the run started at. `480` decision 10 (the Audio tab locked during an export) stays: a pick there would restart the engine under the run.
12. **The same commands are blocked.** `audio_export_command` and `audio_export_transport_command` stand as built: a mute or a fader halfway through would still make the stems disagree with the mix.

## What goes away

- The writer's watch on the device: `AudioLoad::stream_errors`, `StreamStatus::restarts`, "Export stopped: the audio device changed" and "… the audio output stopped" (`130` § A device that changes or goes away).
- The overrun and xrun note in the result ("2 overruns, 1 xrun while playing") and the ring-overflow warning.
- `SetLoop` and `PlayFromTick` as the run's way of playing; the run seeks and is stepped. Whether the loop state still needs saving and restoring falls out of that.
- The `280` § Not planned entry, the README's line and #11's closing reasoning, replaced by this doc.

## Not in this design

- **An export rate of its own.** With no device in the run, the files could be written at any rate (always 48 kHz, say, whatever the interface runs at). It needs every plugin reactivated at that rate, which decision 6's path could carry. Left out until someone wants it.
- **Tails that end on silence** (`280` § Export to audio: what's left). Offline rendering makes "render until quiet" cheaper, since nobody waits in real time, but what you select is still what you get.
- **MIDI Out tracks in the files.** Their audio never reaches the app, offline or not.
- **Freeze or bounce-in-place.** Ruled out with audio tracks (`280` § Not planned). This design renders to files for a DAW and nothing else.
- **Identical files from two exports.** A plugin with free-running modulation or randomness renders differently each time, as it plays differently each time.

## Things to check while building

- **Plugins rendering faster than real time without offline mode.** Phase 1 renders with the plugins still in real-time mode. Try the test machine's list (Omnisphere, Kontakt, Serum 2, Repro, Spire, Nexus, DUNE 3): which drop notes, which don't care. It says how much decision 6 buys and which plugins to watch.
- **Offline mode against what was heard.** Some plugins switch to a higher quality offline. Compare a real-time export from today's build with an offline one for each plugin on the list. A file that sounds *better* than playback is acceptable; one that sounds different in kind (another voice count, a changed envelope) is a finding to write down per plugin.
- **A second `setupProcessing` with another mode.** `480` found the plugins on the list took a second `setupProcessing` for a rate. A mode change is the same call with another field, but it is a less travelled path in a plugin.
- **CLAP's `render` extension in clack 0.2.** Whether `clack-extensions` wraps it, and whether a plugin wants the mode set while deactivated. If it can be set on a live plugin from the main thread, CLAP voices skip the round trip through the UI.
- **What two reactivation rounds cost.** Every export pays one round before and one after. Time it on the Omnisphere-sized project; if it rivals a short export's own length, consider leaving plugins that don't need the switch alone.
- **Mixer commands while the stream is down.** `InstrumentMixer::apply_commands` runs inside `render_into`, so a voice can only be returned or inserted while something renders. The switch to offline happens with the stream still up, and the switch back after it reopens. Check that nothing sends a command during the run and waits on its answer (a plugin removed by an undo can't happen: the run blocks edits).
- **The worker pool off the device.** The offline loop calls `render_into` from the `"audio-engine"` thread, which is not a real-time thread, with the workers still in the closed device's workgroup. `170`'s handshake doesn't care who the caller is, but check the pool's spin-waits behave with an ordinary-priority caller, and whether the pool needs rebuilding for the run.
- **The clock invariant after the run.** Playback has run ahead of the `"clock"` thread's free-running tick for the whole run. `clock_tick ≡ playback_tick (mod region_length)` (`150`) has to hold again once the run's stop has put playback back at the cursor; the stop's `AlignToPlayback` should do it.
- **The mixer's clock after the run.** The anchor points at `t0`, long past. The first device callback's error is far over `RESYNC_SECS`, so `observe` snaps it; check that `Mixer::restart` doesn't leave a block scheduled against the old anchor.
- **Meters and the playhead at render speed.** The track meters and the playhead are fed by the render, so they run fast. Probably fine, and a truthful progress display; look at it.
- **A window that isn't drawing.** The switch to offline needs UI frames (`480` § Reactivation needs UI frames). ⌘H straight after `⌘/Ctrl+⇧+R` could hold the export at "Preparing export" until the window is shown.

## Phases

1. **Measure.** Whether an overrun shortens today's files. No code.
   - *Done 2026-11-14:* it does (§ Measured).
2. **The offline run, plugins left in real-time mode.** `Mixer::render` split into the block render and its device-facing half; the engine's export command, the stream dropped and reopened; the sequencer's stepped arm and the clock's ticks held off; virtual instants for ticks, marks and Esc; MIDI Out and the metronome kept out; the writer waited for; the device watch and the overrun report removed. Tests: the stepped sequencer against the clocked one (same events, same frames), a rendered export's length to the sample, a cancel. The plugin list tried by hand for dropped notes.
3. **Offline mode.** The voice's activation mode beside its rate, VST3's `kOffline`, CLAP's `render` extension, the "Preparing export" panel, the fallback reload. The comparisons against a real-time export from the last build that had one.
4. **Tidy.** `130` § Audio export rewritten as built, `280`, the README, `000`'s thread table, the changelog; this file archived.

## Open questions

To settle before this is agreed:

1. **A plugin that needs real time.** Three ways to meet one: refuse the export and name the plugin; render it anyway and say so in the result; or keep the real-time run as a second path for projects that have one. The first is the smallest and the author has no such plugin. Keeping the real-time run means keeping everything § What goes away removes.
2. **Does offline replace the real-time run, or sit beside it?** Decision 1 says replace. The case for keeping both is question 1 and hearing the export as it is written; the case against is two paths to keep equal, when being one path was the point of the original design.
3. **Is silence during the run acceptable** (decision 10), including external gear on MIDI Out tracks going quiet (decision 5)?
4. **Phase 2 without phase 3?** If the plugins on the list render correctly faster than real time, offline mode could wait, and with it the two reactivation rounds per export. Every DAW sets the mode, so the default here is to build it.
