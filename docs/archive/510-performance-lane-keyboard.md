# The Performance Lane as a Keyboard

**Archived 2026-11-25. Status: design agreed and built 2026-11-18** (issue #37; as built in `110` § Rendering). Two things changed while building it, by hand-testing: the keys are a thin strip, not whole-bar fills, and a key is a beat, not a bar (§ Changes while building). The decisions below are the design as agreed. **Step 1b, choosing a key's length (bar, half bar or beat) from the lane's gutter: design agreed and built 2026-11-18; issue #38** (§ Step 1b; as built in `110` § Scope and § Rendering, `060`). **The 68 keys from C3 (`KEY_COUNT`, `config::PERFORMANCE_LANE_BASE_NOTE`) and "notes below C3 ignored" are gone since `560` phase 1 (2026-11-23):** a key is its note number in a cycle of 120 (C-2 = bar 1), repeating along the timeline, a press taking the copy nearest the cursor. **The labels (decision 3) and the cycle are gone since `570` (2026-11-25):** the hand is placed by octave around the cursor, so a key has no fixed note and no name; the strip's black keys show where the C's are. The performance lane (`110`) draws the keyboard that drives it, lined up with the bars: each bar's slice of the lane is filled with its key's colour, white or black as on the piano roll, the C's carry their name (C3, C4, …), every key does once a bar is wide enough, and a key held while the lane is armed lights its bar. The mapping stays exactly as it is (C3 = bar 1, each semitone the next bar). Nothing is stored, nothing is persisted, no command is added. This is step 1 of the "Performance lane as a keyboard" idea; step 2, one key per idea instead of one per bar, waits to see whether step 1 is enough.

## Background

A workflow has grown out of the marquee transpose (`500`) and the ruler click that plays without clearing the marquee (`020` § Timeline click): put ideas side by side that don't necessarily fit, shift them by ear until they work together, then perform on them with the lane. That is where it gets interesting, and it could become a quicker way to build structure (the "capture a performance-lane performance" idea in the notes).

The trouble is keeping track of which key plays which bar. Two causes:

- **Nothing shows the mapping.** The lane is deliberately barebones (`110` § Rendering): it shows the selection accent and nothing else, so the mapping is counted in the head.
- **The mapping doesn't follow the ideas.** With 4-bar ideas the starts land on C, E, G♯, C: every fourth semitone, half of them on black keys. 8-bar ideas are worse.

Step 1 answers the first cause only. It is small, keeps the mapping, and shows whether seeing it is enough before the second cause is touched.

### Why on the timeline

A keyboard widget of its own (in the gutter, or a strip under the transport) would need its own geometry and could drift out of step with the arranger. Drawn in the lane, the keys are the bars: they follow zoom and scroll through the same `tick_to_x` every clip uses, and the eye goes straight up from a clip to its key ("this idea starts on the E"). The lane row already spans the arranger's width, so nothing in the layout changes.

### Why `110`'s "barebones" rule gives way

`110` says not to bring lane visuals back "without a specific reason". The visuals it removed were a growing span and a "Bar N" label per trigger, feedback about a trigger that the playhead already gives. This is different: it shows the mapping, which nothing else shows, and it is static (it changes with zoom, scroll and the meter, never with playing). That is the reason, and `110` records it.

## Decisions

1. **Each bar's slice of the lane gets its key's colour.** Bar `b` (0-based) is played by note `PERFORMANCE_LANE_BASE_NOTE + b`; its slice, from `meter.bars_to_ticks(b)` to `meter.bars_to_ticks(b + 1)` through `tick_to_x`, is filled with `theme::piano_key_white()` or `theme::piano_key_black()` by `is_black_key` (`piano_keys.rs`), the colours the piano roll's keys use. The meter is the project's (`030`), so a 3/4 or 7/8 project's keys are as wide as its bars. The inverse of `bar_index_from_note` is a new pure `note_for_bar(bar_index) -> Option<u8>` beside it in `models/performance_lane.rs`, `None` past note 127 (bar 68): those bars, and anything before bar 1, keep today's plain lane fill, since no key reaches them.

2. **A seam between two white keys.** E/F and B/C are the only neighbours with no black key between them, and without a line they read as one wide key, the problem the piano roll solves with a `grid_seam_color()` groove (`piano_keys.rs`). The lane draws the same groove at those bar lines. Every other boundary is legible from the colour change alone.

3. **Labels: the C's always, every key once there's room.** *Every key's name removed 2026-11-23: only the C's are named, at every key length; every key named at a beat crowded the lane.* Each C slice carries its name in the piano roll's convention (Cubase-style, MIDI 60 = C3, `octave_of`), so C3 sits over bar 1, C4 over bar 13. Once a bar is wide enough for a label at the lane's font (a threshold measured from the widest name the lane can show, a sharp like `"C#4"`, plus padding, not a zoom level), every key carries its name, sharps only, as the MCP units do (`360`). Labels are in `fg_dim` on white keys and `fg` on black ones, or whatever pair reads on both in every palette; no alarm colour. A label is placed inside its slice and clipped to it, so a scrolled-off bar's label never lands on its neighbour.

4. **Drawn always, armed or not.** The mapping is useful while composing as well as while performing: lining up an idea's start with a key happens with a track selected. Unarmed, the keys are drawn at reduced strength (a `gamma_multiply`, tuned by eye) so the lane doesn't compete with the tracks; armed, at full strength, with today's accent stripe and gutter tint marking the selection. The lane's content fill (`lane_grid_fill`) is replaced by the keys where keys are drawn.

5. **A held key lights its bar, only while armed.** The view already knows which keys the live keyboard holds: `LiveNotes` (`SharedAtomics::live_notes`) is set for every note edge whether the lane is armed or not, and wakes the UI on each change. While the lane is armed, each held key from `PERFORMANCE_LANE_BASE_NOTE` up lights its bar's slice in `theme::accent()`, as the piano roll lights a held key. Every held key lights, not just the one the transport jumped to: the lane is monophonic (the latest `NoteOn` wins), but the lit bars are what the hands hold, and the playhead already shows where the transport went. Unarmed, held keys play the selected track and light nothing in the lane. No new cross-thread state, no lock (`000`).

6. **Unchanged.**
    - **The mapping**: C3 = bar 1, one semitone per bar, notes below C3 ignored (`config::PERFORMANCE_LANE_BASE_NOTE`).
    - **The trigger path**: `handle_performance_lane_midi_input`, the loop-wrap suspension, the release that doesn't stop the transport (`110` § MIDI routing).
    - **Arming and selection** (`450`): still a click on the row; the hit test (`performance_lane_hit_at`) and the lane's height (`PERFORMANCE_LANE_H`) stay as they are.
    - **Persistence and undo**: nothing is stored, so no DTO (`060`) and no edit (`050`).
    - **Keys and commands**: none added; the keymap and `FIXED` are untouched.
    - **The audio export and the MCP server**: neither sees the lane.

## Things to check while building

- **Cost per frame.** Only the visible bars are drawn: from the bar at the left edge to the bar at the right, capped at bar 68. At the farthest zoom-out that is still at most 68 rectangles; labels are skipped below the width threshold before any text is laid out.
- **What else crosses the lane.** The playhead is drawn over it unconditionally while running and must stay visible on a white key (it is the trigger's only feedback, `110` § Rendering). The cursor accent (`draw_selected_track_cursor`) sits on the lane row while it is armed and must read on both key colours. Check whether the arranger's bar grid lines are drawn through the row; if they are, they double the slice edges and the lane may drop them.
- **Palettes.** `piano_key_white` is tuned for a narrow column in the piano roll; across the full width of a 24 pt row it may be loud in a dark palette. Tune the unarmed and armed strengths by eye in every palette, and the label pair with them.
- **A held key past bar 68** can't happen (note 127 is bar 68), but a held key whose bar is scrolled off draws nothing, and that's fine.
- **Tests** (`models/` and pure `view/` geometry): `note_for_bar` as the exact inverse of `bar_index_from_note` across 0–67 and `None` from 68; the visible-bar range for a scroll and zoom, including the cap; the E/F and B/C seam rule; the label threshold against bar width; in another meter, a slice as wide as `bar_ticks`.
- **Docs**: `110` § Rendering (the keys, the labels, the lit bars, and the reason the barebones rule gives way here), `030` (the lane in the arranger's anatomy), `CHANGELOG.md`.

## Changes while building

Decided 2026-11-18 by looking at the built lane:

- **A thin strip, not whole-bar fills.** Filling each bar's slice of the 24 pt row read as heavy alternating slabs across the arranger. Softer tones were tried (accent tints armed, then the lane's own greys moved toward white), and neither worked: the problem was the area, not the colour. The piano roll's key colours work in a narrow band, as in its key column, so the keys are a 5 pt strip along the lane's bottom edge (`KEY_STRIP_H`), in `piano_key_white`/`piano_key_black`, with the seams in it. The plain lane fill (accent-tinted while armed) is above it, holding the names in `fg_dim`. A held key lights its piece of the strip in the accent, with a faint wash over its span above. The strip was also tried along the top edge, under the ruler; the bottom read better. Later (2026-11-20) the flat alternation read as a dashed line, its black keys lost in the dark lane, so the strip became a keyboard: 8 pt, the white unbroken, each black key hanging from the top (`110` § Rendering).
- **A key is a beat, not a bar.** Each semitone jumps one counted beat of the project's meter (`key_tick`, `meter.beat_ticks()`): in 4/4 C3 = bar 1, C♯3 = bar 1 beat 2, E3 = bar 2, C4 = bar 4. This reverses decision 6's "the mapping is unchanged", and the trigger path changes with it (`key_index_from_note` and `key_tick` in place of `bar_index_from_note` and `bars_to_ticks`). Half a bar per key was tried first: in 4/4 it is two beats, but in 3/4 it lands on beat 2½ and in 7/8 halfway through an eighth, where a beat lands on the beat in every meter. Finer than a beat was ruled out: a key per eighth reaches 8½ bars of 4/4, and jumps mid-beat read as stutters rather than moves. The keyboard now reaches 68 beats from C3 (17 bars of 4/4, not 68 bars; `Not in this design` keeps the movable base note out). `note_for_bar` became `note_for_key`, and the keys are drawn a beat wide.

## Step 1b: choosing a key's length

**Status: design agreed and built 2026-11-18.** Issue: #38. As built in `110` § Scope and § Rendering, `060`; one difference from decision 3, noted there. One key per beat (§ Changes while building) suits jumping inside an idea, but a key per bar is still the quicker way to move between ideas, and a half bar sits between them. Which one fits depends on the piece, so the lane gets a choice of three, made from its gutter and saved with the project.

### Decisions

1. **Three key lengths: a bar, half a bar, a beat.** A new `KeyLength { Bar, HalfBar, Beat }` in `models/performance_lane.rs`, with `ticks(meter)`: `meter.bar_ticks()`, half of it, or `meter.beat_ticks()`. `key_tick(meter, length, key_index)` multiplies; it stays the one place the mapping lives, for the trigger and the drawing alike. The keyboard keeps its 68 keys from C3 (`KEY_COUNT`), so it reaches 68 bars, 34, or 68 beats (17 bars of 4/4). Nothing finer than a beat (§ Changes while building).

2. **Half a bar only where it lands on a beat.** In 3/4 half a bar is beat 2½ and in 7/8 half an eighth, so half a bar is offered only when the meter's numerator is even, the rule the arranger grid already follows (`grid.rs`, `ladder`: the half bar only when `numerator().is_multiple_of(2)`). A pure `KeyLength::in_meter(meter)` gives the length that plays: `HalfBar` in an odd meter plays as `Beat`. The stored choice is kept, so a project that goes from 4/4 to 3/4 and back is at a half bar again; nothing changes the choice but the user.

3. **Picked from labels in the lane's gutter.** The gutter shows `Bar`, `½` and `Beat`, laid out and styled as the piano roll's lane switch (`VEL`/`PB`/`MW`, `clip_lane.rs`, `380`): the length that plays in `fg`, the others in `fg_dim` (as built: the switch's own `toggle_icon_color`, the length that plays in the accent, a hovered one `fg`, the rest `fg_dim`; the labels sit side by side in the 24 pt row, not stacked); in an odd meter `½` isn't drawn and `Beat` is lit if `½` is the stored choice. A click on a label picks that length and does nothing else. It doesn't arm the lane or change the selection: picking a length is not choosing the lane. A click anywhere else on the row arms it, as today (`performance_lane_hit_at`). The labels' rects come from one pure geometry function, shared by the drawing and the hit test, as `ArrangerLayout` is for the row. No key and no keymap action: the length is set before playing, not while. A key to cycle can come later if it's missed.

4. **The sequencer owns it; the view reads it.** The trigger needs it (`handle_performance_lane_midi_input` computes the jump), so it lives in the sequencer beside the lane, set by a new `SequencerCommand::SetPerformanceLaneKeyLength(KeyLength)`. The view draws from it through a `SharedAtomics` entry (`Arc<AtomicU8>`), the way it reads the meter (`SharedAtomics::meter`): no lock, no `UiEvent` (`000`). A load sets it like the meter.

5. **Saved with the project; not an undo step.** The right length belongs to the piece (its meter, how long its ideas are), not to the player in general, so it goes in the project, not the settings: one optional field on `ProjectData` (`060`), `#[serde(default)]`. A project saved before the field opens at a beat, what the lane does since § Changes while building; the released one-key-a-bar mapping isn't brought back for old files. New projects and templates start at a bar (changed 2026-11-19; an old project keeps its beat). Changing it counts as unsaved (the fingerprint, `410`, takes it in) but isn't an undo step: it is a setting of how the lane plays, like the loop toggle, not an edit (`050`).

6. **A change takes effect from the next key.** A key held when the length changes keeps the jump it made; its release does nothing (`110` § MIDI routing). The strip redraws at the new width at once, so the new reach of each key shows before the next one is played.

7. **Unchanged.** The trigger path apart from the tick it jumps to; arming and selection (`450`); the strip, seams, labels and lit keys (§ Changes while building), now a key length wide, with the every-key label threshold measured against that width; the base note (C3); the audio export and the MCP server, which don't see the lane.

### Things to check while building

- **The gutter's room.** Three labels in the 160 pt track column at the lane's 24 pt height, beside the 4 px armed marker: check they fit at `FONT_SIZE_TL` and leave the marker clear.
- **A click on a label while armed** must leave the lane armed, and unarmed must leave it unarmed; a click on the gutter outside the labels arms as today.
- **The meter change** with `½` stored, both ways, and what the gutter lights.
- **An old project** (no field) opens at a beat; a project saved at a bar opens at a bar; the recovery copy (`410`) restores the length.
- **Tests** (`models/` and pure `view/` geometry): `KeyLength::ticks` and `in_meter` in 4/4, 3/4, 6/8 and 7/8; `key_tick` for each length; the label rects and their hit test; the DTO round trip and the missing-field default.
- **Docs**: `110` (§ Scope's mapping, § Rendering's gutter), `060` (the field), `030` (the gutter), `000` if the `SharedAtomics` list names its entries, `CHANGELOG.md`.

### Phases

One phase: `KeyLength`, the command and the shared atomic, the field, the gutter labels and their hit test, the tests and the docs above.

## Not in this design

- **Step 2, one key per idea.** The keys would map to where ideas start, worked out from the clips (every bar where a clip starts on some track), so the first idea is C, the second D, the third E, on white keys only. It changes the mapping, needs a rule for staggered clip starts across tracks and a decision about jumps inside an idea, and would make a captured performance read in idea order. Kept in the notes until step 1 has been used and counting keys is still a problem.
- **A movable base note or an octave shift for the lane.** The keyboard's 68 keys from C3 reach 68 bars, 34 or 17 (§ Step 1b). The end was hit in use (2026-11-23); `560` replaces this with keys repeating along the timeline.
- **Labelling bars by number in the lane.** The ruler already numbers them; the lane names keys.
- **Recording what the lane played.** `110` keeps the lane live-only; the "capture a performance-lane performance" idea is its own design.

## Phases

One phase: `note_for_bar`, the key slices with their seams and labels, the lit bars, the tests and the docs above.
