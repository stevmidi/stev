# Launch Quantize on the Performance Lane

**Status: drafted, agreed and built 2026-11-28; the § Tuning values stay open to change in use. No issue yet (`080` § Design Docs and Issues).** A press on the armed lane while running waits for the next beat, then jumps to its key's start exactly. The same key from the same spot then starts from the same place every time, and the beat still never breaks. This replaces `550`'s phase jump, which isn't kept as an option. A marker recall while running waits the same way. Read `110` (the lane), `archive/550-phase-jumps.md` (what this replaces) and `archive/520-performance-capture.md` (the run and its render) first. `550` was archived when this was built, and its stop on the beat lives on in `110`.

## Background

`550` made every press while running a phase jump: playback lands at the key plus the press's offset from the nearest beat, cutting in early or late, so the beat never breaks. That fixed what the old jump broke (the metronome restarting at every press, a capture's bar lines off the grid). Found in use (2026-11-28), though, it breaks something else. When a performance turns up a figure worth repeating (the same key, in the same place in the sequence, again and again), each press starts from a slightly different spot: before the key when the press came early, after it when it came late. The phrase never starts the same way twice, and that's what the hand expects: a key is a place, and pressing it should go there.

There were two ways out (2026-11-28):

1. **Back to the old jump** (no quantize): a press jumps at once to its key's start. The start is exact and there's no wait, but the beat restarts wherever the press fell. That brings back the two problems `550` fixed.
2. **Launch quantize** (this design): a press waits for the next beat, then jumps to its key's start. The start is exact and the beat keeps its place. The cost is the wait, up to a beat.

Launch quantize is also what `archive/520` § Principle and the "capture: no post-processing" rule point to: a gridded result comes from quantizing live, before the ears, never from snapping a capture afterwards.

## What a quantized jump is

A press comes with playback about to play tick `p`. Its key's target is `T` (`key_tick`, a beat start in every meter), worked out at the press from the hand and the key length as today. With `b` the meter's beat (`Meter::beat_ticks`):

- **`p` is on a beat** (`p mod b == 0`): playback jumps to `T` at once, and `T` plays in place of `p`.
- **Otherwise the press waits.** Playback runs on to the next beat, and that beat's tick is played as `T`: the jump is made by the sequencer on its own tick, exact to the tick, tighter than a live press.

Either way `T` sounds on a beat of the grid playback was on, so the pulse is unbroken, and it always sounds from its start. As with `550`, only the bar can move: a bar key pressed on beat 3 starts bar 5 there. That's what was played, and the music's bar line moves with it.

## Decisions

1. **Every press while running waits for the next beat** and lands on its key's start. No setting to begin with, nothing saved, no gutter label: the lane's only saved field stays the key length. `550`'s phase jump goes; keeping it as an option would leave the same confusion one click away. A beat is the finest step that keeps the beat (targets are beat starts, so a 16th boundary would shift the beat by a 16th).
2. **The boundary is playback's beat**: the beats of the position playing, not of the arrangement before the first press, nor the run's own clock. Every jump lands on a beat start and runs on from there, so playback's beat grid is the one the click counts and the one the take is laid on. ½ bar and Bar boundaries are left for later (§ Later: a choice of boundary). Unlike the beat, their grid would move whenever a press shifts the bar.
3. **The press that starts the transport jumps to `T` at once**, as today. Stopped, there's no grid to keep and nothing to wait for. It still starts the run (`archive/520` decision 2).
4. **The latest press wins.** A second press while one waits replaces it, with its own key. A release doesn't cancel it: the press was the decision, as in Ableton.
5. **Things that drop a waiting press**:
    - `Space`, a stop. With a press waiting, `Space` ends the performance: the press is dropped and the stop on the beat happens as today (`550` § The stop on the beat, kept). Both would land on the same beat anyway.
    - A ruler click, a click in the marker strip, or `Space` starting playback (`begin_running_take`). Something else moved playback.
    - Disarming the lane, or a new project. The run ends, and the waiting press with it.
    - A cursor move, a key length or meter change and the octave buttons don't drop it. Its target was set at the press, where the lane lit the key.
6. **No waiting cue, to begin with.** A press waits less than a beat, and the key already lights the moment it's held (`110` § Rendering, `LiveNotes`), so the player sees the press was taken. `archive/520` § Launch quantize asked for a cue on a pending key. That was written for waits of up to a bar, which this design doesn't have. If the wait feels unresponsive in use, a cue (an outline on the waiting key) is the first thing to try. It would need the waiting target published through `SharedAtomics` (no locks on the render path).
7. **The run records the jump when it lands.** A press is added to the run on the tick it jumps (`PerformanceLane::press` at the landing, not at the key-down), with `T` as its target. Every press then jumps to its key's start, so the take needs no shift: `Run::lead` and `take_run`'s shift by it (`550` decision 5) go. A run whose first press came while running starts on its key, and the render's first stretch starts on the cursor.
8. **A marker recall while running waits the same way** (`600` decision 4 changes): `1`–`9` or a flag click waits for the next beat, then jumps to the marker exactly. One mechanism serves both (§ What changes), so the rules above apply to a recall too: the latest press or recall wins and the same things drop it. A recall still adds nothing to the lane's run, and ends a run it interrupts as today. Stopped, a recall moves the cursor at once, as today.
9. **The metronome needs no change.** Every jump plays a beat start in place of a beat tick, so the click counting from the playback tick fires once on it and stays a beat apart. `550`'s test (`a_phase_jump_keeps_the_click_a_beat_apart`) becomes the same check for a quantized jump.
10. **Before bar 1 goes away.** `550` decision 4 let an early press on the first key run below tick 0. A quantized jump always lands on `T ≥ 0`.

## Tuning

How it feels decides these, so they start as below and are changed by hand after playing:

- **No late grace.** A press just after a beat waits almost a whole beat. If that feels sluggish, a press up to a few milliseconds (or a 32nd) after a beat could count as on that beat, jumping in by that much. That brings back a small offset, the thing this design removes, so it starts at zero.
- **The stop** stays on the next beat (`550` § The stop on the beat).

## Later: a choice of boundary

`archive/520` § Launch quantize had Beat, ½ bar and Bar: Bar keys at Bar launch is classic scene launching. Left out until use asks for it. If it comes, it is saved with the project as the key length is, not an undo step, and needs: where the choice lives (the gutter already holds the key length's `Bar`/`½`/`Beat`, so not beside them in the same words), which grid a bar boundary counts from once a press has moved the bar (decision 2), and the waiting cue (decision 6), since a press could then wait a bar.

## What changes

- **One waiting jump on the lane**: `PerformanceLane::waiting`, an `Option<WaitingJump { target, press }>`, set through `PerformanceLane::jump_now` by `handle_performance_lane_midi_input` and `recall_marker_workflow` while running: the one place that decides, on a beat (`is_on_beat`) jump now and drop a waiting jump, off one wait. Jumping now goes through `jump_and_chase` as before, with no offset.
- **`play_tick`** (`event_handlers/audio_export.rs`, the one tick path, an audio export's too): first thing, before the run's `advance` reads the playback tick, `land_due_jump` (`performance_lane_handler.rs`) lands a waiting jump whose tick is on a beat: `PerformanceLane::land_due_jump` (which for a lane jump adds the press to the run), then `Transport::land_jump` (the playback tick, with no `TransportEvent`) and `reanchor_and_chase` (shared with `jump_and_chase`'s event), synchronously, as a loop wrap re-anchors. Checked while building: through `jump_and_chase` the re-anchor would come from the transport's events after the tick had played, and a second time. `play_tick` returns the tick it played, which the pump's click counts from, so the strong click follows the landed tick.
- **`PerformanceLane`**: `press` loses its offset argument, `Run::lead` and `take_run`'s shift go. `stop_now`/`take_due_stop` keep their on-a-beat test.
- **`phase_offset`** goes. The stop's test only needs "is this tick on a beat": `is_on_beat`, shared with the jumps.
- **Dropping it**: `Space` (stop, `stop_now`) and `begin_running_take` (`cancel_waiting`, which replaced `cancel_stop`) drop a waiting jump. Disarming and a new project drop it in the lane's `clear`, which keeps a waiting stop.
- No change to the metronome, the transport, the clock, persistence, the view or the key length.

## Tests

- A press on a beat jumps at once to `T`. A press off a beat doesn't move playback until the next beat, and that beat's tick plays `T`. The press that starts the transport jumps at once.
- A second press while one waits replaces it. A release doesn't cancel it. `Space`, a ruler click, disarming and a new project each drop it. A cursor move doesn't.
- A marker recall while running waits for the next beat and lands on the marker exactly. A lane press while a recall waits replaces it, and the other way round.
- The run: a waiting press joins the run when it lands, not when it was pressed. A run whose first press came while running renders with its first stretch at 0 and no lead.
- The metronome: one click per beat across a quantized jump, with the jump landing on the tick a click falls on.
- An offline audio export of a run plays the same jumps, through the same `play_tick`.

## Docs in the same change

- `550` is archived (`docs/archive/550-phase-jumps.md`) with a status line saying `610` replaced the phase jump and keeps its stop on the beat. Its row in `AGENTS.md` moves to § Archived, and the references to it in code comments (`performance_lane_handler.rs`, `markers.rs`, `performance_lane.rs`) point here or to the archive.
- `110` (§ MIDI routing's NoteOn), `600` (decision 4's running recall), `archive/520` § Launch quantize (its bullets point here).
- `CHANGELOG.md`'s `[Unreleased]`: a change a user hears. Lane presses and marker recalls while running wait for the next beat and start from the key's (marker's) start.

## Phases

One. The wait, the run's landing and the marker recall go together, since half of it would leave the lane and the markers behaving differently.
