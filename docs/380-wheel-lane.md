# Wheel Lane

**Status: design agreed 2026-10-30; not built.** The piano roll's velocity lane becomes one of three: velocity (VEL), pitch bend (PB) or the mod wheel (MW), picked in the gutter to its left. The two wheel lanes show the lead clip's recorded wheel data as a stepped line, read-only. Separately, a ⌘-drag on a note shows the velocity it is setting in the footer, whichever lane is up. Read `370` (wheel capture: what lands in a clip and how), `090` (what the capture buffer records) and `030` § the piano roll and velocity lane first.

## Background

Before wheel capture, recorded bends and mod moves only rode in with notes, so leaving them invisible was a fair trade (`090`, decided 2026-10-14: "if wheel data is ever shown, it is in an automation lane"). `370` changed that. A take now *replaces* a wheel's data from its first move to the window's end and writes the old value back there, and a mod wheel parked off its rest overwrites the clip's mod curve on every take into it. Those rules are deliberate, but with the data invisible the only checks are the ear and ⌘Z, so a take's effect can't be predicted or confirmed. This lane is the one view the 10-14 decision left room for.

## Decisions (2026-10-30)

1. **Pitch bend and the mod wheel only.** The two wheels the buffer records (`Wheel` in `core/midi/message.rs`). Other CCs wait for the CC-mapping follow-up `370` names, and would join as more lanes in the same switch.
2. **In the clip view, not the arranger.** The wheel data belongs to the clip, so the lane belongs to the piano roll, beside the notes it plays with. This is a clip's controller lane (Ableton's clip envelopes), not track automation: no arranger lanes, no data that outlives its clip.
3. **The velocity lane's area, one lane at a time.** The three share `VELOCITY_PANEL_H` and the piano roll's time grid. No stacked lanes: the docked clip panel is already tight, and one lane at a time is enough to look at. No overlaying a curve on the velocity bars: two scales in one box read as neither. A fixed height, shared by all three lanes, so switching never moves the note grid: 80 px gives pitch bend about 40 px each side of centre, enough to read a curve's shape, which is the lane's job while it is read-only.
4. **The switch is in the gutter.** The two piano-roll columns left of the lane (`2 * PIANO_MARGIN_W`) hold three stacked labels, `VEL`, `PB`, `MW`: the shown one in `fg`, the others `fg_dim`, `fg` under the pointer with the pointing hand. A click picks one. No highlight box or colour, like the rest of the chrome. The lane's right-aligned `VEL` label goes, since the gutter says which lane is up.
5. **A key cycles it.** One command, `CycleClipLane` (VEL → PB → MW → VEL), in `ACTIONS` with its row in `010`. Proposed default `W` (for wheel; free outside the computer keyboard, where it is a black key like every letter). Acts on the clip pane, docked or full.
6. **A marker for data.** `PB` or `MW` gets a small dot beside it when the lead clip has events for that wheel, so you know before switching whether there is anything to see. Not shown for `VEL`.
7. **The choice is view state.** A `Display` field, not saved in the project or the settings: every launch starts on `VEL`. It stays when the lead clip changes, so stepping through clips keeps showing the same lane. Never stored per clip.
8. **It never switches on its own.** A wheel take, an undo or a velocity drag leaves the lane as it is (the clip view never changes what it shows on its own, `220`).
9. **Stepped, as it plays.** A wheel has no ramps: each move holds until the next. The lane draws a horizontal line at each value and a vertical step at each move. The line starts at the clip window's start with the value in force there (`Clip::wheels_at`, which folds in moves before the window, as playback's chase does). PB is bipolar: a faint centre line at rest (`0x2000`), bends above and below it. MW runs from 0 at the bottom to 127 at the top. Drawn in the lead clip's track colour, outside the clip's window dimmed like notes outside it (`SILENT_NOTE_SCALE`). No area fill in phase 1; try one only if the line alone reads too thin.
10. **Dense data is folded per pixel column.** Pitch bend can send hundreds of moves a bar. The renderer folds each pixel column to its min/max and draws one vertical span, so the cost follows the lane's width, not the event count (`030` § Rendering performance).
11. **Read-only.** A click or drag in the PB and MW lanes does nothing. A curve is fixed by riding it again (`370`'s latch rule, framed by the loop or `[`/`]`), or taken back with ⌘Z. Editing (deleting a range, drawing) stays in § Out of scope.
12. **⌘-drag velocity works in every lane.** It starts on a note in the note grid (`020` § Velocity Drag), not in the lane, so the lane shown has no bearing on it.
13. **The footer shows the velocity during a ⌘-drag.** While the drag is held, the footer reads `Velocity 96` in `fg_dim` (the footer is where hints go). For a group, the range: `Velocity 64–110`. Shown in every lane, `VEL` included: the bars show the shape, the footer the number. Gone on release.

## Build (phases)

1. **The switch and the wheel lanes.** The lane enum and its `Display` field, the gutter labels and their hit-test, the data markers, `CycleClipLane` with its keymap row, the stepped drawing with the per-column fold, the window dimming. Tests: the per-column fold (pure geometry, beside the clip view's other geometry tests: a column with many moves gives its min/max, an empty column carries the held value), the value in force at the window's start, the PB centre mapping, the keymap stays conflict-free. Docs: `030` (the gutter switch, the lane drawing), `020` (the lane choice as view state), `010` (the command), `090` (wheel data is now shown, in this lane), `370` decision 8, `280` § Not planned. Changelog: the piano roll's lower lane can show pitch bend or the mod wheel.
2. **The velocity readout.** The footer hint during a ⌘-drag, the single value and the group range. Tests: the text for one note, for a group, at the clamps. Docs: `020` § Velocity Drag, `030` § Header & Footer. Changelog: a ⌘-drag on notes shows the velocity in the footer.

## Out of scope

- Editing wheel data in the lane: selecting a range to delete it, drawing, breakpoints. If the read-only lane shows a need, a range delete (select across the lane, `Delete`) is the first candidate.
- Showing what a take replaced: the old curve ghosted under the new one over the landed span, after a take or an undo. It would make `370`'s landing rule visible, but needs the replaced span kept somewhere for the view; decide after using phase 1.
- A resizable lane (dragging the divider between the notes and the lane), if 80 px proves too short in use. Its height would be kept in settings, like the browser's width, and shared by all three lanes.
- Wheel data in the arranger thumbnails.
- Other CCs, aftertouch and sustain (decision 1).
- Arranger or track automation lanes (decision 2).
