# Offline Audio Export

**Status: design agreed 2026-11-14; phase 1 (measuring) done the same day (§ Measured); phase 2 (the offline run) built and tested by hand 2026-11-14, phase 3 (the plugins' offline mode) built and tested by hand 2026-11-15 (§ Phases); phase 4 not built. Issue: #11 (reopened 2026-11-14 after being closed as not planned the same day).** The stem export (`⌘/Ctrl+⇧+R`, `130` § Audio export) stops being a playback run through the device and becomes an **offline render**: the audio device is let go, and a render loop steps the sequencer and the mixer through the selection block by block in *virtual time*, as fast as the plugins can compute. The files are the same files (a WAV per stem plus `Mix.wav`, the selection exactly), exact to the sample however slow the machine, and usually done well before real time. Nothing new renders audio and nothing new schedules events: it is the playback path with another caller.

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

1. **Offline replaces the real-time run.** One export, not a choice between two. `⌘/Ctrl+⇧+R`, the plan (`Sequencer::audio_export_plan`), the folder, the file names, the selection as the length, the stems at unity and the mix at the sketch's levels, the click left out, no tails: all as built. What goes is in § What goes away. The real-time run could do one thing an offline render can't, play a plugin that needs real time (decision 7). Keeping it as a second path for that was turned down: two paths to keep equal, when being one path was the point of the original design.
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
7. **A plugin that won't switch.** One that refuses the offline setup is reloaded from its live state, as one that refuses a rate change is (`480` § Things to check). One that says it can't render offline at all (CLAP's `has_hard_realtime_requirement`: hardware-backed DSP) refuses the export, and the footer names it ("Can't export: <plugin> needs real time"). Rare, and the author has none; if one is ever reported, that is the time to think again.
8. **The block size is the running stream's buffer.** The run renders in blocks of the size playback used, so a plugin whose smoothing or modulation steps per block renders as it played. `schedule_delay` stays that one buffer.
9. **The writer waits and is waited for.** The tap's rings drop a block they have no room for and flag it (`130` § The tap), because the audio thread can't wait. The engine thread can: before a block it checks that every ring has room and yields until the `"audio-export"` thread has caught up. Nothing is ever dropped, and the "samples dropped" warning goes. The writer's 3 s stall timeout stays, as the guard for a run that stops advancing.
10. **No sound while it runs.** The device is closed for the run, and external gear on MIDI Out tracks is quiet too (decision 5). Live input already had no plugin target during an export (`LiveInputRouter::route`), so nothing that could be played is lost. The footer shows the progress; Esc cancels and removes the files, as now.
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
   - *Built 2026-11-14,* as-built in `130` § Audio export. Where it differs from the decisions, or adds to them:
     - **The writer starts the render** (decision 3). The `"audio-export"` thread, its files open, hands the engine an `OfflineRun` (`ExportRender`: the run's start instant, `advance(until)`, `is_done`), so the engine knows nothing of the export or the sequencer, and the handler nothing of the engine. `advance` is the step to the `"sequencer"` thread; `is_done` is `ExportShared::has_ended`.
     - **The run has a transport command of its own**, `TransportCommand::StartExport` (looping off, the seek with its chase, running). The step arm handles every transport command and event waiting before it plays a tick, so the run's seek, and any jump queued before the run, has reached the tracks first. (As first built a marker event followed the seek and a step was answered "not yet" until it came; `/simplify` replaced the marker, its flag and the retry with the drain.) `SetLoop` stays, for putting the loop back; the loop state is still saved and restored (the plugins are told looping is off, as they were).
     - **The marks count the run's own ticks** (start on tick 0, end a selection's length later), not the tracks' position from a seek: the files are the selection's length by construction. After the end mark nothing is stepped.
     - **The tap waits, not the engine** (decision 9). `ExportTap` pushes only for an offline block (`RenderCtx::offline`) and sleeps a millisecond at a time while a ring has no room. A run the writer gives up (`ExportShared::give_up`, after its 3 s) ends the wait and the render. The writer drains every 5 ms, not 100: at 100 the rings' second of slack capped the render at ten times real time.
     - **After `/code-review`:** the writer's 3 s count from the render's first step, with 20 s for it to start (the engine thread may be mid-restart when asked); a step unanswered for 2 s has the render give the export up itself; the engine thread's audio-thread mark is taken off after the run; and a MIDI Out track's playback doesn't advance at all during a run, so the stop after it has no wheel to reset on external gear.
     - **The block is the measured buffer**, capped at the engine's 4096 (`MAX_FRAMES`): a device running a larger buffer was already rendered in passes of that size.
     - **Nothing tells the UI the stream is down** during the run; the Audio tab is locked and the footer shows the export. The restart at the end reports as any other.
     - **Tests**: `plugin_host/offline_tests.rs` (a whole export on one thread: 600,000 frames for § Measured's case, exact off the block and sample grids at three rates, the same length at four block sizes, Esc mid-run and before the first block), the engine's loop with a recording source, the stepped ticks against the clocked ones, the step handler with a real `Transport`, the tap's wait.
     - **Tested by hand 2026-11-14** (the 2018 Mac mini, the Scarlett 4i4 at 48 kHz):
       - *§ Measured's project* (Serum 2, Vital, Spire; bars 1 to 6): 599,999 frames in every file at buffer 256 and at 2048, where the real-time runs gave 595,241 and 600,026. That is exact: the project's tempo is 624,999 µs a beat (a tapped or fitted tempo, shown as 96.0), which makes five bars 599,999.04 frames, not the 600,000 § Measured assumed. Rendered in 7.4 s and 7.2 s, about 1.7 times real time.
       - *Omnisphere and Kontakt* (`_test/Issue #31.stev`, seven bars at 58.9 bpm, five stems): 1,369,453 frames in every file, exact for its tempo (1,369,452.67), and the same on a second run. 28.5 s rendered in 4.4 s, about 6.4 times real time. No dropped notes heard, from the sample players either, with the plugins still told they run in real time.
       - *A MIDI Out track*: silent during the run, left out and named in the footer.
       - *Esc mid-run*: the files gone, the stream back.
       - The stream came back after every run (`RestartCause::Rendered` in the log), and playback after was as before.
     - **Not tested:** an export with no device open. A Mac with built-in output always has one to fall back on, so the case is hard to reach; the engine's loop is unit-tested with no device at all. Repro, Nexus and DUNE 3 were not tried.
     - **For decision 6:** nothing on the list dropped notes when rendered fast in real-time mode, so on this evidence offline mode buys nothing audible here. It is built all the same (§ Settled, 4): these plugins can't speak for other people's samplers.
3. **Offline mode.** The voice's activation mode beside its rate, VST3's `kOffline`, CLAP's `render` extension, the "Preparing export" panel, the fallback reload. The comparisons against a real-time export from the last build that had one.
   - *Built 2026-11-15,* as-built in `130` § The plugins' offline mode. Where it differs from the decisions, or adds to them:
     - **A voice carries an `Activation`**, the rate and the mode, where it carried a rate. What is wanted is the stream's rate and offline while `AudioExportAtomics::running` is set; the UI's per-frame comparison, the `Return`, the one-per-frame reactivation and the insert are `480`'s, unchanged. The mixer compares the mode with `RenderCtx::offline`, so it needs no state of its own for it.
     - **The writer waits for the plugins** (decision 6: "the run can't start on a voice that hasn't switched"). Holding a wrong-mode voice silent would only have written silence, so the UI publishes `plugins_offline` each frame (every loaded plugin in the mixer, offline, no restart under way) and the `"audio-export"` thread waits for it before asking for the render. No timeout as first built (30 s since the review, below); Esc ends the wait through a `cancelled` flag of its own, because with no render yet (and with no device open at all) no mixer block carries the closing marks to the writer.
     - **A VST3 plugin that refuses `kOffline` is not reloaded** (decision 7 said it would be, as for a refused rate). A fresh instance would refuse the same setup, and the failed reload would leave the track without its plugin. It is set up for real time again and renders the export as it did in phase 2, with a line in the log. The reload stays for a plugin that won't deactivate or activate.
     - **A CLAP plugin is told its mode while deactivated**, in the same round trip (§ Things to check asked whether it could be set live: the extension doesn't forbid it, but the round trip is needed anyway for the mixer to know the voice's mode, and deactivated is the safer place). It is told only a mode that differs from the last, so loads and rate changes never touch the extension. One that declines the mode still renders.
     - **`has_hard_realtime_requirement` is asked once, at load**, and any loaded plugin that has it refuses the export, in the selection or not: every plugin is switched, so every plugin counts. The refusal is the view's, before the command is sent.
     - **The panel is "Preparing export" while the run is on**; the switch back after it is an ordinary "Restarting audio".
     - **Tests**: the mixer rendering a voice only in blocks of its mode, the VST3 voice's `ProcessData::processMode` following a reactivation, the writer waiting for the plugins and never rendering a run cancelled meanwhile, the cancel flag.
     - **Tested by hand 2026-11-15** (the 2018 Mac mini, the Scarlett 4i4 at 48 kHz), three exports, read back from the files and the log:
       - *§ Measured's project* (Serum 2, Vital, Spire): 599,999 frames in all eight files, as in phase 2. Rendered in 7.3 s.
       - *`_test/Issue #31.stev`* (Omnisphere, Kontakt 8 and the rest): 1,369,453 frames in all six files, as in phase 2. Rendered in 4.4 s.
       - *`draft2`* (Omnisphere, Repro-5, Spire, Repro-1, Nexus): 1,590,794 frames in all eight files, 33.1 s rendered in 5.9 s.
       - Every plugin on the list took the switch both ways, VST3 (Omnisphere, Kontakt 8, Serum 2, Spire, Nexus, DUNE 3: a second `setupProcessing` with another mode) and CLAP (Repro-1, Repro-5, Vital), with no refusal, fallback or reload in the log.
       - **The two rounds cost about 0.2 s each** for seven to nine plugins (first reactivation to last), against renders of 4 to 7 s: no reason to leave any plugin out of the switch.
       - The render is no slower than phase 2's on the same projects, the stream came back after every run, and both panels showed.
       - *Against a real-time export*: the first Serum 2 stem of § Measured's project, from the 44-overrun real-time run (595,241 frames) and from this build (599,999). No difference heard; the level agrees too (RMS 0.106 against 0.105 over the file, second by second within what a free-running patch varies).
       - *Every CLAP plugin installed* (Surge XT, Repro-1, Repro-5, TAL U-No-LX-V2, TAL Mod, TAL BassLine 101, with Kontakt 8 and DUNE 3): 1,369,453 frames in all nine files, every plugin switched both ways, nothing declined. Whether any of them has the `render` extension that run's log didn't say; taking the mode is logged since ("took the Offline render mode").
       - *⌘H during the export*: the switch is over in about 0.2 s, too short to hide inside. Hiding and showing the app around an export crashed once, with the stack of the known AppKit crash on showing a hidden window (`NSViewUpdateVibrancyForSubtree`, winit#4820), after the export had ended (the crash report has the writer idle and the stream open again): not the export's.
     - **After `/code-review`:** the wait for the plugins gives up after 30 s with a reason for the footer ("the plugins didn't switch to offline rendering"), where it waited until Esc: the switch takes a fraction of a second, and one that can't happen should say so; the handler wakes the UI when the run starts, so the switch doesn't wait for the UI's next idle wake (up to 250 ms); and a CLAP voice records the render mode its plugin took, not the one asked for, so a plugin that declined offline isn't told real time again after the run.
     - **Retested by hand after both passes** (2026-11-15, § Measured's project): 599,999 frames in all eight files, the Serum 2 and Spire stems matching the earlier offline export to four decimals of peak and RMS; Vital (CLAP) took the offline render mode and the real-time one after, so the `render` extension's path has run; Esc mid-run left no files; and an export with Stev hidden (⌘H) ran to its end, the switch back taking about 110 ms a plugin instead of 15, the hidden window's frames being slower.
     - **Not tested:** an export with no device open (no voice can be asked back while nothing renders, so it would stop after the 30 s with the reason above).
4. **Tidy.** `130` § Audio export rewritten as built, `280`, the README, `000`'s thread table, the changelog; this file archived.

## Settled when agreeing (2026-11-14)

1. **Offline replaces the real-time run** (decision 1). No second path and no choice between two.
2. **A plugin that needs real time refuses the export**, named in the footer (decision 7).
3. **Silence during the run is accepted**, external gear on MIDI Out tracks included (decisions 5 and 10). Neither reaches the files today.
4. **Offline mode is built whatever phase 2 shows** (decision 6). A plugin that drops notes when rendered fast while told it is live does so silently, and the author's plugin list can't speak for other people's samplers. No release is cut between phases 2 and 3 unless phase 2's hand test is clean.
