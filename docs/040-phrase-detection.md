# Phrase Detection

How the stopped `/` finds the phrase to commit. It frames the capture buffer by phrase detection, then either places a new clip (`Sequencer::build_stopped_capture_clip`, recorded by `CommitClipEdit`) or inserts the notes into the lead clip (`Sequencer::build_stopped_capture_insert`, recorded by `InsertCaptureEdit`). Both builders are pure and live in `src/core/sequencer/stopped_capture.rs`. The detection itself is in `region/phrase_tokens.rs` and `region/window.rs`.

The detection rules the stopped `/` uses (`220-capture-without-pending-view.md`). **Known gap:** once the stopped `/` has committed, a phrase detected too long or too short can only be corrected with the clip's edges (`[`/`]`, mouse). For an insert the capture is gone once committed, so the phrase can't be re-framed at all. Adjusting the detected phrase in the clip view is to be scoped in its own design doc (see `220`).

## The capture source

- **Bounded at the start**: `detection_capture_source()` copies the capture buffer into a fresh clip (its own region and cursor, so the builders can frame it in place), sorts it, pairs its notes and trims it to the last `CAPTURE_BUFFER_BARS` (8) before any window or token is computed, so a long session can't leak old material into detection. Event ticks stay absolute.
- **Framed by its notes only.** The buffer also holds wheel moves (pitch bend, the mod wheel — `090`), but they never frame a take: the trim measures back from the **last note edge** (a bend easing back after the last note-off neither stretches the source nor trims its notes away), and a buffer without a note commits nothing (`Clip::has_notes`). The wheel moves inside the window come along.
- The new clip keeps that whole source as material outside its window. The insert keeps only the phrase.

## Phrase start

`region/phrase_tokens.rs` owns `phrase_token_starts()` and the boundary scorer, over the last `PHRASE_DETECTION_WINDOW_BARS` (4) of the source.

- It identifies candidate phrase boundaries from **positive silence gaps between note pairs** (`previous NoteOff` / `NoteOn.end_tick()` to next `NoteOn`), not raw NoteOn-to-NoteOn distance, so a long single note pair does not become its own phrase token just because its onset happened early.
- Each boundary candidate is scored from silence prominence, metric placement (bar/half-bar/beat proximity with tolerance), and plausible phrase span.
- Clear 1.5-beat silence can stand on its own, while subtler gaps need musical support from metric or span cues.
- The **silence prominence baseline** must be computed from positive note-pair silences only; legato/overlapping pairs produce zero silence and must not collapse the median baseline.
- The token pass **backtracks the first visible token** to its true first note when the last-N-bars window cuts into the middle of that token.
- When the final detected token spans at least `PHRASE_LAST_TOKEN_REFINE_MIN_BARS`, it gets one extra scoring pass so an obviously merged last phrase can split once more without making earlier tokenization more aggressive.
- `snap_region_start_to_note_on()` keeps single-token windows anchored at their first note, ignores a trailing single-note downbeat marker, and rejects a too-short late token (minimum one bar plus one beat) unless it is still the best eligible choice around the raw start.
- **Pinned to real takes**: every take in `fixtures/captures/` must still start where the user picked (`the_detection_picks_every_fixture_start`). A fixture records the project's meter (`meter`, `[numerator, denominator]`, absent = 4/4) and replays in it. Every checked-in take is 4/4, so detection in other meters is covered by synthetic tests only until a real 3/4 or 6/8 take is pinned (`archive/270-time-signature.md`). A replacement "gap rule" was tried against these and dropped (`220` § "The gap-rule change, tried and dropped").

## Phrase end

- **A new clip** (`build_detected_phrase_clip()` → `detected_phrase_window()`, like the insert's window a call to `snapped_window_from_last`): the raw window is one `Sequencer::loop_reference_length()` (the loop-region length, floored to `REGION_LENGTH_DEFAULT`) back from the last `NoteOn`, its start snapped as above. The end is the last `NoteOff` + 2 beats (`phrase_end_from_last_note_off_with_tail`), falling back to the raw end only when there is no `NoteOff`, and the window is clamped to at least one bar (`clamp_phrase_end_to_min_window`).
- **Bars are bars of the project's meter** throughout: the detection window, the token lengths, the bar/half-bar line and preferred-span scores, the one-bar minimum window, the whole-bar rounding and Enter's fit (`Meter`, `archive/270-time-signature.md`). The beat in the metric score stays a quarter note.
- **The project's first clip keeps that window exactly**, and the tempo is left alone until Enter fits it (below). **Every later clip** is rounded to the nearest whole bars, floored to the room before the next clip: a fractional-bar loop desyncs the clock/metronome grid once it is looped, because both wrap by `region_length` (`150-clock-position-sync.md`). The window is then rebased so it starts on a bar line in event space (`Clip::align_window_start_to_bar`), and swing is detected.
- **An insert** (`detected_insert_window()`): the window ends at the last `NoteOff`, is as long as the played span but at most one bar, and its start is snapped the same way (which may move it earlier to a phrase start). It is cropped once, to that window or the room left before the clip's region end if that is shorter (a note running past it is closed there), and inserted at the clip cursor (`build_stopped_capture_insert`). No whole-bar rounding and no swing detection: the notes land inside an existing clip, which keeps its own `swing_pct`.
- Both are pinned to what the retired pending view's unedited confirm produced: `stopped_commit_window_is_the_retired_confirms`, `stopped_first_clip_is_exact_and_enter_fits_it`, `stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`.

## Fitting the tempo (Enter)

- **`RetimeClipEdit::fit_sole_clip`** (Enter, only while the project has exactly one clip, `220`) calls `fit_first_clip_tempo` (`region/tempo.rs`), which fits the clip to the **nearest whole bar of what was played** — `((played + bar/2) / bar).max(1) * bar` — and makes that the project tempo via `Clip::adjust_to_tempo`. The target is the played length, *not* the loop region (which needn't match the phrase), so the tempo lands near the player's natural speed regardless of loop size.
- **`octave_correct_clip_tempo`** runs straight after: a fitted tempo `>= config::FIRST_CLIP_TEMPO_US_SLOW` (~50 BPM) is reinterpreted an octave up — `adjust_to_tempo` with `tempo/2` doubles the BPM and the clip's bar count; `<= FIRST_CLIP_TEMPO_US_FAST` (~140 BPM) goes an octave down (`tempo*2`, half the bars). 40 BPM over 1 bar was almost always 80 over 2; 160 over 4 was 80 over 2. The clip keeps its wall-clock timing — only the metric reading changes; it's the automatic form of `⌥=`/`⌥-`. **Single step**: anything still out of band is left for the user to rescale by hand.
- Undoing the fit restores the clip, its events and the tempo together (`050-undo-redo.md`).
