# Arranger Performance Lane

A reserved row in the arranger, above track 1, that arms the physical MIDI keyboard
for bar-jump triggering instead of normal note capture. Deliberately live-input
only — no recording/capture, no persistence. Key files:
`src/models/performance_lane.rs`, `src/core/sequencer/performance_lane.rs`,
`src/core/event_handlers/performance_lane_handler.rs`,
`src/view/display/rendering/arranger.rs`.

## Scope

- **Live playing only, by design** — a note held on the physical keyboard jumps
  the transport to that note's bar and plays from there; releasing it leaves
  playback running (deliberately — a release doesn't stop the transport, so a
  performer flowing between bars gets no awkward pause; they stop playback
  themselves when done). Nothing about a trigger is recorded, committed, or persisted —
  `ProjectData` (`060-persistence.md`) does not carry any performance-lane
  field. Do not reintroduce a `PerformanceLaneData`/events/muted concept
  without a deliberate scope change; an earlier iteration of this feature had
  one and it was removed on purpose.
- `PerformanceLane` (`models/performance_lane.rs`) tracks only the
  currently-held trigger note (`live_active: Option<u8>`), monophonic — a new
  NoteOn simply supersedes whatever was previously held. `bar_index_from_note`
  maps a MIDI note to a 0-based bar via `config::PERFORMANCE_LANE_BASE_NOTE`
  (currently C3/60); notes below it are ignored.

## Arming / selection

- `SequencerCommand::SelectPerformanceLane` (sent by clicking the reserved row
  — `Display::performance_lane_hit_at`, part of the same `ArrangerLayout`
  geometry `track_idx_at` uses) arms it via `select_performance_lane_workflow`.
  Mutually exclusive with track selection: selecting a track disarms the lane
  and vice versa (`sequencer/state.rs`'s `performance_lane_armed`).
- `performance_lane_armed: Arc<AtomicBool>` (`SharedAtomics`) is read by
  `MidiInputForwarder` on its own thread to suppress **every audible route**
  while armed — both the MIDI-OUT thru-forward and the hosted CLAP instrument
  tap (`instrument_midi_tx`). The physical keyboard drives the transport
  silently; it must not sound a note on a MIDI-OUT track *or* on an instrument
  track. The routing bookkeeping (`route_channel` / `route_held`, which
  track a note held across an arm/disarm) still runs — only the send/push is
  gated.

## MIDI routing

- `EventHandlers::handle_performance_lane_midi_input` is the chokepoint,
  called directly from the sequencer thread's `midi_in_rx` arm
  (`threads/sequencer_pump.rs`) instead of `Sequencer::handle_midi_input_dispatch`, because
  it needs `&mut Transport`, which `Sequencer` doesn't own.
- **NoteOn**: `begin_live_trigger`, then `transport.suspend_loop_wrap()`,
  starts the transport if not already running, and `jump_and_chase(target_tick)`
  to the note's bar. `loop_wrap_suspended` (`Transport`, thread-local, plain
  `bool`) stops the region loop boundary from fighting the lane's hold.
- **NoteOff**: only acts if `end_live_trigger` confirms the released note
  matches the currently-held one (a stale release of an already-superseded
  note is a no-op) — then `resume_loop_wrap()`. Deliberately does **not**
  call `transport.stop()`: the transport keeps playing past the release, so
  the performer stops it themselves (Play/spacebar) when the performance is
  over.

## Rendering

- The row is reserved layout, not a track: `Display::arranger_layout()`
  (`rendering/mod.rs`) is the single source of geometry shared by drawing
  (`draw_performance_lane_row`) and hit-testing, so they can't drift apart.
  Height is `Display::PERFORMANCE_LANE_H`.
- **Intentionally barebones**: the row only shows the selection accent
  (armed vs. not) — no per-trigger visualization, no "Bar N" label, no
  committed-trigger history. The playhead (drawn elsewhere, unconditionally
  whenever `running`) is the only feedback for an active trigger. An earlier
  version rendered a growing span + bar label per trigger; it was removed as
  unnecessary UI complexity once the playhead was made reliably visible (see
  next section) — don't re-add it without a specific reason.

## Waking the UI from a background-thread trigger

- This app is reactive, not continuously repainting: `Display::ui()`
  only runs again when a native window input event arrives or a previous
  frame scheduled `ctx.request_repaint_after(...)`. The self-sustaining
  ~30fps playhead loop (`rendering/mod.rs`) only perpetuates itself while
  `running` is true — normal transport starts (Play button, spacebar) are
  themselves window input events, so they get an initial frame for free.
- A performance-lane trigger is **not** a window event — it's read by
  `midir` on a background thread and never touches the OS event queue for
  the window. Without an explicit nudge, `running` would flip true in the
  background with nothing to notice and repaint.
- Fix: `EventHandlers` holds `repaint_ctx: Arc<OnceLock<egui::Context>>`,
  filled in once from `main.rs`'s `eframe::run_native` closure
  (`cc.egui_ctx.clone()`) after startup — `EventHandlers` is constructed and
  moved into the sequencer thread before that closure runs, hence the
  `OnceLock` rather than a plain field. `handle_performance_lane_midi_input`
  calls `self.request_repaint()` right after `transport.start()` on NoteOn
  (NoteOff no longer stops the transport, so no repaint nudge is needed
  there — the playhead loop is already self-sustaining). This is the general
  pattern for waking the UI from any future background-
  thread-originated state change — prefer it over polling (e.g. a
  `request_repaint_after` gated on some "might get triggered soon" flag),
  which was tried here first and rejected as wasted cycles while idle.
