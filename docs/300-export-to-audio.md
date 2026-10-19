# Export to Audio — Phase Plan

**Status: phases 1–3 built** (drafted 2026-10-18, phases 1 and 2 2026-10-18, phase 3 2026-10-19). This is the build plan for `280-roadmap.md` § Export to audio. The decisions agreed there on 2026-10-11 are taken as given and not argued again here, with one exception: one real-time pass, a folder per export, `NN Name.wav` stems plus `Mix.wav`, 32-bit float WAV at the device rate, plugin tracks only, stems at unity gain and centred, mute and solo honoured, the click never exported. The exception, changed 2026-10-18: **the arranger's time selection sets what's exported**, replacing "bar 1 to the arrangement's end plus a fixed tail". A fixed tail would cut a long delay or reverb, so the user owns the length: every file starts at the selection's start and ends at its end, and leaving room for the tail is part of making the selection. Once the feature lands, its as-built behaviour moves into `130`, `060`, `010` and `000`, and this file is archived.

## How it works

**The export is an ordinary playback run with a tap on the instrument mixer.** Nothing new renders audio. `InstrumentMixer::render_into` (`130`) already renders each voice into its own buffers (the render pass) before the summing pass adds them, gained, into the engine's mix. That gives the tap two points:

- **Stem:** a voice's raw `sample(ch, f)`, before its gain is applied. That's unity gain and centred, as decided.
- **Mix:** the gained sum of the exported voices. This is the mixer's own contribution to the engine mix, so the metronome is left out by construction: `MetronomeSource` is a separate `AudioSource` and never passes through the tap. A forgotten `K` can't reach `Mix.wav`, and no special case is needed.

The tap runs before the engine's ±1.0 safety clamp, so the files carry float levels that were never clipped, as decided. One consequence: where the live output hit the clamp, `Mix.wav` is cleaner than what was heard. That's the right way round.

**The audio thread never touches a file.** Samples cross to a new `"audio-export"` thread on preallocated `rtrb` rings: one per engine slot plus one for the mix, each holding about a second of stereo float audio, all built once at plugin-host start-up. That's about 6 MB at 48 kHz, for 16 slots plus the mix. Nothing is allocated or freed on the audio thread when an export starts or ends. If the writer can't keep up and a ring fills, the mixer drops samples and sets a flag, and the result reports it (phase 3). The writer drains every ring into its WAV file and fixes the headers at the end.

**Sample-exact alignment comes from the existing scheduling.** The sequencer already stamps each tick with its intended `Instant` (`EventTime::At`), and the mixer maps that stamp onto the `steady` frame timeline with `RenderCtx::scheduled_frame`. The export's start and end go through exactly the same path:

- **Start:** the sequencer sends `ExportMark::Start { at, slots }`, carrying the selection start's `Instant` and the engine slots that get a stem.
- **End:** the sequencer sends `ExportMark::End { at }`, carrying the selection end's `Instant`.

The mixer turns both into frames the same way it does a note-on. So the selection's first tick lands at sample 0 of every file, and a note at any tick lands where the plugin played it. The marks travel on their own small `rtrb` ring from the sequencer thread to the mixer, next to the clip feed. They're `Copy` and carry no heap payload (the slots are a `SlotMask` bit set). `Sequencer::tick` sends them, counting the ticks on the tracks' own position from the run's seek (`reset_to_tick`), not the transport's: a clock tick that slips in before the seek reaches the tracks plays where they were and must not start the files.

**The run is a sequencer workflow, like the take session's** (`290`). `⌘/Ctrl+⇧+R` (Ableton's Export Audio/Video chord; it's free here) works in the arranger. In the clip view it refuses with "Select the time to export in the arranger". It runs these steps:

1. Build the plan, or refuse.
2. Stop the transport, then turn looping off and seek to the selection's start. The loop state is saved first, so it can be restored at the end. The cursor needs no saving: the run plays with `PlayFromTick`, which never moves it.
3. Tell the writer to open the files.
4. Play.
5. Send `Start` at the selection's start tick and `End` at its end tick.
6. Stop the transport once the writer has finished the files (`SequencerCommand::AudioExportWritten`), a moment past the end tick; the files themselves end exactly on the `End` frame. Stopping right at the end tick would send its note-offs ahead of the `End` frame, which sits a buffer later. Until phase 4, whatever is still ringing at the end is cut, so the selection has to leave room for the tail; phase 4 lets it ring out to silence instead.
7. Restore the saved loop state. The stop returns playback to the cursor.

The writer reports back with a `UiEvent::Status` for the footer. The export is not an edit, so there's no undo. Edits never move the transport (`220`), but the export isn't an edit: it is a transport run, and it puts the transport back the way it found it.

**Off macOS**, `⌘/Ctrl+⇧+R` says "Audio export needs the plugin host (macOS)" in the footer. The plan and the WAV writer are cross-platform pure code, but the tap and the writer thread sit behind the same `cfg` as the plugin host.

## Phase 1: the pure parts (WAV writer, names, plan)

There's no audio and no UI in this phase, only code that tests can pin down. It is small, and there's nothing to try by hand.

- **`core/audio/wav.rs`: a streaming WAV writer**, next to the engine whose samples it writes, hand-rolled like `smf.rs`, with no dependency. It writes `WAVE_FORMAT_IEEE_FLOAT` (format tag 3), 2 channels, 32-bit, at the device rate, plus the `fact` chunk that non-PCM WAV requires. The writer streams frames as they arrive. `finish()` seeks back and patches the `RIFF`, `data` and `fact` sizes. A file that would pass 4 GiB stops at the limit. That's about 3 hours of stereo at 48 kHz, so the limit is a guard, not a feature. Tests:
  - The header bytes of a known short file, field by field.
  - The size patching.
  - A round trip through a tiny test-only reader.
- **`storage`: names and folders.**
  - Every export goes in one `Exports` folder in the library, beside `Projects` (`paths::exports_dir()`, `~/Documents/Stev/Exports`; moved there from the projects root after phase 2, 2026-10-18, so the browser needs no rule to hide it). The first export creates it. It works the same for a project saved loose in the Projects root or not saved at all, since nothing goes inside a project's own folder.
  - The `base` of the folder name comes from the open project's name, else its folder's, else `project`, the same fallback as `clip_export_name`.
  - `audio_export_dir_name(base, tempo, meter, start_bar, taken)` gives names like `Sketch 120bpm 4-4 bar 9`. The start bar tells the DAW user where to drop the files; a selection from bar 1 still says `bar 1`. A name that's taken gets ` 2`, ` 3`, … appended; the folder is never overwritten, the same rule as `clip_export_name`.
  - `stem_file_name(n, name)` gives `NN Name.wav`. `NN` is the 1-based, zero-padded track number. `Name` is the track's own name, or for a track never named, its plugin's (`InstrumentRef::display_name`): the header's bare number would add nothing to `NN`. The characters a filesystem refuses (`PROJECT_NAME_FORBIDDEN`, control characters) become `_`, and a blank name leaves `NN.wav`.
  - The mix file is always `Mix.wav`.
  - Tests cover the numbering, padding, sanitising, the x/8 meter in the folder name, and the taken-name suffix.
- **`Sequencer::audio_export_plan(selection)`** takes the arranger's time selection (as the MIDI clip export's `time_bounds` does) and returns either `AudioExportPlan { stems: Vec<StemPlan { slot, file_name }>, start_tick, end_tick, left_out: Vec<String> }` or `AudioExportRefusal`. The rules:
  - A track gets a stem when its output is an instrument and it's audible under mute and solo (`track_audibility`, the same rule the mixer uses). Whether its plugin actually loaded is the view's knowledge, not the sequencer's, so a missing voice is phase 2's to handle: its stem comes out silent and the footer names it.
  - `left_out` holds the positions of the MIDI Out tracks that would otherwise have been heard, so the footer can say they were skipped.
  - `start_tick` and `end_tick` are the selection's tick range, exactly. The selection's tracks don't narrow the export: mute and solo choose the tracks, the selection only chooses the time.
  - It refuses with `NoSelection` when there's no time selection ("Select the time to export"), and with `NothingToHear` when no track qualifies or no unmuted clip on one overlaps the selection.
  - Tests: mute, solo, a mix of MIDI Out and plugin tracks, a selection on only some tracks (all audible ones still export), a selection with no clips in it, no selection, and an empty project.

## Phase 2: the export, end to end

**Built 2026-10-18.** As built, beyond what's below: the writer reports back to the sequencer (`SequencerCommand::AudioExportWritten`), which ends the run and writes the footer; the writer drains every 100 ms and clears its shared flags once an export is finished (not when the next starts, when its start mark may already have armed the mixer); the handler makes the folder before the run and removes it, still empty, if the job can't be sent (no plugin host); ring overflow is already reported (`samples were dropped: listen before you use it`); a second `⌘⇧R` during a run refuses ("An export is already running").

This is the first phase to try by hand. `⌘/Ctrl+⇧+R` writes a real stem set. The guard rails come in phase 3, so during a phase-2 export you just don't touch anything.

- **Remove the `expect(dead_code)` markers** phase 1 left on `mod wav` (in `core/audio`), `mod audio_export` and the `Exports` items in `storage.rs`; with the export wired up they are fulfilled no longer.
- **The tap in `InstrumentMixer`.** It's armed with the plan's slot mask and the two frames resolved from the marks. A pure helper, `export_span(start, end, block_start, frames) -> Option<Range<usize>>`, gives the part of a block that's inside the export, so a block can be cut part-way through at either end. It's tested on its own, including a span that starts and ends inside a single block. The summing pass pushes each exported voice's raw samples to its slot's ring, and its gained samples into a mix scratch. The scratch then goes to the mix ring. A voice that was asleep this block, or whose render failed, pushes silence, so every file keeps the same length. A unit test with a test-only `InstrumentVoice` fake checks two things: stems come out at unity while the mix is gained, and a muted slot pushes nothing.
- **The `"audio-export"` thread** is spawned with the plugin host and parks until it's told to start. It opens one `WavWriter` per stem plus `Mix.wav`, drains the rings, finishes the files when the mixer reports the `End` frame, and reports `Exported "<folder>" — N stems + mix` to the footer. If a write fails, it says so and leaves what it wrote in place.
- **The run** (§ How it works) goes in `event_handlers/audio_export.rs`. That keeps it out of `project.rs`, which already holds the MIDI clip export. The marks are sent from the tick path in `playback.rs`, where each tick's `Instant` is known.
- **The binding:** `InputEvent::ExportAudio` → `SequencerCommand::ExportAudio`, bound in `Display` next to `⌘/Ctrl+⇧+E` (the project name is view state). It goes into the help overlay's `HELP` table.
- **Docs in the same change:**
  - `010`: the binding.
  - `060`: the `Exports` folder, next to the `Clips` bullet in § On-disk layout.
  - `130`: the tap, and the "mix leaves out the click" rule.
  - `000`: the `"audio-export"` thread.
  - `CHANGELOG.md`: a line under `[Unreleased]`.
- **How to check it by hand:**
  - Select a few bars past the last note, export, and drop every file at the bar the folder name gives, in a DAW project at the same tempo. The stems should line up with each other and with the DAW's grid at the downbeat, and a long reverb should ring out inside the files.
  - Select from the middle of a held note: note chase should sound it from sample 0, as playback does from the cursor.
  - Solo one track: you should get one stem plus a `Mix.wav` that matches it.
  - Turn `K` on: the click must not be in the export.

## Phase 3: guard rails

**Built 2026-10-19.** As built, beyond or instead of what's below (the details are in `130` § While an export runs):

- **Looking around still works.** `EnterClip`, `ExitClip` and `FocusPane` go through (`Shift+Tab`, `Tab`, a click on a pane), and so does `K`: the click never reaches the files. Everything else, quitting included, answers "Finish or cancel the export first". A live take refuses to start an export ("Stop recording first").
- **Esc stops the sound once the writer is done**, a tenth of a second or so, not at once: the cancel sends the run's missing marks stamped now, the mixer ends the files on its next block, and the writer's report stops the transport as for a finished run. Stopping at once would need a transport command the guard can tell from the user's own `Stop`. In the arranger the marquee keeps its `Esc` only when no export runs.
- **No start message.** The progress line replaces "Exporting …"; it shows whenever no passing message does. It's driven by milliseconds (`AudioExportAtomics::written_ms`, `total_ms`) rather than a frame count, so the view needs no sample rate.
- **Each export has an id**, carried by its start mark and its job, and the mixer reports the end by id. A writer that gave up on an export can't then read that export's late end as the next one's, and it empties the rings before the next job. The mixer, not the writer, clears the silent-slot and overflow flags, at each start.
- **A device change is noticed through the stream.** The engine never follows a device or rate change (it keeps the rate it opened at), so what the export can see is the stream erroring (`AudioLoad::stream_errors`, which counts every error that isn't an xrun) or going quiet (3 s with nothing from the mixer). Either stops the run, removes its files like a cancel, and says "Export stopped: the audio device changed" (or "… the audio output stopped").
- **Only dropped samples are a warning.** An overrun or xrun glitches what's heard, not the files, which take the samples before the device (checked by ear 2026-10-19: a run with both left `Mix.wav` clean). The result notes them plainly: `Exported "Sketch 120bpm 4-4" — 4 stems + mix (2 overruns while playing)`. "listen before you use it" is kept for ring overflow.
- **A plugin that goes missing mid-run** is noticed per block (`tap_missing`), not only at the start, and the footer names its stem as for one never loaded.

This phase makes the run safe to leave alone or to interrupt.

- **Everything else is blocked during the run**, the same way the take session does it (`take_framing.rs`). Edits, transport keys, `/`, mute, solo, volume and pan, output changes and project actions all refuse, with "Finish or cancel the export first". If they didn't, a mute or fader change halfway through would make the stems disagree with `Mix.wav`.
- **Live input is not routed to plugins during the run.** The export is the arrangement, and playing along would make the result depend on whatever the keyboard did meanwhile (decided 2026-10-18).
- **Esc cancels.** The run stops the transport, the writer drops its files and removes the folder, the loop state and cursor are restored, and the footer says "Export cancelled".
- **Progress in the footer:** `Exporting… 0:42 / 2:10 — Esc cancels`. It's driven by an atomic frame counter the writer bumps (no lock on the render path), and the UI is woken through `repaint_ctx`.
- **Honest results.** The final message reports any overruns or xruns during the run (the `OVR` latch clears on transport start, so the counts at the end belong to the export) and any ring overflow. For example: `Exported "Sketch 120bpm 4-4" — 4 stems + mix (2 overruns: listen before you use it)`.
- **Changes during the run:** if the output device or its sample rate changes, the export stops and says why. If a plugin is removed by its own crash or reload, its stem keeps going as silence and the footer names it.
- **Docs:**
  - `010`: Esc during an export.
  - `130`: the blocking and live-input rules.

## Phase 4: knowing when it's silent

A fixed wait can't know when a delay or reverb has finished: it's too short for a long hall, and wasted time for a dry pluck. So the export measures the output itself. That works the same for CLAP and VST3: VST3 has no sleep status (`180`), but every format produces samples. The same detector serves both ends of the run (split out as its own phase, 2026-10-18).

- **The detector, in `InstrumentMixer`.** In the summing pass the mixer already reads every sample of every voice. While an export is armed, it also tracks each exported slot's peak per block, measured on the raw stem (before gain, so a low fader doesn't count as silence), and records `last_loud_frame[slot]`: the last frame whose absolute value was above the threshold. A sleeping voice (CLAP) is silent by definition and costs nothing. These are `AtomicU64`s in the mixer's shared state, written only by the audio thread, so there are no locks and no allocation. A slot counts as **quiet** once `now - last_loud_frame >= hold`. A pure helper `is_quiet(last_loud, now, hold)`, plus a per-block `block_peak` and threshold test, are unit-tested on their own.
- **Before the start: wait for a clear path.** Between the stop (step 2) and the play (step 4), the run waits until every exported slot is quiet, checking the atomics every ~10 ms on the event-handler thread (never on the render path). The footer says `Waiting for silence… — Esc cancels`. Most of the time the wait is a fraction of a second.
- **After the end: ring out to silence.** The `End` mark becomes `End { at, ring_out: true }`. From the selection end onwards the mixer keeps the files open until every exported slot is quiet, then ends all of them on the same frame: the last loud frame of the latest track, plus the hold. Every file is still the same length. Because the mixer decides the end frame itself on the audio thread, it's exact, and nothing has to poll for it. The selection then means "where the music stops"; selecting extra bars still works and only adds silence before the ring-out.
- **Caps, because some plugins never go quiet.** An analogue-modelled synth with a noise floor, or a plugin with a free-running LFO, may never drop below the threshold. Both waits have a cap. The pre-roll wait starts anyway after the cap, and the tail is cut at the cap. The footer names the tracks that didn't go quiet: `Exported … (Track 3 never went silent: tail cut at 30 s)`.
- **Docs:**
  - `130`: the detector and its atomics.
  - `280`: remove § Export to audio's "Suggested start" and the "Tails that end on silence" item under § Later.
  - Archive this file.

**Starting values, to tune by ear** (constants in the mixer, no setting):

| | Value | Why |
|---|---|---|
| Threshold | −90 dBFS peak | Below any audible tail and 16-bit dither, above denormal noise (flushed anyway, `000`) |
| Hold | 2 s | Longer than the gap between a slow delay's repeats, so a tail isn't ended between two echoes |
| Pre-roll cap | 10 s | Long enough for a big hall reverb to die |
| Tail cap | 30 s | Same, with room for long feedback delays |

The hold is the real trade-off: too short and a sparse ping-pong delay ends early, too long and every export carries more silence at the end. 2 s errs on the safe side.

## Decisions

All decided 2026-10-18:

- **The time selection sets what's exported**, not bar 1 to the end. No fixed tail: until phase 4 the length is the user's to choose, and from phase 4 the files ring out to silence past the selection end.
- **Exports go in one `Exports` folder** in the library, beside `Projects` (first beside `Clips`, hidden from the browser; moved after phase 2).
- **Live input is blocked during an export.** The export is the arrangement only; recording a live performance over it would be a different feature.
- **No stem for a plugin track with no clips in the selection.** Silence adds nothing in a DAW.
- **No fixed settle.** The detector in phase 4 decides when the path is clear and when the tail is done.

## Tests summary

| Phase | Where | What |
|---|---|---|
| 1 | `audio/wav.rs` | Header bytes, size patching, round trip, 4 GiB guard |
| 1 | `project/storage.rs` | Folder name, ` 2` suffix, stem names, sanitising |
| 1 | `sequencer/` (plan) | Track choice under mute/solo/output, `end_tick`, refusals |
| 2 | `plugin_host/mixer.rs` | `export_span` edges; stems at unity vs gained mix with a fake voice |
| 3 | `event_handlers` | Commands blocked during a run; Esc restores the loop state and cursor |
| 4 | `plugin_host/mixer.rs` | `block_peak` / threshold, `is_quiet` across the hold, the ring-out end frame (latest slot wins), both caps |
