# Crash Recovery

**Status: design agreed 2026-11-06, not built. Issue: #12.** If Stev crashes or is force-quit, the next launch offers to bring back what wasn't saved. While a project has unsaved changes, Stev keeps a recovery copy of it in its own support folder, never in the project file.

## Background

From a discussion on 2026-11-06. The unsaved-changes prompt (`060` § Unsaved changes) only covers a quit Stev gets to see. The safe save (`storage::replace_file`, `8e5c76e`) protects the last save during a save, but not the work since then.

This is not autosave. Autosave was rejected because it would overwrite the saved project with throwaway changes (`060`). A recovery copy is never written over a project file, and it is deleted on any clean exit.

Nothing can run at the moment of a crash. The crash that prompted this, showing the app after ⌘H (winit#4820), is a `SIGILL` out of AppKit that never reaches a Rust panic hook. So the copy has to be on disk already.

## Decisions (2026-11-06)

1. **A copy exists exactly when the quit prompt would ask.** One rule covers every case: a project with changes has a copy, a project without has none. A new project or a template left alone has none, and undoing back to the saved state deletes it. `SavedProject::has_changes` decides, as it does for the prompt.
2. **One slot.** One copy, in `config_dir()`, written with `replace_file`. It holds the `ProjectData` and where the project came from (its folder and name, nothing for a project never saved). It is not a `.stev` the browser lists.
3. **When: a timer that compares, not hooks.** Every 5 s the tick arm of the `"sequencer"` loop compares the project's fingerprint with the last copy's, and writes only if it differs. No idle detection and no transport trigger: the loop can run for an hour while takes are committed with `/`, so a rule that waits for a stop or skips playback would miss the work that matters most. A timer needs no hook, so no path that changes the project can be forgotten. At most about 5 s of edits are lost. This depends on the measurement in decision 4.
4. **The check must cost well under a tick of the 1 ms clock, during playback.** `ProjectData::from_sequencer` base64-encodes every plugin blob and `fingerprint` then throws them away, so the check takes a snapshot without blobs. Measured first, on the heaviest project at hand (phase 1). If it costs too much, the fallback is a debounce: both the command and the transport arms of the loop schedule a check (the loop region arrives through the transport arm), with a maximum wait so a held knob can't postpone it for ever.
5. **Writing may leave the thread.** The check never reads the blobs; a write needs them all, since the copy is a whole project. Playing or listening, that is seldom, but during steady editing a write can come every 5 s, and each one encodes every blob again, even if one note moved. The same measurement decides whether that is too slow for the `"sequencer"` thread. If it is, the write gets a thread of its own: the sequencer thread only copies the raw bytes, and the encoding, the JSON and the disk happen there. Keeping each encoded blob until its plugin's state is next captured would avoid most of the encoding, at the price of a second copy in memory, so it waits until the writes themselves prove too heavy.
6. **Plugin state is captured on events, never polled.** A plugin added since the last save has an empty state blob (`view/display/instrument.rs`), so without this a restored project plays its clips through the plugin's init patch. The view also captures a plugin's state when it reports a preset load (`ParameterGesture::ValuesChanged`) and when its editor closes. A sound changed with the editor still open is not caught.
7. **A captured blob rewrites the copy.** Blobs stay out of the fingerprint (`060`), so the comparison in decision 3 also counts captures since the last copy. A capture never makes a project count as changed by itself: a sound tweaked in a saved project with nothing else changed still has no copy, as the quit prompt doesn't ask either.
8. **The capture buffer is left out at first.** A take played but not committed isn't brought back.
9. **The offer is a launch dialog.** A `ProjectDialog` over the empty startup project, on the first launch after a crash only, with Discard and Reopen: `Stev didn't close properly.` and then `Reopen your unsaved changes to "<name>"?`, or `Reopen your unsaved new project?`. Not a footer prompt or a RECENT row. A file opened from Finder at launch waits behind it, as it does behind any dialog (`keyboard_taken`): after Discard it opens, and after Reopen it asks the usual unsaved-changes prompt first. The dialog has to be up by the first frame, when a launch file replaces the startup project (`060` § Opening from Finder), so it is set up at startup, not by a later event from the `"sequencer"` thread.
10. **Reopen keeps the project's name and folder.** ⌘S overwrites the file, as after any edit, and the file on disk stays untouched until then. If the file has gone, or the project was never saved, it reopens untitled. It goes through the usual staged load, so its plugins restore as on any open.
11. **A reopened project always counts as changed.** It takes no baseline to compare against, so the quit prompt asks until it is saved. Comparing with the file on disk wouldn't do: a copy that differs only in a plugin blob has the same fingerprint, and the next check would delete it.
12. **The dialog and one footer line are the only signs.** `Reopened unsaved changes to <name>`, once. Stev has no unsaved marker and this adds none.
13. **The copy outlives Reopen.** It goes on a save, on Discard, or on a clean quit (Save or Don't Save, in `ProjectAction::Quit`, on the sequencer thread, before the process exits). So if a plugin crashes during the restore, the next launch offers it again, and Discard is the way out.
14. **Nothing touches the copy until the offer is answered.** The startup project has no changes, so the check would otherwise delete it.
15. **A copy that won't load** says why in the footer, as a failed open does (`060`), and the next clean quit removes it.
16. **A second instance stays out.** The running instance holds a `File::try_lock` on a lock file beside the copy. An instance that can't get it neither offers nor writes. The lock goes when the process dies, so a copy with a free lock means a crash. Two instances are rare, since macOS brings a running app bundle to the front, but `open -n`, a bare binary or a `cargo run` beside the app all make one, and without the lock it would take the first one's copy for a crash. `stev --mcp` (`360`) is a second process from the same binary on every agent session, and it skips recovery entirely: it takes neither the lock nor the copy, or starting it before the app could turn recovery off. The lock is taken on the app's startup path only.

## Phases

1. **Measure, then the copy.** Time the blob-free check during playback on a heavy project and settle decisions 3 to 5. Build the check, the write, the deletes and the lock.
2. **The offer.** The dialog, Reopen with its name, the footer line, the always-changed baseline.
3. **Plugin state on events.** The captures of decision 6, and decision 7.

## Testing

`kill -9` while the project has changes is the everyday test: it works every time, and to recovery it looks the same as a crash, since no Stev code runs in either. Finish with one real crash from the ⌘H recipe. Each phase's logic carries its tests as usual: what makes a copy exist or go is pure enough to test through the harness.
