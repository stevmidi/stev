# Crash Recovery

**Status: design agreed 2026-11-06, measured, not built. Issue: #12.** If Stev crashes or is force-quit, the next launch offers to bring back what wasn't saved. While a project has unsaved changes, Stev keeps a recovery copy of it in its own support folder, never in the project file.

## Background

From a discussion on 2026-11-06. The unsaved-changes prompt (`060` § Unsaved changes) only covers a quit Stev gets to see. The safe save (`storage::replace_file`, `8e5c76e`) protects the last save during a save, but not the work since then.

This is not autosave. Autosave was rejected because it would overwrite the saved project with throwaway changes (`060`). A recovery copy is never written over a project file, and it is deleted on any clean exit.

Nothing can run at the moment of a crash. The crash that prompted this, showing the app after ⌘H (winit#4820), is a `SIGILL` out of AppKit that never reaches a Rust panic hook. So the copy has to be on disk already.

## Decisions (2026-11-06)

1. **A copy exists exactly when the quit prompt would ask.** One rule covers every case: a project with changes has a copy, a project without has none. A new project or a template left alone has none, and undoing back to the saved state deletes it. `SavedProject::has_changes` decides, as it does for the prompt.
2. **One slot.** One copy, in `config_dir()`, written with `replace_file`. It holds the `ProjectData` and where the project came from (its folder and name, nothing for a project never saved). It is not a `.stev` the browser lists.
3. **When: a timer that compares, not hooks.** Every 5 s the tick arm of the `"sequencer"` loop compares the project's fingerprint with the last copy's, and writes only if it differs. No idle detection and no transport trigger: the loop can run for an hour while takes are committed with `/`, so a rule that waits for a stop or skips playback would miss the work that matters most. A timer needs no hook, so no path that changes the project can be forgotten. At most about 5 s of edits are lost. Settled by the measurements: with decision 4's hash the check is cheap enough that the debounce isn't needed.
4. **The check must cost well under a tick of the 1 ms clock, during playback, so the fingerprint hashes the project directly.** The fingerprint as it stood took a snapshot and walked it through JSON: 1.65 ms on the biggest real project, over budget before any growth. Skipping the blobs wouldn't have been enough, since the JSON is most of the cost. A hash straight from the sequencer took 0.06 ms there, so it replaces the JSON walk, and `SavedProject` and the check share it, as decision 1 needs. It has to cover everything the fingerprint covers today, not only the events, and leave the blobs out as before; tests prove the two agree on what counts as a change.
5. **The write has a thread of its own, `"recovery"`.** A write needs every blob, since the copy is a whole project, and during steady editing one can come every 5 s. On the biggest real project it took 5.4 ms, 8.65 at worst, with no audio overrun and no audible glitch, and it grows with the project (83 to 254 ms at the synthetic stress sizes). But size isn't the main risk: a disk can stall for reasons of its own, and that must not land on the thread that drives playback. So the `"sequencer"` thread only takes the snapshot, with the plugin state as raw bytes (the copy measured 0.09 ms), and hands it over; the encoding, the JSON and the disk happen on `"recovery"`. `from_sequencer` is split so the encoding can move. Every write and every delete of the copy goes through the same channel to this one thread, so they run in order and a write in flight can't land after a delete and bring the copy back. The last copy's fingerprint is recorded at the handover. A failed write logs a warning and sets a flag the next check reads, so it writes again 5 s later whether or not the project changed. A save itself stays on the `"sequencer"` thread.
6. **Plugin state is captured on events, never polled.** A plugin added since the last save has an empty state blob (`view/display/instrument.rs`), so without this a restored project plays its clips through the plugin's init patch. The view also captures a plugin's state when it reports a preset load (`ParameterGesture::ValuesChanged`) and when its editor closes. A sound changed with the editor still open is not caught.
7. **A captured blob rewrites the copy.** Blobs stay out of the fingerprint (`060`), so the comparison in decision 3 also counts captures since the last copy. A capture never makes a project count as changed by itself: a sound tweaked in a saved project with nothing else changed still has no copy, as the quit prompt doesn't ask either.
8. **The capture buffer is left out at first.** A take played but not committed isn't brought back.
9. **The offer is a launch dialog.** A `ProjectDialog` over the empty startup project, on the first launch after a crash only, with Discard and Reopen: `Stev didn't close properly.` and then `Reopen your unsaved changes to "<name>"?`, or `Reopen your unsaved new project?`. Not a footer prompt or a RECENT row. A file opened from Finder at launch waits behind it, as it does behind any dialog (`keyboard_taken`): after Discard it opens, and after Reopen it asks the usual unsaved-changes prompt first. The dialog has to be up by the first frame, when a launch file replaces the startup project (`060` § Opening from Finder), so it is set up at startup, not by a later event from the `"sequencer"` thread.
10. **Reopen keeps the project's name and folder.** ⌘S overwrites the file, as after any edit, and the file on disk stays untouched until then. If the file has gone, or the project was never saved, it reopens untitled. It goes through the usual staged load, so its plugins restore as on any open.
11. **A reopened project always counts as changed.** It takes no baseline to compare against, so the quit prompt asks until it is saved. Comparing with the file on disk wouldn't do: a copy that differs only in a plugin blob has the same fingerprint, and the next check would delete it.
12. **The dialog and one footer line are the only signs.** `Reopened unsaved changes to <name>`, once. Stev has no unsaved marker and this adds none.
13. **The copy outlives Reopen.** It goes on a save, on Discard, or on a clean quit (Save or Don't Save, in `ProjectAction::Quit`). Each is a delete sent to the `"recovery"` thread (decision 5), after any write already queued. On a quit the `"sequencer"` thread then waits for that thread to finish before the process exits, or a copy left behind would offer a false `Stev didn't close properly.` on the next launch. So if a plugin crashes during the restore, the next launch offers it again, and Discard is the way out.
14. **Nothing touches the copy until the offer is answered.** The startup project has no changes, so the check would otherwise delete it.
15. **A copy that won't load** says why in the footer, as a failed open does (`060`), and the next clean quit removes it.
16. **A second instance stays out.** The running instance holds a `File::try_lock` on a lock file beside the copy. An instance that can't get it neither offers nor writes. The lock goes when the process dies, so a copy with a free lock means a crash. Two instances are rare, since macOS brings a running app bundle to the front, but `open -n`, a bare binary or a `cargo run` beside the app all make one, and without the lock it would take the first one's copy for a crash. `stev --mcp` (`360`) is a second process from the same binary on every agent session, and it skips recovery entirely: it takes neither the lock nor the copy, or starting it before the app could turn recovery off. The lock is taken on the app's startup path only.

## Phases

1. **Measure, then the copy.** The measuring is done (§ Measurements) and settled decisions 3 to 5. Build the direct fingerprint, the check, the `"recovery"` thread with its write and deletes, the wait on quit and the lock. The ordering is tested apart from the thread, so tests needn't wait on it.
2. **The offer.** The dialog, Reopen with its name, the footer line, the always-changed baseline.
3. **Plugin state on events.** The captures of decision 6, and decision 7.

## Measurements

The synthetic half of phase 1, from `recovery_timing` (`src/core/project/recovery_timing.rs`), on 2026-11-06: a release build on the Intel Mac, 16 tracks, the median of 11 rounds in ms. Caches are warm and no audio plays, so these are a floor.

| Events | Blobs | File | Check as it stands | Direct event hash | Write |
|---|---|---|---|---|---|
| 10,000 | none | 2.4 MB | 2.1 | 0.15 | 7.3 |
| 100,000 | none | 23.5 MB | 19.3 | 1.5 | 83 |
| 200,000 | none | 47.1 MB | 37.9 | 2.8 | 159 |
| 10,000 | 50 MB | 69 MB | 46.9 | 0.15 | 170 |
| 100,000 | 50 MB | 90 MB | 57.1 | 1.4 | 254 (worst 338) |

- **Both grow in a straight line**: the check with events, about 0.19 ms per 1,000, and the write with events and blobs together.
- **Skipping the blobs is not enough.** At 100,000 events the blob-free check takes 19 ms, 16 of it the fingerprint streaming JSON into the hasher. It passes 1 ms at about 5,000 events.
- **Hashing the events directly** (no snapshot, no JSON) is 13 times cheaper, and doesn't depend on the blobs. It still takes 1.5 ms at the stress case, and 0.15 ms at 10,000 events. It covers the events only; the fields around them are few.
- **The write is far too slow for the `"sequencer"` thread** at any heavy size: 83 to 254 ms, against a 1 ms tick. The snapshot, the part that has to stay on that thread if the rest moves, is 34 ms at 50 MB, nearly all of it the base64.

The real-project half came from a temporary timing log in the tick arm of the `"sequencer"` loop, removed afterwards: 41 rounds of playback on the biggest real project, 8 tracks of Omnisphere, Repro, Spire and Nexus, 3,716 events, 0.8 MB of blobs, a 2 MB file. No audio overrun during playback and no glitch heard, with a check and a trial write every 5 s.

| While playing | Median (ms) | Worst (ms) |
|---|---|---|
| Check as it stands | 1.65 | 2.80 |
| Direct event hash | 0.06 | 0.33 |
| Raw blob copy | 0.09 | 0.82 |
| Trial write | 5.40 | 8.65 |

Stopped or playing made little difference. The 50 MB blob guess was about 60 times this project's, and 100,000 events about 27 times.

## Testing

`kill -9` while the project has changes is the everyday test: it works every time, and to recovery it looks the same as a crash, since no Stev code runs in either. Finish with one real crash from the ⌘H recipe. Each phase's logic carries its tests as usual: what makes a copy exist or go is pure enough to test through the harness.
