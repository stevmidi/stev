# The Performance Lane as a Keyboard

**Status: design agreed 2026-11-18; not built.** The performance lane (`110`) draws the keyboard that drives it, lined up with the bars: each bar's slice of the lane is filled with its key's colour, white or black as on the piano roll, the C's carry their name (C3, C4, …), every key does once a bar is wide enough, and a key held while the lane is armed lights its bar. The mapping stays exactly as it is (C3 = bar 1, each semitone the next bar). Nothing is stored, nothing is persisted, no command is added. This is step 1 of the "Performance lane as a keyboard" idea; step 2, one key per idea instead of one per bar, waits to see whether step 1 is enough.

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

3. **Labels: the C's always, every key once there's room.** Each C slice carries its name in the piano roll's convention (Cubase-style, MIDI 60 = C3, `octave_of`), so C3 sits over bar 1, C4 over bar 13. Once a bar is wide enough for a label at the lane's font (a threshold measured from the widest name the lane can show, a sharp like `"C#4"`, plus padding, not a zoom level), every key carries its name, sharps only, as the MCP units do (`360`). Labels are in `fg_dim` on white keys and `fg` on black ones, or whatever pair reads on both in every palette; no alarm colour. A label is placed inside its slice and clipped to it, so a scrolled-off bar's label never lands on its neighbour.

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

## Not in this design

- **Step 2, one key per idea.** The keys would map to where ideas start, worked out from the clips (every bar where a clip starts on some track), so the first idea is C, the second D, the third E, on white keys only. It changes the mapping, needs a rule for staggered clip starts across tracks and a decision about jumps inside an idea, and would make a captured performance read in idea order. Kept in the notes until step 1 has been used and counting keys is still a problem.
- **A movable base note or an octave shift for the lane.** Today's keyboard reaches bars 1–68 from C3; nobody has hit the end.
- **Labelling bars by number in the lane.** The ruler already numbers them; the lane names keys.
- **Recording what the lane played.** `110` keeps the lane live-only; the "capture a performance-lane performance" idea is its own design.

## Phases

One phase: `note_for_bar`, the key slices with their seams and labels, the lit bars, the tests and the docs above.
