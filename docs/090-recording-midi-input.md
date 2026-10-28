## Recording MIDI input

Every note and wheel move played on the armed track goes into the capture
buffer, whatever the transport is doing; `/` commits from it (`100`, `220`,
`archive/290`). There is no record mode: live recording on `R` was removed, since
capture covers it. Key files: `src/core/midi/input.rs`,
`src/core/midi/input_tick.rs`, `src/core/sequencer/capture.rs`.

### What a take records

- **Note edges and two wheels — pitch bend and the mod wheel (CC1).** `MidiInputForwarder`'s
  callback forwards a message to the sequencer only when `message::is_recorded` says so
  (`parse_note` or `parse_wheel`); everything else on the input — sustain, other CCs,
  aftertouch, program change — is played live (thru and the plugin tap) and never
  recorded. The `NoteLogger` still gets only note edges. The capture buffer behind
  `/` stores a wheel move like a note, at its own tick; the clip thumbnail
  ignores it.
- **Played back like any clip event, and kept honest at every discontinuity** — a wheel
  has no release, so `Track`'s `WheelTracker` chases the value in force on every seek
  and resets on a stop (`050-undo-redo.md` § Invariants). Sustain is deliberately not a
  recorded controller: a sustain stuck on holds every note, so it would need the
  note-release machinery rather than this chase; it can join later as one more `Wheel`.
- **Invisible in the base.** No lane shows controller data and no gesture edits it
  (`280` § Not planned). No lighter hint either (a badge or a strip on the clip): if wheel
  data is ever shown, it is in an automation lane, a feature of its own (decided 2026-10-14); note edits, quantize and transpose leave it where it was recorded
  (`020` § Event Selection, `070`).

### Tick handling

- **Every message carries the `clock_tick` it landed on**, its arrangement
  *position*: `midi_in_tx` carries `RecordedMidi`, `(Vec<u8>, i32)`, and
  `Sequencer::handle_midi_input` feeds it to the capture buffer. A
  seek repositions `clock_tick` (`150-clock-position-sync.md`), which is what
  running capture's window math needs.
- **The tick is sub‑tick corrected at the port**: `MidiInputForwarder`'s
  callback doesn't just read the raw counter (which only steps on the ~1 ms
  `"clock"` firing, so a note between firings would record up to ~2 ticks early). It
  interpolates — `precise_input_tick` / `input_tick_at_arrival` /
  `interpolate_input_tick` in `src/core/midi/input_tick.rs` — using
  `SharedAtomics.clock_tick_instant_nanos` (the `time::monotonic_nanos` the
  counter last advanced, published by `Clock`) and the tempo, mapping the
  `midir` packet timestamp onto that timeline via a self‑calibrating min‑offset
  so a burst processed late still keeps its spacing. The correction is capped
  (`MAX_INPUT_INTERP_NANOS`). The computer keyboard's notes take the same path
  from the frame's stamp (`MidiInputForwarder::tick_at`).
