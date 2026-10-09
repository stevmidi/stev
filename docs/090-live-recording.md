## Live Recording

Live recording spans the sequencer (capture & commit) and the renderer (thumbnail display).
Key files: `src/core/sequencer/live_recording.rs`, `src/core/sequencer/capture.rs`,
`src/core/shared_atomics.rs`, `src/view/display/rendering/arranger.rs`.

### Dual-state design

- **`LiveRecSession`** (`Sequencer.live_rec_session: Option<LiveRecSession>`) is sequencer‑internal.
  It holds the target track and `rec_start_tick` (transport position where recording began).
  The `Sequencer` owns the committable clip during recording.
- **`LiveRecState`** (`SharedAtomics.live_rec_state`) is the cross‑thread atomics struct the
  renderer reads every frame. It exposes `elapsed_start_tick`, `last_note_on`,
  `last_note_velocity`, and `thumbnail_snapshot`.
- These are distinct: the session manages the real clip, the shared state exposes a
  lightweight snapshot for rendering.

### What a take records

- **Note edges and two wheels — pitch bend and the mod wheel (CC1).** `MidiInputForwarder`'s
  callback forwards a message to the sequencer only when `message::is_recorded` says so
  (`parse_note` or `parse_wheel`); everything else on the input — sustain, other CCs,
  aftertouch, program change — is played live (thru and the plugin tap) and never
  recorded. The `NoteLogger` still gets only note edges. Both recorders (`R` and the
  capture buffer behind `/`) store a wheel move like a note, at its own tick; the
  thumbnail ignores it.
- **Played back like any clip event, and kept honest at every discontinuity** — a wheel
  has no release, so `Track`'s `WheelTracker` chases the value in force on every seek
  and resets on a stop (`050-undo-redo.md` § Invariants). Sustain is deliberately not a
  recorded controller: a sustain stuck on holds every note, so it would need the
  note-release machinery rather than this chase; it can join later as one more `Wheel`.
- **Invisible in the base.** No lane shows controller data and no gesture edits it
  (`240` § D); note edits, quantize and transpose leave it where it was recorded
  (`020` § Event Selection, `070`).

### Tick handling

- **Live recording runs on the odometer, not the position clock.** A live‑input message
  carries *two* coordinates (`InputTicks`, `src/core/midi/input.rs`):
  `position` in `clock_tick` space for running capture, and `elapsed` in
  `SharedAtomics.elapsed_ticks` space for live recording.
  `Sequencer::handle_midi_input_dispatch` routes one to each.
  `elapsed_ticks` is credited by the `"clock"` thread on the same firings as `clock_tick`,
  but **only while the transport runs** and **never** repositioned by
  `ClockCommand::AlignToPlayback`. That is the point: a take measures a *duration* by
  subtraction, so a seek must not move its base — before the split, a region edit or an
  arranger↔clip‑view switch mid‑recording moved every note recorded after it. See
  `150-clock-position-sync.md`.
- **The incoming MIDI tick is sub‑tick corrected at the port**: `MidiInputForwarder`'s
  callback doesn't just read the raw counters (which only step on the ~1 ms
  `"clock"` firing, so a note between firings would record up to ~2 ticks early). It
  interpolates — `precise_input_tick` / `input_ticks_at` / `interpolate_input_tick` in
  `src/core/midi/input_tick.rs` — using `SharedAtomics.clock_tick_instant_nanos` (the
  `time::monotonic_nanos` the counters last advanced, published by `Clock`) and the tempo,
  mapping the `midir` packet timestamp onto that timeline via a self‑calibrating min‑offset
  so a burst processed late still keeps its spacing. The correction is capped
  (`MAX_INPUT_INTERP_NANOS`) and the *same* correction is applied to both bases — valid
  because both counters step at the same instant. The channel to the sequencer carries
  `(Vec<u8>, InputTicks)`.
- **Events are stored in clip‑local ticks at capture time**: `add_midi_event_to_live_rec`
  (`live_recording.rs`) subtracts `elapsed_start_tick` (the odometer value at record start) from the incoming
  `elapsed` tick before storing the event in `live_rec_clip`. This eliminates all downstream
  rebasing — `end_live_recording` commits the clip without per‑event tick adjustment, and
  the thumbnail snapshot builder reads local ticks directly from the stored events.
- **`elapsed_start_tick`** lives in `LiveRecState` as `Arc<AtomicI32>` and, as a copy, on
  `LiveRecSession`. Stored once by `start_live_recording` with `Relaxed` ordering. The
  sequencer reads its own `LiveRecSession` copy — in the capture path (to compute local
  ticks) and in `should_end_live_recording` (to measure the take's length) — and the
  renderer reads the atomic (to compute the dynamic recording span). Never
  updated after recording begins.
- **Arming while stopped needs no special case.** The odometer is frozen while the transport
  is stopped, so `start_live_recording_workflow` anchors on `sequencer.elapsed_tick()`
  unconditionally and the take begins counting when play is pressed. (It used to branch on
  `is_running()` and fall back to `playback_tick`, which only lined up because pressing play
  happened to emit a clock align afterwards.)
- **On recording end, the clip's arrangement position is `rec_start_tick`**: the transport
  position where the user pressed REC — an absolute arrangement position independent of
  loop-wrapping. The clip appears where recording started.
- **A completed take is undoable**: `end_live_recording_workflow` records the
  placed clip as a `CommitClipEdit::from_placed_clip` on the undo record — the same edit that
  backs the capture commits — so ⌘/Ctrl+Z lifts the
  clip and ⇧⌘/Ctrl+Z puts it back (`050-undo-redo.md`). The edit's forward result is dropped
  there: the clip is already on the track and its shape already in the UI from
  `RecordingStarted`. Because a take also ends on the stop-family transport commands
  (`PlayFromTick`/`TogglePlayback`/`Stop`) and on `should_end_live_recording` in the tick
  pump, `handle_transport_command` and the workflow both take `undo_record: &mut
  Record<SequencerEdit>` from `sequencer_pump.rs`. A `Canceled` take never touches the record.

### Thumbnail snapshot

- `thumbnail_snapshot: Arc<Mutex<Vec<(i32, i32, f32)>>>` stores `(start_tick, end_tick,
  pitch_frac)` triples in clip‑local coordinates, rebuilt in one pass over the take by
  `live_thumbnail_entries` (`live_recording.rs`) on every note‑on/off. A note‑off closes
  every open onset of its pitch; a held note uses `LiveRecState::HELD_NOTE_SENTINEL`
  (`i32::MAX`) as the end tick and an earlier, retriggered onset of the same pitch ends at
  the current tick; the renderer replaces it with
  `elapsed_tick() - elapsed_start` at draw time so the note visually extends to the current
  playhead position.
- **Renderer uses the odometer for the snapshot span, not `playback_tick`**: the snapshot
  is in clip‑local coordinates anchored at `elapsed_start_tick`, so
  `span = elapsed_tick() - elapsed_start` gives the correct `[0,1]` fractional width.
  `playback_tick` wraps with the transport loop and cannot serve as the growing right edge,
  and `clock_tick` would jump whenever it is realigned.
- **The clip shape's dynamic right edge during recording still uses `playback_tick`**: the
  arranger clip body represents the transport‑domain position, so its visual end tick comes
  from the wrapped transport position — separate from the thumbnail coordinate system.