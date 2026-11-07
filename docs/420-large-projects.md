# Large Projects

**Status: design agreed 2026-11-07, not built.** Stev should stay on time with a project far bigger than its author makes: no late note or click while playing, through a save, a recovery copy or an edit. The work that grows with the project moves off the `"sequencer"` thread, or stops growing with it.

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
2. **The save on `"storage"`.** Decisions 3 to 6.
3. **Plugin state shared.** Decision 7.
4. **Clips shared, the check cached.** Decisions 8 and 9.

Each phase ends with the probe run again at the target size.

## Testing

Each phase carries its tests as usual. The ordering in decisions 4 to 6 is testable through the harness, as `410`'s jobs are: a save behind a recovery write, an edit during a save, a quit behind one, a failed save. Decision 8's cache is tested against a full hash after every kind of edit. The probe is the hand test: the stress project playing, with no late tick past 1 ms.
