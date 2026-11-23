# The Performance Lane's Reach

**Status: agreed 2026-11-23; phase 1 built 2026-11-23, phase 2 built 2026-11-23 (both as built in `110` § Scope and § Rendering, the setting in `060`); issue #44.** Revised while hand-testing phase 1 (2026-11-23): note 0 (C-2) starts bar 1, not C3, and a press takes the copy nearest the **cursor**, running too, not the playhead. Revised again while hand-testing phase 2 (2026-11-23): **the hand jumps as one block**, the copy chosen for the middle of the guessed keyboard, so a press depends on the guess, which is no longer display only (§ 2, § 4, § Changes while building). The lane's keys repeat along the timeline, and the keys under the player's hand sit on the stretch of them nearest the cursor, so the keyboard reaches every part of a song however long it is. A keybed-size setting lets the lane light the keys the player's hand is on. Read `110` (the lane) and `510` (the keys drawn in it) first. This replaces `510` § Not in this design's "A movable base note or an octave shift" bullet.

## Background

Today the lane has 68 keys, from note 60 (C3, `config::PERFORMANCE_LANE_BASE_NOTE`) to 127 (`KEY_COUNT`), and notes below 60 are ignored. At a beat a key in 4/4 the keys end at bar 18, and nothing after it can be reached (found in use, 2026-11-23). A small keybed makes it worse: a 25-key keyboard covers 25 of those 68 keys at a time.

Weighed and set aside (2026-11-23):

- **Fixed zones or pages, switched by a command.** Predictable, but one more thing to operate, and the zone has to be shown and remembered.
- **The cursor deciding the mapping** (keys counted from the cursor). Every cursor move would relabel the keys, so A/B-ing between performing and the arranger breaks the keyboard, and muscle memory with it.
- **Stev-side octave shifting.** Exact, but it makes the player give up the keyboard's own octave buttons.

## The design

### 1. The keys repeat along the timeline

The keys form a **cycle of 120** (ten octaves, so the note names run unbroken across the wrap). Note 0 (C-2) starts bar 1, and a key is its note number; notes 120–127 wrap onto keys 0–7.

```
key_index = note mod 120          // 0 .. 119
```

At a beat a key in 4/4, one cycle is 30 bars:

```
bar   1                             31                            61
      |███████ keys ███████████████|███████ keys ███████████████|████…
      C-2                           C-2                           C-2
```

A key's meaning, its place in the cycle, never changes. The lane draws the keys repeating for as long as the timeline runs.

### 2. The hand jumps as one block, around the cursor

Every key now exists many times. The keys under the player's hand (the guess, § 4) take **one stretch of copies together**: the one where the hand's middle key is nearest the arranger cursor. A press, running or stopped, jumps to its key in that stretch, the notes of the hand lying unbroken along the timeline in order, across a cycle's wrap too (note 125 is key 5 of the next cycle).

```
49-key keyboard at home (notes 36–84, middle C3), a beat a key in 4/4.
C3's copies: bars 16, 46, 76, 106.

Cursor at bar 73: C3's nearest copy is bar 76, so the hand is on cycle 3.

bar  61        70   73        76               82
     |C-2 ······|▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓|·····
                C1   C2        C3               C4
                     ● cursor: C2 lands on it
```

- **No wall.** The cursor picks the stretch of the song the hand reaches; the keyboard's octave buttons move the hand within it (an octave is 3 bars at a beat a key, 12 at a bar).
- **The cursor alone moves it.** The playhead, by playing, by a press or by the song running on, never moves the hand; a cursor move does, and a hardware octave shift does at the next press (§ 5). A performance keeps its mapping for as long as the cursor stays put.
- **Never split.** The hand is always one unbroken stretch, and every press lands where it is lit.
- **The default octave isn't bar 1.** A keyboard at its home octave reaches bars 10–22 from a cursor at bar 1; bar 1 is a few octaves down. Accepted.
- **Before bar 1.** A hand whose stretch would put low notes before bar 1 (only near the very start, with a high hand) sends those notes a cycle on: a copy before bar 1 is never chosen.
- **The phase jump stays** (`550`): the offset from the nearest beat is added to the chosen key's tick as it is to `key_tick` today.

### 3. The keybed size

A setting (settings, not the project: it describes the hardware): the keyboard's number of keys, 25, 37, 49, 61 or 88. It is assumed to start on a C for now.

### 4. The lit window

Stev keeps a guess of the notes the keyboard covers: a window of the keybed size starting on a C. A note outside the guess moves it by whole octaves, just enough to include the note (a hardware octave button shifts by whole octaves too, so the guess stays in step), before the note's press is handled. The lane lights the window on the stretch § 2 places it on.

```
bar   1     7                       31    37
      |▓▓▓▓▓|░░░░░░░░░░░░░░░░░░░░░░░|░░░░░|░░░░░
       └ lit: the keys the hand is on
```

**The guess decides the stretch.** It can be an octave off until a key near the keyboard's edge is played (one note on a 25-key C-to-C keybed fits two windows). That moves the hand's middle by an octave, so it only matters with the cursor within an octave's worth (3 bars at a beat a key) of where the hand switches copies, and then the whole hand lands a cycle away (30 bars). The lit window shows exactly where, and the next note near an edge puts the guess right. Accepted (the first design kept the guess display only, § Changes while building).

### 5. When the lit window moves

- **At once on a cursor move** (a click in the lane or the arranger) when a different copy of the hand's middle becomes the nearest; the whole window moves together:

  ```
  25-key keyboard on notes 0–24 (bars 1–7), middle C-1 at bar 4:
  Click at bar 10:   |▓▓▓▓▓|                       |░░░░░|
  Click at bar 18:   |▓▓▓▓▓|                       |░░░░░|   nothing changes
  Click at bar 19:   |░░░░░|                       |▓▓▓▓▓|   moves to the next copy
  ```

- **After a hardware octave button, only at the next key press.** Most keyboards send nothing for the button, so Stev can't know until a note arrives; that note moves the guess first, and its press jumps by the moved guess. Accepted.
- **Never by playing.** Nothing about the playhead moves it.

### Unchanged

The phase jump (`550`), the key length (`510` step 1b), arming and selection (`450`), the capture and its render (`520`), the strip's look (`510`).

## Open points (decided while building)

- **A tie** at exactly half a cycle picks the copy ahead. A copy before bar 1 is never chosen. *Built so.*
- **Notes 120–127**: *decided in phase 1:* they wrap onto keys 0–7 like the rest of the mod, sharing them with notes 0–7 (the same names). A key is named by its note in `0..120` (`note_for_key`), so the names run C-2 … B7 across a cycle, and a held alias lights its key.
- **Where the guess lives**: first built as view state fed each frame from `LiveNotes`. *Revised in phase 2:* in `LiveNotes` itself, one atomic, moved by the input thread at each note-on before the note goes on to the sequencer, so the press handler and the drawing read the same guess and a press always jumps by the guess its own note left. No lock (`000`). It follows notes armed or not: the keyboard's octave is the hardware's. The keybed size is set on it from settings at launch and from the settings modal.
- **The lit window when unarmed**: drawn dimmer with the rest of the lane, or not at all. *Decided in phase 2:* not at all, as held keys light nothing unarmed.
- **The default keybed size** *(decided in phase 2)*: 49, whose home octave is the 36–84 § 2 names. A window starts at its home octave, centred on C3 to the octave below (25 keys: 48–72; 88: 24–111). A size changed in settings keeps the window's lowest C until the next press puts it right.
- **The setting's place** *(decided in phase 2)*: the MIDI tab, a `KEYBED (KEYS)` row of five choices under OUT OFFSET, ↑/↓ or a click picking one, applied and saved at once.

## Changes while building

Hand-testing phase 1's first build (2026-11-23) found note 0, the lowest, landing on bar 16 from a cursor at bar 1: C3 was bar 1, so note 0 sat halfway round the cycle, and the copy a press took followed the playhead while running. Neither was what was meant:

- **Note 0 = bar 1.** C3 = bar 1 dated from 0.1.0 with no reason written down (most likely middle C at a keyboard's home octave); once the keys repeat it buys nothing, and a key being its note number needs no explaining. `config::PERFORMANCE_LANE_BASE_NOTE` is gone.
- **The cursor, running too.** A press takes the copy nearest the cursor whether the transport runs or not, so a performance's mapping is fixed for as long as the cursor stays put.

Hand-testing phase 2's first build (2026-11-23), which kept each key on its own nearest copy and lit the window key by key, showed the window split in two with the cursor near bar 59: the top of the hand around bars 45–48, the bottom around 73–75, low keys to the right of high ones. Each lit key was where its press would land, which was the trouble: the keyboard itself was split, a run up the keys jumping forward to bar 73 and back to bar 45. So:

- **The hand as one block** (§ 2): the stretch chosen for the hand's middle key, every press in it, so the window is never split and every press lands where it is lit.
- **The guess decides jumps** (§ 4). The first design kept it display only so a wrong guess could never send playback anywhere wrong; with the guess on screen, a wrong one shows where it sends the hand.
- **The A/B argument dropped.** In use the player performs, captures with `/`, and A/B's the capture against other sections; going back and forth between performing and arranging, and the muscle memory a cursor move could cost, weighed less than the first design gave them. The cursor still decides, running or stopped, and the playhead never does: a window that moved because the song played past it was rejected.
- **A held key lights only where its press landed**: its key in the hand's stretch. Phase 1 lit every copy on screen.

## What changes in code

- `models/performance_lane.rs`: `note_for_key` over the 120-key cycle; a pure `nearest_copy_tick(meter, length, key, around_tick)`; the guess as a small pure type (`KeybedWindow`, updated by a note), and the hand's stretch from it (`KeybedWindow::hand`, a `Hand` mapping a note to its key along the timeline and back). Tests: the inverse, the nearest copy either side of the halfway point, the tie, before bar 1, other meters and key lengths, the window moving by whole octaves, the hand unbroken across a wrap, before bar 1, a cursor move switching it whole.
- `core/shared_atomics.rs`: the guess in `LiveNotes`, moved at each note-on.
- `core/event_handlers/performance_lane_handler.rs`: the jump target from the note's key in the hand, then the phase offset as today.
- `view/display/rendering/performance_lane.rs`: `visible_keys` no longer capped at `KEY_COUNT`; keys drawn repeating; the hand's stretch lit, and held keys there.
- Settings: the keybed size in `SettingsData` (`060`), a row in the settings modal.
- Docs: `110` § Scope and § Rendering, `510`, `060`, `CHANGELOG.md`.

## What to check by hand

- At a beat a key, a song past bar 18: perform forward through bar 40 and reach every bar.
- Bar 10 / bar 25: C3 lands on bar 1 and on bar 31.
- Cursor at bar 73, 49 keys at home: C2 lands on bar 73, the hand lit over bars 70–82, and it stays there through a whole performance however far the song plays on.
- A 25-key keybed: the lit window follows a hardware octave shift at the next press, and that press lands inside the moved window.
- Move the cursor until the hand switches copy: the window moves whole, never split.
- The phase jump still keeps the beat on a copy past the first cycle.

## Not in this design

- **Stev-side octave keys** that move the window at once, and **a CC from the keyboard's octave buttons** mapped to them. Follow-ups if the next-press update is a problem in use.
- **A keybed not starting on C** (a "press your lowest key" learn).

## Phases

1. The 120-key cycle and the nearest copy: the mapping, the jump, the repeating keys in the lane, the tests.
2. The keybed size and the lit window: the setting, the guess, its drawing; revised while hand-testing to the hand as one block, which presses follow.
