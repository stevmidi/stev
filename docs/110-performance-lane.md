# Arranger Performance Lane

A reserved row in the arranger, above track 1, that arms the physical MIDI keyboard
for jump triggering (a bar, half a bar or a beat a key) instead of normal note
capture. Played live; `/` renders what was played into ordinary clips at the
arranger cursor (`520-performance-capture.md`). Saved: the key length, nothing
else. Key files:
`src/models/performance_lane.rs`, `src/core/sequencer/performance_lane.rs`,
`src/core/event_handlers/performance_lane_handler.rs`,
`src/view/display/rendering/arranger.rs`.

## Scope

- **Played live, kept as material** — a note pressed on the physical keyboard
  jumps the transport to that note's key and plays from there; releasing it
  leaves playback running (deliberately — a release doesn't stop the
  transport, so a performer flowing between bars gets no awkward pause; they
  stop playback themselves when done). **The scope changed deliberately on
  2026-11-18, revised 2026-11-19** (`520-performance-capture.md` § A
  deliberate change): an earlier iteration recorded its triggers as lane data
  shown on the timeline, and that was removed on purpose; what came back is
  different, a capture that leaves ordinary clips on the tracks and nothing on
  the lane. No trigger is drawn on the lane or saved: the lane keeps its
  **run** (the presses since playback started, in memory), and `/` renders it
  from the arrangement and inserts the result at the arranger cursor, one
  undoable edit. `ProjectData` (`060`) carries one performance-lane field,
  the key length (`performance_lane_key_length`). Don't add trigger data,
  stored performances or per-trigger drawing to the lane (a first design kept
  performances on eight pads, with a piano roll and a replay; it was removed
  because a stored path drifts with the arrangement, `520` § Why the pads
  went).
- `PerformanceLane` (`models/performance_lane.rs`) holds the **run**
  (`520` decision 2): the presses since the one that started the transport
  (or since the last `/`), each stamped with the run's own clock, the ticks
  playback has advanced since its first press (`PerformanceLane::advance`,
  called from `play_tick` before the transport moves). Monophonic, the latest
  press wins; two on one tick keep the last. Playback anywhere but where the
  presses left it (a ruler click, `Space` from the cursor) ends the run, as
  does disarming (`set_performance_lane_armed(false)`) or a new project. `/`
  takes it (`take_run` → a `Take`: a stretch per press from where it jumped,
  lasting until the next press, the last until the capture), the stretches
  of the arrangement it played (`520` decision 1). Each press keeps its
  jump target, so a key length or meter changed in the middle of a run
  renders what was heard. `key_index_from_note`
  maps a MIDI note to a 0-based key via `config::PERFORMANCE_LANE_BASE_NOTE`
  (currently C3/60); notes below it are ignored. `key_tick(meter, length,
  key)` is where a key jumps, one **key length** (`KeyLength`) per key: a
  bar, half a bar or a beat (`KeyLength::ticks`: `bar_ticks`, half of it,
  `beat_ticks`, the counted beat, a quarter in x/4, an eighth in x/8), so
  every jump lands on a beat in every meter. Half a bar is offered only where
  the meter's numerator is even (the arranger grid's half-bar rule);
  `KeyLength::in_meter` plays a stored half bar as a beat in an odd meter, and
  the stored choice is kept, so 4/4 → 3/4 → 4/4 is at a half bar again. A beat
  a key in 4/4: C3 = bar 1, C♯3 = bar 1 beat 2, E3 = bar 2, C4 = bar 4; a bar
  a key: C♯3 = bar 2, C4 = bar 13. 68 keys (to note 127) reach 68 bars, 34 or
  68 beats. The length is picked from the gutter (§ Rendering), owned by the
  sequencer (`Sequencer::set_performance_lane_key_length`, from
  `SequencerCommand::SetPerformanceLaneKeyLength`), mirrored to the view
  through `SharedAtomics::performance_lane_key_length`, saved with the project
  (a project saved before it opens at a beat, as do new projects and
  templates), counted as unsaved by the fingerprint and **not an undo step**.
  A change applies from the next key pressed: a held key keeps its jump.
  One key per bar until 2026-11-18, then briefly half a bar for every meter,
  then a beat (`510` § Changes while building), then the choice (`510`
  § Step 1b).

## Arming / selection

- `SequencerCommand::SelectPerformanceLane` (sent by clicking the reserved row
  — `Display::performance_lane_hit_at`, part of the same `ArrangerLayout`
  geometry `track_idx_at` uses) arms it via `select_performance_lane_workflow`.
  The arranger has **one selected row**, a track or the lane
  (`450-performance-lane-selection.md`): arming drops the lead clip, the
  track selection and the marquee (`clear_clip_selection_workflow`,
  `Sequencer::select_track(None)`, `clear_marquee_workflow`), and selecting a
  track disarms the lane (`switch_track_workflow`). Armed implies no selected
  track, in the sequencer and in the view (`Display::selected_track_idx` is
  an `Option`, `None` from `UiEvent::PerformanceLaneSelected`, and the view
  derives "the lane is selected" from it). In the sequencer the reverse
  doesn't hold (a load has no selected track for a moment), so its flag is
  stored, not derived.
- **While the lane is selected, a command aimed at the selected track does
  nothing**: no lead clip (the clip view shows "No clip at the cursor"), no
  clip capture (`/` renders the lane's run instead, § MIDI routing), no knob, no `v`, no rename, no plugin from the browser with Enter;
  no marquee, so `⇧`-arrows, a `⇧`-click and `⌘A` start none, and `⌥←`/`⌥→`
  have no track's clip edges to jump to; a marquee-scoped `⌘V` pastes nothing
  (`ClipClipboard::target_track`), while an all-track one pastes as ever.
  `↓` selects the first track and `↑` the last
  (`cycle_selected_track_workflow`); the lane itself is reached only by a
  click. The cursor still moves with `←`/`→`, and its accent sits on the lane
  row (`draw_selected_track_cursor`). A press on a track row selects that
  track first, and so leaves the lane.
- **An undo or redo that puts a track back leaves the lane**: the restore
  selects the step's track the ordinary way, which disarms. Arming is never
  restored: a step recorded while armed has no track in its snapshot and
  leaves the selection as it is (`restore_view_workflow`,
  `050-undo-redo.md` § Invariants).
- **No loop wrap while the lane is selected** (`520` decision 5): `Transport`
  holds the same `performance_lane_armed` atomic and `Transport::tick` never
  wraps at the loop end while it is set, so `Space`, a ruler click and every
  press play straight past it, and a run's render agrees with what was
  heard. Select a track and it wraps again at the next
  loop end, if playback is inside the region. The loop itself (`loop_enabled`,
  the region) is untouched, nothing is saved or undoable, and the arranger
  draws the region dimmed as with looping off (`Display::is_loop_enabled`
  reads both). Until 2026-11-18 a press suspended the wrap and its release
  resumed it, so a key let go before the loop end let playback wrap.
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
  (`threads/sequencer_pump.rs`) instead of `Sequencer::handle_midi_input`, because
  it needs `&mut Transport`, which `Sequencer` doesn't own.
- **NoteOn**: joins the run (`PerformanceLane::press`, a new run when it
  starts the transport), starts the transport if not already running, and
  `jump_and_chase(target_tick)` to the key (`key_tick`, at the length the
  sequencer holds). **A phase jump** (`550-phase-jumps.md`): while running, the
  target is the key's start plus `phase_offset`, the distance of playback's
  next tick from the nearest beat of the meter (−½…+½ beat, half counting as
  early, `snap_to_grid`), so the key's start sounds on a beat of the grid playback was on: a
  late press skips the start of its key, an early one plays the end of what
  comes before it (below 0 for an early press onto bar 1), and the beat never
  breaks, so the metronome, counting from playback, stays steady through
  presses. The press that starts the transport jumps to the key's start
  exactly. The run keeps its first press's offset, and `take_run` shifts the
  take by it so the first key's start lands at 0, the cursor: a capture's bar
  lines stay on the grid (`550` decision 5).
- **`Space` while running stops on the next beat** (`550` § The stop on the
  beat): off a beat `PerformanceLane::stop_now` leaves the stop waiting, and
  `play_tick` stops the transport once playback's next tick is a beat
  (`take_due_stop`), so a run stopped with `Space` is whole beats. A second
  `Space` stops at once; a press cancels the wait, and a start
  (`begin_running_take`) drops one left over.
- **NoteOff**: does nothing. Deliberately not `transport.stop()`: the
  transport keeps playing past the release, so the performer stops it
  themselves (Play/spacebar) when the performance is over; and no loop wrap
  waits on it (§ Arming).
- **`/`** (`SequencerCommand::Commit`, while armed): `capture_performance_workflow`
  instead of a clip capture: the run rendered (`Sequencer::render_performance`)
  and inserted at the arranger cursor (`DuplicateTimeEdit::capturing`, Paste
  Time's edit), one undo step, nothing selected, the clipboard untouched; the
  footer names it (`Captured 6 bars at bar 13`) or says there was nothing to
  keep (`520` decision 1).

## Rendering

- The row is reserved layout, not a track: `Display::arranger_layout()`
  (`rendering/mod.rs`) is the single source of geometry shared by drawing
  (`rendering/performance_lane.rs`) and hit-testing, so they can't drift apart.
  Height is `Display::PERFORMANCE_LANE_H`.
- **The row draws the keyboard that drives it**
  (`510-performance-lane-keyboard.md`). The whole row is drawn with the
  arranger's backgrounds (`draw_performance_lane_row`,
  `rendering/performance_lane.rs`): the gutter (the accent tint and 4px marker
  while armed, and the key-length labels), the lane fill and the keys.
- **The gutter's key-length labels**: `Bar`, `½` and `Beat` side by side from
  the track names' inset (`TRACK_HEADER_INNER_PAD_X`), clear of the marker, in
  `FONT_SIZE_TL`, coloured as the piano roll's `VEL`/`PB`/`MW` switch
  (`toggle_icon_color`): the length that plays in the accent, one under the
  pointer `fg`, the rest `fg_dim`, with the pointing hand on a label that
  would switch (`HoveredControl::LaneSwitch`, no footer hint). In an odd meter
  `½` isn't drawn (the other two close up) and `Beat` is lit when `½` is the
  stored choice. One pure layout (`key_length_labels`, each cell its label's
  width and the row's height) serves the drawing and the hit test
  (`Display::key_length_label_at`). A click on a label picks that length and
  does nothing else: it doesn't arm or disarm the lane, select anything or move
  the focus (checked first in `handle_mouse_click`'s header-column branch); a
  click anywhere else on the row arms as before. No key and no keymap action. `draw_timeline` starts the
  arranger's in-lane grid lines under the row (`ArrangerLayout::lanes_top`), so
  no bar or beat line crosses the lane; the key edges, every beat, are the lines
  there. Each key (`KEY_COUNT`, 68 from C3; `note_for_key`, the
  inverse of `key_index_from_note`) spans its key length, from `key_tick` to the next,
  in a thin strip along the lane's bottom edge (`KEY_STRIP_H`, 8pt), in the
  piano roll's `piano_key_white`/`piano_key_black`, drawn as a keyboard: the
  white runs unbroken along the strip, each black key is a block hanging
  `BLACK_KEY_DEPTH` (0.6) of the way down from its top, and a 1px groove under
  it, at its middle, is where the white keys either side meet
  (`black_key_parts`). Drawn flat, white and black alternating at 5pt, the
  black keys vanished into the dark lane and the strip read as a dashed line
  (changed 2026-11-20). From `tick_to_screen_x` in
  the project meter, so it follows zoom, scroll, the meter and the key length
  (redrawn at the new width as soon as it changes); time past the 68th key has
  none. Filling the whole row with the keys was tried first and read
  as heavy slabs, whatever the tones: the piano roll's colours work in a narrow
  band, as in its key column. Seams at E/F (1px) and B/C (2px) in the strip, as
  on the piano roll (`row_seam_w`). The plain lane above the strip carries the
  names in `fg_dim`: the C's always (Cubase-style, C3 over bar 1), every key
  once a key fits the widest sharp's name and its padding, but only at a beat a
  key: at a bar or half a bar the C's alone mark the octaves (`labels_every_key`),
  each clipped to its key. Only the visible keys are drawn (`visible_keys`). The
  strip is drawn armed or not, unarmed blended opaquely
  `UNARMED_KEY_STRENGTH` of the way from the lane fill; the lane fill
  is the usual one (the accent tint while armed). While armed every key the live
  keyboard holds (`LiveNotes`, which wakes the UI on each edge) lights its piece
  of the strip (a black key's hanging block only) in `theme::accent()`, with a wash (`HELD_KEY_WASH`) over its span
  above: what the hands hold, not just the key the transport jumped to. Unarmed,
  held keys light nothing in the lane.
- **Otherwise barebones**: no per-trigger visualization, no "Bar N" label, no
  committed-trigger history. The playhead (drawn elsewhere, unconditionally
  whenever `running`, over the keys) is the only feedback for an active
  trigger. An earlier version rendered a growing span + bar label per
  trigger; it was removed as unnecessary UI complexity once the playhead was
  made reliably visible (see next section) — don't re-add it without a
  specific reason. The keys are such a reason: they show the mapping, which
  nothing else shows, and they are static (zoom, scroll, the meter and the key
  length change them, playing never does), where the removed visuals repeated what the
  playhead already says.

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
