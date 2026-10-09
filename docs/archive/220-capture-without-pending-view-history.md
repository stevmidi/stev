> **Archived snapshot (2026-09-30).** The full `220-capture-without-pending-view.md` before its history was trimmed. The live doc is `../220-capture-without-pending-view.md`; this copy keeps the phase narratives, bug stories, superseded designs and dated decisions that were removed from it. Not maintained.

# Capture Without the Pending View

**Status (2026-09-26): complete, merged to `main`.** Phases 1–2 (`cabcad8`) and 4–5 (`8cadffe`) are implemented to the spec; phase 3 was dropped. The gap-rule start detection was tried on real takes and dropped: the scored rule stays (see "The gap-rule change, tried and dropped"). The stopped `/` into a lead clip is an undoable insert, and `PendingClip` is deleted. Left open on purpose: adjusting the detected phrase after commit, which the user is evaluating in use before any design doc (see "Known gap: adjusting the detected phrase"), and the separate threads this spec scoped out (the metronome, fractional-bar loops, the Enter-while-playing phase glitch). Read `archive/210-docked-clip-panel.md` first: this brief finishes the job it scoped out ("`PendingClip` keeps its full-window takeover"). `040-phrase-detection.md` holds the detection rules the stopped `/` uses.

## Goal

`PendingClip` is the last modal view in the clip family. Undo is gated off while it's open, it takes over the whole window, it has its own keys, and `/` means two different things depending on the transport. The goal is to keep its valuable part, the phrase **detection** (`region/phrase_tokens.rs`, `region/window.rs`, `region/tempo.rs`), and replace the modal session with ordinary, undoable edits on a committed clip.

## Principles

1. **Everything is an edit.** Every change is a `SequencerEdit`: it lands in the arranger and one ⌘Z takes it back.
2. **Edits never move the transport**, with one exception (below). No edit changes the loop region, the loop flag or the playhead. If an edit changes the music under a running playhead, the current pass plays out and the next loop wrap's seek picks up the change. New transport behaviour during edits is added only when the user asks for it. **The exception, asked for by the user (2026-09-26): while the project has exactly one clip, the loop region is that clip**, so its end can be judged against the wrap before Enter sets the tempo.
3. **While a project has one clip, that clip's length is the tempo reference, so it is never rounded.** The tempo changes only when the user asks for it (Enter). Nothing about this is stored: every rule reads what's there at the moment of the action.
4. **Once there's more than one clip, loops are whole bars** unless the user places an edge exactly. A loop that isn't whole bars drifts against the metronome click (`150`). Adapting the clock to arbitrary loop lengths is out of scope, for a later thread.

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

- **Enter fits the tempo to the clip** while the project has exactly one clip, checked when Enter is pressed. The tempo changes so the clip's length becomes whole bars, with the octave correction the pending confirm used, and the window goes onto a bar line. It is one undoable edit (`FitFirstClipEdit`). With more than one clip, Enter does nothing. Pressing it on a clip already whole bars from a bar line does nothing either.
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
- **The app never zooms or scrolls on the user's behalf.** No re-fit after an edit, no pinning, no paging to follow the clip cursor, and no 8-bar cap on the default framing. The view changes only when the user zooms or scrolls, or opens another clip (framed afresh). A window resize keeps the same span of the clip visible at the new width. Lengthening a clip beyond the frozen framing puts the new end off-screen; the user scrolls to it. What the app should eventually do by itself is decided later, from real use.
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

Settled with the user:

- **Detection stays, the modal goes** (phase 5 deletes `PendingClip`).
- **The stopped commit keeps outside material; the running commit doesn't.** Running passes overlap the same phase, so there's no linear timeline to keep. The kept span is the pending view's 8 bars, stored like any other event with no DTO change.
- **`[`/`]` in both views** with the operands above. The ⌥-modified alternative was not taken.
- **Clip view `[` moves only the start** (2026-09-26); the end changes only deliberately, with `]`.
- **Fitting the tempo is an action (Enter), not a stored state** (2026-09-26). The branch had a stored "provisional tempo" (a pending mark, a shared flag published after every command, resets on load); it goes. Every rule reads the clip count when the action happens.
- **The metronome is out of scope for this branch** (2026-09-26). The branch had silenced it while the tempo was provisional, via a second shared flag in `Metronome`. The user decided the metronome is not to be touched here: if it needs a change, it gets a separate fix. `metronome.rs` goes back to `main`.
- **With more than one clip, clip-view `]` rounds up** (2026-09-26). While there is one clip it is exact.
- **One bar of end headroom** (2026-09-26).
- **Narrowest grid in the piano roll** (2026-09-26); more grid options are future work.
- **Snap-end-to-content is dropped.**
- **Phase 3 (phrase tokens as cursor stops) is dropped** (2026-09-26): the user hasn't missed it. Detection keeps picking the start at commit; correcting it is a mouse or `[`/`]` job.
- **The start detection stays the scored rule** (2026-09-26). The planned switch to F4's gap rule was checked on 15 real takes first and dropped; see "The gap-rule change, tried and dropped".
- **The loop follows a clip's edges only while the project has one clip** (2026-09-26). The user first removed loop-follow entirely, then found the first-clip workflow needs it: judging the end against the wrap before Enter. The narrow rule "one clip ⇒ the loop is that clip" restores it without stored state; with more clips the loop never follows.
- **No header hint and no automatic fit** (2026-09-26): Enter is the one way to fit the tempo. The branch's `TEMPO not set · Enter` chip and the stopped `/` auto-fitting the first clip before adding a second both go. The header shows the BPM as on `main`.
- **The clip view never zooms or scrolls on the user's behalf** (2026-09-26). It frames the clip's whole reach when the clip is opened (window, kept notes, headroom) and holds that framing through edge edits, so only the shading changes while editing. This includes behaviour already on `main`: the clip view's cursor-follow paging, its re-fit on window and clip changes, and its 8-bar default cap (`210` phase 4) all go. The arranger is unchanged. The user decides from use what, if anything, the app should adjust automatically later.
- **Enter works while playing** (2026-09-26). The playhead is not retimed, so the rest of the pass may be out of phase until the next wrap corrects it, as with the pending view's confirm. Any fix for that phase glitch is a separate branch.
- **Edits never move the transport** (2026-09-26), except the one-clip loop above. The user removed the transport moves the branch had added (`WrapPlayhead`, `RetimePlayhead`, the stored auto-loop) and will ask for any transport behaviour during edits explicitly; the one-clip loop is that ask (`LoopOver` came back for it).

## Open questions

None for this spec. All decided with the user on 2026-09-26; see Decisions. The one gap found afterwards is below, and is for a separate design doc.

## Known gap: adjusting the detected phrase (found 2026-09-26)

Found by the user after phase 4, on real takes: **a stopped `/` commits detection's best guess, and a take the detection framed too long or too short can't be corrected the way the pending view allowed.** The pending view kept the take open: the user adjusted the phrase's start and end against the whole capture, then confirmed. Now:

- **A new clip** keeps the capture outside its window (the last `CAPTURE_BUFFER_BARS`), so `[`/`]` and the mouse can still move its edges over it. That covers most of it, but there is no view of the capture as a phrase: no token markers, no one-key "end at the last note".
- **An insert into a lead clip** adds only the detected phrase. The rest of the capture buffer is cleared on commit, so a phrase detected too short can't be lengthened, and a wrong start can only be fixed by editing notes.

The user wants this solved in the clip view rather than by bringing the modal back, **scoped in its own design doc** before any code. Questions for it: whether an insert should keep the capture as material outside the phrase, as a new clip does (where would it live inside an existing clip?); what the clip view shows of the take (token markers, the last-note end); which keys adjust it (the pending view had F1–F4, `+`/`-` and ⇧←/→ between tokens); and how that stays within this spec's principles (everything an edit, nothing stored, the view never moves on its own).

## Changes made to reach this spec (done 2026-09-26)

From the pre-spec checkpoint (`04cabab`):


- **Revert `src/core/metronome.rs` to `main`**, and its wiring: the extra `Metronome::new` argument in `setup.rs`.
- **Remove** `TransportCommand::LoopOver`, `RetimePlayhead` and `WrapPlayhead`, `Transport::loop_over` and `place_playhead`, `Sequencer::relocated_playhead`, `rewrapped_tick`, `PlayheadMove` and their tests.
- **Remove loop-follow** from `clip_resized_workflow`, and **shrink** `EditResult::ClipResized` to `{ clip }`: `from`, `to` and `tempo_change` all go.
- **Remove the stored provisional state entirely:** `Sequencer::first_clip_tempo_pending`, `tempo_provisional`, `share_`/`set_first_clip_tempo_pending`/`publish_tempo_provisional`, `is_tempo_provisional` (replaced by a one-clip check at the call sites), `SharedAtomics::tempo_provisional`, the `Display` field and the header chip, the publish-after-every-command line in `sequencer_handler.rs`, the resets in `project.rs`, `start_provisional_tempo_workflow`, and the flag handling in `FitFirstClipEdit`. `fit_first_clip_tempo_workflow` checks the clip count instead.
- **Drop the stopped `/` auto-fit** (`fit_first_clip_tempo_workflow` from the `Commit` arm) and the header chip (`overlays.rs`, the `Display` field).
- **Replace the automatic clip-view zoom and scroll with a framing frozen at open:** remove `hold_clip_view_on_cursor`, `held_scroll_x`, `RenderState::clip_edit_anchor` and its `EventsUpdated` call, the per-frame zoom re-judge's special case, and, from `main`, the clip view's cursor-follow paging (`sync_clip_scroll`'s follow), its re-fit on clip changes, and the 8-bar home cap (`CLIP_HOME_MAX_BARS`). The home framing becomes the clip's reach at open time, held in the view's state until the user zooms, scrolls or opens another clip. `200` and `210` lose the parts this retires.
- **Delete the `#[cfg(test)]` resize shims** in `sequencer/region/mod.rs` (`resize_selected_clip_region_end_to_tick`, `resize_selected_clip_region_start_to_tick`, `apply_selected_clip_bounds`). On `main` the first two were the mouse drag's production mutators. The branch made them pure calculators (`clip_end_trimmed_to` / `clip_start_trimmed_to`) applied by `ResizeClipEdit`, and kept the old names as test-only methods on `Sequencer` so about 10 `capture.rs` tests (the running-capture "reveal the pre-roll with a left-edge drag" regressions) didn't need rewriting. That's test-only API inside production code. Move those tests onto the real path, recording a `ResizeClipEdit` so they exercise what the app does, undo included, and delete the shims. No behaviour change.
- **Tests:** the behaviour table's rows are pinned (see above). The handler-test harness moved into `event_handlers/test_harness.rs`, shared with the selection tests.
- **Docs:** `010`, `020`, `030` and `050` follow this spec; `200` and `210` mark their replaced parts as superseded.

## Phased plan

All phases are done (below); each left the app shippable with the gate green.

1. **Stopped `/` commits.** *Done.* `Sequencer::build_stopped_capture_clip` and `CommitClipEdit::from_stopped_capture`.
2. **Edge editing, material outside the window, the clip view, fitting the tempo with Enter.** *Done.* `ResizeClipEdit`, `FitFirstClipEdit`, `reach_over`, `SetClipEdgeToCursor`, `FitTempo`, `GridSurface::PianoRoll`.
3. ~~**Phrase tokens as cursor stops.**~~ **Dropped 2026-09-26.** The detection's value is at commit: picking the last phrase's start, which the stopped `/` keeps doing. Stepping between token markers was a navigation aid from the hardware-keypad era; with the mouse, the fine piano-roll grid and `[`/`]`, the start is adjusted directly. The pending view's token navigation, the refine pass (F4) and the markers go with phase 5; the detection core (`phrase_token_starts` and the start snapping it feeds) stays.
4. **Insert without the modal.** *Done (2026-09-26).* A stopped `/` with a lead clip inserts the detected phrase at the clip cursor as an undoable event insert (`InsertCaptureEdit`), which also makes the running insert undoable. Both go through pure builders (`Sequencer::build_running_capture_insert`, `build_stopped_capture_insert`) that return the notes in the clip's event ticks; the edit adds them, clears the buffer on its first `edit()` only, and undo restores the clip's events (`050`). The stopped insert takes exactly the phrase an unedited `InsertIntoClip` session inserted (the window ending at the last `NoteOff`, at most a bar, start snapped to a phrase start, trimmed at the clip's end): checked against that confirm before it was deleted, then pinned (`stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`). Neither changes the event selection or the transport. With its last way in gone, the pending view's entry path went too, so the gate stays free of dead code: `begin_pending_phrase_workflow`, the `InsertIntoClip` target (`PendingPhraseTarget`, `PendingPhraseResult`), the global cursor/region stash (`TransportCommand::SetGlobal*`/`RestoreGlobal*`), `add_clip_unchecked` and `get_two_clips_by_id_mut`. `begin_pending_phrase` became a test-module helper for the `NewClip` oracle tests. The view itself, its confirm/cancel, commands, F-keys and canvas are unreachable and left for phase 5.
5. **Delete `PendingClip`.** *Done (2026-09-26).* `ViewState::PendingClip`, `ClipContext::PendingClip`, `pending_phrase.rs`'s session parts (`build_pending_phrase_clip`, `build_stopped_capture_insert` and their helpers move next to `CommitClipEdit`/`InsertCaptureEdit`), `event_handlers/pending_phrase.rs`, the test-module `begin_pending_phrase`, the `has_pending_phrase()` gates, the pending commands and F-keys, the pending canvas, the `NewClip` confirm (today only an oracle for the equivalence tests, which get pinned values instead). `040` keeps only the detection rules, renamed `040-phrase-detection.md`. As built: the sequencer module became `sequencer/stopped_capture.rs` (the two stopped `/` builders and the detection framing they share); the equivalence tests were pinned to the values the confirm produced, recorded from the tree before deletion (`stopped_commit_window_is_the_retired_confirms`, `stopped_first_clip_is_exact_and_enter_fits_it`), and the confirm-based tempo tests moved onto the stopped `/` + Enter. Also gone as dead once the view was: `ClipCursorMode` (only `Grid` was left) and its atomic; F4's gap-refine pass; `TransportCommand::SetCursor`/`PlayFromCursor`; the pending commands (`ConfirmPendingPhrase`, `NudgePendingCursor*`, `SetPendingRegionEndTo*`, `RefinePendingPhraseTokensByGaps`, `NudgeClipCursorTo{Next,Prev}Event`, `NudgeClipRegionStartToCursor`, `NudgeClipRegionEnd`) and their sequencer/model helpers; `detect_initial_tempo_from_first_clip` (Enter's fit replaced it); the pending theme colours; the clip's own playback atomic in the view; `EventHandlers`' running flag. The per-frame clip-pane snapshots stayed, renamed `clip_frame_*`. `ViewState` was renumbered.

## The gap-rule change, tried and dropped (2026-09-26)

The plan was to replace the scored start detection with F4's gap rule: the start after the latest of the two longest silences in the pending viewport. The user reached for F4 often in the pending era, and its markers looked like a superset of the automatic ones. Before switching, real takes were recorded as fixtures and the rules compared on them.

**Result.** The user recorded 15 typical takes with the stopped `/` and accepted the scored rule's start on 14 of them. The gap rule as written would have started 6 of the 15 elsewhere, 3.6 to 9.6 beats late. Structurally, with only two phrases in view, one of the two longest silences is inside the last phrase, and "the latest" picks it. F4 never hit this because the user chose among its markers by eye. Two variants ("after the last silence at least half the longest", "after the longest silence") did no better than the scored rule either.

**Decision: the scored rule stays** (`detected_phrase_window` → `snap_region_start_to_note_on`). It judges a silence against the take's own median, has an acceptance test, and uses bar position and phrase length as support. F4 was a marker set for a person to choose from, not a start picker. Phase 5 still deletes F4 with the pending view.

**What stays from the attempt:** the takes, as `fixtures/captures/` and `the_detection_picks_every_fixture_start`, which fails if a detection change moves the start of a take the user accepted. The debug-only capture dump and `extract_picked_starts` stay, so new takes can be added (`000` § Build and Test).

## Must not regress

- **The insert into a lead clip** lands the notes the pending insert confirm did (`stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`), and the running insert's window rules are unchanged (`100`).
- **Detection output:** every take in `fixtures/captures/` still starts where the user picked (`the_detection_picks_every_fixture_start`); the committed window, and Enter's fit, keep the values an unedited pending confirm produced (`stopped_commit_window_is_the_retired_confirms`, `stopped_first_clip_is_exact_and_enter_fits_it`).
- **Notes crossing a window edge** play and release correctly (`track.rs` boundary handling; the adjacent-clips case is the one a naive fix misses).
- **A commit never changes the event selection or the loop region.**
- **The running-capture commit** (last pass, pre-roll kept) is unchanged.
- **No edit moves the playhead, and only the one-clip rule moves the loop.**
- **The metronome is unchanged from `main`.**

## Appendix: how we got here (2026-09-25/26)

The design was found by trying it, so the branch went through several rounds that this spec replaces:

- **First-clip tempo.** Fitted once at commit (sloppy) → re-fitted on every `]`, with a playhead "tap" → the tap was dropped for the cursor → per-edit re-fitting was found to be the root of the playback complexity (rescaled ticks, retimes, clock re-alignment) → a stored "provisional tempo" settled by Enter → that stored state needed rules on every path the modal used to forbid → **Enter as a plain action, decided from the clip count**, which is what the pending view got right without storing anything.
- **Playback during edits.** A restart on every edit → a relocate-with-chase (jumped the music, jittered) → retime/wrap (worked, but was special-case machinery in the transport handler) → **edits never move the transport**.
- **`[` in the clip view.** Kept the length and slid the end → **moves only the start**.
- **Other discoveries along the way:** the piano roll's grid stopped at 16ths, too coarse for placing an end by ear; a set end needed headroom; the view re-fitted and paged on every edit (first pinned, then the user chose no automatic zoom or scroll at all); the cursor didn't move with a tempo rescale.
- **Lesson:** agree the behaviour table first, and pin the playing column with playback tests before listening.
