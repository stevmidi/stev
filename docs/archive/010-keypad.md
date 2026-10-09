# Physical Keypad Layout (archived)

**Archived 2026-09-18.** The user has moved to a conventional keyboard as the primary input surface and is no longer designing bindings around this hardware. This file is kept for the record and is deliberately *not* in the `AGENTS.md` index — do not read it unless the user explicitly brings up the physical keypad, its row/column legends, or the parked MIDI-CC controller route. The live keyboard-binding reference is `../010-keybindings.md`.

**The layout is historical; the hardware is not.** This layout described a dedicated embedded-Linux hardware sequencer controller (serial keypad via `start_keypad_thread`, `/dev/ttyAMA0`). That standalone-hardware vision was scrapped in favor of a regular cross-platform desktop app, and `start_keypad_thread` has been removed from the codebase (recoverable from git history if ever revisited).

**The physical unit may still be plugged in, as a plain HID keyboard.** Its keys arrive as ordinary `egui::Key` events through `InputPoller` exactly like any attached keyboard, so there is no keypad-specific code path and nothing to special-case — the legends below are simply the caps printed on the keys behind the normal bindings. Do not assume a *serial* keypad is wired up, but do not assume the hardware is gone either.

**The MIDI-controller route is intentionally parked.** `start_controller_thread` (`src/core/threads/controller.rs`) mirrors this same key mapping for a MIDI-CC-based controller. It is `#[allow(dead_code)]` and never called, and it is kept deliberately as a still-open design option for driving the app from the keypad over MIDI instead of HID — do not propose deleting it or its `#[allow(dead_code)]`. It is also the only other potential producer of `InputEvent`s besides `Display`, which matters when reasoning about whether an input path is reachable (note it never sets `modifiers.command` or `modifiers.alt`). `CONTROLLER_DEVICE_NAME` in `src/core/config.rs` is the port it would bind to.

When the user refers to the physical keypad by row, column, or printed legend, assume the canonical 6-row x 4-column layout below unless the user explicitly says they are experimenting with a different hardware revision.

| Row | Col 1 | Col 2 | Col 3 | Col 4 | Primary role |
|---|---|---|---|---|---|
| 1 | `F1` | `F2` | `F3` | `F4` | Context-sensitive softkeys |
| 2 | `CLEAR` | `DUPLC` | `QUANT` | `MUTE` | Object operations |
| 3 | `METRONOME` | `TAP` | `SPARE` | `UNDO` | Global utilities / expansion |
| 4 | `STOP` | `PLAY` | `REC` | `COMMIT` | Transport / commit |
| 5 | `PREV` | `NEXT` | `-` | `+` | Local edit / logical navigation |
| 6 | `LEFT` | `RIGHT` | `SHIFT` | `ENTER` | Spatial navigation / modifier / confirm |

## Keypad Grammar

- **Rows are function-grouped top-to-bottom**: `Row 1` = softkeys, `Row 2` = current-object operations, `Row 3` = global utilities or future expansion, `Row 4` = transport, `Row 5` = local edit, `Row 6` = navigation and confirmation.
- **`PREV/NEXT` is logical navigation, `LEFT/RIGHT` is spatial navigation**: `PREV/NEXT` means previous/next meaningful item in the current context (event, clip, option), while `LEFT/RIGHT` means timeline or cursor movement.
- **The bottom-half columns are intentionally paired**: `PREV` sits above `LEFT`, `NEXT` above `RIGHT`, `-` above `SHIFT`, and `+` above `ENTER`.
- **`+/-` are generic adjustment keys**: they should usually mean apply an increase/decrease in the current mode rather than a one-off command unique to a single screen.
- **Softkey legends are physical, meanings are view-specific**: references to `F1`-`F4` mean the physical top row; the action bound to each softkey depends on the current `ViewState`.
- **`SPARE` means intentionally unassigned**: treat those keys as reserved capacity for future hardware/software features, not as implied existing bindings.
- **Legend → key**: `METRONOME` (R3C1) = `K`, `SAFE` (R3C4) = `S`, `UNDO` (R3C4) = `U`, `MUTE` (R2C4) = `M`, `DUPLC` (R2C2) = `D`.

## What the keypad can no longer reach

The keypad has no ⌘/Ctrl, ⌥, or TAB key. As the bindings moved to conventional desktop chords, these keypad keys became no-ops or lost their job:

- **`UNDO`**: Undo/Redo moved from bare `U`/`Shift+U` to ⌘/Ctrl+Z / ⌘/Ctrl+Shift+Z. The keypad's `UNDO` key sends whatever raw keystroke is baked into its own firmware (outside this repo) — if that's still a bare `U`, it does nothing until reconfigured at the hardware level to send ⌘/Ctrl+Z. The dead-code MIDI-CC route (`start_controller_thread`) synthesizes `command: true` for its Undo softkey so it stays functionally in sync if ever revived; the live HID passthrough has no such software hook to patch.
- **`SAFE`**: a bare `S` does nothing; save is ⌘/Ctrl+S, Save As is ⌘/Ctrl+⇧+S.
- **`DUPLC`**: there is no bare `D` binding at all — `⌘/Ctrl+D` / `Shift+⌘/Ctrl+D` (Duplicate Time) cover every duplicate need, so the key is a no-op in the Arranger.
- **`SHIFT+ENTER`**: was the keypad-era "back/cancel" chord (exit `Clip`/`ClipEdit`/`PendingClip`, close modals, and for a while "select every clip in the loop region" in the Arranger). All of that is gone — ESC closes modals, `Shift+Tab` exits the clip views, and `SelectClipsInRegion` was deleted outright rather than rebound. Do not reintroduce a `SHIFT+ENTER` binding.
- **`SHIFT++`**: select-all moved to ⌘/Ctrl+A; the chord is spare.
- **⌥-arrow clip-edge navigation, ⌘/Ctrl-arrow block move, `Shift+Tab` clip exit**: keyboard-only by construction.
