# Capture Without the Pending View

**Status: complete (merged 2026-09-26).** The modal `PendingClip` view is gone: the stopped `/` commits a clip or inserts into the lead clip as ordinary undoable edits. `040-phrase-detection.md` holds the detection rules the stopped `/` uses. Left open on purpose: adjusting the detected phrase after commit (see "Known gap"), and the threads this spec scoped out (the metronome, fractional-bar loops, the Enter-while-playing phase glitch). The build history — the changes made to reach this spec, the phased plan, the gap-rule experiment and how the design was found — is in `archive/220-capture-without-pending-view-history.md`.

## Goal

Keep the valuable part of the retired modal `PendingClip` view, the phrase **detection** (`region/phrase_tokens.rs`, `region/window.rs`, `region/tempo.rs`), and replace the modal session with ordinary, undoable edits on a committed clip.

## Principles

1. **Everything is an edit.** Every change is a `SequencerEdit`: it lands in the arranger and one ⌘Z takes it back.
2. **Edits never move the transport**, with one exception (below). No edit changes the loop region, the loop flag or the playhead. If an edit changes the music under a running playhead, the current pass plays out and the next loop wrap's seek picks up the change. New transport behaviour during edits is added only when the user asks for it. **The exception, asked for by the user (2026-09-26): while the project has exactly one clip, the loop region is that clip**, so its end can be judged against the wrap before Enter sets the tempo.
3. **While a project has one clip, that clip's length is the tempo reference, so it is never rounded.** The tempo changes only when the user asks for it (Enter). Nothing about this is stored: every rule reads what's there at the moment of the action.
4. **Once there's more than one clip, loops are whole bars** (of the project's meter, `archive/270`) unless the user places an edge exactly. A loop that isn't whole bars drifts against the metronome click (`150`). Adapting the clock to arbitrary loop lengths is out of scope, for a later thread.

## The design

### `/` commits

| Transport | Lead clip under cursor | Result |
|---|---|---|
| Playing | none | Commit the last pass, cropped (unchanged, `100`). |
| Playing | yes | Insert the last pass into that clip (`InsertCaptureEdit::from_running_capture`; the window rules of `100` unchanged, undoable since phase 4). |
| Stopped | none | **Commit a new clip framed by phrase detection** (`CommitClipEdit::from_stopped_capture`). |
| Stopped | yes | **Insert the detected phrase at the clip cursor** (`InsertCaptureEdit::from_stopped_capture`, phase 4). |

The stopped commit places the clip at the arranger cursor on the selected track. It keeps the detected window: detected start, and tail at last NoteOff + 2 beats. **The project's first clip keeps that window exactly** and leaves the tempo alone (see "Fitting the tempo"). **Every later clip is rounded to the nearest whole bars**, floored to the room before the next clip. The clip is **not cropped**: the last `CAPTURE_BUFFER_BARS` (8) of the capture stay in it as material outside its window. The event space is rebased so the window starts on a bar line (`Clip::align_window_start_to_bar`), which gives quantize, swing and the clip view the phrase's grid. A commit never changes the event selection and never touches the loop region.

### Fitting the tempo is an action, not a state

The pending view kept the tempo fixed until confirm without storing anything: it *was* the state, and being modal it forbade everything else meanwhile. Without the modal, a stored "tempo not set yet" flag would need a rule for every path that can touch the clip (split, paste, undo, a second capture, load). So there is no such flag. Instead:

- **Enter fits the tempo to the clip** while the project has exactly one clip, checked when Enter is pressed. The tempo changes so the clip's length becomes whole bars, with the octave correction the pending confirm used, and the window goes onto a bar line. It is one undoable edit (`RetimeClipEdit`, which `⌥=`/`⌥-` share). With more than one clip, Enter does nothing. Pressing it on a clip already whole bars from a bar line does nothing either.
- **While the project has one clip, `[`/`]` on it are exact**, same check. You shape the tempo reference freely and press Enter to fit, or reshape and press it again.
- **If Enter is never pressed**, the tempo stays what it was, as in any DAW where the tempo is what you set it to.
- **The metronome** clicks at the current tempo like anywhere else (out of scope here; see Decisions).

### While the project has one clip, the loop is that clip

The pending view looped the phrase while it was open, which is how its end was judged by ear. The same, scoped by the rule Enter already uses (exactly one clip), with nothing stored (`loop_sole_clip_workflow`, `Sequencer::sole_clip_span`):

- **The stopped `/` that makes the project's first clip** loops over it and turns looping on (`TransportCommand::LoopOver`).
- **Every edge edit on it, and Enter's tempo fit,** move the loop to its new span (`SetRegion`), leaving the loop on/off setting alone: a loop the user switched off stays off.
- **With a second clip the rule stops applying.** The loop stays where it was, and from then on edits never touch the transport. Unlike the pending view, nothing is restored after Enter: the loop stays on the clip.
- **The playhead is never moved.** An end pulled in behind a running playhead leaves it past the new loop end, where the transport doesn't wrap, so playback runs on. The user considers that the correct behaviour.

### Edge edits `[` / `]`

Every edge change is an undoable `ResizeClipEdit`: keyboard and mouse alike. A mouse drag's per-move edits merge into one undo step by drag id, the `050` pattern for continuous drags. `[`/`]` accept any modifier but ⌘/Ctrl, so Nordic layouts that type brackets with ⌥ reach them.

| | Arranger (arranger cursor) | Clip view (clip cursor) |
|---|---|---|
| `[` | Trim the start to the cursor; the content stays put in time. | Move only the start to the cursor. The end stays, the clip keeps its place in the arrangement, and the length follows. |
| `]` | Trim the end to the cursor. | **One clip in the project:** end exactly at the cursor. **More than one:** end on the bar line at or after the cursor (rounded *up*), floored to the room before the next clip. |
| Which clip | The one under the cursor, otherwise the nearest on the side the edge faces, so the keys can grow a clip as well as shrink it. | The lead clip. |

The mouse edge drag is the arranger trim, snapped to the view grid.

### Material outside the window

- **Reach.** A clip's *reach* is its window plus **one bar of headroom** after the end, widened to any event further out (`reach_over`, the single rule). The clip cursor is clamped to the reach (`Clip::reach`), and the clip view scrolls over it. The headroom means a set end is never a wall: `→` into it and `]` lengthens the clip.
- **Shading.** Outside the window, the note grid is shaded and notes are dimmed like muted ones. Those notes never play.
- **`⌥Space`** from a cursor outside the window plays from the clip's start.

### The clip view

- **Grid:** Ableton's "Adaptive: Narrowest" density. A fitted 2-bar clip snaps to 64ths, and zooming in goes down to 256ths (`GridSurface::PianoRoll`). The arranger's grid is unchanged.
- **A stable view while editing edges.** When a clip is opened, the view frames the clip's whole reach at that moment (the window, any kept notes outside it, and the headroom bar) across the width, and that framing is then frozen. Edge edits (`[`, `]`, drags, and their undo) never change it: the only thing that changes on screen is the shading and which notes are dimmed.
- **The app never zooms or scrolls on the user's behalf.** No re-fit after an edit, no pinning, no paging to follow the clip cursor, and no 8-bar cap on the default framing. The view changes only when the user zooms or scrolls, or opens another clip (framed afresh). A window resize keeps the same span of the clip visible at the new width. Enter's tempo fit (and its undo/redo, and `⌥=`/`⌥-`) rescales the clip's event ticks, so the view is mapped through the same retime (`UiEvent::ClipRetimed`, `follow_clip_retime`): every note stays on its pixel and only the grid moves under it. Without it the view kept the old ticks and a long capture's window landed far off screen. Lengthening a clip beyond the frozen framing puts the new end off-screen; the user scrolls to it. The vertical axis follows the same rule: the note range is framed when the clip opens and held through edits; only the user's scroll and octave-legend zoom drag move it — plus a capture insert (or its redo) whose take would land out of view, which re-frames as on opening (decided from what's on screen, not from whether the user scrolled; `030` § Piano Roll). What the app should eventually do by itself is decided later, from real use.
- **This applies to the clip view only;** the arranger's zoom and follow are unchanged.

### Playback during edits

Nothing is done to the transport (principle 2). Because a clip's events don't change when its edges move, the current pass plays out as it was and the next loop wrap re-seeks onto the new window. Specifically:

- **Moving the start in the clip view** switches the music at the next wrap. That's the seamless option: switching immediately jumps the music mid-bar or puts the loop out of phase.
- **Pulling the end in behind the playhead** leaves the playhead past the new end, where the transport doesn't wrap. Playback runs on linearly until the user restarts, which the user considers correct.
- **Enter while playing** works. The fit rescales the playing clip's ticks while the playhead keeps its tick, so the rest of that pass may play out of phase; the next loop wrap's seek corrects it. The pending view's confirm behaved the same way. Accepted (see Decisions).

### Behaviour table

| Edit | Clips in project | Stopped | Playing |
|---|---|---|---|
| stopped `/` | none yet | first clip, exact detected window; tempo untouched; loop over it, looping on | n/a |
| stopped `/` | one or more | nearest whole bars, floored to the room (auto-fit dropped; see Decisions) | n/a |
| `[` clip view | any | start to the clip cursor; end stays | pass plays out; new start from the next wrap |
| `]` clip view | one | exact at the clip cursor; the loop follows | same; the loop follows, the playhead isn't moved |
| `]` clip view | more than one | up to the bar line at or after the cursor | same; nothing moved (a playhead left past the end runs on) |
| `[`/`]` arranger | any | trim at the arranger cursor; with one clip the loop follows | same; the playhead isn't moved |
| edge drag | any | trim on the grid; one undo step per drag | same |
| Enter | one | fit the tempo to the clip, once; the loop follows | same; the rest of the pass may be out of phase, corrected at the next wrap |
| Enter | more than one | nothing | nothing |
| ⌘Z on Enter | one | old length, tempo and note timing back | same; corrected at the next wrap |

The stopped `/` into a lead clip: the detected phrase at the clip cursor, one undoable `InsertCaptureEdit`, stopped or playing alike; nothing sent to the transport.

Pinned by: `stopped_first_clip_is_exact_and_enter_fits_it`, `stopped_commit_window_is_the_retired_confirms`, `stopped_commit_floors_to_the_bars_before_the_next_clip`, `stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`, `stopped_insert_is_undoable_and_keeps_the_selection`, `insert_running_capture_is_undoable`, `stopped_commit_into_the_lead_clip_is_one_undoable_insert` (`/`); `start_marker_moves_only_the_start`, `end_marker_rounds_up_to_whole_bars_and_stops_at_the_next_clip`, `the_end_is_exact_with_one_clip_and_rounds_up_with_more`, the `resize_clip` tests (edges); `enter_fits_the_tempo_to_the_only_clip_and_undoes_with_it`, `nothing_to_fit_on_a_whole_bar_clip_or_with_a_second_clip`, `enter_does_nothing_with_more_than_one_clip` (Enter). The playing column and the loop: `the_only_clip_keeps_the_loop_on_it_and_never_moves_the_playhead` and `with_two_clips_an_edge_edit_sends_nothing_to_the_transport` (handler-level) and `a_start_moved_while_playing_is_heard_from_the_next_wrap` (drives `Sequencer::tick` across a wrap and checks the notes emitted).

## Decisions

Settled with the user (2026-09-26):

- **Detection stays, the modal is gone.** Correcting a detected start is a mouse or `[`/`]` job; phrase tokens as cursor stops were dropped (the user didn't miss them).
- **The stopped commit keeps outside material; the running commit doesn't.** Running passes overlap the same phase, so there's no linear timeline to keep. The kept span is 8 bars, stored like any other event with no DTO change.
- **`[`/`]` in both views** with the operands above. Clip view `[` moves only the start; the end changes only deliberately, with `]`. With more than one clip, clip-view `]` rounds up; with one clip it is exact.
- **Fitting the tempo is an action (Enter), not a stored state.** Every rule reads the clip count when the action happens. No header hint and no automatic fit: Enter is the one way to fit the tempo.
- **The metronome is out of scope.** If it needs a change, it gets a separate fix.
- **One bar of end headroom.** **Narrowest grid in the piano roll**; more grid options are future work. Snap-end-to-content is dropped.
- **The start detection stays the scored rule.** F4's gap rule was checked on 15 real takes and did worse (6 of 15 started 3.6–9.6 beats late); the takes are pinned as `fixtures/captures/` (`the_detection_picks_every_fixture_start`). Details in the archive.
- **The loop follows a clip's edges only while the project has one clip**, so the first clip's end can be judged against the wrap before Enter. With more clips the loop never follows.
- **The clip view never zooms or scrolls on the user's behalf.** It frames the clip's whole reach when the clip is opened and holds that framing through edge edits; a tempo fit maps it onto the retimed notes, so they stay where they were on screen. The arranger is unchanged. What, if anything, the app should adjust automatically is decided later from use.
- **Enter works while playing.** The playhead is not retimed, so the rest of the pass may be out of phase until the next wrap corrects it. Any fix for that is a separate branch.
- **Edits never move the transport**, except the one-clip loop. Any transport behaviour during edits is added only when the user asks for it.

## Known gap: adjusting the detected phrase (found 2026-09-26)

Found by the user after phase 4, on real takes: **a stopped `/` commits detection's best guess, and a take the detection framed too long or too short can't be corrected the way the pending view allowed.** The pending view kept the take open: the user adjusted the phrase's start and end against the whole capture, then confirmed. Now:

- **A new clip** keeps the capture outside its window (the last `CAPTURE_BUFFER_BARS`), so `[`/`]` and the mouse can still move its edges over it. That covers most of it, but there is no view of the capture as a phrase: no token markers, no one-key "end at the last note".
- **An insert into a lead clip** adds only the detected phrase. The rest of the capture buffer is cleared on commit, so a phrase detected too short can't be lengthened, and a wrong start can only be fixed by editing notes.

The user wants this solved in the clip view rather than by bringing the modal back, **scoped in its own design doc** before any code. Questions for it: whether an insert should keep the capture as material outside the phrase, as a new clip does (where would it live inside an existing clip?); what the clip view shows of the take (token markers, the last-note end); which keys adjust it (the pending view had F1–F4, `+`/`-` and ⇧←/→ between tokens); and how that stays within this spec's principles (everything an edit, nothing stored, the view never moves on its own).

## Must not regress

- **The insert into a lead clip** lands the notes the pending insert confirm did (`stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`), and the running insert's window rules are unchanged (`100`).
- **Detection output:** every take in `fixtures/captures/` still starts where the user picked (`the_detection_picks_every_fixture_start`); the committed window, and Enter's fit, keep the values an unedited pending confirm produced (`stopped_commit_window_is_the_retired_confirms`, `stopped_first_clip_is_exact_and_enter_fits_it`).
- **Notes crossing a window edge** play and release correctly (`track.rs` boundary handling; the adjacent-clips case is the one a naive fix misses).
- **A commit never changes the event selection or the loop region.**
- **The running-capture commit** (last pass, pre-roll kept) is unchanged.
- **No edit moves the playhead, and only the one-clip rule moves the loop.**
- **The metronome is unchanged from `main`.**
