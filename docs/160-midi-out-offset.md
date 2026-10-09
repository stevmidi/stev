# MIDI-output offset and the delay queue

Read alongside the `"midiout"` and `"audio-engine"` rows in
`000-architecture.md`, the sample-accurate scheduling section of
`130-plugin-host.md`, and `archive/140-device-frame-clock.md` (a *different* timing
concern — see *Relationship to 140/150* below).

**Status:** implemented 2026-09-03. Build gate green; the maintainer tests audio
behaviour manually — see the checklist at the end.

---

## The problem

A clip note bound for an external synth used to leave the app the instant the
sequencer thread processed its tick. A clip note bound for a hosted CLAP
instrument, or a metronome click, does not: both are placed at a target output
frame `AudioClock::frame_for(at) + SCHEDULE_DELAY_FRAMES` — one whole buffer
(`DESIRED_BUFFER_FRAMES = 256`, ≈5.3 ms at 48 kHz) into the future, and then sit
behind the audio device's own output latency on top of that.

So MIDI OUT ran **early** against everything the app plays itself: at least the
one-buffer schedule delay, realistically 10–20 ms once the device's output
latency counts. On a track playing hardware and a track playing a plugin, the
hardware was ahead.

This is a **constant offset**, not jitter — which is why the fix is a
calibration setting rather than a clock change. The sequencer thread's own
scheduling jitter (a few hundred µs) is a rounding error beside it.

## Why it cannot be auto-derived

Two of the three terms are unknown to the app:

1. `SCHEDULE_DELAY_FRAMES` — known exactly.
2. The audio device's output latency — **not** known. `Mixer::render`
   (`src/core/audio/engine.rs`) takes `Instant::now()` at callback entry and
   ignores cpal's `OutputCallbackInfo::timestamp().playback`. Reading that would
   improve the default; it is not read today.
3. Whatever the external synth adds between receiving a byte and making a
   sound — unknowable in principle.

Hence a user-facing value in milliseconds, defaulted to the one term we do know
and dialled in by ear against the click.

## Design

**Scheduling, not frame placement.** MIDI bytes go to a port; the only lever is
*when `send()` is called*. So the equivalent of the CLAP path's frame offset is
to hold the message and write it later.

### The message

`src/core/midi/out_queue.rs` defines what travels on `midi_out_tx`:

```rust
pub(crate) struct MidiOutMessage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) at: Option<Instant>,   // None = send on arrival
}
```

Only **clip playback** carries an instant — `Sequencer::tick` already receives
the tick's intended `Instant` for the CLAP path and now passes the same value to
MIDI out (`MidiOutMessage::at`). Everything else is `MidiOutMessage::now`:

| Sender | Why immediate |
|---|---|
| `MidiInputForwarder` live thru (`midi/input.rs`) | Playing a key must not feel laggy — same rationale as the CLAP live-note path bypassing `SCHEDULE_DELAY_FRAMES` |
| `Sequencer::chase_notes` | A seek has no tick instant; the instrument path uses `EventTime::Immediate` here and MIDI out matches it |
| `NoteLogger::release_notes` | The stuck-note safety net; delaying a release is never right |
| `Sequencer::reset_wheels` / `silence_leaving_track` | A stop or a track leaving: the wheel resets go with the note releases. A seek's wheel *chase* is ordinary clip playback (`Track::tick` drains it) and carries the next tick's instant |
| `Sequencer::preview_note` / `preview_notes` (`MidiOut` tracks) | UI-driven auditioning, not clip playback |

### The queue

`MidiOutQueue` — a `VecDeque<(Instant, Vec<u8>)>` kept in deadline order, with
**stable insertion**: a message inserts *after* everything already queued at the
same deadline. That matters because a clip that repeats a pitch back to back
puts the note-off and the next note-on on the same tick; reordering them hangs
the note.

`deadline_for(at, offset_ms, now)` returns `None` — meaning "send now" — for a
message with no instant, and for one whose deadline has already passed. So the
queue only ever holds messages with a real wait ahead of them, and **an offset
of 0 reproduces the old undelayed behaviour exactly** (the tick instant is
always slightly in the past by the time the output thread sees it).

### The thread

`start_midi_output_thread` (`src/core/threads/midi_output.rs`) now runs:

1. flush everything due (`pop_due`), writing each through `send_midi_out`, which
   also records note on/off with the `NoteLogger`;
2. compute the wait — the next deadline, or `MIDI_OUT_IDLE_TIMEOUT` (1 s) when
   the queue is empty;
3. `select!` over the three existing channels plus a `default(timeout)` arm.

**`ReleaseNotes` clears the queue before releasing.** This is the one place the
design can bite: a scheduled note-on belongs to a playback that has just ended,
and letting one out *after* the release pass would hang a note the logger no
longer knows about. Dropping the whole queue is safe precisely because
`NoteLogger` only ever logs what actually reached the port — anything still
sounding is in the log and gets its note-off; anything dropped never sounded.
A port reconnect clears the queue for the same reason (the messages were
addressed to the old port).

### The setting

`SettingsData.midi_out_offset_ms`, persisted in `settings.json`, mirrored live
into `SharedAtomics.midi_out_offset_ms` and read by the `"midiout"` thread per
message — so a change applies to the next note with no restart.

- Default `config::MIDI_OUT_OFFSET_DEFAULT_MS = 5` (nearest whole ms to one
  256-frame buffer at 48 kHz). `SettingsData::Default` is **hand-written**, not
  derived, so a fresh install lands on 5 rather than `i32::default()`; the
  `#[serde(default = "…")]` does the same for an upgrading user's existing file.
- Range `0..=MIDI_OUT_OFFSET_MAX_MS` (200), clamped in
  `settings::clamp_midi_out_offset_ms`.
- No negative values. "Hardware should be *earlier* than the plugins" is what
  `0` already means — nothing can be sent before now.

### UI

The settings modal's MIDI tab (`⌘/Ctrl+,`) has a
third focus row, `MidiSettingsFocus::OutOffset`. Tab / ←/→ cycle
IN PORT → OUT PORT → OUT OFFSET (a click on the row focuses it too); on the
offset row **↑/↓ change the value** (it is a number, not a list) and each nudge
applies and saves at once (`Display::nudge_midi_out_offset` sends
`InputEvent::SetMidiOutOffset`, which publishes the atomic and saves the
offset alone). Enter does nothing
there — unlike a port, a value change touches no hardware. No drag.

---

## Tests

`src/core/midi/out_queue.rs` (pure, unit-tested):
deadline ordering regardless of push order; **stable order at equal deadlines**
(the note-off/note-on regression guard); nothing pops early; `clear` empties;
and the four `deadline_for` cases — no instant, elapsed deadline, pending
deadline, zero/negative offset.

`src/core/sequencer/playback.rs`: a `MidiOut` track's clip event carries the tick
`Instant`; a chased note carries `None`. The harness split
(`sequencer_with_both_outputs`) keeps the MIDI-out receiver alive so the message
can be inspected.

## Manual test checklist

1. Hardware synth and a CLAP instrument on the same beat — they land together at
   the default 5 ms, and audibly separate as the offset is dialled to 0 or 200.
2. Stop the transport mid-phrase on a MIDI-OUT track: no hanging note. This is
   the queue-clear path.
3. Change the loop region / seek while playing a MIDI-OUT track — chased notes
   still sound immediately, no lag on the seek.
4. Live keyboard thru to an external synth stays snappy at a 200 ms offset —
   proof that live thru bypasses the queue.
5. Change the OUT PORT while playing: no stuck note from the old port.
6. Set the offset, quit, relaunch — value persisted and still applied.
7. First run with no `settings.json` (or a hand-corrupted one) shows 5 ms, not 0.

---

## Relationship to 140/150

Independent of both, and the only one of the three with an audible symptom today.

`140` (device-frame crediting) is about *what unit credits musical time* and is
deferred until an external timeline exists. `150` phase 2 is about *who owns
musical position* and concerns recording correctness. Neither affects whether
hardware and plugins land on the same beat — this file is that concern, and the
skew it removes was a fixed latency mismatch, not a clock problem.
