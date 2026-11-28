# Markers

**Status: agreed 2026-11-26; both phases built 2026-11-27; revised 2026-11-27 (the ruler and the strip one place to click, decision 6). Issue: #49.** Up to nine numbered markers in the arrangement, each recalled by its digit: `⌘1`–`⌘9` sets marker *n* at the cursor, `1`–`9` recalls it. Stopped, a recall moves the cursor there; running, it moves the playhead and leaves the cursor where it is. The markers sit as flags in a strip of their own under the arranger's ruler, where a click recalls one, a drag moves it, a click away from the flags plays from there as the ruler does, and with the strip focused `Delete` removes it and `⌘R` names it. They are arrangement data: saved in the project, undoable, carried along by the edits that insert or delete time.

## Background

Getting around a song today means the arrows, a click in the lanes (the cursor), a click on the ruler (plays from there, `020` § Timeline click), the overview (`590`), or the performance lane's keys (`110`, `570`) while it is armed. None of them names a place in the song you can come back to with one key. A marker does: "the chorus is `2`".

What the design leans on:

- **The digits are free.** `010` lists numpad `1`–`9` as spare; egui-winit merges the numpad and the main row into the same `Key::Num1`…`Key::Num9`, so both work. `⌘1`–`⌘9` are unbound (`⌘0` is the UI scale). The computer keyboard's piano uses letters only.
- **The ruler has no free band** (`030` § Strip band budget: range markers on top, bar numbers, the loop band at the bottom), so markers need a strip of their own.
- **A running jump already has a rule**: the performance lane's phase jump (`550`), which keeps the beat. A marker recall is the same kind of jump and should feel the same.
- **A focus area with its own `Delete` has precedents**: the track headers (`Delete` deletes the track, `⌘R` renames it) and the piano roll's wheel lane (`KeyFocus::WheelLane`).

## Decisions

1. **Every marker has a number, 1 to 9, and there are at most nine.** A number is a slot: one marker per number. No unnumbered markers: a marker the keyboard can't reach is only a click target, the ruler click and the overview already cover getting around by mouse, and leaving them out removes the question of mapping a number to a marker. If nine turns out to be tight in use, unnumbered markers can be added later without changing anything here.

2. **The marker strip sits right under the arranger's ruler.** About 14–16px (tuned when built), arranger only, between the timeline strip and the performance lane, the `bg_timeline` family so it reads as part of the ruler group, closed off by the same groove seam. Each marker is a small flag at its tick, its edge on the tick like the cursor triangle's, showing its number and its name if it has one (`2 Chorus`). Nothing is drawn down through the lanes. The left gutter beside the strip shows a quiet `Marker` label in `fg_dim`. (Changed after use, 2026-11-27: the lanes' grid lines run through the strip and the seam between it and the ruler is gone, so the two read as one header block; `030` § Marker strip.) The overview (`590`) shows no markers for now.

3. **`⌘1`–`⌘9` sets marker *n* at the arranger cursor.** If marker *n* exists elsewhere, it moves to the cursor, keeping its name. If another marker already sits on that tick, it is replaced (one marker per tick). Running or stopped, it is always the cursor, never the playhead: edits act at the cursor. Commands (`Action::SetMarker1`…`9`), rebindable, rows in the keymap table.

4. **`1`–`9` recalls marker *n*.** A slot with no marker does nothing, with a footer hint (`No marker 4`).
   - **Stopped**: the arranger cursor moves to the marker, like any cursor move: the view follows it, and the marquee collapses as it does for any cursor move.
   - **Running**: the playhead jumps and the cursor stays put. The jump is the performance lane's (`610-launch-quantize.md` decision 8; it was `archive/550`'s phase jump until 2026-11-28): when playback is as far into a beat as the marker (on a beat for a marker on one) at once, otherwise as soon as it is, landing on the marker exactly, so the beat never breaks, a marker off a beat too. It goes through the same transport path as a lane press, so the sustain lift (`archive/530`), the clock alignment (`150`) and the capture's view of the jump all follow from it. A recall and a lane press share the one waiting jump: the latest wins.
   - **From the clip view**: the digits recall too (the arranger's cursor or playhead moves); markers are arrangement state, and a digit means nothing else there.
   - Commands (`Action::RecallMarker1`…`9`), rebindable.

5. **A recall ignores the loop.** Running with looping on, the playhead jumps to the marker and plays on from there, and wraps only if it reaches the loop region's end. Looping is not turned off. What the transport does when a jump lands past the region's end is checked when building, so that it plays on rather than wrapping at once.

6. **A click on a flag recalls it**, exactly as its digit would, and gives the strip the keyboard with that marker selected (decision 7). A press in the strip away from any flag plays from there, exactly as the ruler above does (`020` § Timeline click), and gives the keyboard back to the pane. (Revised after use, 2026-11-27: at first it did nothing, which left two rails over the same span with different rules, the strip mostly dead. Now the ruler and the strip are one place to click: the loop band first, then a flag, then playback from the click. The cursor triangle and the range arrows stay in the ruler's region band, beside the loop band they match in height; down in the strip they would cover the flag a marker set at the cursor puts there.)

7. **The strip takes the keyboard like the track headers do.** A click on a flag or on the gutter's `Marker` label gives it the keyboard (a new `KeyFocus::Markers`); the selected marker is drawn in `accent`. With it focused:
   - `Delete`/`Backspace` removes the selected marker (one undo step), and the selection moves to the nearest marker left (else right), so presses chain. Nothing left: the keyboard goes back to the pane.
   - `⌘R` renames the selected marker with an inline field over its flag, the same kind of field as the track's: `Enter` commits, `Esc` cancels. A double-click on a flag does the same. An empty name clears it, leaving the number.
   - `←`/`→` select the previous/next marker (selecting only; recall stays on the digits and the click).
   - The digits and `⌘1`–`⌘9` work as everywhere.
   - `Esc`, `Tab` or a click elsewhere gives the keyboard back to the pane, as from the track headers.

8. **A drag on a flag moves the marker.** Snapped to the arranger grid (`snap_to_grid` + `cursor_grid_ticks()`), held to `>= 0`, previewed while dragging and committed on release as one undo step. Past the `DRAG_THRESHOLD_PX` latch only, so a click stays a recall. A marker dropped on another one's tick replaces it. `Esc` cancels mid-drag.

9. **Time edits carry markers along.** Markers belong to the music at their place:
   - **Insert time** (`⌘I`, `InsertSilenceEdit`) at tick *t* of length *d*: markers at or after *t* move *d* later.
   - **Delete time** (`⌘⌫`, `DeleteTimeEdit`) over `[a, b)`: markers inside are **removed**, markers at or after *b* move `b - a` earlier.
   - **Duplicate time** (`DuplicateTimeEdit`, the performance capture's insert included): markers after the insertion point move later by the inserted length. Markers inside the source span are not copied, since a number is one slot.
   - Every other edit leaves markers alone: clip moves, marquee block moves, carving the marquee with `Delete`, pastes.

   Each of these happens inside the existing edit, so its undo puts the markers back with the clips.

10. **Undoable, saved, cleared in templates.** Setting, moving, renaming and deleting a marker are each one `SequencerEdit` (one `SetMarkerEdit` that replaces the marker list, before and after, is enough at nine entries). Undo follows `050` § Undo and the view: it puts back the cursor and marquee the step recorded and never moves the focus. Markers are saved in `ProjectData` (`markers: Vec<MarkerData { number, tick, name }>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, so older projects load with none), hashed by the unsaved-changes fingerprint. **Save as template drops them**, as it drops clips; a new project starts with none.

## What it does not do

- **No unnumbered markers** (decision 1), no banks of nine.
- **No marker colours**, no regions (start and end markers), no "play to marker".
- **No markers in the clip view** or the overview.
- **No recall quantize of its own.** A recall lands like a lane press, and the lane's launch quantize (`610`) applies to both.
- **No marker set at the playhead.** Always the cursor (decision 3).
- **No MIDI mapping** of the recall.

## Docs and tests the build owes

- `010`: the digits, `⌘`+digits, the strip's keys and the mouse (and `FIXED` in `settings/keyboard_tab.rs` for the strip's non-command keys and the mouse). `020`: `KeyFocus::Markers` beside the track header focus, the flag drag in § Clip Edge Drag-Resize's family. `030`: the strip in the layout and its drawing. `050`: the marker edit and how time edits carry markers. `060`: `MarkerData` and the template rule. `110` or `550`: a line saying a marker recall uses the phase jump. `CHANGELOG.md` `[Unreleased]`: Added.
- Tests: the marker list model (set into an empty slot, moving an existing number, replacing a marker on the same tick, delete, rename keeping the tick), the time-edit shifts (insert before/at/after a marker, delete time removing the inside ones and shifting the later ones, duplicate time shifting and not copying), undo of each, the save round trip and an old project without the field, the template dropping them, the recall's stopped/running split, the flag hit test and the drag latch.

## As built (phase 1)

- **The model** is `models/markers.rs`: `Marker { number, tick, name }` and `Markers` (sorted by tick, one per number and per tick; `set`, `rename`, `insert_time`, `delete_time`, `from_saved`). The sequencer holds it (`Sequencer::markers`) and the view mirrors it (`UiEvent::MarkersChanged`, `Display::markers`).
- **The commands**: `Action::SetMarker(n)` / `Action::RecallMarker(n)` (one variant each with the number, not nine; ids `marker.set_n` / `marker.recall_n`, the Keyboard tab's *Markers* group, both `Scope::Global`), sending `SequencerCommand::SetMarker` (→ `SetMarkerEdit::at_cursor`) and `TransportCommand::RecallMarker` (→ `recall_marker_workflow`, `event_handlers/markers.rs`, where the transport is at hand). A flag click sends the same `InputEvent::Action`.
- **Decision 5's check**: `Transport::tick` wraps only from inside the loop region (`current >= start && current < end`), so a jump past the region's end plays on, and one before its start plays into it and then loops.
- **Duplicate Time** gets its marker shift through the `InsertSilenceEdit` it composes: the markers at or after the insertion point move (the clips' rule), none is copied.
- **The strip** is `MARKER_STRIP_H` (18px since 2026-11-27, when the seam above it went; 20 after phase 2's hand test; 16 at first), drawn by `draw_marker_strip` (`rendering/markers.rs`), hit-tested by `marker_flag_at` / `is_mouse_on_marker_strip`.

## As built (phase 2)

- **The changes** are one `MarkerChange` (`Move`, `Rename`, `Remove`, `Markers::change`), sent as `InputEvent::ChangeMarker` → `SequencerCommand::ChangeMarker` → `SetMarkerEdit::changed`: the same before/after swap as the set, one undo step, none when nothing changes.
- **The focus** is `KeyFocus::Markers(n)`, the selected marker by number; its keys are `input/marker_focus.rs` (`marker_strip_key`). The pane commands that hand the keyboard back are `Action::leaves_marker_strip`: the track headers' rule, less setting a marker, `⌘R`, the zoom and undo / redo, so finding a flag by zooming keeps it selected, and an undo of a drag leaves the dragged marker selected and on top of the flag it overlaps (hand-tested: handing the keyboard back on `⌘Z` dropped the selection, and the flag it had covered came back on top). An undo of a `Delete` selects the marker it brings back, and a redo moves the selection on again (`follow_selection`), for the same reason: otherwise the neighbour the `Delete` selected stayed on top of the returned marker.
- **The label click** selects the marker at or before the cursor (else the first), not the first: the one most likely meant. With no markers it does nothing, and the keyboard stays with the pane.
- **The click recalls on its release**, not its press, so a drag never recalls on the way.
- **The drag** is `MarkerDrag` (`pointer_state.rs`), keeping where the flag was grabbed; its preview applies the drop's own rule to a copy of the list (`shown_markers`), so a marker it would replace disappears while it hovers there.
- **Names** are `MARKER_NAME_MAX_CHARS` (24) at most; a flag is as wide as its label, measured once a frame for the hit test (`measure_marker_flags`). The rename field is `fields/marker_rename.rs`, at least 120pt wide over the flag.

## Phases

1. **Markers you can set and recall.** The model, `SetMarkerEdit`, persistence and templates, the time-edit shifts, `⌘1`–`⌘9` and `1`–`9` (stopped and running), the strip drawn with its flags and numbers, a click on a flag recalling it.
2. **Editing them in the strip.** `KeyFocus::Markers` with the selected marker, `Delete`, `←`/`→`, the drag to move, names (`⌘R`, double-click, drawn on the flag).
