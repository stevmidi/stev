## Recording MIDI input

Every note and wheel move played on the armed track goes into the capture
buffer, whatever the transport is doing; `/` commits from it (`100`, `220`,
`archive/290`). There is no record mode: live recording on `R` was removed, since
capture covers it. Key files: `src/core/midi/input.rs`,
`src/core/midi/input_tick.rs`, `src/core/sequencer/capture/mod.rs`.

### What a take records

- **Note edges and two wheels — pitch bend and the mod wheel (CC1).** A
  `LiveInputRouter` forwards a message to the sequencer only when
  `message::is_recorded` says so: a note edge or a wheel move (`parse_note`,
  `parse_wheel`), from a port or the computer keyboard alike. Everything else on the
  input — sustain, a hardware controller's other CCs, aftertouch, program change — is
  played live (thru and the plugin tap) and never recorded. The `NoteLogger` still gets
  only note edges. The capture buffer behind `/` stores a controller move like a note, at
  its own tick; the clip thumbnail ignores it. One type names what is recorded besides
  notes: `Controller` (`core/midi/message.rs`), pitch bend, a CC by number or a plugin
  parameter by ID; only the two wheels have a rest (`Controller::rest`).
- **A knob's parameter, turned by hand** (`400-parameter-knobs.md` decision 10). A turn is
  not MIDI, so it doesn't come through the router: `Display` sends `HoldKnob` at the
  turn's start, `SetKnobValue` per move and `ReleaseKnob` at its end, whether the turn is
  a drag on Stev's knob or the control in the plugin's editor (`knob_panel.rs`,
  `knob_params.rs`), each stamped with the `clock_tick` it happened at (`InputClock`,
  `core/midi/input_tick.rs`: a plugin's editor stamps a move as it reports it, and the
  frame handles it later, so the stamp can lie before the clock's last step).
  `Sequencer::hold_knob` puts the value the knob stood at in the buffer ahead of the
  turn, `turn_knob` each move, as a `ParamMove` event (`models/event.rs`). Only a held
  knob's moves are recorded: a move with no turn around it is a preset the plugin loaded.
  The buffer is the selected track's, so another track's turn stays out, and a turn of a
  parameter no knob holds never reaches the sequencer. A commit drops any move no knob of
  the selected track holds any more (`Sequencer::keep_knob_moves`). A turn can reach the
  buffer behind a note played since, so a late one slots in by its tick (`add_late_event`).
- **Played back like any clip event, and kept honest at every discontinuity** — a
  controller has no release, so `Track`'s `ControllerTracker` chases the value in force on
  every seek, and a stop resets the wheels; a knob's parameter has no rest and stays where
  it was (`050-undo-redo.md` § Invariants). Sustain is deliberately not a recorded
  controller, and a clip's sustain moves (imported) are never chased: a sustain stuck on
  holds every note, so it would need the note-release machinery rather than this chase.
- **Shown in the piano roll's controller lanes** (`380-wheel-lane.md`, `archive/390-knob-panel.md`):
  the lane under the notes switches to pitch bend, the mod wheel or a knob's parameter and draws
  the lead clip's moves as a line (move to move as a DAW draws it, a held value and its jump stepped). The one edit there clears
  a span (a wheel back to rest, a parameter held at the value before it), or with `⌘A` the whole
  controller (`Clip::clear_controller_span`); nothing draws values (`280` § Not planned). No lighter hint
  on the clip (a badge or a strip, decided 2026-10-14) and nothing in the arranger
  thumbnails; note edits, quantize and transpose leave it where it was recorded
  (`020` § Event Selection, `070`).

- **Starting the transport empties the buffer, except what is held into the take**: the
  notes still down and each wheel when off its rest land on the start tick, the notes
  spread as they were pressed (`100` § Notes held into a take). A knob has no rest to be
  off, so where it stands is never carried in.
- **A take into a clip replaces that clip's controller data over the span played**
  (`370-wheel-capture.md`, `Clip::land_take`): its notes are added, but for each controller
  the take moved — a wheel or a knob's parameter — the clip's own moves of it go from the
  take's first move to the end of the take's window, the take's come in, and the old
  curve's value at the window end is written back (for a parameter, only where the clip
  had one). A parameter the clip has no earlier data for gets the value the knob turned
  from at the window's start, as its stand-in for a rest (`Clip::hold_start_values`,
  `400` decision 10). A
  controller the take didn't move is left alone; a wheel off its rest where the take's
  window starts counts as a move there (`Clip::carry_wheels_onto`), for new clips too.
- **Ride a wheel or turn a knob alone over a playing loop and `/` keeps it in the lead
  clip**: running, a take of controller moves with no notes lands in the lead clip by the
  same rule, framed by the loop pass holding its last move. It needs a lead clip (a clip of automation alone
  isn't a thing Stev makes) and the transport running: stopped, or without a lead clip, a
  take still needs notes to commit (`100` § Invariants).

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
