# Transposing Clips from the Arranger

**Status: design agreed 2026-11-16; built 2026-11-16 (issue #36).** With an arranger marquee that covers whole clips, `⌥↑`/`⌥↓` move every note in those clips up or down a semitone, across all the marquee's tracks at once; `⇧⌥↑`/`⇧⌥↓` move them an octave. A marquee that cuts through a clip is refused. One undo step per press. Nothing detects a key: you press, listen in the loop, and press again until the idea sits with the others.

## Background

A workflow that suits Stev: no song structure, just short ideas of 4–8 bars one after another, played back with the performance lane (`110`). The ideas drift apart, mostly because they aren't in the same key.

The first plan was key detection: count the 12 pitch classes weighted by duration, match them against the 24 major and minor key profiles (Krumhansl–Schmuckler), and fit a section to another's key. It was tried as a proof of concept through the MCP server (`360`), with a deterministic script doing the detection and Claude only moving the data (2026-11-15, the `stev-key-transpose` skill). The detected keys looked right and the transposed ideas fitted on the first try. But all the result ever did was shift every note in the section by one fixed number of semitones. The detection only picked *how many*, and that is a choice the ears make in a few presses, with the decision staying with the user rather than a model (`280` § Principle: capture-first). So the detection is dropped and the shift is built on its own.

The shift already exists in the clip view: `⌘A` then `↑`/`↓` moves every note of the open clip a semitone, `⇧` an octave (`SequencerCommand::TransposeSelectedEvents`, `TransposeSelectedEventsEdit`, `Clip::transpose_selected_events` → `Clip::transpose_note_pair`, which moves a `NoteOn` and its paired `NoteOff` together). What's missing is reach: an idea is a few clips stacked over the same bars, one per track, and fitting it means opening each clip in turn and pressing the same keys the same number of times, with the idea out of tune with itself in between.

### Why whole clips

A transpose of *the marqueed content* was drafted first (2026-11-16) and dropped. A marquee cuts through clips and notes, so it needed rules for a note held in over its start edge, a note ringing past its end, notes hidden outside a clip's window, and a held note across a `⌘E` cut, chased into the right half from that half's own copy of the events. Every rule was defensible and none was obvious. Note-level choices belong in the clip view, where the selection is exactly the notes chosen. The arranger's job is the coarse one: these clips, all their notes, exactly what `⌘A` `↑` does in each, done together. Requiring whole clips removes the edge cases instead of ruling on each.

## Decisions

1. **The keys are `⌥↑`/`⌥↓`, `⇧` for an octave.** In the arranger with a marquee that has a tick range (`TimeSelectionRect::has_tick_range`), `⌥↑` moves its clips' notes up a semitone and `⌥↓` down; with `⇧` held, an octave, as `⇧` is the octave in the clip view. Without a tick range (none, zero-width, track-only) the chord does nothing: today `⌥↑`/`⌥↓` fall through `handle_prev_next_keys` as a plain `↑`/`↓` (the track step, the modifier ignored), and they stop doing that. In the clip view `⌥↑` already transposes the selected notes (the modifier is ignored there too), so the chord means "transpose" in both views. `⌘↑`/`⌘↓` were the other candidate, as the vertical twin of the `⌘←`/`⌘→` block move, but that move is a ghost committed on releasing `⌘` and swallows every other key while it lives; a transpose is an edit per press, so it stays off `⌘`. The arrows are fixed keys, never in the keymap (`keymap::refusal`), so the chord is matched in code and gets a row in `FIXED` (`keyboard_tab.rs`) and `010`.

2. **The marquee must cover whole clips.** The clips that move are those on the marquee's tracks lying fully inside its tick range, the set `M` already finds for its interior pieces (`interior_clip_ids`, `Track::find_clip_ids_within`). If any clip on those tracks overlaps the range without lying fully inside it, the press is refused whole and nothing moves. A clip touching the range only at an edge (its end on `m.start`, its start on `m.end`) doesn't overlap it and is neither moved nor a reason to refuse. The everyday way to get an exact marquee is the clip-edge jumps: cursor on the idea's first clip start, `⇧⌥→` to its end (`010` § Selection keys), `⇧↓` over the tracks.

3. **Every note in each clip moves, whole.** Exactly what `⌘A` then `↑` does in the clip view: every `NoteOn` and its paired `NoteOff`, including notes hidden outside the clip's window (dragging an edge out later reveals them in the same key) and muted notes. Muted clips move too: they're part of the idea and would be out of key when unmuted. Drum tracks get no flag: leave them out of the marquee, and a mistake is heard at once and undone. Pitch bend, the mod wheel and parameter moves (`400`) are left as they are; a bend is relative to the note, so it still fits. After a `⌘E` cut each half is a clip of its own: with both covered both move, with one covered only that one does, so a note held across the cut changes pitch there. That is what "these clips" means, and it is heard at once.

4. **One edit through the existing per-clip path.** A new `TransposeClipsEdit` (in `clip_edits/`, built `from_marquee`, the refusals its `Err`) finds the covered clips and, for each with notes, runs the move through `Sequencer::edit_clip_events` → `Track::edit_clip_events`, keeping that clip's event list from before (the `Arc` snapshot `ClipEventsEditState` keeps) for undo. The pitch change itself is a new `Clip::transpose_all_notes`, `Clip::transpose_note_pair` over every `NoteOn` (unit-tested in `models/clip`). The clip's event selection is left alone. No sounding-note handling is written: `Track::edit_clip_events` already queues a `NoteOff` for a note the edit stops sounding at the playhead (the #19 family, `050`), so a note moved while it sounds is released and heard at its new pitch from the next time it starts, exactly as a clip-view transpose behaves today.

5. **One undo step per press.** Each press is its own `SequencerEdit::TransposeClips` step, so undo walks back press by press, and it is a note edit in `440`'s sense: the step records the view state before and after, which is the same marquee, lead clip and cursor (the transpose changes none of them). The marquee stays after the press, so presses chain. Undo puts back each clip's snapshot; redo replays the edit on the clips it found the first time.

6. **A press that would take a note out of MIDI's range is refused whole.** If any note in the covered clips would go below 0 or above 127, nothing moves. Clamping each note, as the clip view does, would squash the intervals at the edge and change the idea; refusing keeps every interval exact. The clip view's clamp stays as it is.

7. **The footer says what happened; no running total.** In the usual fg/fg_dim (no alarm colour), with the bars in the project's meter (`agent::span_label`), as a marquee capture does (`470` decision 7):
    - a press: "Transposed bars 15–19 up a semitone", "… down an octave";
    - a clip cut through: "The marquee cuts through a clip";
    - out of range: "A note would leave the MIDI range";
    - no clip covered, or none with notes: "No clips in the marquee".

    Refusals record no undo step. A running total ("+3 so far") would need a session remembering where the shift began, stored mode-like state that `220`'s principles avoid; the undo history already holds it.

8. **No audition.** The clip view's transpose previews the first selected note when it moves (`Sequencer::preview_note`), on the selected track. An idea spans tracks, and a single note on one of them says little about whether it fits. Listening means playing the loop over the idea, which is the whole workflow; stopped, a press is silent.

9. **Unchanged.**
    - **The clip view's transpose**: `↑`/`↓` on the selected notes, as today; it stays the tool for anything finer than whole clips.
    - **The loop region, the loop flag, the playhead and the selection**: never touched (`220` principle 2).
    - **Persistence**: pitches only change, so no DTO changes (`060`).
    - **The audio export**: a run blocks every command it doesn't allow (`EventHandlers::audio_export_command`), this one included, with nothing to add.
    - **The MCP server** (`360`): no tool. An agent can already do this with `replace_clip_notes`, and the point is that the ears decide.
    - **The performance lane** (`450`): selecting it drops the marquee, so the chord does nothing there.

## Things to check while building

- **Where `⌥↑` is matched.** The marquee lives in the view, so `Display::forward_input_event` binds the chord in the arranger (`marquee_transpose_key`) and sends `InputEvent::TransposeClipsInSelection { rect, semitones }`, forwarded as `SequencerCommand::TransposeClips`, beside the `⇧↑`/`⇧↓` track-span arm there (which must still require `!alt`, or it would take `⇧⌥↑`). `handle_prev_next_keys`' Arranger arms then stop matching with `⌥` held.
- **The refusal's overlap test** must agree with `Track::find_clip_ids_in` (overlapping) and `find_clip_ids_within` (fully inside): refuse when the first finds a clip the second doesn't. Check the edge-touching clips on both sides.
- **Running, test through `run_command`** (the `muted-note-release` lesson): a note sounding at the playhead when the press lands is released and doesn't sound twice. As built, the walk still meets the moved `NoteOff`, a stray off at the new pitch where the note would have ended: the clip view's transpose and pitch drag do the same through the same path, so it is left as it is.
- **Redo** can trust the clips found on the first `edit()` (the undo stack is linear); freeze them there, as `MuteInRangeEdit` freezes its targets. As built they are found at construction, which the refusals need anyway.
- **Tests**: `Clip::transpose_all_notes` (hidden notes, muted notes, pairs kept); the edit and its undo across two tracks; the three refusals (a clip cut through, out of range, nothing covered) each leaving the project unchanged and recording no step; edge-touching clips neither moved nor refused; the chord in the arranger with and without a tick range, and `⇧⌥` for the octave.
- **Docs**: `010` (the chord, and that `⌥↑`/`⌥↓` no longer step tracks), `020` § Views where the marquee commands are listed, `050` (the new `SequencerEdit` variant), the `FIXED` row, `CHANGELOG.md`.

## Not in this design

- **Transposing part of a clip from the arranger**: refused by decision 2; the clip view does it.
- **A key readout.** Detection is cheap and the footer could name a marquee's key ("Bars 15–19: D minor, or F major"). It is a separate idea, kept in the notes; the shift doesn't need it.
- **Scale changes** (C major to C minor, Logic's Scale Quantize, Ableton 12's scale tools): they change the idea's notes, not just where they sit, and need a rule for notes outside the scale.
- **Snapping live input to a scale**: it changes what was played, against capture-first.

## Phases

One phase: `Clip::transpose_all_notes`, `TransposeClipsEdit`, the command and the chord, the footer lines, the tests and the docs above.
