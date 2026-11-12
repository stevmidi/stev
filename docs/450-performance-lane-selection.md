# The Performance Lane Is an Exclusive Selection

**Status: design agreed 2026-11-12, not built.** Selecting the performance lane deselects the track, as selecting a track already deselects the lane. While the lane is selected there is no selected track and no lead clip. The undo restore loses its special case for an armed lane.

## Background

`110` and the code comments call the lane "mutually exclusive with track selection", but only one direction is built. Selecting a track disarms the lane (`switch_track_workflow`, `selection.rs`). Arming the lane only sets a flag (`select_performance_lane_workflow`): the track stays selected in the sequencer, the lead clip stays, and the arranger draws both rows as selected (`arranger.rs`: `is_selected_track` and `performance_lane_selected` are independent).

So while the lane is armed, everything keyed on the selected track still acts on a track the user has, by their own click, left: `Delete` on its marquee, `⌘V`, `v`, the knob panel, the clip view.

It also costs the undo restore a special case (`440` decision 3, `reselect_surviving_clip` in `clip_range_edits.rs`). Putting a track back goes through the track selection, which disarms, so with the lane armed the restore skips the track, and then has to drop a lead clip the undo moved off the still-selected track. Two tests pin that (`an_undo_never_disarms_the_performance_lane`, `an_armed_undo_of_a_clip_move_drops_the_lead_it_took_away`, `sequencer_handler.rs`).

## Decisions

1. **One selected row.** The arranger has one selected row: a track or the performance lane. Arming the lane clears the track selection and with it the lead clip (`clear_clip_selection_workflow`, then `Sequencer::select_track(None)`); selecting a track disarms the lane, as today. Armed implies no selected track. The reverse does not hold, since a project can be without a selected track for a moment (a load), so the armed flag stays what it is, an `Arc<AtomicBool>` that `MidiInputForwarder` reads, and is not derived.
2. **The view's selected track becomes `Option<usize>`.** `UiEvent::PerformanceLaneSelected` sets it to `None`; `TrackSelected` sets it and clears `performance_lane_selected` as today. No "last track" is remembered on either side.
3. **The marquee goes when the lane is selected.** A marquee is anchored to the cursor and the selected track, and collapses when either moves (`sync_time_selection_to_cursor`). Selecting the lane is a track change like any other, so it collapses the marquee. With no anchor track, `⇧←/→/↑/↓` and a `⇧`-click start nothing while the lane is selected. A press on a track row selects that track first, as today, and so leaves the lane.
4. **An undo or redo that puts a track back leaves the lane.** The restore selects the snapshot's track through the ordinary track selection, which disarms. Decided 2026-11-12, reversing `440` decision 6 and `110` ("an undo never disarms it") for the track only. The reason: with the lane selected nothing shows which track or clip the step changed, and the alternative is the special case this design removes. Arming is still never restored: a step recorded while the lane was armed has no track in its snapshot, and undoing it leaves the selection as it is (`restore_view_workflow` already skips a missing track). The loop is untouched, as before.

   Rejected: keeping the lane armed with one guard in `restore_view_workflow` (armed: skip the track and the lead). It is what to fall back to if leaving the lane on `⌘Z` turns out wrong in use.
5. **`↑`/`↓` from the lane.** `cycle_selected_track_workflow` reads no selection as track 1 (`unwrap_or(0)`), so `↓` from the lane would land on track 2. From the lane, `↓` selects the first track and `↑` the last. The lane itself stays reachable only by a click; putting it in the `↑`/`↓` cycle is not part of this.
6. **The cursor accent sits on the lane row** while the lane is selected (decided 2026-11-12): the cursor still moves with `←`/`→` and marks where playback starts.
7. **`⌥←`/`⌥→` do nothing with the lane selected** (decided 2026-11-12): the jump to the next clip edge reads the selected track's clips, and there is none.
8. **A scoped `⌘V` does nothing with the lane selected** (decided 2026-11-12). Today a track-anchored clipboard pasted with no selected track falls back to the tracks it was copied from (`ClipClipboard::target_track`), a track the user didn't pick. `PasteClipsEdit::from_sequencer` returns `None` instead when the clipboard is track-anchored and no track is selected. An all-track copy never used the selected track and pastes as before; the Duplicates pass no target track and are unaffected. The rule this and decisions 3 and 7 share: a command aimed at the selected track does nothing while the lane is selected.

## Audit: what reads the selected track

The sequencer side already takes `Option`: `selected_track_index()` and `selected_track()` return one and nearly every reader leaves with `?`. The view side holds a bare `usize` (`Display::selected_track_idx`) and is where the work is.

### Sequencer (`src/core/`)

| Reader | With the lane selected |
|---|---|
| Lead clip (`lead_clip`, `current_clip_view`, `selection.rs`, `view.rs`) | None. The clip view shows "No clip at the cursor". |
| Capture: `/` running and stopped, the take session (`capture.rs`, `stopped_capture.rs`, `take_framing.rs`, `commit_clip.rs`) | Returns `None`, nothing happens. Nothing was captured either: while armed the keyboard goes to `handle_performance_lane_midi_input`, not the capture buffer (`sequencer_pump.rs`). |
| Clip edits on the lead or selected track (`resize_clip.rs`, `retime_clip.rs`, `split.rs` at the cursor, `event_edits.rs`, `export.rs`'s SMF export, `region/mod.rs`) | Return `None`, nothing happens. |
| Marquee edits (`delete_in_range.rs`, `merge_clips.rs`, `duplicate_time.rs`) | Take their tracks from the rect; the selected track is only used to keep the lead, or as a fallback span. No marquee exists with the lane selected (decision 3). |
| Paste (`paste.rs`) | Checked 2026-11-12: with no selected track every piece lands at the cursor on the track it was copied from (`ClipClipboard::target_track`, pinned by `paste_does_not_need_a_selected_track`), for a scoped copy (`⌘C`) as for an all-track one. A scoped `⌘V` does nothing instead (decision 8); the all-track paste is unchanged. |
| Note preview and audition (`playback.rs`) | No channel, no sound. |
| Live input channel (`arm_selected_track`) | Left on the last track's channel. Harmless: every audible route is suppressed while armed (`110` § Arming). |
| Knobs (`knobs.rs`) | No track, no parameter moves. |
| Track add (`track_edits.rs`) | Inserts after the selected track, else at the end. The added track becomes selected, which leaves the lane. |
| Track remove (`track_edits.rs`, `tracks.rs`) | By index from the header; `was_selected` is false. |
| Loading a project (`project.rs`) | Selects a track; a new project already disarms (`state.rs`). |
| Agent bridge (`agent/reads.rs`) | `get_context` reports no selected track. Its writes name their tracks. |
| `ViewSnapshot::of` (`undo_step.rs`) | `track: None`, `lead_clip: None`. |

### View (`src/view/display/`)

| Reader | With the lane selected |
|---|---|
| Lane highlight (`rendering/arranger.rs`) | No track row is drawn selected. |
| Cursor accent and marquee fallback span (`rendering/overlays.rs`, `draw_selected_track_cursor`) | The accent on the lane row (decision 6). |
| Marquee anchors (`input/gestures.rs`: `extend_time_selection_to_click`, `set_nudged_tick_selection`, `nudge_time_selection_track`, `next_clip_edge_on_selected_track`) | No anchor: return early (decisions 3 and 7). |
| Collapse watcher (`state/scroll.rs`, `gesture_state.rs`: `last_selected_track_idx`) | Compares `Option`s; a change to or from `None` collapses. |
| Track add/remove shift (`state/ui_events.rs`) | A remove with nothing selected keeps `None`. |
| Lead clip colour (`mod.rs`: `lead_clip_color`) | No lead, so no caller; falls back to the theme accent. |
| Knob panel (`knob_panel.rs`, `rendering/knob_panel.rs`) | Already handles no track (`selected_track_idx < tracks.len()`); draws empty in the theme accent. |
| Plugin editor `v`, the status chip (`input/mod.rs`, `instrument.rs`) | Nothing. |
| Rename track (`input/mod.rs`) | Nothing. |
| Browser: a plugin onto the selected track with Enter (`browser.rs`) | Nothing. A drag onto a track row still works and selects it (`plugin_drag.rs`). |
| Reveal the selected track on a scroll (`rendering/mod.rs`) | Nothing to reveal. |

## What goes away

- The armed branch in `reselect_surviving_clip` and its doc paragraph; the function is back to "this track, its lead if it survived".
- `an_undo_never_disarms_the_performance_lane` and `an_armed_undo_of_a_clip_move_drops_the_lead_it_took_away`, replaced by one test of decision 4.
- The "never disarmed" clauses in `restore_view_workflow`'s doc, `050` § Undo and the view, `110` § Arming and `440` decisions 3 and 6 (`440` gets a pointer here, not a rewrite).

## Things to check while building

- **Every `selected_track_idx` use in the view** compiles against `Option` with no `unwrap_or(0)`: a silent fallback to track 1 is the bug this design removes.
- **The clip view with the keyboard** when the lane is clicked: the focus stays (only the user moves it, `020` § Views), on "No clip at the cursor".
- **The take session**: arming during one. The lane row is in the arranger, so a click there moves the keyboard first; check what the session does when its track is deselected, or refuse the arm while it is open.
- **An audio export run** (`130`): the lane click is already blocked or not; no change expected.
- **Tests**: arming clears the track and the lead and collapses the marquee; a track click after it selects the clip under the cursor; `↓`/`↑` from the lane; a scoped `⌘V` with the lane selected pastes nothing and an all-track one still does (`paste_does_not_need_a_selected_track` is rewritten for the all-track copy); an undo that puts a track back leaves the lane; an undo of a step recorded while armed changes no selection; `get_context` with the lane selected.
- **Docs**: `110` § Arming, `020` (track selection, the marquee's anchor), `050`, `010` if `↑`/`↓` is listed there, and a `CHANGELOG.md` line (a user would notice).

## Phases

One phase. The sequencer half is small (decisions 1, 4, 5); most of the diff is the view's `Option<usize>`.
