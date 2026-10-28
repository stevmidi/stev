# Project Templates

**Archived 2026-10-28. Status: all four phases built and merged 2026-10-28.** The as-built behaviour lives in `060` (layout, Save as template, opening one, the −12 dB default), `020` (the browser), `030` and `010`; this file is the plan and its rationale, not maintained. Design agreed the same day. A template is a saved instrument setup: the tracks, their plugins with the presets loaded, their names, colours and mix, but no clips. Opening one gives a new untitled project to capture into, as ⌘N does, and nothing is written to disk until you save it. Read `060-persistence.md` (the DTOs, the on-disk layout, the unsaved-changes check) and the browser's section in `030-ui-design.md` first.

## Decisions (2026-10-28)

1. **A template is a `.stev` file in `~/Documents/Stev/Templates/`**: a root folder in the library beside `Projects/` and `Exports/` (`paths::templates_dir()`), holding `.stev` files directly, with no per-template folders. It is the same `ProjectData` JSON as a project, so there is no new format and no new load path.
2. **The browser lists them in a TEMPLATES category below PROJECTS.** Enter or a double-click opens one. Double-clicking one in Finder opens it the same way (agreed 2026-10-28), never copying it into Projects as an outside `.stev` is.
3. **Opening a template is ⌘N with a different starting point.** The unsaved-changes check runs first, as for every action that replaces the open project. Then the template loads as a new project that has never been saved: named `Untitled`, the undo history cleared, ⌘S opening Save As, and the "Save in" default the current project folder (never `Templates/`). The template file is only ever read. The saved-state fingerprint is taken right after the load, so closing an untouched template-based project asks nothing.
4. **⌘N stays blank.** There is no default template. Revisit only if usage asks for it.
5. **A template never holds clips.** There's no option to keep them: a project with clips is a project, and opening it and using Save As already covers that.
6. **What a template keeps and what it resets.** It keeps what describes the instruments and resets what describes a performance or a passing state:

   | Field | In a template |
   |---|---|
   | `output` (plugin, `state` blob), track count, `name`, `color` | kept |
   | `volume_db`, `pan` | kept: balancing the instruments is part of the setup |
   | `muted` | reset to `false`: a muted track in a template is a trap |
   | solo | not persisted anyway |
   | `meter`, `tempo_us` | kept (Enter's tempo fit replaces the tempo on the first capture anyway) |
   | `region_start` / `region_end` | reset to the default region (it is sized from clips, and there are none) |
   | `clips` | always empty |

7. **Save as template makes one from the open project.** It writes `Templates/<name>.stev` from the open project with the resets above applied. The open project isn't affected: it keeps its name, its file and its unsaved state.
   - **The command**: a new `Action`, labelled `Save as template` on the Keyboard tab (sentence case, like `Save as`), bound to **⌘⌥S** by default, beside ⌘S and ⌘⇧S, and rebindable like any command.
   - **The mouse route**: the hovered TEMPLATES category row shows a `+` at its right end, like the Projects row's new-folder `+` (same rect, stroke, hover wash and pointing hand). A press opens the dialog and selects nothing. It is a `HoveredControl` with a footer hover hint, `Save as template — Cmd+Alt+S` (the live chords, so a rebinding shows), which brings the hint list in `030` § Header & Footer to five controls.
   - **No menu item**: the app menu holds only About and the settings items, and a File menu for one command isn't worth it.
   - **The dialog** is the Save As dialog in a template variant (`SaveAs::template`), titled `SAVE AS TEMPLATE` so it doesn't read as Save As. The name field holds the project's name, selected whole (`Untitled` for one never saved). The "Save in" row stays, as fixed text: `Save in  Templates` in `fg_dim`, with no dropdown and no `+`. A taken name shows `Replaces the template <name>` on the note line and replaces it on Enter, as Save As does. The keys line reads `Enter save template · Esc cancel`. When the template is written, the footer's passing message says `Saved template <name>`.
8. **New tracks start at −12 dB, not 0 dB.** Plugin presets are often loud, and four tracks at unity sum past full scale into the output clamp, which is worse still in a stem export (`stev-arranges-daw-mixes`). At −12 dB four uncorrelated tracks sum to about −6 dB. This applies to every fresh track: `ProjectData::default` (⌘N), an added track and `Sequencer::new`. The track keeps its volume when its output changes. **`TrackData::volume_db`'s serde default stays `0.0`**, because a project saved before the field existed was at unity and must load as it was. Templates keep whatever volume they were saved with.

## Phases

Each phase is one branch and one merge, with tests, docs and a changelog line in the same change.

1. **Default track volume −12 dB.** A `config::TRACK_VOLUME_DB_DEFAULT` used by `ProjectData::default`, `Sequencer::new` and track add, and `SharedAtomics` slots initialized to it. Also check that a MIDI Out track, where the volume has no effect, shows nothing misleading. Tests: a new project's and an added track's volume is the default, a project file without `volume_db` still loads at `0.0`, and changing the output keeps the volume. Docs: `060` § volume and pan, `shared_atomics.rs`'s comment.
2. **Open a template.** `paths::templates_dir()`, `storage::list_templates`, the TEMPLATES browser category (empty list when the folder doesn't exist yet), `ProjectAction::NewFromTemplate(path)` running the unsaved-changes check and then the load-as-untitled, through `ProjectData::into_template()` (decision 6's resets), so a template holding clips opens without them. Tests: loading a template leaves the project unnamed with no file and an unchanged fingerprint, and a failed template load keeps the open project. Docs: `060` (layout, unsaved changes), `030` (browser), `010` (keys).
3. **Save as template.** The `Action` and its keymap row (⌘⌥S; the defaults-conflict test covers it), the TEMPLATES row's `+` and its hover hint, the dialog variant, `ProjectData::into_template()` (built in phase 2, which applies it on open too), and the write into `templates_dir()` (made on first save). Saving a template leaves the open project's name and saved state as they were.
4. **Managing templates in the browser**: rename (⌘R on a TEMPLATES row) and delete to the Trash (Delete/Backspace, then Enter), reusing the project and clip code paths — `storage::rename_template` / `delete_template` beside `rename_clip` / `delete_clip`, the browser's `BrowserItem::Template` joining the renamable and deletable rows. Renaming or deleting a template touches only the file: a project opened from one was never linked to it (decision 3). Tests: the tree's rename and delete-confirm tests cover a template row. Docs: `020` § Views (the browser), `010`, `030`.
