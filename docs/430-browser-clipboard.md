# Browser Copy, Cut and Paste

**Status: design agreed 2026-11-10, not built.** The browser panel copies, cuts and pastes projects with ⌘C, ⌘X and ⌘V, and moves them Finder's way with ⌥⌘V. It is how a project moves to another folder, or gets duplicated, without leaving Stev.

## Background

From a discussion on 2026-11-10. Before this, a project could only move between folders in Finder, and Save As copied it, leaving the original behind. Folders exist to sort projects, so moving one between them was the missing step. #22 was done first to prepare for this (`060` § The open project's file): the sequencer owns the open project's file, and every change to the library goes through one `LibraryChange` and `Display::library_changed`, so the Recent list, the open project and ⌘S follow a move with no new path.

Finder has no Cut for files (Edit ▸ Cut is greyed out on every file). It moves on the paste instead: ⌘C, then ⌥⌘V (Paste Item turns into Move Item Here with ⌥ held), so nothing ever looks cut and then gets lost. Stev keeps Finder's rule that nothing moves until the paste, and offers both ways in.

## Decisions (2026-11-10)

1. **Projects only.** Clips and templates get no copy, cut or paste, and no folders. Clips are exports into one flat folder, material to grab, not a library to sort. Templates are a short list of rigs, and copying one to tweak it is already: open it, change it, ⌘⌥S. Folders for either come only when use shows a list too long to read, and cut and paste can follow them then.
2. **Nothing crosses categories.** A file's category is its kind of file, and every move between categories is a conversion that already has its own command, which does it properly:

   | Paste tried | What it really is | Done instead by |
   |---|---|---|
   | A clip into Projects | a project made from a MIDI file | Importing onto a track (Enter, or a drag) |
   | A project into Clips | a `.stev` among `.mid` files | Clip export (⌘⇧E) |
   | A project into Templates | a template holding clips and plugin state, against "no clips ever in a template" | Save as template (⌘⌥S), which strips it |
   | A template into Projects | a project never opened or saved | Opening the template, then ⌘S |

   A paste would be a file copy dressed up as a conversion. So copy and cut take only a project row (under its folder, under the projects root, or under Recent: the same file), and paste lands only in a project folder or the projects root. Anywhere else it does nothing, and the footer says why, in `fg_dim` (no alarm colours).
3. **Folders don't move.** Folders stay one level deep under the projects root, so there is nowhere to move one to. ⌘C or ⌘X on a folder row does nothing, with a footer line.
4. **The chords.** While the browser has the keyboard it takes ⌘C, ⌘X and ⌘V (they arrive as `InputEvent::Copy`, `Cut` and `Paste`), and ⌥⌘V too, as Finder's move. Today the browser lets them fall through to the pane, so ⌘C in the panel copies the arranger's clips; that stops, as for every key the panel owns. ⌘X matches cut everywhere else in Stev (clips, notes); ⌥⌘V is for Finder habits. ⌥⌘V after ⌘X is an ordinary paste of the cut (a move either way). The ⇧ forms do nothing in the browser. Not commands: hand-matched in `handle_browser_key`, like the browser's other keys, and listed in `FIXED` (`keyboard_tab.rs`) beside `010`.
5. **A clipboard of the browser's own.** The browser keeps the copied or cut project in view state (`BrowserPanel`), separate from the arranger's clip clipboard and the clip view's note clipboard, so copying a project never touches either, and one doesn't overwrite another. Not the system clipboard, and not persisted: it goes with the session.
6. **A cut moves nothing until the paste.** The cut row is drawn dimmed (`fg_dim`) until then, and the footer says `Cut riff: paste in a folder to move it` once. Esc (while a cut is pending, before giving the keyboard back), a new copy or cut, or a reload that finds the file gone cancels it. Pasting a cut moves the file and empties the clipboard; pasting a copy leaves it, so the next paste makes another copy. The pending cut is a small piece of stored state, accepted on purpose because it is drawn (`220`'s "no stored mode-like state" is about state the user can't see).
7. **Where a paste lands.** The selected folder, or the folder of the selected project (the projects root for a loose one). The selection decides, not the default folder (`060` § default project folder): the default folder is where Save As starts, and pasting doesn't change it. Selected under Recent, a project gives its own folder. On a category, Recent or Templates, or nothing at all, the paste does nothing, with the footer line from decision 2; on the Projects category it lands at the root.
8. **Name clashes take a number.** A paste into a folder holding the name already takes the next free name with `_2`, `_3`, …, the rule a file opened from Finder is copied in under (`060` § Where the project ends up; `storage::copy_project_into`, case-insensitive as the volume is). No prompt: a rename fixes the name afterwards (no redundant prompts). So a copy pasted into its own folder is a duplicate, `riff_2`. A cut pasted into its own folder moves nothing and empties the clipboard.
9. **What a copy copies.** The file on disk, byte for byte, as Finder would: a copy of the open project with unsaved changes holds the last save, not the changes. The open project stays the open project, in the folder it was in, and the copy is not opened.
10. **A move is a `LibraryChange::Moved`.** The move that already covers a rename: `old` and `new` origins in different folders. It goes through `Display::library_changed`, so the Recent list follows it (the entry keeps its place), the sequencer's open project follows it (`SavedProject::follow`, `UiEvent::OpenProjectFile`), so ⌘S saves into the new folder, and the default folder stays put (decision 7; #22's rule that a move of the open project doesn't move the default folder). A save on its way when the move lands behaves as for a rename (`060` § The sequencer follows a browser rename or delete). A copy is not a library change: nothing anyone holds names the new file, so it only reloads the tree.
11. **On disk.** A new `storage::copy_project(from, to_folder) -> io::Result<String>` and `storage::move_project(from, to_folder) -> io::Result<String>`, each returning the name it used (decision 8), unit-tested beside `rename_project`. A move is a rename across folders (`fs::rename`; the projects root and its folders are on one volume), so it is atomic and never replaces anything. A copy writes with the safe save's temp-then-rename, so a failed copy leaves no half-written `.stev`.
12. **After a paste** the tree reloads, the pasted row is selected (its folder expanded) and the footer says `Moved riff to Jams`, `Copied riff to Jams` or `Copied riff as riff_2`, or why it failed. A failure leaves everything as it was, the clipboard included.
13. **No undo.** Like rename and delete in the browser: files aren't in the project's undo stack. A wrong move is undone with another cut and paste; a wrong copy, with Delete (to the Trash).

## Things to check while building

- **Does ⌥⌘V arrive as a paste?** egui-winit's `is_paste_command` only asks for ⌘ and the V key, so ⌥⌘V should become `Event::Paste` with `modifiers.alt` set, and `InputEvent::Paste` would grow an `alt` field the way `Copy` and `Cut` carry `shift`. But egui-winit picks the logical key first, and on macOS ⌥V types `√` on many layouts, which could hide the V (the physical key is only the fallback when there is no logical one). Check it with the `log::trace!` in egui-winit's key handling on a US and a Nordic layout before relying on it. If it doesn't arrive, match the raw `Key` event in the browser and say so here.
- **egui only sends `Paste` when the OS clipboard holds text.** The clip clipboard primes it with `CLIP_CLIPBOARD_SENTINEL` for this reason (`input_poller.rs`). A browser copy or cut needs the same priming (its own sentinel, or the same one), or ⌘V after a browser ⌘C with an empty system clipboard never reaches the browser.
- **Leaving the panel.** A clipboard chord is no longer a pane command from the browser, so it must not go through `Display::leave_browser`. Esc's order becomes: cancel a pending delete, else cancel a pending cut, else give the keyboard back.

## Phases

1. **Copy and paste.** `copy_project`, the browser's clipboard, ⌘C and ⌘V on project rows, the `_2` rule, the footer lines, the ⌥⌘V and sentinel checks above. A duplicate by copy and paste into its own folder works from here.
2. **Cut and move.** ⌘X, the dimmed row and its cancels, ⌥⌘V as move, `move_project`, the `LibraryChange::Moved` across folders through `library_changed`. Then `020` § Views, `010`, `060` and `FIXED` updated, and a `CHANGELOG.md` line.
