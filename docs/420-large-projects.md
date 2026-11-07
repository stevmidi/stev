# Large Projects

**Status: design agreed 2026-11-07; phase 1 (measure) done 2026-11-07; phase 2 (the save on `"storage"`) built and measured 2026-11-07; phases 3 and 4 not built.** Issue: #23. Stev should stay on time with a project far bigger than its author makes: no late note or click while playing, through a save, a recovery copy or an edit. The work that grows with the project moves off the `"sequencer"` thread, or stops growing with it.

## Background

From a discussion on 2026-11-07, after crash recovery (`410`) landed. The author's projects are small (the biggest is 3,716 events, a song in progress 469 notes), and they won't show what someone else's will do: an imported MIDI file, dense drum rolls, a wheel ridden through a whole song. Wheel and knob moves are events like notes, so a few minutes of riding adds thousands.

Several routines on the `"sequencer"` thread handle the whole project at once. `410` § Measurements timed them on synthetic projects:

| At 100,000 events | Time |
|---|---|
| ⌘S: snapshot, encode, write | 83 ms; 254 ms with 50 MB of plugin state |
| The recovery check's hash, every 5 s | 3.0 ms |
| The snapshot after a change, at most every 5 s | 2.8 ms; about 8 ms more with 50 MB of plugin state |

How late the `"sequencer"` thread can run before anyone hears it: each tick carries the instant it was meant for (`ClockTick { at }`). Plugin notes and the click are scheduled one audio buffer after it (about 5.3 ms at 256 frames, 48 kHz), and MIDI Out is held until the MIDI-out offset after it (5 ms by default, settable down to 0; `160`). The system's own timer jitter already takes up to about 4.5 ms of that under load (`160`). What is left for Stev's own work is about one tick.

## Decisions (2026-11-07)

1. **The target is 100,000 events and 50 MB of plugin state,** in 16 tracks, about 27 times the author's biggest project by events and 60 times by plugin state. Every decision below is measured against it.
2. **The budget is one tick,** kept over a looser one that would spare phase 4 (decided 2026-11-07). At the target size, nothing on the `"sequencer"` thread takes more than 1 ms in a pass, while playing, through a save, a recovery write and an edit. Anything that can't be made that small moves off the thread. The default MIDI-out offset then still covers the system's jitter, as `160` sized it.
3. **A save writes on the thread that writes the recovery copy.** The `"recovery"` thread becomes the project's one writer, `"storage"`, so a save and the copy's writes and deletes stay in one order. The `"sequencer"` thread takes the snapshot, as for the copy (`ProjectSnapshot`, plugin state raw), and hands it over. The encoding, the JSON and the disk happen on `"storage"`. Templates go the same way.
4. **A save is never dropped.** Of the recovery writes waiting, only the last runs (`410` decision 5); a save always runs, in its place in the order. On success it deletes the copy itself, after the file is safe.
5. **The project counts as saved as it was at the snapshot.** The fingerprint is taken at the handover and comes back with the result. `SavedProject` takes it only on success, so an edit made while the save was on its way still counts as unsaved, and a failed save changes nothing.
6. **What follows a save waits for its result.** Save then Quit, Save then Open and the like queue their action behind the save (`save_project_as`). While a save is on its way, a `ProjectAction` is held until the result arrives, then runs as it would have: on success it finds nothing unsaved, on failure it prompts again. A quit also waits for the `"storage"` thread to finish, as it does for the copy (`410` decision 13). Edits, playback and everything else carry on during a save.
7. **Plugin state is shared, not copied.** `InstrumentRef.state` becomes `Arc<[u8]>`. A snapshot then shares the blobs, and a captured state is made into an `Arc` once, where today the view's event, the sequencer command and the store each copy it.
8. **Clips are shared, and the check reuses what didn't change.** A clip's events become `Arc<Vec<Event>>`, edited only through `Arc::make_mut`. A snapshot shares every clip. The fingerprint keeps an `Arc` of each clip and its hash from the last time: a clip still pointing at the same allocation hasn't changed, since shared contents can't be edited in place, so its hash is reused, and only edited clips and the fields around them are hashed again. Holding the old `Arc` is what makes the pointer comparison sound: the allocation can't be freed and reused while the cache holds it. `make_mut` is the only way in, so no edit path can forget, and no dirty flag is kept, as the author prefers. A test checks that the cached fingerprint equals a full hash, as `fingerprint.rs` already checks the direct hash against the JSON one.
9. **What 8 doesn't fix is the single huge clip.** Sharing works per clip. A project whose events sit in one clip gains little: the first edit after a check copies the whole clip (the cache holds a second reference), and its hash is redone. The measurements say whether that needs more; splitting a clip's events into chunks would be the next step, not taken until then.
10. **Saving a plugin's state stays on the UI thread.** Both formats expect it on the main thread, and its cost is the plugin's own. Stev only controls how often it asks: a second after the last preset, knob turn or editor close (`410` decision 6), and once per plugin on ⌘S.
11. **Out of scope:** loading, which stops the transport and stages plugins already (`060`); undo memory; the agent bridge's reads. Phase 1's sweep may find others. They go in this doc if it does.

## Phases

1. **Measure.** A stress project at the target size, written by an ignored test into the library (`recovery_timing`'s synthetic project, saved as a `.stev`; its plugins won't load, but their state still travels). A temporary probe in the tick arm records how late each tick is handled (`now - at`), and the worst per second goes to the log. Played back with ⌘S, edits and recovery writes, it sets a baseline. A sweep times each `SequencerCommand` at the target size, to find whatever else passes 1 ms. Decisions 3 to 9 are checked against what it finds before they are built.
2. **The save on `"storage"`.** Decisions 3 to 6. Built 2026-11-07:
   - **The thread.** `"recovery"` became `"storage"` (`core/threads/storage.rs`), started by every app instance: one without the recovery lock keeps no copy but still saves. Its jobs are `StorageJob` (`core/project/writer.rs`): `WriteCopy`, `DeleteCopy` and `Save { snapshot, target, delete_copy }`, the target a project (`ProjectOrigin`) or a template (stripped there). `run_storage_jobs` takes what is waiting as one batch, dropping each of the copy's jobs a later one makes pointless as it gathers them, and runs every save in its place. A save's outcome (`SaveOutcome`, its error if any) goes back as `SequencerCommand::ProjectWritten`, which goes ahead of every hold, as the renames do.
   - **The handover.** `save_project_workflow` and `save_template_workflow` (`core/event_handlers/save.rs`) take the `ProjectSnapshot` (a template's without its clips, `ProjectSnapshot::template_of`) and send it through `OpenProject::storage` (`Storage`), the way to the thread; the `RecoveryKeeper` holds a clone of the same channel for the copy's jobs, so they and the saves share one order. The keeper says whether a project save deletes the copy (`deleted_by_save`: not while a crash's copy is on hold, nor in an instance that keeps none) and counts the copy gone from then; should the save fail, the next check writes it again. Every save, a template's too, waits in `OpenProject::saving` as a `PendingSave`, a project's with its fingerprint and origin; the outcome pops the oldest and, for a project saved, makes it `saved` (`SavedProject::saved_as`). A browser rename follows into a save on its way.
   - **What waits.** `project_action_workflow` holds every `ProjectAction` in `OpenProject::held` while `saving` isn't empty, a template's save included; once the last outcome is in, they run in order through `handle_sequencer_command`, so the take session's refusals still apply. The recovery check is skipped while a save is on its way (`OpenProject::tick`): the save deletes the copy once its file is safe, and a copy written behind it would only be deleted again. If the thread has gone, a save fails at once in the footer, so nothing waits on an outcome that won't come.
   - **Tests.** `writer.rs`: every save runs in order, a save deletes the copy written before it and a later write stays, a failed save keeps the copy, a template is stripped and leaves the copy. `save.rs`: nothing counts as saved before the outcome, an edit during a save stays unsaved, a failed save changes nothing, a quit behind a save waits and goes ahead, behind a failed one prompts again, held actions wait for the last save, the check waits, a template leaves the project as it was, a rename is followed, no writer fails at once. `recovery.rs`' save tests now go through the outcome.
   - **The sweep** times `SaveProject` and `SaveTemplate` as the handover alone, the snapshot and the fingerprint.
3. **Plugin state shared.** Decision 7.
4. **Clips shared, the check cached.** Decisions 8 and 9.

Each phase ends with the probe run again at the target size.

## Measurements

### The tools (phase 1)

- **The stress project** (`src/core/project/stress.rs`): `fill_stress_project` builds the 16 tracks, five back-to-back clips on each, the events in groups of ten (two notes, three bends, three parameter moves, the two notes ending) and the plugin state split over the tracks. `recovery_timing` builds its projects there too. `cargo test --release write_stress_project -- --ignored` writes the target, `Stress/Stress 100k.stev`, into the library. Its plugins point at a bundle that doesn't exist, so none loads, but their state travels.
- **The probe** (`src/core/threads/tick_probe.rs`, temporary): once a second while ticks arrive, `stev.log` gets the worst lateness of a tick's work (`now - at` after the tick is handled), how many ticks were past 1 ms, the longest any arm of the `select!` loop held the thread and which arm, and how many arms held it past 1 ms. The lateness includes the clock's own wake (a tick's `at` lies inside the timer's last interval, so up to about 1 ms of it is the timer's); the arm times are Stev's own work. It goes when phase 4 has been measured with it.
- **The sweep** (`src/core/event_handlers/command_timing.rs`): `cargo test --release command_timing -- --ignored --nocapture` times each `SequencerCommand` through the handlers on a fresh stress project, five rounds, with its undo and redo when it records a step; marquee commands cover every track and the whole project. Its module docs list what it leaves out: the capture commits (they need a take), tap tempo, the commands that touch the user's files or settings, the audio export and the agent bridge. Save and Save as template are timed as their work against a scratch file.

### The sweep (2026-11-07)

A release build on the Intel Mac at the target size, median and worst of five, in ms. Everything not listed stayed under 0.3 ms, apart from single worst rounds a little past 1 ms with a median far under it (noise). Playback itself is cheap: `Sequencer::tick` took 0.00 ms in the median and 0.09 at worst over 20,000 ticks.

| Command | Median | Worst | Undo | Redo |
|---|---|---|---|---|
| Save (`ProjectData`, pretty JSON, write, `SavedProject`) | 360 | 373 | – | – |
| Save as template | 177 | 215 | – | – |
| Merge clips (⌘J, all) | 155 | 174 | 11.0 | 6.5 |
| New project (⌘N) | 11.7 | 13.6 | – | – |
| Duplicate time (all) | 10.4 | 12.1 | 3.4 | 2.7 |
| Duplicate clips (all) | 10.4 | 10.7 | 3.9 | 2.7 |
| Paste clips (all) | 6.6 | 10.0 | 10.3 | 6.4 |
| Move range (all, a bar on) | 6.6 | 8.1 | 6.2 | 6.3 |
| Cut clips (all; scoped alike) | 3.1 | 4.3 | 6.6 | 3.1 |
| Copy clips (all; scoped alike) | 3.2 | 3.9 | – | – |
| Remove clips (all) | 0.75 | 0.94 | 7.1 | 2.9 |
| Delete time (a bar) | 1.9 | 1.9 | 2.3 | 1.7 |
| Split clips (all, mid) | 1.3 | 2.1 | 0.6 | 0.7 |
| Paste notes (a 1,250-event clip) | 1.2 | 1.3 | 0.1 | 0.7 |
| Set track output (undo puts 3 MB of state back) | 0.19 | 0.20 | 1.7 | 0.17 |
| A captured plugin state (3 MB) | 0.60 | 1.5 | – | – |

What it says against the decisions:

- **The save is the worst by far,** at 360 ms: more than the 83 to 254 ms of `410`, since the `.stev` is pretty-printed and the copy isn't, and it adds `SavedProject`'s fingerprint. Decisions 3 to 6 stand.
- **Edits over a whole project cost milliseconds**, all of it copying clips and their events: every marquee edit over the whole project passes 1 ms, as do the undos that put clips back. Decision 8's shared clips would make a copy of an unchanged clip a pointer, which should cover copy, cut, paste, remove and their undos; duplicates and moves still write their clips' new positions. To be checked again after phase 4.
- **Merge clips is out of line** at 155 ms for 6,250 events a track, against 10 ms for duplicating the same clips: something in it grows faster than its events. Not one of decisions 3 to 9; to look into on its own.
- **A new project** takes 12 ms, most likely dropping the old project's clips and state on the thread. Loading is out of scope (decision 11), but ⌘N isn't a load; to look into with merge.
- **A captured plugin state** (`SetTrackInstrumentState`) copies its 3 MB, as decision 7 says; it passed 1 ms once.

### The hand run (2026-11-07)

The stress project written by `write_stress_project` (a 90 MB `.stev`), opened in a release build on the Intel Mac, played, saved with ⌘S, edited, with the recovery check running. Two minutes of the probe's lines:

| What | Longest arm (ms) | Worst late (ms) |
|---|---|---|
| Nothing but ticks, clean or edited | 0.0–0.15 | 1.0–1.2 |
| The recovery check, project unchanged since the save (every 5 s) | 2.8–3.8 | up to 3.3 |
| The recovery check writing a copy, after edits (every 5 s) | 12–24 | up to 24 |
| ⌘S | 364 | 364 |
| ⌘S again, a second later | 448 | 449 |
| Opening the project (a load, out of scope) | 206 | 206 |
| The edits themselves | at most 1.5 | – |

- **The floor is the clock, not Stev.** With nothing else running, a tick's work finishes about 1.0 ms after its `at`: the timer's interval, as the probe's docs expect. While playing, 10 to 60 ticks a second finish just past 1 ms (1.0 to 1.2), with the arms under 0.2 ms. That is the timer, so the budget is read from the arms, not from that count.
- **The recovery check is over budget every 5 s,** even with nothing to write: 3 ms for the fingerprint, as `410` measured synthetically. Once the project differs from the save, each check also takes the snapshot, 12 to 24 ms with its 50 MB of state. Decisions 7 and 8 are aimed at exactly these two.
- **⌘S holds the thread for 364 ms,** as the sweep found. Decisions 3 to 6.
- **No audio overrun was journalled during the run.** The plugins didn't load, so the audio thread had nothing to render; MIDI Out and the click were the only paths a late tick could be heard on.
- **Nothing new beyond the sweep** came up: the hand-made edits stayed under 1.5 ms.

Phase 1 is done: decisions 3 to 8 stand as written, and the sweep added merge clips and ⌘N, which no phase covers yet.

### Phase 2 (2026-11-07)

The sweep, release build on the Intel Mac at the target size, median and worst of five, in ms:

| Command | Median | Worst |
|---|---|---|
| Save, the handover (snapshot and fingerprint) | 11.3 | 15.9 |
| Save as template, the handover (snapshot) | 9.0 | 13.4 |

The template's row was taken before its snapshot left out the clips (`ProjectSnapshot::template_of`), which it now does; not measured again.

From 360 and 177 ms, but still past the budget: the snapshot copies every clip's events and the 50 MB of plugin state, and the save hashes the project besides. Those are what phases 3 and 4 are for (decisions 7 and 8), the same cost the recovery check's copy write shows.

The hand run (2026-11-07): the stress project opened in a release build on the Intel Mac, played, saved with ⌘S, the recovery check running.

| What | Longest arm (ms) | Worst late (ms) |
|---|---|---|
| Nothing but ticks | 0.0–0.09 | 1.0–1.7 |
| The recovery check, project unchanged since the save (every 5 s) | 2.8–3.6 | up to 3.6 |
| ⌘S, the handover | 20 | 21 |
| Opening the project (a load, out of scope) | 210–218 | 218 |

- **⌘S holds the thread for 20 ms, down from 364.** The writing no longer shows on the `"sequencer"` thread at all. The 20 ms is the snapshot and the fingerprint, more than the sweep's 11 to 16 with the app's other threads running; phases 3 and 4 are aimed at it.
- **One second, two after the save, had a tick 17.5 ms late with no arm past 0.06 ms** (and 1,466 ticks, the clock catching up): the thread was kept from running, or the ticks came late, with none of Stev's own work on it. It isn't the save's: phase 1's hand run had the same, 23.9 ms late with no arm past 0.07 (`20:39:14` in that run's log), with no save near it. Something outside Stev's arms, to look into on its own; the probe can't say what.
- **The recovery check stays over budget every 5 s,** as phase 1 found: phase 4.

## Testing

Each phase carries its tests as usual. The ordering in decisions 4 to 6 is testable through the harness, as `410`'s jobs are: a save behind a recovery write, an edit during a save, a quit behind one, a failed save. Decision 8's cache is tested against a full hash after every kind of edit. The probe is the hand test: the stress project playing, with no late tick past 1 ms.
