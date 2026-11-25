# Capturing a Performance-Lane Performance

**Archived 2026-11-25. Status: revised 2026-11-19, the revision agreed and built the same day (§ Build plan). Issue: #39 (closed with the first design; the revision's commits say `Part of #39`, § Open questions 1).** The first design (agreed 2026-11-18, all four phases built 2026-11-18/19) kept a performance as the path of jumps on one of eight pads, edited in the piano roll, replayed with `⌥Space` and rendered to clips on `⌘C`. Hand-testing showed the stored path drifts with the arrangement (§ Why the pads went), and the first design's pads, piano roll, replay and pad clipboard are gone from the code. The revision: **`/` on the armed lane renders what you just heard into ordinary clips and inserts them at the arranger cursor**, one undo step. Nothing is kept on the lane. The pads, the performance piano roll, the replay and the pad clipboard go; the run, the render rule, the no-wrap rule and Paste Time stay. Launch quantize is still deferred to a design of its own (§ Launch quantize).

## Background

The marquee transpose (`500`), the ruler click that plays without moving the cursor (`020` § Timeline click) and the lane drawn as a keyboard with a choice of key length (`510`) together opened a workflow: put ideas side by side, shift them by ear until they work together, then play freely on the lane trying combinations. Sooner or later a pattern comes out that would work as a transition or a section of its own. Without capture it is gone once played: the lane is live-only by design (`110` § Scope).

What's wanted is to keep such a pattern as material: a new set of clips that can go anywhere on the timeline.

### A deliberate change of `110`'s scope

`110` says not to bring back a `PerformanceLaneData`/events concept "without a deliberate scope change": an earlier lane recorded its triggers and that was removed on purpose. The revision needs no such concept: the lane remembers its presses only for the current run, in memory (decision 2), and a capture leaves ordinary clips on the tracks. Nothing about the lane is saved but its key length. One change to the live lane stays from the first design: no loop wrap while the lane is selected (decision 5), in place of the press suspending the wrap and the release resuming it.

## What a performance is

The lane only ever moves the transport (`handle_performance_lane_midi_input` → `jump_and_chase(key_tick)`); between presses playback runs straight on, and a release doesn't stop it. So a performance is a list of jumps, "at this moment, go to this key", and what it plays is a **path through the arrangement**: bars 5–6, then bar 13 to beat 3, then bars 1–2, and so on.

There were two ways to keep it:

1. **Keep the path** and let it drive the transport on playback: song mode, scene chaining. A second level of arrangement: the timeline would no longer show what plays, and the path means something different as soon as the arrangement under it changes.
2. **Render the path into clips**: each stretch a copy of what was under the playhead on each track, laid end to end. What you see is what plays, and every existing tool works on the result (transpose, `⌥`-drag, `⌘E`, edge drags, undo).

The revision is the second, at the moment of capture. The first design was both: the path kept while shaping and auditioning, rendered when copied. § Why the pads went says why that didn't hold.

## Principle: what you play is what you get

A capture records exactly what was heard. Nothing in the capture post-processes timing: no snapping press times to a grid, no thresholds, no clean-up that would clear the groove of the performance. An early suggestion was to snap press times at capture, since `jump_and_chase` fires on the press and a press is never exactly on the beat; it was rejected for that reason (2026-11-18).

Rendering at `/` is the same principle applied to the arrangement: the capture holds what the arrangement sounded like while you played, not a recipe that re-reads it later.

Where a gridded result is wanted it comes before the ears, from launch quantize (§ Launch quantize), or after, by hand, on the rendered clips (edge drags on the grid), both the player's choice and both heard.

## Decisions

1. **`/` on the armed lane renders the run and inserts it at the arranger cursor** (agreed 2026-11-19).
    - **The render** is the rule the first design's `⌘C` used (phase 4 as built; now `PerformanceLane::take_run` and `Sequencer::render_performance`): the stretches the run played once through from its start, each `{ at, len, from }`; a press jumps (the last on a tick wins) to where it jumped live, kept with the press, so a key length or meter changed in the middle of a run renders what was heard (found in review 2026-11-19: re-deriving each jump from the key length at the `/` rendered stretches nobody heard), a stretch lasts until the next press, and at the end of the run the last one ends. Each stretch is a copy of every track's clips at `from..from + len`, windows trimmed to the stretch, laid end to end from 0, each piece on the track it came from; a stretch over a gap or past the arrangement's end places nothing there. The pieces share their source clips' `Arc` events (`420`), and a trimmed window keeps the rest of the idea hidden, so dragging a piece's edge out shows it, as hidden notes do (`500` decision 3). It is taken from the arrangement **as it is at the `/`**, which is what was heard (an edit made to the arrangement in the middle of a run is the one way they can differ: rare, accepted). Every track is rendered, a muted or unsoloed one too: its pieces land on that same track, so they stay silent until it is unmuted, as its own clips do; mute and solo are mixing, not the arrangement.
    - **It is inserted, not overwritten**, at the arranger cursor (agreed 2026-11-19): Paste Time's edit (`DuplicateTimeEdit::pasting`, § Paste Time): an Insert Silence of the render's length at the cursor on every track, then the pieces in the gap. With the cursor after the last clip, insert and overwrite are the same; between clips, insert makes room and loses nothing; with the cursor inside a clip, that clip is split at the cursor and its second half pushed along, as `⇧⌘V` does. A loop region at or after the cursor slides along, as for Paste Time. The length inserted is the run's length, so a last stretch that plays nothing still takes its room (`ClipClipboard::span`).
    - **The cursor is where you put it.** A lane press moves only the playhead (`jump_and_chase`), never the arranger cursor, and `Space`/a ruler click don't move it either (`020`). So the workflow is: put the cursor in free space (or where the material should go), select the lane, play, `/`.
    - **One undo step.** `⌘Z` right after removes the inserted clips and closes the gap (Paste Time's undo); `⇧⌘Z` puts them back. The undo doesn't give the run back: the capture is final, as the track capture's commit is (`220`); `/` with no presses since does nothing.
    - **Nothing new is selected** (`220`, a commit never changes the selection) and the lane stays selected. The clipboard is untouched.
    - **The footer** names what landed and where: `Captured 6 bars at bar 13` (whole bars and beats, `length_label`). A run whose stretches all fall in gaps or past the arrangement's end places nothing, inserts nothing, and says `Nothing to keep: the run played no clips`; a `/` with no run says `Nothing played on the lane to keep`, as today.
    - **Running or stopped.** Running, the run ends at the `/` press and playback runs on; the inserted clips land behind or ahead of the playhead like any edit (an insert while playing is Paste Time's case already). Stopped, the run ended at the stop.

2. **The run** (decision 8 of the first design, built in phase 1, kept). While armed, the lane remembers its presses, as the capture buffer remembers notes (`090`), and `/` frames the take after the fact. From the press that started the transport, or the first press after the last `/` if that came later, up to the capture. Its time is musical time elapsed while running: a count of advancing transport ticks on the `"sequencer"` thread, not the transport position (it jumps) nor `clock_tick` (realigned at every jump, `150`). Anything else that moves playback (a ruler click, `Space` from the cursor, a stop and restart) ends it, as disarming and a new project do. Nothing is detected or guessed: a long noodle is captured whole, and trimmed afterwards on the timeline.

3. **Nothing is kept on the lane.** No pads, no performance clip, no piano roll of presses, no `⌥Space` replay, no pad clipboard, nothing saved but the key length. With the lane selected the knob column and the clip panel show what they did before the pads: the column `No plugin on this track` (no track), the panel no clip (`450`). `⌥Space` with the lane selected goes back to its meaning before phase 3.

4. **Shaping happens on the clips.** Trimming, splitting, moving, transposing, repeating: the arranger's tools, on the rendered pieces. The cost against the first design: a press can't be moved before rendering. Re-playing it is the edit; a capture is cheap and `⌘Z` puts the arrangement back.

5. **No loop wrap while the lane is selected** (decision 7 of the first design, built in phase 1, kept). With the lane selected (armed), playback never wraps at the loop end; select a track and it wraps again at the next loop end, if playback is inside the region. The loop itself is untouched: `loop_enabled` and the region stay as they are, nothing is saved or recorded as an undo step. The arranger draws the loop dimmed meanwhile (`Display::is_loop_enabled`). **Why**: a press records only where it jumped, so a wrap would be heard live but missing from the render, against § Principle; with this rule a release has no effect on playback at all, and capture and render agree by construction. It also makes `Space` and a ruler click on the lane play straight past the loop end, which is what trying spots wants.

## The workflow, end to end

1. Put the arranger cursor where the material should go, after the last clip for a new section.
2. Select the lane and play on it (the loop doesn't wrap there, decision 5).
3. `/` keeps the run: the render is inserted at the cursor (decision 1).
4. Listen from just before it; `⌘Z` and play again if it doesn't sit; shape the pieces with the arranger's tools.

## Why the pads went

Found hand-testing phase 4 (2026-11-19, the project `Issue #39 - Phase 4`). A pad stored where each press jumped, not what it played, and every use of it re-read the arrangement as it was then:

- **An arrangement edit after `/` changed the performance.** The replay (`⌥Space`) played the arrangement as it is now, and a copy's render was a snapshot at `⌘C`, so after any edit the two disagreed with each other and with what was played.
- **Placing it changed it too.** With ½-bar keys, G♯4 jumps to bar 11. The pad pressed it twice, and the render was pasted at bar 11, so the replay's G♯4 now found the pasted render where the copy had found the tail of the last clip and silence. A/B between the replay and the render sounded different without anything visibly edited.

A frozen copy of the arrangement under the path was considered (2026-11-19): it would keep the presses editable, but the replay would then have to play clips that aren't on the timeline, a mechanism of its own. Storing the render on a pad was considered too: no drift, but a pad's content is clips on several tracks, so the piano roll can't show it, and auditioning it would need the same off-timeline playback. Rendering straight onto the timeline needs neither and leaves only plain clips. Chosen 2026-11-19.

## Build plan

**Built 2026-11-19**, one phase, mostly removal. The piano roll, replay and clipboard commits were reverse-applied whole (their `src` changes only, newest first, skipping `848ba2f`); the pads were taken out of `3f361f0` file by file, keeping the run, the no-wrap rule and `ClipData::of`/`into_clip`. As built: the render is `Sequencer::render_performance` (`sequencer/clipboard.rs`), the insert `DuplicateTimeEdit::capturing`, the footer's position `format_position` (`Captured 6 bars at bar 13`, `at 13+2` off a bar line), its length `length_label` (`core/agent/units.rs`). The first design's phases are commits `3f361f0` (capture and pads), `28d29c4`/`eeb106d`/`9f806c2` (the piano roll), `b1c2c4e`/`00488bb`/`fc8eac6` (the replay), `cd74a23`/`086f6f9`/`d16bb07` (the clipboard), with `848ba2f` (lane key names) and the shortcut commits after them interleaved; reverting them whole would also undo what stays, so remove by hand with their diffs as the map.

**Keep**
- `PerformanceLane`'s run (`press`, `advance`, `take_run`, `Run::expected_tick`) and `performance_clip`, the run as an intermediate clip the render reads.
- `render_segments` and what it needs (`replay_notes`, `play_at`, `performance_key_target`), `render_performance` (`sequencer/clipboard.rs`): the render rule.
- *As built, after review*: the run keeps each press's jump target and `take_run` gives its stretches directly (a `Take` of `Segment`s); `performance_clip`, `render_segments` and the helpers that re-derived a jump from the key length went, since a run's clip has neither gaps nor overlaps and re-deriving got a mid-run key length change wrong.
- Decision 5: `Transport`'s no-wrap while armed, `Sequencer::playback_is_looping`, the dimmed loop.
- Paste Time (`DuplicateTimeEdit::pasting`) and `ClipClipboard::span`, unchanged.
- The key length and the lane keyboard (`510`).

**Change**
- `capture_performance_workflow` (the `Commit` arm on the armed lane): take the run, render it, and record a `DuplicateTimeEdit` inserting the render at the arranger cursor (a constructor beside `pasting` that takes the rendered clipboard rather than reading `clip_clipboard`, so the clipboard stays untouched); the footer lines of decision 1. Nothing to insert → no edit.

**Remove**
- The pads: `PerformancePads`, `selected_pad`, `SetPerformancePadEdit` (`edit/performance_pad_edit.rs`), `EditResult::PerformancePadChanged`, `SequencerCommand::SelectPerformancePad`/`ClearPerformancePad`/`CopyPerformancePad`/`PastePerformancePad` and their `InputEvent`s, `UiEvent::PerformancePads`, the column (`view/display/performance_pads.rs`, the pad drawing in `rendering/knob_panel.rs`, `KeyFocus::Pads`, `handle_pad_column_key`), `pad_footer_hint`.
- The piano roll on the lane: `ClipHome` (back to a track index in `clip_on`/`clip_on_mut`/`edit_clip_events`, `ClipEventsEditState`, `EditResult::EventsModified`), `selected_clip_home`'s lane arm, `EditResult::is_on_lane` and `UndoStep::leave_view` (if nothing else uses them), `show_performance_workflow`, `shows_performance` and the lower lane's zero height, the accent note colour, the note clipboard's "from a performance" flag.
- The replay: `Replay`, `step_replay`, `start_performance_replay`/`stop_performance_replay`, `ReplayCue`, `ReplayPosition` and `SharedAtomics::performance_replay`, `TransportCommand::PlayPerformance` and its routing in `input_handler.rs`, the silent-gap mute in `Sequencer::tick`, `Transport::move_playback` if only the replay uses it, the replay's playhead in `lead_clip_playback_tick`.
- The pad clipboard: `performance_clipboard`, `copy_performance_pad_to_clipboard`, `paste_performance_pad_workflow`, `Clip::detached_copy` if nothing else uses it.
- Persistence: `ProjectData::performance_pads`, `PerformancePadData`, the fingerprint's pads. The pads were never released (only `[Unreleased]` in `CHANGELOG.md`), so no released file has them; a development file that does opens with the field ignored if serde allows it, otherwise check that `ProjectData` doesn't deny unknown fields.

**Tests**
- `/` on the armed lane inserts the render at the cursor on the tracks it played from; the clips after the cursor move along by the run's length; the selection and the clipboard are unchanged; the footer line.
- The render is the arrangement as it was at `/`: an edit after the capture changes nothing already inserted (the regression this revision is for).
- A press into a gap or past the end: that stretch is silence, its length still inserted; a run that plays no clips inserts nothing and says so.
- `⌘Z` removes the insert and closes the gap; `⇧⌘Z` puts it back; a `/` after the undo with no new presses does nothing.
- The cursor inside a clip: split and pushed, as `⇧⌘V`.
- Keep the run's tests (now on the `Take`'s stretches); the replay-against-render test goes with the replay.

**Docs in the same change**: `110` (§ Scope, the run, what `/` does), `010` (`/` on the lane, `⌥Space`, the pad keys), `020` (the clip view on the lane, the pad column), `030` (§ Knob Panel's pads, the lower lane note), `050` (`PerformancePadChanged`, `is_on_lane`, `ClipHome`), `060` (the pads in `ProjectData`), the `520` row in `AGENTS.md`, and `CHANGELOG.md`'s `[Unreleased]` performance entry rewritten to the new behaviour (not breaking: never released). Check `000`, `130`, `400`, `420`, `450` and `500` for passing mentions of pads or the replay.

## Paste Time (built)

Placing a render needs a paste that inserts. Stev had none (`⌘V` overwrote, `⇧⌘D` inserted only straight after its source), so Paste Time was built first, on `main` since 2026-11-18 (`6189688`; `010`, `020` § Arranger Time Selection). Nothing of it is specific to the lane; what decision 1 relies on:

- **`⇧⌘V` is `⌘V` inserting.** The same clipboard, anchor and tracks as `⌘V` (the selected track for a scoped copy, the source tracks for an all-track one), after an Insert Silence at the cursor on every track. One undo step: `DuplicateTimeEdit::pasting`, Duplicate Time's edit with the clipboard at the cursor.
- **It inserts the copied span, not the pieces' extent.** `ClipClipboard::span` holds the copied range's length, so a copy ending in silence keeps it. A render sets its span to the run's length, so a last stretch that plays nothing still takes its room.
- **No marquee is needed and the marquee stays**, as with `⌘V`, so it works with the lane selected. A loop region at or after the cursor slides along, as for Duplicate Time (and as in Ableton).

The time clipboard as it stands, against Ableton's:

| | Stev | Ableton |
|---|---|---|
| `⇧⌘C` | Copy time: every track's slice | Copy Time |
| `⇧⌘X` | Cuts the slice, leaves the gap | Cut Time: closes the gap |
| `⌘V` | Paste, overwrites | Paste |
| `⇧⌘V` | Paste Time: inserts, pushes the rest | Paste Time |
| `⇧⌘D` | Duplicate Time, inserts | Duplicate Time |

`⇧⌘X` as Cut Time, closing the gap as in Ableton, was left out: `⌘Delete` closes gaps already, and it would change a released key.

## Launch quantize

**Deferred (2026-11-18).** Its own design, not needed for capture: a capture with Off keeps the groove (below), and a gridded result can come from shaping the rendered clips by hand (§ Principle). Useful while just performing and testable by ear alone, so it can come later.

- **Two settings, two questions.** The key length (built) is where a key jumps *to*; launch quantize is *when*: Off (on the press), Beat, ½ bar where the meter allows, Bar. They pair naturally (Bar keys at Bar launch is classic scene launching; Beat keys with Off is stutter playing), but "jump to bar 5, on the next beat" is valid too, so they stay separate. Saved with the project, not an undo step, as the key length.
- **A scheduled press**: jumps at the next boundary, done by the sequencer on its own tick, so exact to the tick, tighter than a live press. The latest press wins. A release before the boundary probably doesn't cancel (the press was the decision, as in Ableton); to check by hand. A press while stopped starts at once. **A pending key must show**, or a press waiting half a bar feels broken: an outline or dimmer accent on its piece of the strip until it lands.
- **What Off implied** (history: since `550` a press while running lands at its key plus its offset from the nearest beat, so this drift is gone). Targets are always on the grid (`key_tick`), moments aren't. Press 0.3 of a beat late into bar 5 and the source then plays 0.3 beats against the output: every bar line after it sits off the grid it lands on. That is what was played; a capture with Off has pieces at off-grid ticks (clip windows hold any tick) and its downbeats won't line up. That *is* the groove, and keeping it is the point. With Beat or Bar everything is on the grid by construction, nothing corrected.
- **Phase jumps** (`550`, built 2026-11-21): a press while running jumps to the target *plus* its offset from the nearest beat (cutting in early or late, the grid kept), in place of Off. It changes what's heard, so it is a playing mode, not a capture fix. Off's drift was a problem in use: the metronome restarted at every press.

## Open questions

1. **An issue for the revision.** Settled 2026-11-19: no new issue. #39 closed with the first design and stays closed; the revision's commits say `Part of #39`.
2. **Settled 2026-11-19**: insert or overwrite (insert, decision 1); whether an undo gives the run back (no, decision 1); where the render lands (the arranger cursor, decision 1).
3. **Does the render sound like the live run?** It should by construction (checked for the replay 2026-11-18, the same rule): a lane jump chases notes in the clips the target falls strictly inside (`Sequencer::chase_notes`); a trimmed-window piece landed on at its start chases its hidden `NoteOn`s itself (`Track::tick`'s arrival chase, `pending_note_ons`), and `chase_notes` leaves a clip's exact start to it on purpose. A stretch's end matches too: live, the next jump re-anchors and flushes what sounds; rendered, the window end releases it (`pending_note_offs`). Wheels, the sustain pedal and parameters are latched in both (`chase_controllers(clip.controllers_at(tick))`), and the pedal is lifted at each jump in both (`archive/530-sustain-capture.md` decision 4); other plain CCs in neither. Worth an A/B by hand once built, with the render inserted after the last clip so no key's target moves.
4. **A sliver found in the test project.** `Issue #39 - Phase 4` has a 181-tick clip at bar 5 on three tracks (window 1625–1806) that pushed everything after it off the grid; something earlier inserted it, perhaps a render paste. Not investigated; look at it if it recurs with the revision.

## Not in this design

- **The path on the timeline** (song mode, scenes): § What a performance is.
- **Post-processing a capture's timing**: § Principle.
- **A pending, uncommitted capture**: the `PendingClip` model `220` removed.
- **Keeping performances off the timeline** (pads, a performance clip, a replay): § Why the pads went.
- **Step 2 of `510`, one key per idea.** Less urgent with capture, since the result is plain clips whatever the mapping.
