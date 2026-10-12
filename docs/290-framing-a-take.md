# Framing a Take

**Status: design, agreed 2026-10-12. Phase 1 (the window band) built 2026-10-12, as-built in `020` § Clip Edge Drag-Resize and `030` § Timeline; phase 2 not started.** Closes the known gap in `220-capture-without-pending-view.md`: a stopped `/` commits detection's best guess, and a take inserted into an existing clip can't be re-framed at all. Two phases, each its own branch: a **window band** in the clip view for every clip (phase 1), then a **take session** that frames a take before it is inserted (phase 2). Read `220` and `040-phrase-detection.md` first.

## The problem

- **A new clip** (the stopped `/` with no lead clip) keeps the last `CAPTURE_BUFFER_BARS` of the take as material outside its window, so its edges can be moved over the rest of the take with `[`/`]`. That works, but in the clip view only from the keyboard: the mouse edge drag lives in the arranger.
- **An insert** (the stopped `/` with a lead clip, from either view) adds only the detected phrase (`detected_insert_window`: at most one bar, ending at the last note-off) and clears the capture buffer. A phrase detected too short can't be lengthened, and a wrong start can only be fixed by editing notes.

What the user knows when it goes wrong: inserting into a clip is deliberate. The wanted notes are among the last played, detection usually lands on them, and when it doesn't the user knows exactly where the phrase should start and end, and judges it **by ear**, in the clip, against what is already there.

## Principles

`220`'s principles hold, with one deliberate exception:

1. **Everything is an edit.** The band's moves are `ResizeClipEdit`s; a confirmed take is one `InsertCaptureEdit`.
2. **Edits never move the transport.** Neither the band nor the take session touches the loop region, the loop flag or the playhead, except `220`'s one-clip loop rule, which the band inherits through `ResizeClipEdit`.
3. **The clip view never zooms or scrolls on its own.** The take opens framed like any clip (its whole reach, frozen), and the band moves only shading and handles.
4. **The exception: the take session is a stored mode.** `220` retired the modal `PendingClip`; this brings back a much smaller one, scoped to inserts, because an insert lands inside music that is already there and needs a confirm. It has no keys of its own (`[`, `]`, `←`/`→`, Enter and Esc keep their meaning, see § Keys), it is the ordinary clip view, and it is short-lived: it has one rule for everything else (§ Session rules).

## Phase 1: the window band

The clip view gets a band in its timeline strip spanning the clip's window, with a handle at each edge. **Every clip has it**: nothing records that a clip was just captured, and none is needed. A fresh capture benefits because it carries the rest of the take as material outside its window.

- **Look.** Modelled on the arranger's region band (`030` § Timeline, `draw_region`): `REGION_BAND_H` tall, translucent, a 1px line at each edge, flush down to the strip-bottom groove, in the clip's colour rather than `theme::region()` so it doesn't read as the loop. The arranger's region band stays arranger-only. The cursor's play triangle draws over it. Edges go through `region_bound_x` like every other time-anchored x. The pointer turns into the horizontal-resize cursor over a handle.
- **Grab.** A press within a few px of an edge, in the strip, grabs it; elsewhere the strip keeps what it does today.
- **Snap.** The piano roll's visible grid (`GridSurface::PianoRoll`), the same rule as the arranger's edge drag (snapped to the view grid). As built, `]` itself became exact with any number of clips (2026-10-12), so the end handle is simply `]` at the snapped tick.
- **What it does.** The start handle is clip-view `[` (moves only the start; the end stays, the clip keeps its place in the arrangement). The end handle is `]` at the snapped tick. As built, the drag only previews the window and the release commits one `ResizeClipEdit` (Esc cancels): an edit per move showed the clip changing under the drag (`020` § Clip Edge Drag-Resize). While the project has one clip the loop follows, through the same `loop_sole_clip_workflow` call the other edge edits make.
- **Reach.** A handle can't leave the clip's reach (`Clip::reach`, which starts on the bar line at or before the window start and the first note — `220` § Reach); the start can't pass the end and vice versa (the minimum window as for `[`/`]`).

Pinned by: band-drag tests next to the `resize_clip` ones (start drag = `[`, end drag snapped to the grid, one undo step per drag, loop follows with one clip and not with two) and pure geometry tests for the handle hit-test and band span.

## Phase 2: the take session

### Flow

1. The user plays, with a lead clip selected (cursor on a clip in the arranger, or the clip view on it), and presses the stopped `/`.
2. **The take opens as its own clip in the clip view**: the clip the arranger's stopped `/` would have made (`build_detected_phrase_clip`: the detected window, the rest of the take as material outside it), except that the window ends at the **last note-off**, with no two-beat tail (§ The window). If the clip view was hidden or the arranger had the keyboard, the panel shows and the clip view takes the keyboard, as Shift+Tab does.
3. The user frames the notes with `[`/`]`, `←`/`→` and the band (§ Keys).
4. **While framing, the target clip already holds the framed notes at its clip cursor**, so playing the loop around the insert point hears the take in place (§ Hearing it in place).
5. **Enter** inserts the framed notes at the clip cursor: one `InsertCaptureEdit`. **Esc** drops them and leaves the clip as it was. Either way the clip view goes back to the target clip, and the keyboard goes back to the pane that had it.

The running `/` is unchanged: it inserts the last pass at once (`100`).

### Two clips, two cursors

The take is a fresh `Clip` with its own region and its own cursor, off the arrangement. The **framing cursor** is the take's; the **insert point** is the target clip's own cursor, which nothing moves during the session, so it needs no storing. The two never share a screen, so the take's time and the clip's time are never overlaid out of phase.

The session is `Option<TakeFraming>` on the `Sequencer`, holding the take, the target's track index and clip id, and the target's events from before the session (for Esc and for the undo state). **While it is open, `Sequencer::selected_clip()` / `selected_clip_mut()` return the take**, so every clip-view command and the clip-view snapshot act on it without per-command changes; the edges are moved on the take directly (not as `ResizeClipEdit`s, it isn't in the arrangement).

### The window

**The take window selects whole notes by where they start**: a note is in the take when its note-on is inside the window, and it comes along whole.

- **It runs from the first note-on to the last note-off of the notes it selects.** No tail: the take is a set of notes, not a stretch of time, and a tail only eats into the room before the clip's end.
- **The first selected note-on lands on the clip cursor**, wherever in a silence `[` sits.
- **A note that starts inside and ends past `]` is kept whole**, cut only at the target clip's region end (closed there, as today).
- **A note that starts before `[` and sounds into the window is left out**: it belongs to the previous phrase, and the cropped fragment today's insert keeps would sound like a glitch.
- **Wheel moves** (pitch bend, mod wheel, `090`) between the first selected note-on and the window end come along, offset with the notes.
- **The initial window** is detection's: the start `snap_region_start_to_note_on` picks (the same start the new clip gets), the end at the take's last note-off. The window can't be shorter than one note.

This replaces `detected_insert_window` for the stopped insert. Pressing `/` then Enter straight away inserts the whole last phrase, where today it inserts at most its last bar: a deliberate change to `stopped_insert_lands_the_detected_phrase_at_the_clip_cursor` (§ Must not regress).

### Keys

Inside the session the keys pick **notes**, not times, so the edges follow from the notes picked:

- **`←`/`→` step the cursor to the previous/next note start** in the take. Starts within a 32nd (120 ticks) of each other are one stop, so a chord is one press; the stop is at the earliest of them. Shift doesn't change this. The cursor stays within the take's notes.
- **`[` = start with this note**: the window starts at the stop under the cursor.
- **`]` = end with this note**: the window takes every note starting up to and including the stop under the cursor, and ends at the last of their note-offs.
- **The band** (phase 1) snaps to the same stops: the start handle to note starts, the end handle to "after note N", drawn at that window's end. Keyboard and mouse give the same window for the same notes.
- **Enter** confirms, **Esc** cancels. `[`/`]` accept any modifier but ⌘/Ctrl, as everywhere.

Outside the session nothing changes: a new clip from the arranger's stopped `/` is shaped on the grid, because there the edges are a loop length the tempo gets fitted to.

### Hearing it in place

While the session is open the target clip holds the framed notes at the insert point, so the ordinary transport plays them: there is no playback path for an off-arrangement clip. Each window change swaps them through `edit_clip_events` (the path note edits use, so nothing hangs under the playhead), and, as for any edit, the current pass plays out and the next loop wrap picks the change up. The user loops the section around the insert point (Space and the loop region work as usual) and frames while it plays.

These temporary notes are never undo steps of their own. **Enter** restores the target's pre-session events and records the `InsertCaptureEdit` normally, so its undo state is the clip as it was before the session and one ⌘Z removes the take. **Esc** restores the pre-session events. The inserted notes are not selected, and the target's event selection is untouched (a commit never changes the selection, `220`).

### Session rules

- **Esc**, **⌘Z** and **⇧⌘Z** cancel. The capture buffer is left as it was, so `/` reopens the take (with anything played since added to it). The edge moves inside a session are not undoable one by one.
- **Leaving the clip view** cancels: Shift+Tab, a click in the arranger, selecting another clip or track.
- **Enter** consumes the buffer, as the insert does today (`InsertCaptureEdit`'s first `edit()`).
- **Everything else that edits is blocked** while the session is open: note edits, quantize, `M`, `⌥=`/`⌥-`, another `/`, save, load, new project, the browser's open/import. The take only chooses a window; it isn't edited.
- **Transport, zoom and scroll work** (Space, `⌥Space`, the loop toggle, `+`/`-`, `Z`/`X`, the wheel).
- **Quit or window close** cancels the session first, so the unsaved-changes check (`060`) compares the project without the temporary notes.
- **Nothing to frame:** no notes in the buffer, or no room at the clip cursor, opens no session (as the insert does nothing today).

The footer names the session (e.g. "Framing take: Enter inserts, Esc cancels") while it is open; it is the one place the mode is visible beyond the take itself.

Pinned by: the window rule (`[` in a silence still lands the first note on the cursor; a note across `]` kept whole; a note across `[` left out; chord onsets one stop), Enter = one undo step whose undo restores the pre-session clip, Esc and ⌘Z restore it and keep the buffer, the selection unchanged, blocked commands are no-ops, and a run across a loop wrap that hears a changed window from the next pass (driving `Sequencer::tick`, like `a_start_moved_while_playing_is_heard_at_once`).

## Decisions

Settled with the user (2026-10-12):

- **Inserts are framed before they land, in a session**, not re-framed after (rejected: a re-frame of the newest undo step, which needed new keys and a hidden "last edit was an insert" condition) and not by keeping the rest of the take as muted notes in the clip (rejected: an 8-bar take would scatter muted notes through existing material, indistinguishable from deliberately muted ones).
- **The take is the same clip the arranger's stopped `/` makes**, shown in the clip view, so both capture paths show the take the same way with the same keys.
- **No tail on a take**: first note-on to last note-off.
- **The window selects whole notes by their start.**
- **Note-start cursor stops and "with this note" `[`/`]`**, inside the session only.
- **By ear, in place**: the target clip holds the framed notes while framing.
- **The band is for every clip**, grid-snapped, built first; the take session reuses it with note snapping.

## Changes elsewhere when built

- `220` § Known gap and `040` § Phrase end (the insert window) point here, then take the as-built rules.
- `010-keybindings.md` and the help overlay's `HELP` table: the band (phase 1), the session's keys (phase 2).
- `020` (the session as a sequencer-side mode: what `selected_clip()` returns, what is blocked), `030` (the band), `050` (the session's edits aren't undo steps; Enter's `InsertCaptureEdit`).
- `CHANGELOG.md` under `[Unreleased]`, per phase.
- `280-roadmap.md`: the candidate is removed when phase 2 lands.

## Must not regress

- **Detection:** `the_detection_picks_every_fixture_start`, and the new clip's window and Enter's fit (`stopped_commit_window_is_the_retired_confirms`, `stopped_first_clip_is_exact_and_enter_fits_it`). The take's initial start is the new clip's start.
- **The running insert** and its window rules (`100`), and `insert_running_capture_is_undoable`.
- **The stopped insert changes on purpose**: `stopped_insert_lands_the_detected_phrase_at_the_clip_cursor` is rewritten for the session's Enter (whole phrase, notes kept whole) in phase 2; `stopped_insert_is_undoable_and_keeps_the_selection` keeps its meaning.
- **Edits never move the transport**; only the one-clip rule moves the loop (`the_only_clip_keeps_the_loop_on_it_and_never_moves_the_playhead`, `with_two_clips_an_edge_edit_sends_nothing_to_the_transport`).
- **Notes crossing a window edge** play and release correctly (`track.rs`), including while the session swaps the temporary notes under a running playhead.
- **A commit never changes the event selection or the loop region.**
