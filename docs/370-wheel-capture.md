# Wheel Capture

**Status: design agreed 2026-10-30; phase 1 built 2026-10-30 (`feature/wheel-capture-landing`); phase 2 built 2026-10-30 (`feature/wheel-only-running-takes`). Issue: #13. A proof of concept: if riding a wheel into a clip earns its place, mapping a plugin parameter to a CC is the follow-up (§ Out of scope).** Capture-first automation for the two wheels the capture buffer already records, pitch bend and the mod wheel (CC1): play along with the loop moving only a wheel, press `/`, and the movement replaces that wheel's data in the lead clip over the stretch you touched, leaving the rest of the clip's curve as it was. One framing rule and one landing rule cover every take, running or stopped, with notes or without. Read `090` (what the buffer records), `100` (running capture), `040` (the stopped `/` and the take session) and `050` (edits and undo) first.

## Background

From a discussion on 2026-10-29/30 that began with the MCP server writing automation curves (`360`) and turned to capture instead: the curves should come from playing, as notes do (`280` § Principle), with the MCP route at most a side door to the same data later.

What the code does today:

- **Recorded, never committed alone.** Every bend and mod-wheel move on the armed track lands in the capture buffer (`090`), but "a take is its notes" (`100` § Invariants): every gate and window reads notes, and a buffer of wheel moves alone commits nothing (then `a_capture_of_wheel_moves_alone_commits_nothing`; phase 2 split it in two).
- **Wheels ride inside the notes' window and are added.** A running insert brings the wheel moves in its window, the one after the last note-off included (`Clip::cloned_events_in_range`); the take session's Enter brings those from the window start to the last note-off (`TakeFraming::framed_events`). Both *add* them to the clip's own, so a second pass over a clip that already has a curve interleaves the two.
- **Other controllers aren't recorded.** Sustain, other CCs and aftertouch play live only (`090`).
- **Playback is already right.** `WheelTracker` (`src/models/wheels.rs`) chases the value in force on every seek, from `Clip::wheels_at`, which folds every wheel move before the playback point, the clip's material outside its window included.

## Decisions (2026-10-30)

1. **Both wheels, CC1 and pitch bend, and nothing else.** They are one `Wheel` enum, one tracker and one row format already; splitting the mod wheel off would add a branch everywhere for no gain. Any other CC waits for the parameter-mapping follow-up.
2. **Running and stopped alike.** A stopped take always has notes, so it needs nothing new for its framing; only its landing changes. A stopped take of wheel moves alone still commits nothing: with no notes there is nothing to frame it, and stopped automation on its own has no use.
3. **Into the lead clip.** A wheel-only take lands in the lead clip, as a running insert does (`100`). Without a lead clip it commits nothing: a clip of automation alone, with no notes, isn't a thing Stev makes.
4. **One framing rule.** A take's window is its notes' window, as today, whenever it has notes. A running take with no notes is framed by the pass holding its last wheel move. The wheels never pick a window of their own when there are notes: a bend easing back just after a loop wrap would otherwise frame the next pass and lose the bends played over the notes.
5. **One landing rule, per wheel, latched to the window's end.** For each wheel that moved in the take's window, its span runs from its first move to the window's end. Across the span the clip's own data for that wheel goes and the take's moves come in; at the window's end the value the clip's old curve had there is written back, if it differs from the take's last value. A wheel that didn't move is left alone, so riding the mod wheel never touches the bends, and a notes-only take never touches either wheel. A new clip has no old curve, so the rule reduces to writing the moves.
6. **One span per take.** No gesture splitting: a wheel held still sends nothing, so a gap can't tell holding from letting go.
7. **A wheel off its rest at the window's start counts as a move there.** Set the mod wheel before the first note and hold it, and the take carries that value from its start, as a running take already carries a mod wheel off its rest onto its start tick (`Sequencer::begin_running_take`). That carry is how the value reaches the buffer, which the transport's start empties; the landing reads the value in force at the window's start from the buffer. Today it carries only the mod wheel, so it grows to both wheels (`Sequencer::live_mod_wheel` becomes one value per wheel): a bend held into a take is carried onto the start tick too, by decision 1. The consequence is deliberate: a mod wheel left parked off its rest overwrites the clip's mod curve on every take into it, because that is what was played.
8. **Shown, not edited.** Decided invisible at first (no lane, no hint; the ear the check and ⌘Z the way back); the piano roll's wheel lanes now show what a take landed (`380`, phase 1), and can return a span of it to rest or clear the wheel (`380` phase 3, `Clip::rest_wheel`, built on this landing rule). ⌘Z is still the way back; nothing draws a curve.
9. **One undo step.** The insert edits already restore the clip's whole event list on undo, so the removed data comes back with no new machinery (`050`).
10. **The take session's wheels run past the last note-off.** Its window ends at the last note-off, but a bend easing back during the release belongs to the phrase. So for the wheels the window ends at the next note-on in the take after it, or, with none, at the take's last wheel move after the last note-off, or, with none of those either, at the last note-off itself; never past the target clip's window end. The latch and the write-back of decision 5 use that end. An ease-back played after the note therefore lands as played, and a bend still held when `/` was pressed drops to the old value at the last note-off. The running insert already carries the move after the last note-off (`100`); this brings the stopped insert in line.

### Why latch, not touch

A DAW's *touch* mode hands back to the old curve when the fader is released. A wheel has no release message, and the last move is a poor stand-in: push the mod wheel up and hold it, and the old curve would snap back the moment you stopped moving. So the take's value holds to the end of the window, and **the window is how the player says where the tweak ends**: the loop region while running (loop just the bar to fix), the take session's `[`/`]` when stopped. Both are tools the player already frames takes with. A fixed release time (hold one beat, then glide back) was the alternative; it adds a magic number and still guesses.

## Walkthrough

A 4-bar clip with a mod sweep across it, the lead clip, the loop on bar 3 only.

1. Play. On the second pass, ease the mod wheel up halfway through bar 3, then leave it.
2. `/`. No notes in the buffer, so the take is the pass holding the last move: bar 3. The mod wheel moved, so its span runs from the first move to the end of bar 3.
3. The clip's mod data from that first move to the end of bar 3 is replaced by the take's moves; at the start of bar 4 the old curve's value there is written back. Bars 1–2, the first half of bar 3, and bar 4 play as before. The clip's bends are untouched.
4. ⌘Z restores the old curve.

## Open question (settled in phase 1)

**The running insert's window over a short loop.** A running insert's window is the clip's region length pinned at the anchor's phase in the arranger loop (`100`), so with a loop shorter than the clip it runs past the pass. The wheel span therefore ends on its own, at the end of the pass the window starts in (`CaptureInsert::wheel_end`), never past the clip's end: a 1-bar loop over a 4-bar clip latches to the loop's end (`a_short_loop_latches_the_wheel_to_the_pass_end`). A span ending at the clip's window end writes nothing back: nothing plays there, and the wrap's chase hands back to the clip's start.

## Out of scope

- Any CC other than CC1, and with it the follow-up: a mapping from a plugin parameter to a CC, so riding one knob captures a parameter. Designed in `390` (the knob panel): the plugin does the mapping, Stev's knobs send and capture the CC.
- Showing the data (an automation lane, `280` § Not planned).
- A wheel-only stopped take, gesture splitting, a glide at the hand-back.
- MCP writes of wheel rows (`360`): the reads list them, the writes stay notes-only. If this lands, a write tool could feed the same landing rule later.
- Sustain (`090`: it needs the note-release machinery, not this chase).

## Build (phases)

1. **The landing rule, for the takes that exist.** A pure helper on `Clip` that lands a take's wheel moves over a window by decisions 5–7, unit-tested in `models/clip` (a span per wheel, the write-back, an untouched wheel, a new clip, the in-force value at the start). Route the running insert and the take session's Enter through it, replacing their "add the wheels" paths; new clips (running and stopped) go through it too, so there is one path. The take session's wheel end per decision 10. The running take's start carries both wheels off their rest (decision 7), with `a_mod_wheel_off_its_rest_is_carried_into_the_take` joined by its bend twin. Settle the open question. Tests: a second take over a clip with a curve replaces it instead of interleaving; a notes-only take leaves both wheels; riding one wheel leaves the other; a take-session ease-back after the last note-off lands, a held bend drops to the old value at the last note-off, neither past the clip's window end. Docs: `090`, `100` (§ Notes held into a take: the bend joins the mod wheel), `040`, `050` § Invariants. Changelog: a take into a clip now replaces its wheel data over the span played; a bend held when the transport starts is kept in the take.
2. **Wheel-only running takes.** Decision 4's fallback framing (the pass holding the last wheel move) and decision 3's gate: `Clip::has_notes` stays the gate for a new clip, the running insert also accepts a take with wheel moves. `a_capture_of_wheel_moves_alone_commits_nothing` splits in two: still nothing without a lead clip, a landing with one. Docs: `100` § Invariants ("a take is its notes" becomes "a take is its notes, or its wheels when inserting"), `090`. Changelog: ride the mod wheel or pitch bend over a playing loop and `/` keeps it in the lead clip.
