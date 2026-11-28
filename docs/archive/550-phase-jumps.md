# Phase Jumps on the Performance Lane

**Archived 2026-11-28: the phase jump was replaced by `610-launch-quantize.md` (a press while running waits for the next beat and lands on its key's start), because the same key started early or late by the press's timing. § The stop on the beat is still as built (`110` § MIDI routing). Status: drafted, built and agreed 2026-11-21 after hand-testing; the § Tuning values stay open to change in use. No issue: it was built the day it was designed (`080` § Design Docs and Issues).** A press on the armed lane still jumps at once, but it lands at the target plus the press's offset from the nearest beat, so the beat never breaks. The metronome stays steady, and a capture's bar lines stay on the grid. This replaces today's jump to the key's start, which isn't kept as an option. Read `110` (the lane) and `520` (the run and its render) first. This also replaces `520` § Launch quantize's "Not now" bullet.

## Background

Today a press jumps playback to its key's start the instant it arrives (`handle_performance_lane_midi_input` → `jump_and_chase(key_tick)`), and a press is never exactly on a beat. Press 0.3 of a beat late into bar 5 and playback is now 0.3 beats behind the grid it was on: the beat restarts where the press fell. That is `520` § Launch quantize's "What Off implies", and it has two effects found in use (2026-11-21):

- **The metronome is useless while performing.** It counts from the playback tick (`Metronome`'s module docs, `sequencer_pump.rs`), so every press restarts the click from wherever the press fell. There is nothing steady to play against, which is what the click is for.
- **A capture's downbeats don't line up** (`520` § Launch quantize). That was accepted as the groove; it is the same drift.

Three ways out were weighed (2026-11-21):

1. **A free-running click while armed**: the metronome counts its own ticks and ignores jumps. Steady, but it then disagrees with the music after every off-beat press, and the capture still drifts. It fixes the click, not the cause.
2. **Launch quantize** (`520` § Launch quantize, still deferred): a press waits for the next boundary. Gridded, but a beat is the finest step that keeps the beat (targets are beat starts, so a 16th boundary shifts the beat by a 16th), and waiting up to a beat loses the playing feel of stutter jumps.
3. **Phase jumps** (this design): no waiting and no drift. The press cuts in at once, at the phase it came on.

Today's jump isn't kept beside it as an option (decided 2026-11-21). Without a steady reference it doesn't work as something to play, and an option would bring a setting, a gutter label and a saved field for a behaviour nobody would use.

## What a phase jump is

A press comes at playback tick `p`. Its key's target is `T` (`key_tick`, a beat start in every meter). With `b` the meter's beat (`Meter::beat_ticks`), the press's **offset** is its distance from the nearest beat:

```
offset = p - round_to_nearest_beat(p)      // in -b/2 .. b/2
jump to T + offset
```

- **A late press** (offset > 0) skips the first `offset` ticks of the target: press 0.3 beats after beat 3 and playback lands at bar 5 + 0.3 beats.
- **An early press** (offset < 0) plays the last `-offset` ticks before the target, and the target's start lands exactly on the beat the press anticipated. Pressing just ahead of the beat to be in time is the natural way to play, and this makes it land.
- **Either way the beat keeps its place in time**: `T` sounds at a beat of the old grid, so the pulse is unbroken. Only the bar can move: a bar key pressed on beat 3 starts bar 5 there, so the next downbeat comes two beats later than the old bar's would have. That is what was played, and the music's bar line moves with it.

It is how the lane plays, not a correction: what is heard is the jump, made at the press, and the capture records exactly that (`520` § Principle). Nothing is snapped afterwards.

**A marker recall jumps the same way** (`600-markers.md` decision 4): `1`–`9` while running lands at the marker plus the press's offset (`recall_marker_workflow`, `event_handlers/markers.rs`, the same `phase_offset` and `Transport::jump_and_chase`), so a marker sounds on a beat of the grid playback was on, and the sustain lift, the clock alignment and the capture follow from the same path. It is no lane press: it adds nothing to the lane's run, and a run it interrupts ends as a ruler click ends it.

## Decisions

1. **Every press while running is a phase jump.** There is no setting, no gutter label and nothing saved; the lane's only saved field stays the key length.
2. **Nearest beat of the project meter.** The offset is measured to the nearest counted beat (a quarter in x/4, an eighth in x/8), not to the key length. This keeps the pulse, which is what the click counts, and it lets a bar key start a bar on the beat it was pressed on. Measuring within the key length (beat-jump/legato style, the bar kept too) is a different instrument: a bar key pressed on beat 3 would enter bar 5 on beat 3, and no bar's start would be heard unless pressed on a downbeat. Not this design. The window is ±½ beat to start with (§ Tuning).
3. **The press that starts the transport jumps to `T` exactly.** Stopped, there is no grid to keep. It is also what starts the run (`520` decision 2), so such a run's first stretch starts on its key.
4. **Before bar 1.** An early press on a key at tick 0 (C3) would jump below 0. To start with, playback runs there: it plays nothing until 0, and bar 1 then lands on the beat. A note below 0 is already safe in playback (the nudge is unclamped by decision, checked 2026-10-28). The playhead is off the left edge for less than half a beat. If anything draws or chases oddly there, clamp to 0 for that press (the pulse moves by the offset, once) (§ Tuning).
5. **The render keeps the grid** (`520` decision 1). The run stores each press's jump as it was, `T + offset` (`PerformanceLane::press` takes the tick it jumped to, unchanged), so `take_run`'s stretches are what was heard. One thing is new: a run whose **first** press came while running (after a `/`, without a stop) starts off the grid, so the render is placed **so that the first press's key `T` lands on the cursor**:
    - First offset late (`d > 0`): a silence of `d` before the first stretch. Every stretch moves `d` later and the length grows by `d`, so its content sits on the grid it was heard on.
    - First offset early (`d < 0`): the first stretch's `-d` lead-in before `T` is dropped (it would fall before the cursor). Every stretch moves `-d` earlier and the length shrinks by `-d`.
    - A run started by a press while stopped: `d = 0`, unchanged.

    The run keeps its first press's offset (`Run::lead`, set by the press that leaves the run's list non-empty, so a press replacing the first on the same tick replaces it too), and `take_run` shifts the stretches by it, so the shift is in one place. With the cursor on a beat, the capture's downbeats then line up with the arrangement's, which `520` § Launch quantize accepted losing.
6. **The metronome needs no change.** Playback's beat phase is continuous across a jump, so the click counting from the playback tick stays steady. A late jump lands past the beat that already clicked, and the next click is one beat after it. An early jump lands just before `T`, which clicks on time. The metronome already re-arms on a discontinuity (`last_beat`), so a jump that lands exactly on the beat a click just fired on must click once, not twice; worth a test. The strong click follows the music's bar (§ What a phase jump is).
7. **Launch quantize later composes.** Quantize makes every jump land on a boundary, so its offset is 0 and the phase rule is moot there. When it is built it adds a choice of when a press lands (Now, Beat, ½ bar, Bar), with Now being this design. Leave that to its own design.

## The stop on the beat

Added 2026-11-21 after the first hand test. A performance usually ends with `Space`, then the cursor is moved to where the take goes and `/` inserts it. `Space` stopped where it was pressed, so the run ended off the beat and the inserted clips ended mid-beat. With phase jumps, playback's beat grid is the take's grid, so a stop on a beat gives a take of whole beats.

- **`Space` during a performance, off a beat, waits for the next beat** and stops there: the lane armed and a run going (a press since playback started, `520` decision 2). With the lane selected and no key pressed, playback is just the arrangement, and `Space` stops at once (found in review 2026-11-21) (`PerformanceLane::stop_now`; `play_tick` stops the transport once playback's next tick is a beat, `take_due_stop`). Up to a beat more plays, and it is in the take, since it was heard. On a beat it stops at once.
- **A second `Space` while the stop waits stops at once**, the way out.
- **A press while it waits cancels it**: playing on means the performance isn't over. Starting playback again (`Space`, a ruler click: `begin_running_take`) drops one left over.
- **Disarming while it waits doesn't cancel it**: the stop was asked for, and it still lands on the beat.
- Unarmed, `Space` stops where it is pressed, as before. Nothing shows that a stop is waiting: it lasts under a beat. If it ever feels unresponsive, a waiting cue on the play state is the first thing to try.

## Tuning

How it feels decides these, so they start as below and are changed by hand after playing (2026-11-21: "a lot of feel goes into this"). Each is one constant or one branch:

- **The window**: ±½ beat. A late press past half a beat counts as early for the next beat, so a lead-in plays. If that feels wrong, a window favouring late (say −¼…+¾) is a one-line change.
- **Before bar 1**: playback below 0 (decision 4), or the clamp.
- **The stop**: to the next beat (§ The stop on the beat). Nearest beat (cutting back what played past it) or the next bar are the alternatives.

## What changes

- `handle_performance_lane_midi_input`: the jump target is `T` when the press starts the transport, otherwise `T + phase_offset(meter, p)`, with `p` the playback tick at the press (`sequencer.playback_tick()`: between two ticks of the pump it holds the tick about to play, the one the run's `expected_tick` holds). `press` and `jump_and_chase` both get that tick. The pure helper `phase_offset` sits beside `key_tick` in `models/performance_lane.rs` and holds the arithmetic and its tests.
- `PerformanceLane::press` takes the offset beside the jumped-to tick; `Run::lead` holds the first press's, and `take_run` shifts by it (decision 5).

As built, the pump reads the click's position before the advance, so a press between ticks sees the tick about to play: it measures that tick's offset and the next tick plays the jumped-to position. A press exactly on a beat jumps to the key's start, which clicks; one a tick after it jumps a tick in, which doesn't. No beat clicks twice or is skipped (`Metronome`'s `a_phase_jump_keeps_the_click_a_beat_apart`).
- No change to the metronome, the transport, the clock, `jump_and_chase`, persistence or the view.

## Tests

- `phase_offset`: on the beat → `T`; late by `d < b/2` → `T + d`; early by `d < b/2` → `T - d`; exactly half a beat counts as early (`snap_to_grid`'s tie); x/8 measures to the eighth; `T = 0` early → below 0 (or clamped, as decision 4 settles).
- A press while running jumps to `T + offset`; the press that starts the transport jumps to `T`.
- The run keeps the phase jump, and `advance` doesn't end the run after it (`expected_tick` is the jumped-to tick).
- The render: a run started by a press while stopped renders as today; a run whose first press came while running, late or early, is shifted so `T` is at the cursor, with the length adjusted; later presses' content stays on the grid it was heard on.
- The metronome: a phase jump keeps one click per beat across it (late, early, on the beat), using the existing `play` helper in `metronome.rs`'s tests with a jump in the tick sequence.

## Docs in the same change

`110` (§ MIDI routing's NoteOn), `520` (§ Launch quantize: the "Not now" bullet points here, and "What Off implies" becomes history: Off now means a phase jump), the `550` row in `AGENTS.md`, `CHANGELOG.md`'s `[Unreleased]` (a change a user hears: lane jumps keep the beat).

## Phases

One: the jump and its render together (without decision 5 the render of a run started while running would be off the grid).
