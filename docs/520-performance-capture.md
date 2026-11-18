# Capturing a Performance-Lane Performance

**Status: design agreed 2026-11-18; phases 1 (capture and pads) and 2 (the piano roll) built, phases 3–4 not (§ Phases). Issue: #39.** Launch quantize is deferred to a design of its own (§ Launch quantize); Paste Time, which it builds on, is built (§ Paste Time). A performance played on the lane (`110`, `510`) is captured as a **performance clip the lane holds**: a note clip whose notes are the keys pressed. With the lane selected, the knob panel's column (which has no plugin to show then) holds the performances as eight pads, and the docked clip panel's piano roll shows the selected one, edited there with the clip view's tools. It is auditioned with `⌥Space` against the arrangement's `Space`, and placed through the clipboard: a pad selected and `⌘C` copies its performance, rendered as clips on the tracks it played from, and `⌘V` overwrites or `⇧⌘V` inserts it at the cursor. It builds on Paste Time, `⇧⌘V`, built on its own first (§ Paste Time); launch quantize, when a press jumps, is deferred (§ Launch quantize).

## Background

The marquee transpose (`500`), the ruler click that plays without moving the cursor (`020` § Timeline click) and the lane drawn as a keyboard with a choice of key length (`510`) together opened a workflow: put ideas side by side, shift them by ear until they work together, then play freely on the lane trying combinations. Sooner or later a pattern comes out that would work as a transition or a section of its own. Today it is gone once played: the lane is live-only by design (`110` § Scope).

What's wanted is to keep such a pattern as material: a new set of clips that can go anywhere on the timeline.

### A deliberate change of `110`'s scope

`110` says not to bring back a `PerformanceLaneData`/events concept "without a deliberate scope change": an earlier lane recorded its triggers and that was removed on purpose. This is that change, made deliberately. What was removed was trigger data shown on the timeline as lane feedback; what's proposed here is a captured performance as material, kept off the timeline (decision 3) and turned into ordinary clips on commit. If it goes ahead, `110` § Scope records the change and the reason. One change to the live lane comes with it: no loop wrap while the lane is selected, in place of the press suspending it and the release resuming it (decision 7).

## What a performance is

The lane only ever moves the transport (`handle_performance_lane_midi_input` → `jump_and_chase(key_tick)`); between presses playback runs straight on, and a release doesn't stop it. So a performance is a list of jumps, "at this moment, go to this key", and what it plays is a **path through the arrangement**: bars 5–6, then bar 13 to beat 3, then bars 1–2, and so on.

There were two ways to keep it:

1. **Keep the path** and let it drive the transport on playback: song mode, scene chaining. Non-destructive (edit an idea and the performance follows), but a second level of arrangement: the timeline would no longer show what plays, and it would fight the loop, the playhead and the cursor.
2. **Render the path into clips**: each segment a copy of what was under the playhead on each track, laid end to end. What you see is what plays, and every existing tool works on the result (transpose, `⌥`-drag, `⌘E`, edge drags, undo).

The direction is both, in that order: the performance is *kept* as the path (the performance clip, decision 1) while it's being shaped and auditioned, and *rendered* to clips when copied for placing (decision 5). The timeline only ever holds rendered clips, so it stays flat.

## Principle: what you play is what you get

A capture records exactly what was heard. Nothing in the capture post-processes timing: no snapping press times to a grid, no thresholds, no clean-up that would clear the groove of the performance. An early suggestion was to snap press times at capture, since `jump_and_chase` fires on the press and a press is never exactly on the beat; it was rejected for that reason (2026-11-18).

Where a gridded result is wanted, it comes from one of two places, both the player's choice and both heard:

- **Before the ears**: launch quantize schedules the jump on the next beat or bar while playing (§ Launch quantize).
- **After, by hand**: quantizing the performance clip's notes in the piano roll (decision 2), an explicit, visible, undoable edit.

The same holds for playback (decision 4): the performance is replayed through the live trigger path, so it isn't a reconstruction of what was played but the same code playing it again.

## Decisions

1. **The lane holds performance clips, as data.** The sequencer already owns the lane's state (the key length, `510` § Step 1b); the performance sits beside it: an ordinary note clip whose notes are the keys pressed. A note's pitch is the key (C3 and up, where it jumps at the key length), its start is when it was pressed, its length is **until the next press** (not until the key's release: a release doesn't stop the transport, so "from this press to the next" is what played). Ticks count from the performance's own start, not from the timeline. It is saved with the project (one field beside the key length: a list, empty by default, decision 6), a capture is an undoable edit, and editing it is ordinary undoable clip editing.

    **Why data and not a pending capture.** The first picture was a pending capture: a take not yet committed that you could leave to audition the arranger and come back to. That is the `PendingClip` model `220` removed, and a stored mode `220`'s principles avoid (the take session is the one exception). The questions it raises ("if the capture isn't committed yet…", "go back to where it still sits") are the ones a mode creates. Held by the lane as data, there's nothing to suspend or resume:

    | | Pending capture | Data on the lane |
    |---|---|---|
    | Leave for the arranger | Suspend it, remember to come back | Ordinary selection; it's still there |
    | Save, quit, reopen | Lost, or a special case | Saved like the key length |
    | A mistake | Discard the session | `⌘Z` |
    | Try it in three places | Must stay uncommitted throughout | `⇧⌘V`, `⌘Z`, move, `⇧⌘V` |

2. **The panel shows it as a piano roll.** A performance *is* a monophonic melody on the lane's keyboard, so the view is the piano roll itself, with the lane's keys as its rows, and it maps one to one onto what was played:

    ```
     Performance · 6 bars                      ← clip panel, lane selected
            |1         |2         |3         |4         |5
       G3   |                     ██████
       E3   |           ██████████      ███
       C#3  |                              ████
       C3   |███████████                       ██████████████ …
    ```

    Every edit has a musical meaning: a note dragged sideways cuts earlier or later; dragged up or down, the segment plays from somewhere else; shortened, it cuts out sooner; `⌥`-dragged, a segment repeats; quantized, the cuts land on the grid, by choice. Rows are labelled as on the lane's strip, perhaps with where they jump ("E3 · bar 2"). The velocity lane, the wheels and the knobs don't apply and aren't shown. Live there are no gaps or overlaps; after editing, **a gap is silence**, in the replay and the render alike (2026-11-18): the replay releases what sounds at the note's end, and the render places nothing there, so the two stay the same thing (§ Open questions 3) and a gap is a tool of its own, a stop before the drop. An overlap resolves as live, the latest note wins.

3. **In the docked clip panel, not the lane row.** Everything in the lane row lines up with the bars above it; that's the point of `510` ("the eye goes straight up from a clip to its key"). A performance has its own time, 0 to its length, and its six bars aren't the arranger's bars 1–6: drawn in the row, its notes would sit over bars they don't play. The row is also 24 pt for up to 68 rows of keys, and it already has a job, the keyboard. The panel needs no new layout (a third thing it can show: a track's clip, or the lane's selected performance), and during playback (decision 4) it gives two views at once: the arranger above shows the *path* (the playhead jumping between ideas, the strip lighting keys), the panel below the *time* (its own playhead running steadily through the notes).

    **The column beside it holds the performances as pads.** The knob panel's column (`030` § Knob Panel) is left of the piano roll; its knobs belong to the selected track's plugin, so with the lane selected (no track) it has nothing to show and reads `No plugin on this track`. With the lane selected it shows eight pads instead, in the knobs' own cells: two columns of four (`knob_layout`), numbered as the knobs are, down the first column then the second, so switching between a track and the lane changes what's in the cells, never the layout. Think of a grid controller's pads (a Launchpad): a pad is **lit** when it holds a performance (filled) and **unlit** when it's empty (an outline), a placeholder waiting for a capture. A pad is the performance as a whole, an object to select, as a clip is in the arranger; the piano roll beside it shows its events. Lit pads also answer whether a performance exists (§ Open questions 7) without touching the lane row.

    A pad has no text. Hovering one puts its number and length in the footer ("Pad 6 · 6 bars"; an empty one "Pad 6 · empty"), as a knob's full name goes there (`knob_footer_hint`). Clicking a pad selects it, lit or not, and gives the column the keyboard, as a clicked knob does (`KeyFocus::KnobPanel`); the selected pad gets the panel's selection mark, the corner brackets the selected knob gets (`frame_brackets`), so the column keeps one selection mark. Mouse only, as the knobs are (`400` decision 1). The pad's keys are decision 5's copy and paste and decision 6's delete. Colours are settled while building, within `030`'s theme (no alarm colours).

4. **`Space` plays the arrangement, `⌥Space` the performance.** With the lane selected, both play from where you are, without leaving the lane:

    - `Space`: the arrangement from the arranger cursor, as today, unchanged.
    - `⌥Space`: the performance, from the panel's cursor (or its start), **whichever pane has the keyboard** while the lane is selected.
    - `←`/`→`: move the arranger cursor along the lane, as today (`450` decision 6): the spot to try next.
    - A ruler click: the arrangement from any bar without moving the cursor (`020`).

    This is the clip view's existing grammar (`010`): `Space` from the arranger cursor, `⌥Space` "from where you're editing", the clip cursor, leaving the arranger cursor alone. On the lane, what you're editing is the performance. Today `⌥Space` picks the clip cursor only with the panel focused; on the lane it plays the performance with either pane focused, since otherwise A/B would be focus-switching instead.

    **Why not up and down.** The first picture was A/B by selection: a track and `Space` for the arrangement, `↑` to the lane and `Space` for the performance. It changed `Space`'s meaning on the lane, and every comparison was a trip between rows. `Space`/`⌥Space` needs neither.

    The performance plays **through the live trigger path**: each note fed to the lane's trigger at its time, as if pressed again, so the playhead jumps and the strip lights as when it was played, and an edit is heard on the next pass. It loops over its own length. While it plays the transport position jumps, so the performance needs **its own clock** (time since it started) driving the jumps and the panel's playhead: the one new mechanism, on the sequencer thread beside the trigger, and the same count capture stamps presses with (decision 8). `⌥Space` is an unconditional restart, so it switches from the arrangement to the performance mid-play; back is `Space` (stop) then `Space`. A live lane key pressed during the replay takes over, the latest press wins as today, and the replay stops (recording over a performance is another question).

5. **Placed through the clipboard: a pad and `⌘C`, then `⌘V` or `⇧⌘V` where the keyboard is.** The focus decides what a clipboard key acts on: `⌘C` copies what is selected in the focused place, and a paste lands there, never in a pane that doesn't have the keyboard (`020`, focus owns the keyboard):

    | Focus | `⌘C` | `⌘V` | `⇧⌘V` |
    |---|---|---|---|
    | The arranger | a marquee's clips, as today | clips, or a pad's render, overwriting at the cursor | the same, inserting (Paste Time, § Paste Time) |
    | The piano roll | selected notes | notes, inside performances only when they came from one | as today |
    | The pad column | the selected pad: its performance and its render | the copied performance onto the selected pad, replacing it | nothing |

    **One copy, two forms.** `⌘C` on a lit pad copies the performance and its render together, and where it is pasted picks the form, as a copy of rich text carries plain text too. The **performance** is the pad's clip, for another pad. The **render** is what the arrangement gets: each segment a copy of the clip under the playhead on each track, its window trimmed to the stretch that played, laid end to end from 0, each piece tagged with the track it came from and no track anchor, the span the performance's length. That is exactly the shape of an all-track time copy (`⇧⌘C`, `020`), so the ordinary paste keys place it, with the arranger focused, from the lane or from a track:

    - `⌘V`: overwrite at the cursor, as today. With no track selected an all-track paste already puts every piece at the cursor on its source track (`ClipClipboard::target_track`), so it works from the lane.
    - `⇧⌘V`: insert at the cursor and push the rest along (§ Paste Time).

    **Pad to pad.** With the column focused, `⌘V` puts the copied performance on the selected pad, lit or not, replacing what it held (as a capture does, decision 6), one undo step: a copy of the clip, so editing one pad never changes the other. That makes trying a variation cheap: copy a pad, paste it onto an empty one, edit the copy, A/B them with `⌥Space`. With no performance in the clipboard (the last copy was clips or notes) `⌘V` there does nothing, and `⇧⌘V` in the column does nothing (a pad has no time to insert into). `⌘X` (copy, then clear the pad) would follow the same grammar; not now.

    Trimmed windows keep it cheap and reversible: the copies share the source clips' `Arc` events (`420`), and dragging a segment's edge out shows the rest of the idea, as hidden notes do (`500` decision 3). A copy isn't an edit and records no undo step; each paste is one, selects nothing new (`220`), and leaves the performance in its pad and in the clipboard, so it can be placed again.

    **Why the clipboard, not a commit key.** The first version was `Enter` committing at the cursor (and then `Enter`/`⇧Enter` for overwrite and insert, mirroring `⌘V`/`⇧⌘V`). That is a second placement grammar beside the paste one, with keys and an edit of its own. Once rendered, a performance *is* a time copy, so it goes through the clipboard and the paste grammar places it. It also answers "insert or overwrite" without choosing (§ Open questions 2), and works from anywhere, which is what was wanted from the start: a new set of clips that can go anywhere on the timeline. And `Enter` on the lane stays free (capture is `/`, decision 8). This also follows the habit of routing through the general path rather than keeping a variant of it.

    **Why a pad, not the lane or the piano roll.** Two other places were considered (2026-11-18). `⌘C` while standing on the lane, copying the performance implicitly: but Stev never copies without a selection (every arranger clipboard chord is marquee-only, `020`), and the lane doesn't show the performance (decision 3: the row is the keyboard, and the cursor on it is a position in the arrangement), so it copies something you can't see from where you stand. `⌘C` in the piano roll, copying what the selected notes play: but there a copy should be the clip's events, isolated to the performance clip, as in every clip; making the piano roll's copy produce arrangement clips would be a grammar of its own. The pad gives the performance a visible, selectable form, so copying it is the ordinary "select a thing, copy it". The piano roll keeps note copy and paste between and within performances; arrangement clips only come from a pad.

    **Getting to the paste.** `Esc` in the column gives the keyboard back to the arranger, as it does from a knob (`010`), so the loop needs no mouse after the pad is clicked: pad, `⌘C`, `Esc`, `⇧⌘V`, listen, `⌘Z`, `→`, `⇧⌘V`. The clipboard keeps the copy, so each retry is the last keys only. A click in the arranger does the same as `Esc`. With the column focused, `⌥Space` still plays the selected performance (decision 4: lane selected, either pane).

    **Why not paste into the arranger from the column.** A knob already hands pane commands back to the pane (`Action::leaves_knob_panel`), and `⌘V`/`⇧⌘V` from a pad could have done the same, pasting at the arranger cursor and saving the `Esc`. But with the column focused, `⌘C` copies the selected pad: a `⌘V` beside it that lands in a pane without the keyboard is a surprise, and it would take the key a pad-to-pad paste needs (2026-11-18). So the clipboard keys stay with the column, as `Delete` does on a knob; only `Esc` and a click move the keyboard.

    **What it costs.** The render happens at the copy, not the paste: an idea transposed after `⌘C` is still in its old key in the clipboard, and is copied again (as any copy behaves). A copy of a pad replaces whatever was in the clipboard, both forms, and any later copy replaces both. The clipboard gains a performance beside its clips; the render carries a span as any copy now does (§ Paste Time).

    The transition into the performance is heard after pasting: play from a bar before the cursor; if it doesn't work, `⌘Z`, move the cursor, paste again. After the first `⌘C` each try is one key. A preview playing "the arrangement up to the cursor, then the performance" without pasting is possible (decision 4 started at the cursor) but is a virtual timeline, the pending state in another form; left out until paste + `⌘Z` proves too slow.

6. **Eight pads, a fixed grid.** The lane holds up to eight performances, one per pad (2026-11-18). No names, no ordering, no scrolling: the grid is fixed, and you select the pad you want to work on.

    - **Exactly one pad is always selected**, possibly an empty one. Pad 1 when a project opens; which pad is selected isn't saved and isn't an undo step (view state, as the selected knob).
    - **The selected pad is the current performance**: the one the piano roll shows, `⌥Space` plays and `⌘C` copies. On an empty pad the piano roll is empty and `⌥Space`, `⌘C` and `Delete` do nothing.
    - **A capture goes to the selected pad** and lights it. On a lit pad it **replaces** what was there, one undo step, as recording into a slot does on a grid controller; `⌘Z` brings the old performance back. A full grid means picking a pad to reuse.
    - **`Delete` on the selected pad clears it**, one undo step, as a clip or a track goes; the pad goes unlit and stays selected.
    - **Saved with the project as a sparse list** of pad and clip pairs, so an empty grid saves nothing and an old project opens with every pad unlit.

    **Why pads.** With one performance, comparing two takes means capturing again. With several, comparing is clicking between pads and `⌥Space`, the same A/B as decision 4. A list of tiles was the first version (a capture added a tile, so it never replaced anything); it needed names, an order, room past the column and a limit to settle. A fixed grid has none of those, fits the knob panel's cells exactly, and says at a glance which slots hold something. The cost is that a capture onto a lit pad replaces, which undo covers. The stored shape is the decision that is hard to change later (a field in the project file, `060`); a sparse list keeps it open to more pads.

    **Not yet**: dragging a pad into the arranger (a placement besides the clipboard, decision 5); launching pads from the lane's keys or a hardware grid controller, or playing them in a chain, which would be the path on the timeline again (§ What a performance is).

7. **No loop while the lane is selected.** With the lane selected (armed), playback never wraps at the loop end; select a track and it wraps again at the next loop end, if playback is inside the region. The loop itself is untouched: `loop_enabled` and the region stay as they are, nothing is saved or recorded as an undo step, and leaving the lane brings the loop back. Arming sets `Transport`'s wrap suspension and disarming clears it; a press no longer suspends and a release no longer resumes (`110` § Arming changes with it, and the changelog says so, since it changes how the live lane behaves).

    The arranger draws the loop as it already does with looping off (dimmed): what it reads is whether the loop applies, looping on and the lane not selected. Nothing new is drawn.

    **Why.** Today a press suspends the wrap and its release resumes it, so a key let go before the loop end lets playback wrap to the loop start, and that wrap is part of what was heard. A performance records presses only (decision 1), so neither the replay (decision 4) nor the render (decision 5) could tell that a wrap happened: they'd play straight on where the live take wrapped, against § Principle. Recording the wraps as well was considered: a wrap goes to the loop start, which needn't be a key, so it isn't a note in the piano roll, and the lane would be back to a list of events that aren't notes. Tying the wrap to "while the lane drives the transport" was considered too, but that's a state of its own to track; selection already exists. With this rule a release has no effect on playback at all, and capture, replay and render agree by construction.

    It also changes `Space` and a ruler click on the lane: both play straight past the loop end. That's what trying spots wants. `⌥Space` is unaffected; the performance loops over its own length on its own clock (decision 4).

8. **Framing: `/` keeps the run** (agreed 2026-11-18). Capture-first, no record mode. While armed, the lane always remembers its presses, as the capture buffer remembers notes (`090`), and `/` frames the take after the fact.

    - **The key is `/`**, the capture verb everywhere else ("keep what I just played"). On the lane it does nothing today (`450`: capture returns `None` there, and while armed the keyboard never reaches the capture buffer). `Enter` stays free.
    - **Which presses count: the run.** From the press that started the transport, or the first press after the last `/` if that came later, up to the capture. Nothing is detected or guessed: a long noodle is captured whole and trimmed in the piano roll, a visible, undoable edit, as a marquee frames without cropping (`470`). Phrase detection on jumps (a much simpler cousin of `040`) was considered and left out: it's guesswork, and § Principle says the capture doesn't post-process.
    - **Where it ends**: at the `/` press while running, at the stop while stopped. A stopped `/` is "keep the run I just stopped". Exactly what was heard either way; the last note lasts until then (decision 1).
    - **The time is musical time elapsed while running**: a count of advancing transport ticks, on the `"sequencer"` thread. Not the transport position (it jumps) nor `clock_tick` (realigned onto playback's phase at every jump, `150`). This is the same counter decision 4's replay runs on, so capture and replay share the one new mechanism.
    - A `/` with nothing in the run does nothing. A capture goes to the selected pad, replacing what's there (decision 6), one undo step.

## The workflow, end to end

1. Play on the armed lane (the loop doesn't wrap there, decision 7); `/` keeps the run (decision 8).
2. The selected pad lights up (replacing what it held, decision 6); the piano roll shows the performance.
3. `⌥Space` to hear it; move notes, quantize.
4. `←`/`→` to a spot, `Space` to hear the arrangement there; `⌥Space` for the performance; repeat.
5. Click its pad and `⌘C` copies it; `Esc` gives the keyboard back to the arranger; `⇧⌘V` inserts the render at the cursor (`⌘V` to overwrite); play from just before it to hear the join; `⌘Z`, move, paste again if it doesn't sit. For a variation, `⌘V` onto another pad first and edit the copy.

## Phases

1. **Capture and pads (built 2026-11-18).** Decision 7, the run and its clock (decision 8), the pads (decision 6) and their column (decision 3) without the piano roll:
    - `Transport` reads the lane's armed flag and doesn't wrap while it is set; the press no longer suspends the wrap and the release no longer resumes it (a `NoteOff` on the lane does nothing now). The arranger draws the region dimmed meanwhile (`Display::is_loop_enabled`).
    - `PerformanceLane` holds the run (`models/performance_lane.rs`): a press with no run, or one that starts the transport, begins one; each is stamped with the run's elapsed ticks, counted in `play_tick` before the transport advances. The clock counts only while playback sits where the presses left it (`expected_tick`): anything else that moves playback (a ruler click, `Space` from the cursor, a stop and restart) ends the run, as disarming and a new project do. Two presses on one tick keep the last.
    - `/` on the armed lane (the `Commit` arm → `capture_performance_workflow`): the run as a performance clip (`performance_clip`: a note per press, its window `0..` the run's end, a zero-length note dropped) on the selected pad, `SetPerformancePadEdit`, one undo step, replacing what it held; the footer says `Captured 6 bars to pad 2`. With nothing in the run (none pressed, or playback moved since and ended it) nothing is kept and the footer says `Nothing played on the lane to keep`. `Sequencer::playback_is_looping` is false while armed too, as the transport doesn't wrap. Not a view-restoring step (`SequencerEdit::restores_view`): the pads aren't the arrangement.
    - The pads: `PerformancePads` on the sequencer, `selected_pad` beside it (pad 1 on load, not saved, not undoable), `SequencerCommand::SelectPerformancePad` and `ClearPerformancePad` (`Delete`, `SetPerformancePadEdit` to none), the pads mirrored to the view by `UiEvent::PerformancePads` (each pad's length, on every pad change and every load). The selected pad is set by the view on the click and told to the sequencer, never sent back, so quick clicks don't flicker; both go back to pad 1 on a load. Leaving the lane gives the column's keyboard back to the pane, so coming back to the lane never finds it there.
    - The column (`view/display/performance_pads.rs`, drawn in `rendering/knob_panel.rs`): with the lane selected `knob_rects` is `None` and `pad_rects` the knobs' cells; a pad is a rounded square the dial's size, filled in `accent` when lit, a `fg_dim` outline when empty (`fg` under the pointer); the selected one in the knobs' corner brackets, always drawn, since one pad always is. Hover: `Pad 6 · 6 bars` / `Pad 6 · empty` in the footer (`length_label`, whole bars and beats). A click selects (at once in the view, then the sequencer) and focuses the column (`KeyFocus::Pads`, the focus edge across its top): `Delete` empties the pad, `Esc` leaves, a pane command hands back (`Action::leaves_knob_panel`), `/` and the transport act from it.
    - Saved as `ProjectData::performance_pads`, a sparse list of `{ pad, clip }` (`PerformancePadData`, the clip a `ClipData` with its events), absent when empty; a template keeps none. In the fingerprint.
2. **The piano roll (built 2026-11-18).** The selected pad's performance in the docked panel (decision 2):
    - **The open clip.** `Sequencer::selected_clip_home` is the one rule for the open clip: `(ClipHome::Lane, id)` of the selected pad's performance while the lane is armed (`None` on an empty pad), else the lead clip on its track; `selected_clip` (and `_mut`) look it up (a take session aside), so the piano roll, the clip cursor, the note selection and every note edit act on it; the lead clip (`selected_clip_id`) stays a track's, so the undo snapshot and the arranger are untouched.
    - **`ClipHome`** (`edit/mod.rs`, § Open questions 5): `Track(idx)` or `Lane`, resolved in `clip_on`/`clip_on_mut`/`edit_clip_events` (a track index converts, so the track callers read as before); a lane clip is found by id on whichever pad holds it (`PerformancePads::find`). `ClipEventsEditState` and `EditResult::EventsModified` carry a `home` instead of a track index. `edit_clip_events` releases nothing for a lane clip: no track plays it.
    - **Undo.** An event edit on a performance isn't view-restoring, decided when it is recorded from its result (`EditResult::is_on_lane` → `UndoStep::leave_view`, so `restores_view` stays an exhaustive match of kinds), as `SetPerformancePad` isn't: undone from a track, it changes the pad and leaves the arranger's cursor, marquee and selection alone.
    - **What the panel shows follows the lane.** Selecting the lane, another pad (`select_performance_pad_workflow`), or an edit of the shown pad (`PerformancePadChanged { pad }`: a capture, `Delete`, their undo) sends `LeadClipChanged` with the performance, framed fresh; leaving the lane deselects its notes while it is still the open clip and sends an empty one before the track's lead clip. An empty pad reads `Pad 2 · empty` in place of the piano roll. The arranger cursor is never mirrored into a performance's cursor (`sync_selected_clip_cursor_with_arranger`).
    - **The view.** The lower lane (velocity, wheels, knobs) has no height under a performance (`lower_lane_h`, `shows_performance`), its gutter switch and hit-tests with it, so the keys get the room. Rows are the piano roll's own, named as on the lane's strip (both `octave_of`: C3 is note 60, bar 1); the "· bar 2" suffix was left out for the narrow legend column. Notes take the accent colour (no track). No playhead yet: the transport's doesn't map into a performance, and phase 3's clock brings its own.
    - **The note clipboard** remembers whether its notes came from a performance; only such a copy pastes into one (`Sequencer::note_clipboard`). A performance's notes still paste into a track's clip.
    - **Not yet: the window.** `[`/`]` and the window band need a track (`ResizeClipEdit`), so on a performance `[`/`]` do nothing and the band can't be grabbed (`window_edge_at`). Trimming a long run is deleting notes for now; moving a performance's window end (its length) would need `ResizeClipEdit` on `ClipHome` and the pads' lengths re-sent.
3. **`⌥Space`**: the replay through the live trigger path on the run's clock, looping over the performance's length (decision 4).
4. **The clipboard**: a pad's `⌘C` (the performance and its render), `⌘V`/`⇧⌘V` in the arranger, `⌘V` pad to pad (decision 5).

## Paste Time (built)

Placing a render with `⇧⌘V` needs a paste that inserts. Stev had none (`⌘V` overwrote, `⇧⌘D` inserted only straight after its source), so Paste Time was built first, on `main` since 2026-11-18 (`6189688`; `010`, `020` § Arranger Time Selection). Nothing of it is specific to the lane; what decision 5 relies on:

- **`⇧⌘V` is `⌘V` inserting.** The same clipboard, anchor and tracks as `⌘V` (the selected track for a scoped copy, the source tracks for an all-track one), after an Insert Silence at the cursor on every track. One undo step: `DuplicateTimeEdit::pasting`, Duplicate Time's edit with the clipboard at the cursor.
- **It inserts the copied span, not the pieces' extent.** `ClipClipboard::span` holds the copied range's length, so a copy ending in silence keeps it. A render sets its span to the performance's length, so a last segment that plays nothing still takes its room.
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

**Deferred (2026-11-18).** Its own design, not needed for capture: a capture with Off keeps the groove (below), and a gridded result comes from quantizing the performance clip by hand (§ Principle). Useful while just performing and testable by ear alone, so it can come later.

- **Two settings, two questions.** The key length (built) is where a key jumps *to*; launch quantize is *when*: Off (on the press), Beat, ½ bar where the meter allows, Bar. They pair naturally (Bar keys at Bar launch is classic scene launching; Beat keys with Off is stutter playing), but "jump to bar 5, on the next beat" is valid too, so they stay separate. Saved with the project, not an undo step, as the key length.
- **A scheduled press**: jumps at the next boundary, done by the sequencer on its own tick, so exact to the tick, tighter than a live press. The latest press wins. A release before the boundary probably doesn't cancel (the press was the decision, as in Ableton); to check by hand. A press while stopped starts at once. **A pending key must show**, or a press waiting half a bar feels broken: an outline or dimmer accent on its piece of the strip until it lands.
- **What Off implies.** Targets are always on the grid (`key_tick`), moments aren't. Press 0.3 of a beat late into bar 5 and the source then plays 0.3 beats against the output: every bar line after it sits off the grid it lands on. That is what was played; a capture with Off has segments at off-grid ticks (clip windows hold any tick) and its downbeats won't line up after a commit. That *is* the groove, and keeping it is the point. With Beat or Bar everything is on the grid by construction, nothing corrected.
- **Not now**: an Off that jumps to the target *plus* the press's offset within the beat (cutting in early or late, the grid kept). It changes what's heard, so it would be a playing mode, not a capture fix; only if Off's drift turns out to be a problem in use.

## Open questions

1. **Framing a capture.** Settled by decision 8 (2026-11-18): `/`, the run, ending at the `/` press or the stop. Considered first: `Enter`, a phrase-start rule on jump data, the last N bars, a marquee as in `470`, an end at the last press plus one key length.
2. **Insert or overwrite at the cursor.** Settled by decision 5 (2026-11-18): both, through the paste keys, `⌘V` overwriting and `⇧⌘V` inserting. Kept here for the trail: an `Enter`/`⇧Enter` commit pair was considered first, then dropped for the clipboard once Paste Time (§ Paste Time) was seen to be missing from Stev on its own account; it has since been built.
3. **Does the render sound like the replay?** Yes, already (checked 2026-11-18). A lane jump chases notes in the clips the target falls strictly inside (`Sequencer::chase_notes`); a trimmed-window piece landed on at its start chases its hidden `NoteOn`s itself (`Track::tick`'s arrival chase, `pending_note_ons`), and `chase_notes` leaves a clip's exact start to it on purpose. A segment's end matches too: live, the next jump re-anchors and flushes what sounds; rendered, the window end releases it (`pending_note_offs`). The render cuts per source clip: a segment crossing a clip boundary is several pieces on that track, a gap or past the arrangement's end is silence. The loop was the one way they could differ, and decision 7 removes it.
4. **Wheels and parameters at a segment's start.** Already latched (checked 2026-11-18): the same arrival chase runs `chase_controllers(clip.controllers_at(tick))`, the wheels and the knobs' parameters, as a jump does through `Track::seek`. Plain CCs (sustain) are chased in neither, so render and replay still agree.
5. **The lane owning a clip without being a track.** Settled by phase 2 (2026-11-18): a clip's home, `ClipHome::Track(idx)` or `Lane`, resolved in `clip_on`/`clip_on_mut`/`edit_clip_events`, so the note edits, quantize and undo stay as they are. `edit_clip_events`'s release of sounding notes doesn't apply: no track plays the lane's clip, the replay will feed it to the trigger, so an edit is heard on the next pass.
6. **Decision 6's details.** Settled by the fixed grid (2026-11-18): no names or ordering, eight pads in the knobs' cells, so no scrolling and a limit of eight; dragging a pad out is left for later. Considered first: a list of tiles with names, an order and a scroll.
7. **Showing that a performance exists**: answered by the lit pads (decision 3); nothing on the lane row.

## Not in this design

- **The path on the timeline** (song mode, scenes): § What a performance is.
- **Post-processing a capture's timing**: § Principle.
- **A pending, uncommitted capture**: decision 1.
- **A preview of the performance in place before pasting**: decision 5.
- **A commit key of its own** (`Enter`, `Enter`/`⇧Enter`): decision 5.
- **An implicit copy from the lane, or arrangement clips from the piano roll's copy**: decision 5.
- **Dragging, launching or chaining pads**: decision 6.
- **Step 2 of `510`, one key per idea.** Less urgent with capture, since the pasted result is plain clips whatever the mapping; but a performance clip reading "E3, E3, G3, C4" is easier to follow than "bar 5, bar 5, bar 9, bar 13" once ideas sit on keys.
