# Earlier Pass

**Status: design agreed 2026-11-20; both phases built 2026-11-20.** After a running `/`, `,` swaps the take it just landed (a new clip, or an insert into the lead clip) for the pass played before it, and `.` steps forward again, so a great pass followed by a fumbled one is not lost. `/` itself is unchanged. Each step changes the same clip in place; there is no mode, no view and no confirm, and one ⌘Z still removes the take. Read `100` (running capture, *What a new clip retains*), `050` (undo) and `440` (undo view state) first.

## Background

A running `/` keeps the **last pass**: the region-grid pass holding the last note-on, nothing from earlier wraps (`100` § *What a new clip retains*). That fits the workflow it was made for, one idea per wrap and `/` when the last one is right. It misses one common case: a great pass, then a fumbled one, and `/` keeps the fumble.

The "most recent note at each phase" fold, tried and reverted on 2026-09-19, is not this. That mixed passes together and left notes that didn't belong. This picks one whole pass.

Reading the code on 2026-11-20:

- **While running, the capture buffer holds every pass.** Input is appended to `capture_clip` (`add_midi_event_to_capture`), with no trim at a wrap and no cap. It is emptied by starting playback (`begin_running_take`), the Stop command (`TransportCommand::Stop`; a plain `Space` stop leaves it), a new project, and the commit.
- **After `/`, nothing is left.** `CommitClipEdit::edit` calls `reset_capture()` once the clip is on its track. That drops every pass and the clock offset. The edit keeps only the framed clip.
- **The pass is chosen in one place.** In `normalize_running_capture`'s looping `NewClip` arm, `calculate_running_region` picks the pass holding the last note-on. Everything after it works on any pass window: the early-note relocation, `carry_resting_onto`, `hold_start_values`, `fold_into_loop` and the region slice. Because it reads the whole buffer, a wheel, pedal or knob left off its rest by an earlier pass is still carried onto the chosen pass's start.

Other apps: Logic Pro's "Select Previous/Next Take or Comp" (`⇧⌘↑`/`⇧⌘↓`), Ableton Live's take-lane cycling (`⌘↑`/`⌘↓`), REAPER's "Switch items to previous/next take" (`⇧T`/`T`). All of them step through recorded takes after recording. None of them makes it a mode.

## Decisions (2026-11-20)

1. **`/` is unchanged.** It commits the last pass as today. A step is always something the player chooses afterwards.
2. **`,` steps to the pass before, `.` to the pass after.** Two keymap actions, `capture.earlier_pass` and `capture.later_pass`, scope `Global` like `Commit`, so they work from the arranger and the clip view. Plain `,` and `.` by default: one key each, the `<`/`>` keys on US layouts, side by side on Norwegian ones, and on US layouts in one row with `/`. Considered and set aside:
   - `<`/`>`: egui has no such keys. US layouts send them as `,`/`.` anyway, and Nordic ones as `IntlBackslash`, which US lacks.
   - PageUp/PageDown: they're fn+↑/↓ on a MacBook.
   - The DAWs' arrow chords: Stev's arrows are fixed keys (`keymap::refusal`), and `⌥↑`/`⌥↓` (the marquee transpose) and `⇧↑`/`⇧↓` (the selection's track span) sit next door.
3. **A step changes the clip `/` made, in place.** Only its events change: notes, wheel and pedal moves, knob moves. The clip keeps its id, track, start, length and region; the new pass is framed with the anchor, region length and loop the commit used, so it lands at the same phases. The swap goes through `Track::edit_clip_events`, so notes of the old pass that are sounding are released. The transport keeps running and doesn't move: playback plays the new events from wherever it is. The selection, the lead clip, the cursor, the marquee and the keyboard focus stay as they are (`commit-never-changes-selection`, `focus-owns-the-keyboard`). The arranger thumbnail and an open piano roll show the new notes.
4. **A step amends the commit and is not an undo step of its own.** The step on top stays the one `CommitClipEdit`, now holding the stepped clip, so one ⌘Z removes the take whatever pass it shows, and a redo puts back the pass it showed when undone. To go back to a pass, step to it. The step's view snapshot (`440`) is unchanged, since a step changes no view state.
5. **The keys act on the newest step, and only when it is a running commit.** `,`/`.` act while the newest step in the undo history is a running new-clip commit and has not been undone. This is read from the record (`top_step`, with nothing to redo), not kept as a flag (`derive-over-dirty-flags`). Any other edit, an undo of the commit, or a new `/` ends it. Things that are not undo steps leave it alone: the transport, the cursor, the loop, the mixer, opening or closing the clip view. Otherwise the keys do nothing and say so in the footer.
6. **The commit keeps its buffer.** At the commit, the edit takes the capture buffer it consumes (`std::mem::take` rather than dropping it), along with the anchor, region length, loop start and length, and clock shift it was framed with. A later loop change therefore doesn't shift the passes. Notes played after the `/` go into the fresh buffer as before and aren't part of the steps. The kept buffer is dropped when the next step is recorded on top of the commit, since the keys can never act on it again (decision 5). At most one buffer is kept at a time.
7. **Passes are counted on the commit's pass grid.** They run from the first pass the buffer holds (the transport start or the last `/`) to the one `/` took. A pass holding no note-on is skipped, so a pass spent listening doesn't count. The first pass may be partial (a start mid-loop) and the last one too (a mid-pass `/`); each is framed as `/` would frame it. Each step re-frames the buffer exactly as `/` does, only with the pass index moved: the early-note rule (`LATE_NOTE_TOLERANCE_TICKS`), the carry of a held wheel or pedal, the knob start values and the retained pre-roll before the region.
8. **Footer lines.** A step says where it is: "Pass 2 back of 5", or "Last pass" back at the one `/` took. Refusals: "No earlier pass" at the first, "Already the last pass" at the last, "No running take to step through" when decision 5 doesn't hold, "The take isn't looping" for a linear running take (decision 9). In fg/fg_dim, no colour (`no-alarm-colours-in-ui`).
9. **Phase 1 covers the looping new-clip commit**, the fumble case, with or without a marquee framing it (`470`; moved from phase 2 on 2026-11-20 after hand-testing: ⌘L loops over the marquee and leaves it, so the usual `/` is marquee-framed, and while looping it is framed on the same pass grid). A linear running take (the transport not looping) has no passes, and its steps are refused. The running insert/overdub into the lead clip (`InsertCaptureEdit`, `470` phase 2) is phase 2, built the same day. A stopped `/` and the take session are not affected.

## Walkthrough

A 4-bar loop, no lead clip, the transport running.

1. Play an idea on the first pass. On the second, a better one. On the third, fumble the start and stop playing halfway.
2. `/`. As today, the clip holds the third pass. Footer: nothing new.
3. `,`. The clip now holds the second pass. Footer: "Pass 1 back of 3". The loop plays it from the next note on, and the piano roll, if open, shows it.
4. `,` again: the first pass ("Pass 2 back of 3"). `,` once more: "No earlier pass".
5. `.`: back to the second pass. Leave it there and drag the clip's end: that's an edit, so `,`/`.` now say "No running take to step through".
6. ⌘Z undoes the drag, ⌘Z again removes the clip. ⇧⌘Z twice brings back the clip with the second pass, then the drag.

## What to check

- **The pass index round-trips.** Stepping back k and forward k gives back exactly the clip `/` made, events and region. A test per framing case: a start at phase 0, a start mid-loop (partial first pass), a mid-pass `/` (partial last pass), an early chord just before a wrap (`commit_clip_to_track_early_chord_starts_the_next_pass`'s case, stepped back to the pass before it), a silent pass skipped.
- **The carry across passes.** A pedal pressed in pass 1 and held, stepped back to pass 2: carried onto pass 2's start, as `/` would.
- **Sounding notes release** on a step while running (`muted-note-release`'s lesson: test through `run_command`, then `reset()`).
- **The amend.** One ⌘Z after k steps removes the clip; a redo restores the stepped pass. The record's length doesn't change on a step.
- **Decision 5's edge.** An undone commit, a commit under a newer step, a commit with something to redo, an `InsertCaptureEdit` on top, a stopped commit on top: all refused.
- **The kept buffer is dropped** once another step is recorded (a test reading the edit's buffer after the next edit).
- **The keymap**: the two rows conflict-free (`ACTIONS`' test), the Keyboard tab listing them.

## As built (phase 1)

- The grid is `PassGrid` (`sequencer/capture/passes.rs`), returned by `Sequencer::build_committed_capture` and used by the `/` itself, so the last pass and a stepped one are framed by one function (`PassGrid::frame`).
- Framing a pass drops the notes in its last `LATE_NOTE_TOLERANCE_TICKS` (`Clip::drop_notes_starting_in`): by the grid they are the next pass's early downbeat, and the late-note relocation would otherwise have pulled them onto this pass's start. The last pass never holds such notes, so `/` is unchanged.
- The step is `PassStepEdit` (`commit_clip.rs`), merged into the commit by `SequencerEdit::merge`; the same merge drops the kept buffer when any other edit goes on top. The workflow is `step_pass_workflow` (`clip_lifecycle.rs`), which records with `undo_record.edit` rather than `record_edit` so the commit's view snapshots are untouched.
- A marquee-framed `/` steps too: it runs the same looping framing with the marquee's start as the anchor and its width as the region, so the clip keeps the marquee's span on every pass.
- A step re-detects the clip's swing on the new pass, as `/` detects it.
- The keys follow `acts_on_pane`'s default, as `/` does: pressed with the browser focused they take the keyboard to the pane.

## As built (phase 2)

- The insert's grid is `InsertGrid` (`passes.rs`), returned by `build_running_capture_insert` with the events and used by the `/` itself (`InsertGrid::land`), so the insert's last pass and a stepped one land one way. It freezes the anchor (capture clock), the loop and the anchor's phase, the window length, where the window lands in the clip, the room, the wheel end and the punch-in. `normalize_running_capture` now frames a new clip only; the insert arm moved into `InsertGrid::frame`.
- The insert's passes are its anchor-pinned windows, not the new clip's loop passes; the last is the one `calculate_running_region` pins, as before. A note-on within `LATE_NOTE_TOLERANCE_TICKS` of a loop wrap counts toward the window whose loop pass starts there (which relocates it onto its downbeat), and framing an earlier window drops those notes, as phase 1 does. In a clip longer than the loop an earlier window also stops one loop from its start, so it never takes the start of the pass after it. A wheel-only take counts the windows holding a controller move.
- A step re-lands the pass over `InsertCaptureEdit`'s `pre_events` with `Clip::land_take`, keeping the clip's current event selection less the old pass's notes. The merge re-lands the pass into the insert's `events`, so a redo lands the pass it showed.
- What the commit and the insert share (the kept buffer as `Passes<G>` over a `PassFraming` grid, `PassStep`, `PassRefusal`, `PassStepEdit`) moved to `edit/pass_steps.rs`. The footer lines are the same for both.

## Not in this design

- Browsing or auditioning passes before the `/`: what `/` takes is still the last pass. Seeing the passes while playing is the separate "visual feedback during capture" idea.
- Comping: taking bars from different passes. Each step is one whole pass.
- Steps after the stopped `/`, or in the take session.
- A pass list or a picker in the UI.

## Phases

1. **The new-clip running commit** (decisions 1–9), a marquee-framed one included. `CommitClipEdit` keeps the buffer and its framing; a pass-indexed variant of the looping `NewClip` framing; the two actions and their workflow; the amend of the step on top; the footer lines. Tests: `capture/mod.rs` (the framing per pass index, the skipped silent pass, the carry), `commit_clip.rs` (the amend, the undo and redo, the dropped buffer), the workflow through `run_command` (the refusals, the release while running), `keymap.rs`. Docs: `100` (*What a new clip retains*: the steps), `050` (the amend), `010` (the keys), `020` (that it isn't a mode). Changelog: `,`/`.` step a running take back to an earlier pass.
2. **The running insert** (built 2026-11-20). (The marquee-framed commit moved into phase 1, decision 9.) A running insert or overdub into the lead clip keeps its buffer the same way, and a step re-lands that pass over the clip's events from before the insert (`InsertCaptureEdit`'s undo state), so passes never stack. Tests: `passes.rs` (the windows, the early chord, a clip longer than the loop, the punch-in), `event_edits.rs` (a take session's insert refused), the workflow through `run_command` (the steps, the amend, undo and redo, the dropped buffer, an insert after a commit).
