# The Performance Lane's Reach

**Status: agreed 2026-11-23; phase 1 built 2026-11-23 (as built in `110` § Scope and § Rendering), phase 2 not built; issue #44.** Revised while hand-testing phase 1 (2026-11-23): note 0 (C-2) starts bar 1, not C3, and a press takes the copy nearest the **cursor**, running too, not the playhead (§ Changes while building). The lane's keys repeat along the timeline, and a press jumps to the nearest copy of its key, so the keyboard reaches every part of a song however long it is. A keybed-size setting lets the lane light the keys the player's hand is on. Read `110` (the lane) and `510` (the keys drawn in it) first. This replaces `510` § Not in this design's "A movable base note or an octave shift" bullet.

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

### 2. A press jumps to the nearest copy

Every key now exists many times. A press, running or stopped, jumps to the copy of its key **nearest the arranger cursor**. The keys then reach about half a cycle back and half a cycle ahead of the cursor: ±15 bars at a beat a key in 4/4, ±60 at a bar a key.

```
Cursor at bar 10, press C-2:

bar   1        10                   31
      C-2 ◀─────●                   C-2
      ✔ 9 bars back                 21 bars ahead

Cursor at bar 25, press C-2:

bar   1                     25      31
      C-2                    ●─────▶ C-2
      24 bars back                   ✔ 6 bars ahead
```

- **No wall.** The cursor picks the stretch of the song the keys reach; the keyboard's octave buttons pick where in it the hand is.
- **A performance keeps its mapping.** The playhead moving, by playing or by a press, never changes which copy a key takes; only a cursor move does.
- **A/B-ing is safe.** A cursor move only changes which copy is nearest, and moving back gives the same mapping again. A song shorter than half a cycle is unaffected by the cursor.
- **The default octave isn't bar 1.** A keyboard at its home octave (about notes 36–84) reaches bars 10–22 from a cursor at bar 1; bar 1 is a few octaves down. Accepted: the note number is the whole mapping.
- **The phase jump stays** (`550`): the offset from the nearest beat is added to the chosen copy's tick as it is to `key_tick` today.

### 3. The keybed size

A setting (settings, not the project: it describes the hardware): the keyboard's number of keys, 25, 37, 49, 61 or 88. It is assumed to start on a C for now.

### 4. The lit window

Stev keeps a guess of the notes the keyboard covers: a window of the keybed size starting on a C. A note outside the guess moves it by whole octaves, just enough to include the note (a hardware octave button shifts by whole octaves too, so the guess stays in step). The lane lights the window on the copy nearest the cursor.

```
bar   1     7                       31    37
      |▓▓▓▓▓|░░░░░░░░░░░░░░░░░░░░░░░|░░░░░|░░░░░
       └ lit: the keys the hand is on
```

**The guess is display only.** A jump depends on the note and the nearest-copy rule, never on the guess, so a wrong guess lights the wrong keys but never sends playback anywhere wrong. It can be an octave off until a key near the keyboard's edge is played (one note on a 25-key C-to-C keybed fits two windows).

### 5. When the lit window moves

- **At once on a cursor move** (a click in the lane or the arranger) when a different copy becomes the nearest:

  ```
  Click at bar 10:   |▓▓▓▓▓|                       |░░░░░|
  Click at bar 14:   |▓▓▓▓▓|                       |░░░░░|   nothing changes
  Click at bar 25:   |░░░░░|                       |▓▓▓▓▓|   moves to the next copy
  ```

- **After a hardware octave button, only at the next key press.** Most keyboards send nothing for the button, so Stev can't know until a note arrives, and that press also jumps. Accepted.

### Unchanged

The phase jump (`550`), the key length (`510` step 1b), arming and selection (`450`), the capture and its render (`520`), the strip's look (`510`).

## Open points (decided while building)

- **A tie** at exactly half a cycle picks the copy ahead. A copy before bar 1 is never chosen. *Built so.*
- **Notes 120–127**: *decided in phase 1:* they wrap onto keys 0–7 like the rest of the mod, sharing them with notes 0–7 (the same names). A key is named by its note in `0..120` (`note_for_key`), so the names run C-2 … B7 across a cycle, and a held alias lights its key.
- **Where the guess lives**: view state fed from the lane's `LiveNotes`, with no lock on the render path (`000`).
- **The lit window when unarmed**: drawn dimmer with the rest of the lane, or not at all.

## Changes while building

Hand-testing phase 1's first build (2026-11-23) found note 0, the lowest, landing on bar 16 from a cursor at bar 1: C3 was bar 1, so note 0 sat halfway round the cycle, and the copy a press took followed the playhead while running. Neither was what was meant:

- **Note 0 = bar 1.** C3 = bar 1 dated from 0.1.0 with no reason written down (most likely middle C at a keyboard's home octave); once the keys repeat it buys nothing, and a key being its note number needs no explaining. `config::PERFORMANCE_LANE_BASE_NOTE` is gone.
- **The cursor, running too.** A press takes the copy nearest the cursor whether the transport runs or not, so a performance's mapping is fixed for as long as the cursor stays put.

## What changes in code

- `models/performance_lane.rs`: `key_index_from_note` and `note_for_key` over the 120-key cycle; a pure `nearest_copy_tick(meter, length, key, around_tick)`; the lit-window guess as a small pure type (`KeybedWindow`, updated by a note). Tests: the mod and its inverse across 0–127, the nearest copy either side of the halfway point, the tie, before bar 1, other meters and key lengths, the window moving by whole octaves.
- `core/event_handlers/performance_lane_handler.rs`: the jump target from the copy nearest the cursor, then the phase offset as today.
- `view/display/rendering/performance_lane.rs`: `visible_keys` no longer capped at `KEY_COUNT`; keys drawn repeating; the lit window on the nearest copy.
- Settings: the keybed size in `SettingsData` (`060`), a row in the settings modal.
- Docs: `110` § Scope and § Rendering, `510`, `060`, `CHANGELOG.md`.

## What to check by hand

- At a beat a key, a song past bar 18: perform forward through bar 40 and reach every bar.
- Bar 10 / bar 25: C3 lands on bar 1 and on bar 31.
- A/B: move the cursor away and back; the same key lands in the same place.
- A 25-key keybed: the lit window follows a hardware octave shift at the next press, and a click past the halfway point moves it to the next copy.
- The phase jump still keeps the beat on a copy past the first cycle.

## Not in this design

- **Stev-side octave keys** that move the window at once, and **a CC from the keyboard's octave buttons** mapped to them. Follow-ups if the next-press update is a problem in use.
- **A keybed not starting on C** (a "press your lowest key" learn).

## Phases

1. The 120-key cycle and the nearest copy: the mapping, the jump, the repeating keys in the lane, the tests.
2. The keybed size and the lit window: the setting, the guess, its drawing.
