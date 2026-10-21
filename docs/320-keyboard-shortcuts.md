# Keyboard Shortcuts as a Settings Tab, with Rebinding

**Status: design agreed 2026-10-21; phase 1 (the keymap) built 2026-10-21 on `feature/keyboard-shortcuts-tab`; phase 2 (the read-only Keyboard tab, the overlay retired) built 2026-10-21 on `feature/keyboard-tab`; phase 3 not started.** The `280` candidate *Keyboard shortcuts as a settings tab, with rebinding* (user direction 2026-10-07). Today the bindings are inline `match` arms and the `?` help overlay is a hand-written copy of a subset of them. This doc moves the commands into one keymap, makes the settings modal show and rebind it, and retires the overlay. Read `010-keybindings.md` (the bindings and where each is matched) and `020-views-and-state.md` § Views (overlays, key focus) first.

## Decisions (2026-10-21)

1. **Commands are rebindable; navigation and dialog keys are not.** A *command* is a discrete action with a name: `Space`, `\`, `[`/`]`, `T`, `K`, `R`, `Q`, `M`, `⌘L`, `⌘E`, `⌘J`, `⌘Z` and the rest. Fixed and listed read-only: `⌘S` / `⇧⌘S` / `⌘N` (save, save as, new project — decided 2026-10-21), the arrow-key grammar (⇧ extends, ⌥ steps to clip edges, ⌘ is the block move or the fine nudge, the note nudges, DAS/ARR repeat in `InputPoller`), `Esc` / `Enter` / `Tab` / `⇧Tab`, every key inside a dialog, text field, the browser and the track-header column, and the `⌘K` piano layout. The modifier conventions (`arranger-modifier-conventions`) stay intact because no one can rebind half of a family.
2. **`?` opens Settings on a new Keyboard tab**, with the search box focused. The help overlay (`help_overlay.rs`, `input/help.rs`, `rendering/modals/help.rs`, `Overlay::Help`) and its `HELP` table are deleted. The header's `?` chip opens the same tab. Bindings live in one place.
3. **A conflict replaces, and the loser is left unbound.** Recording a chord that another action uses in an overlapping scope names that action and asks for a second press to confirm (Logic's Key Commands). On confirm the chord moves, and the other action shows **unbound** in red in the list until it gets a key.
4. **An action has a list of chords; recording replaces the whole list.** Defaults may have several, kept as layout fallbacks (`\` and `/` for Commit, `+` and `=` for zoom in). Recording one chord makes it the only one. **Reset** restores the action's default list, and **Reset all** restores every action.

## The model

A new `src/core/keymap.rs` (pure, `mod tests`, no egui types beyond `Key`).

- **`Action`**: one variant per command, e.g. `TogglePlayback`, `Restart`, `Stop`, `Commit`, `ClipStartToCursor`, `TapTempo`, `ToggleMetronome`, `ToggleLiveRecord`, `SetLoop`, `Split`, `Merge`, `MuteRange`, `MuteNotes`, `Quantize`, `DuplicateClips`, `DuplicateNotes`, `Undo`, `Redo`, `Save`, `SaveAs`, `ZoomIn`, `ZoomOut`, `ExportClip`, `ExportAudio`, `ToggleBrowser`, `TogglePluginEditor`, …. **One meaning per action**: a chord that today means different things in the two panes becomes two actions with the same default (`M` is `MuteRange` in the arranger and `MuteNotes` in the clip view; `⌘D` is `DuplicateClips` / `DuplicateNotes`), so each can be rebound on its own.
- **`ActionInfo`**: per action, a stable string **id** (`"transport.play"`, never renamed once released, because `settings.json` stores it), a **label** ("Play / stop"), a **category** (the tab's groups, today's `HELP` section titles), a **scope** and the **default chords**. One `const` table, the single source the tab and the defaults are built from.
- **`Scope`**: `Global` (any pane), `Arranger` or `Clip`, meaning which pane has the keyboard (`focused_pane()`). Two actions **overlap** when either is `Global` or both have the same scope. Only overlapping actions conflict.
- **`Chord`**: `key: Key` plus `command`, `alt`, `shift` and `ctrl` (the `KeyModifiers` fields; `command` is ⌘ on macOS and Ctrl elsewhere, as today, so a stored chord is portable). Matching is **exact**, on the chord a press normalizes to (`Chord::from_press`), which drops what a keyboard layout adds to type the key at all:
  - **Every punctuation key ignores `shift`**: egui-winit reports the logical key, so the key already says what Shift made of it (the same reasoning as `is_help_key`). That keeps `?` working as `⇧/` on US layouts and `⇧+` on Nordic ones, `+` as Shift+`=`, and `/` as Nordic Shift+7. Built for all punctuation rather than a list of "shifted somewhere" keys: no layout question to get wrong, at the cost of never binding `⇧,` apart from `,`.
  - **The brackets, `\` and `|` ignore `alt` too**: Nordic and German layouts type them with ⌥ on macOS (`\` is ⇧⌥7) and AltGr elsewhere, and AltGr arrives as Ctrl+Alt off macOS, so there `command` goes with it. Today's `[`/`]` ("any modifier but ⌘") and `\` ("any modifier") arms relied on this. `=`/`-` keep `alt`: `⌥=`/`⌥-` are the clip stretch.
  - **Off macOS `ctrl` folds into `command`**, since egui reports both for the one Ctrl key there.
- **`Keymap`**: the defaults with the user's overrides applied, indexed `Chord → [(Scope, Action)]`. `Keymap::resolve(chord, focused_scope) -> Option<Action>` returns the scoped action ahead of a global one. The defaults are conflict-free (pinned by a test), and rebinding keeps them so, so there is never more than one candidate for a scope.

## Resolution

The keymap lives in `Display` (main thread, view-local, plain data: no lock on the render path). `forward_input_event` resolves once, **after** the guards that already take keys first and **before** any binding arm:

1. The keyboard marquee-nudge guard (every key consumed mid-nudge), as today.
2. Whatever has the keyboard and takes every key: open overlays and dialogs, text fields (`keyboard_taken`), the browser and track-header keymaps. These stay hand-matched (decision 1).
3. **`KeyPressed` → `Keymap::resolve`**: a hit becomes `InputEvent::Action(action)`, a miss stays `KeyPressed`.
4. The existing arms then match `InputEvent::Action(Action::…)` in place of `KeyPressed { key: …, modifiers } if …`. The arms in `input_handler.rs` change the same way, because the event reaches `EventHandlers` already resolved. Their modifier guards (`!modifiers.alt && …`) disappear, since the chord is exact. The fixed keys (arrows, Esc, Enter, Tab, digits in fields) keep their `KeyPressed` arms.

Ordering between arms stops mattering for commands: today `⌘⌥E` must come before the `⌘E` split arm, but exact chords make that impossible to get wrong.

### Edges to carry over

- **Clipboard**: egui-winit turns `⌘C`/`⌘X`/`⌘V` into `Copy`/`Cut`/`Paste` events, and the raw key never arrives. Copy, cut and paste are listed read-only, and the recorder refuses those three chords.
- **Reserved by egui or the OS**: `⌘+`/`⌘-`/`⌘0` (egui's UI scale), `⌘Q`, `⌘M`, `⌘H`, `⌘,` and `?` are refused, the last two because they are the way into this tab (below). So are the fixed project chords `⌘S`, `⇧⌘S` and `⌘N`.
- **The plugin-editor key guard** (`plugin_host/key_guard.rs`) reserved bare `Space`, `v`, `.` and `0` while an embedded editor has macOS focus, and sends the raw key on. It keeps sending raw keys, so they resolve like any other — where `Display` drains the input channel, after the modal check. Its reserved set follows the keymap: the bare chords of `TogglePlayback`, `Stop` and `TogglePluginEditor` (`Display::guard_plugin_editor_keys` → `set_guarded_keys`, once at startup; phase 3 calls it again when the keymap changes). Otherwise a moved Play key would stop working inside a plugin window and the old key would be eaten for nothing. As built, `.` is no longer reserved: it has not been Stop since the keypad days (`010` § Retired bindings), so it was being eaten for nothing already.
- **The computer keyboard** (`⌘K`) sets its layout keys aside in `InputPoller` before they become `KeyPressed`. A command rebound to a bare key in the piano layout is shadowed while the piano is on, as `K` and `T` are today. The tab says so on that row. The recorder refuses a bare piano-layout key for `ToggleComputerKeyboard` itself, which would leave the piano with no key to turn it off.
- **`ZoomIn`/`ZoomOut` repeat**: they rely on the OS key repeat, which works for any key, so moving them costs nothing.
- **`⌘,` and `?` are fixed**, not rebindable. Settings and the Keyboard tab must always be reachable, whatever the user has done to the rest.

## The Keyboard tab

A third tab, `SettingsTab::Keyboard`, after Appearance. It opens there from `?` and the `?` chip; `⌘,` keeps opening on the last tab used.

- **Layout**: a search field at the top, then the actions grouped by category in two columns: the label on the left, the chords on the right, with the platform spelling `key_label` uses today (`Cmd+` / `Ctrl+`, `Opt+` / `Alt+`, `Shift+`). The fixed keys follow as read-only categories ("Navigation", "Dialogs", "Computer keyboard", "Clipboard") in `fg_dim`, so the tab is the whole reference that the overlay used to be. An unbound action reads "unbound" in `track_mute()`. An action whose chords differ from its defaults gets a small dot.
- **Search** filters as you type, on the label and the key label both ("cmd+l" finds Set loop, "loop" does too). Typing goes to the field. `↑`/`↓` move the selection through the matching rows, and `Esc` clears a non-empty search before it closes the modal.
- **Recording**: `Enter` or a click on a row's chords records. The row reads "Press a key…" (in the row: modals never show hint rows, `030`). The next key press with any modifiers becomes the chord, a lone modifier does nothing, and `Esc` cancels. A refused chord says why, in the footer's passing message ("⌘C is the clipboard"). A conflict turns the row into "Replaces *Split* — press again" until a second press of the same chord confirms it or any other key cancels.
- **Unbind / reset**: on a selected row (not recording), `⌫` unbinds it and `⌘⌫` resets it to the defaults. **Reset all** is a button at the foot of the tab, with a confirm.
- **Saved on every change**, as the Appearance tab saves a theme as it is reached (`update_settings`). No Apply button.

## Persistence

`SettingsData` gains `#[serde(default)] keymap: BTreeMap<String, Vec<ChordDto>>`: **only the actions the user changed**, keyed by action id, with an empty list meaning unbound. Storing differences means a new default in a later version reaches an upgrading user for every action they never touched. Load skips unknown ids and unparseable chords (a downgrade, or an action that was removed) and keeps the rest. `ChordDto` stores the key by egui's name (`Key::name` / `Key::from_name`) and the four modifier flags. If the loaded overrides conflict (an edited file), the later id in table order loses its chord.

## As built (phase 1)

What phase 1 settled that the design above left open:

- **`⌥Space` is one global action, `PlayFromCursor`**: in the clip view it restarts from the clip cursor, in the arranger it is the play / stop toggle, as before. Splitting it into a `Clip` action would have left the arranger's `⌥Space` unbound or put a global and a scoped action on one chord. It is the one action whose meaning follows the pane, and the one place arm order still matters (`input_handler.rs`: its clip-view arm comes first). Phase 3 could let a scoped action and a global fallback share a default chord instead, since `resolve` already prefers the scoped one.
- **`⌘A` is two actions**, `SelectAllClips` (arranger) and `SelectAllNotes` (clip), like `M`, `⌘D` and `Delete`. The scope is the only pane gate: the handlers no longer re-check the view for a scoped action, only state the keymap can't see (notes selected or not).
- **`⌘S`, `⇧⌘S` and `⌘N` are fixed, not commands** (user decision 2026-10-21): `SAVE`, `SAVE_AS` and `NEW_PROJECT` are `Chord` consts in `core/keymap.rs`, matched in `Display::handle_project_input_event` ahead of the overlays and the browser's keys, so they work from any key focus. The recorder refuses them, and a test keeps every action off them.
- **`⇧F5`/`⇧F6` (step the theme) are gone** (user decision 2026-10-21): the Appearance tab is the one way to pick a theme.
- **Deleting is `DeleteSelection` (arranger) and `DeleteNotes` (clip), each on `Delete` and `Backspace`**; `⌘Delete` is `DeleteTime`. The track headers' `Delete` stays hand-matched (decision 1).
- **Exact matching retired some accidental chords** that matched before because an arm ignored modifiers: `⌘\`, `⇧Space`, `⌥M` (arranger mute), `⌘⌫` and `⇧Delete` (carve), `⌘Delete` in the clip view (delete notes) and the like now do nothing. `⌃` chords on macOS are their own chords too.
- **The plugin-editor key guard has one path**: every reserved key goes on as a raw `KeyPressed` and is resolved where `Display` drains the input channel, which also handles `TogglePluginEditor` there (the old "toggle pending" flag is gone).
- **The help overlay** still matches `?` and `⌘,` by hand (`key_taken_before_keymap`, ahead of the keymap), and its `HELP` table is unchanged.

## As built (phase 2)

- **`view/display/keyboard_tab.rs`** holds the tab: its state (`KeyboardTab`: the search, the selection counted among the matching rows, the scroll in lines), the rows (`shortcut_rows`: `ACTIONS` with their default chords, then `FIXED`), the search filter and the lines it shows (`shortcut_lines`, headings over each group's first match), the key labels (`key_label`, `chord_glyphs`) and `?` (`is_keyboard_tab_key`). The lines are rebuilt from the search where they are used, never stored. Drawing, the field and the hit-test are in `rendering/modals/settings.rs` (`keyboard_tab_layout`).
- **The fixed rows are a hand-written table, `FIXED`**, in seven groups after the commands: Files & settings (`⌘S`, `⇧⌘S`, `⌘N`, `⌘,`, `?`), Navigation, Enter & Esc (including the take session's), Clipboard, Mouse, Browser and Computer keyboard. They are matched by hand where they are handled (decision 1), so nothing can generate them; `AGENTS.md` says to update a row when its key changes.
- **The tab shows each action's `defaults`**, not the live keymap: the defaults are the only keymap until phase 3, which reads the chords from `Keymap` instead.
- **The search field is the app's name field** (`show_name_field`, which now returns its `Response`), shown over the panel in the canvas `ui` and holding the egui focus while the tab is up, so typing always lands there; the modal's handler still sees every key, and acts only on `↑`/`↓`, Esc and the tab chords. A change to the text puts the selection and the scroll back at the top.
- **`?` inside the modal** shows the Keyboard tab from the other tabs; on the Keyboard tab it is typing, for the search. Opening the modal, by either way in, starts a fresh search.
- **A click on a row selects it** (`SettingsHit::Shortcut`); phase 3 makes a click on the chords record.
- **Only a fixed row's keys are `fg_dim`**, not the whole row as the Layout bullet above has it: an all-dim row read like the dim group headings. The label keeps `fg`, and a heading sits at the foot of its line so the room above it starts the group.
- **Not in phase 2**: the unbound red, the changed dot and the recorder (`Enter`, `⌫`, `⌘⌫`, Reset all) — they need overrides, phase 3.

## Phases

Each phase is its own branch and leaves the app shippable.

1. **The keymap, no visible change.** `core/keymap.rs` with the full default table, and `Display` resolving to `InputEvent::Action`. Every command arm in `input/mod.rs` and `input_handler.rs` moves to actions, and the key guard reads the keymap. Tests: no default conflicts, every action has a label, a category and an id, the ids are unique, exact matching with the Shift exception, and scoped-before-global resolution. The existing per-key tests keep passing. Behaviour is identical, and the help overlay is still the hand-written one.
2. **The Keyboard tab, read-only, and the overlay retired.** The tab with search, generated from the table plus the fixed rows. `?` and the `?` chip open it, and the overlay code and `HELP` are deleted. `010` § Help Overlay becomes § Keyboard Tab, and the ground rule in `AGENTS.md` about `HELP` goes.
3. **Rebinding.** The recorder, conflict replace, unbind, reset, reset all, and the `settings.json` overrides. Tests: override round trip, unknown ids dropped, defaults reaching an upgrading user, the refused chords, and a conflict leaving the loser unbound.

## What changes in the docs

- `010`: the intro says commands resolve through the keymap (`core/keymap.rs`) and lists what stays fixed. The "Bound in `Display`, not `input_handler.rs`" list shrinks to the view-local actions. Every binding row keeps its default chord, and a rebound key changes none of them.
- `020` § Views: the overlay list loses the help overlay.
- `030`: the Help overlay section becomes the Keyboard tab, and the `?` chip opens it.
- `060`: the `settings.json` keymap field.
- `AGENTS.md`: the "keybinding change also updates `HELP`" rule becomes "a new command adds a row to the keymap table".
- `250-first-feature.md`: the walkthrough's `M` press goes through `Action::MuteNotes`.

## Not in scope

- Rebinding the arrows, the dialogs, the browser or the piano layout (decision 1).
- Several keymap presets ("Ableton", "Logic") or import/export of a keymap file. `settings.json` can be copied.
- Mouse bindings and MIDI-learn of commands (the parked keypad route, `archive/010-keypad.md`).
- Multi-key sequences (`⌘K ⌘S` style).
