//! `handle_input_event` — the `match` over every
//! [`InputEvent`] the view forwards, and
//! the per-binding `handle_*` helpers it calls.
//!
//! Most arms match a raw `KeyPressed` / mouse variant and branch on
//! `view_state` + [`ClipContext`] here; a few match a purpose-built variant the
//! view pre-resolved (see `input_event.rs`). Keyboard is the primary surface —
//! `010-keybindings.md` catalogues the bindings, `020-views-and-state.md`
//! the transitions.

use egui::Key;

use crate::core::sequencer::TempoGesture;
use crate::core::{
    input_event::{InputEvent, KeyModifiers, is_pane_focus_key},
    settings::{clamp_midi_out_offset_ms, update_settings},
};
use crate::models::clip::ClipEdge;

use super::*;

/// Which clip-family context a key press lands in — used to pick the right
/// behaviour for a shared binding. The two `Clip*` contexts are one view
/// (`ViewState::Clip`) split by whether any notes are selected
/// (`SharedAtomics::has_event_selection`): the arrows, `Delete`, `M` and `⌘/Ctrl+D`
/// act on the selection when there is one, and move the cursor (or do
/// nothing) when there isn't. `Q` is the exception: it quantizes the whole
/// clip when nothing is selected.
enum ClipContext {
    /// The arrangement timeline.
    Arranger,
    /// A clip's piano roll with nothing selected: navigation.
    Clip,
    /// A clip's piano roll with notes selected: editing them.
    ClipWithSelection,
}

/// Position-nudge step for Cmd/Ctrl+Left/Right on selected notes — much finer
/// than [`GRID_NUDGE_TICKS`]. Kept as a standalone constant so it can be tuned
/// by feel independent of any grid resolution.
const FINE_NUDGE_TICKS: i32 = 10;

/// The grid step for Left/Right (position) and Shift+Left/Right (length) on
/// selected notes: a 64th.
const GRID_NUDGE_TICKS: i32 = sixteenth_straight_ticks() / 4;

impl EventHandlers {
    // --- Event handlers ---
    /// Dispatches every `InputEvent` the view forwards. Most arms match a
    /// raw `KeyPressed`/mouse variant directly and branch on `view_state`/
    /// `clip_context` here; a handful of arms instead match a purpose-built
    /// variant (e.g. `SetRegionToTimeSelectionOrToggleLoop`,
    /// `RemoveClipsInSelection`) that `Display::forward_input_event`
    /// pre-resolved because the operand — a view-local time selection or drag
    /// anchor — isn't available here. That's the only reason a key gets
    /// intercepted upstream instead of arriving raw; see the doc comment on
    /// `forward_input_event` before adding a new binding.
    pub(crate) fn handle_input_event(&self, event: &InputEvent) {
        match event {
            InputEvent::KeyPressed {
                key: Key::ArrowLeft,
                modifiers,
            } => self.handle_left_right_keys(-1, modifiers),

            InputEvent::KeyPressed {
                key: Key::ArrowRight,
                modifiers,
            } => self.handle_left_right_keys(1, modifiers),

            InputEvent::KeyPressed {
                key: Key::ArrowUp,
                modifiers,
            } => self.handle_prev_next_keys(-1, modifiers),

            InputEvent::KeyPressed {
                key: Key::ArrowDown,
                modifiers,
            } => self.handle_prev_next_keys(1, modifiers),

            InputEvent::KeyPressed {
                key: Key::Plus | Key::Equals,
                modifiers,
            } => self.handle_plus_minus_keys(1, modifiers),

            InputEvent::KeyPressed {
                key: Key::Minus,
                modifiers,
            } => self.handle_plus_minus_keys(-1, modifiers),

            // `0` (main row or numpad — egui-winit maps both to `Key::Num0`):
            // Cubase-style transport "Stop", paired with `⌥Space`'s "Start"
            // below.
            InputEvent::KeyPressed {
                key: Key::Num0,
                modifiers: _,
            } => self.send_transport(TransportCommand::Stop),

            // `⌥Space`: Cubase-style transport "Start" — an unconditional
            // restart, running or not. In the clip view it starts from the
            // clip cursor (the arranger cursor stays put). In `Arranger` it
            // falls through to the bare `Space` toggle below. Moved off bare
            // `Enter`, now spare.
            InputEvent::KeyPressed {
                key: Key::Space,
                modifiers,
            } if modifiers.alt
                && matches!(
                    self.clip_context(),
                    ClipContext::Clip | ClipContext::ClipWithSelection
                ) =>
            {
                self.send_sequencer(SequencerCommand::PlayFromClipCursor);
            }

            InputEvent::KeyPressed {
                key: Key::Space,
                modifiers: _,
            } => self.send_transport(TransportCommand::TogglePlayback),

            // Bare `R` (⌘/Ctrl+R is rename track, bound in `Display`).
            InputEvent::KeyPressed {
                key: Key::R,
                modifiers,
            } if !modifiers.command => self.handle_toggle_live_recording(),

            // `\` or `/` (main row or numpad — egui-winit maps both to
            // `Key::Slash`, and nothing else claims either). `\` is the one
            // shown: a single press beside `[` `]` on US ANSI and Norwegian (Windows)
            // layouts. `/` stays for the numpad and for layouts where `\`
            // takes a chord (Swedish, Danish and German among them).
            InputEvent::KeyPressed {
                key: Key::Backslash | Key::Slash,
                modifiers: _,
            } => self.send_sequencer(SequencerCommand::Commit),

            InputEvent::KeyPressed {
                key: Key::M,
                modifiers: _,
            } => self.handle_mute(),

            // `Q` quantizes in `Clip` only: the selected notes, or the whole
            // clip window with none selected. No Arranger `Q` — see
            // `070-quantization.md`.
            InputEvent::KeyPressed {
                key: Key::Q,
                modifiers,
            } if !modifiers.command
                && matches!(
                    self.clip_context(),
                    ClipContext::Clip | ClipContext::ClipWithSelection
                ) =>
            {
                self.send_sequencer(SequencerCommand::QuantizeEvents);
            }

            // ⌘/Ctrl+A — Select All. The Arranger form (marquee every clip)
            // never arrives here: `Display::forward_input_event` consumes it
            // because its operand is view-local.
            InputEvent::KeyPressed {
                key: Key::A,
                modifiers,
            } if modifiers.command && !modifiers.shift && !modifiers.alt => {
                self.handle_select_all();
            }

            // Note: ⇧F5/⇧F6 (theme cycling) are intentionally absent here. They are
            // consumed view-locally by `Display::handle_theme_input_event` before
            // input reaches this handler — the theme is a `static` in `view::theme`
            // plus view-local shape colors, so no core state is involved.
            InputEvent::KeyPressed {
                key: Key::Z,
                modifiers,
            } if modifiers.command => {
                if modifiers.shift {
                    self.send_sequencer(SequencerCommand::Redo);
                } else {
                    self.send_sequencer(SequencerCommand::Undo);
                }
            }

            // ⌘/Ctrl+V pastes the clip clipboard at the cursor in the Arranger
            // and the note clipboard at the clip cursor in the clip view.
            // Arrives as `InputEvent::Paste` — egui-winit turns the paste chord
            // into `egui::Event::Paste` and swallows the raw `V` key.
            // (Shift+⌘/Ctrl+C likewise arrives as `InputEvent::Copy { shift: true }`,
            // resolved in `Display::forward_input_event` against the view-local
            // time selection before it reaches here.)
            InputEvent::Paste if self.view_state() == ViewState::Arranger => {
                self.send_sequencer(SequencerCommand::PasteClips);
            }
            InputEvent::Paste if self.view_state() == ViewState::Clip => {
                self.send_sequencer(SequencerCommand::PasteNotes);
            }

            // Plain ⌘/Ctrl+C / ⌘/Ctrl+X in the clip view copy / cut the
            // selected notes. The Arranger resolves both (and the Shift forms)
            // in `Display::forward_input_event`; the clip view's Shift forms
            // fall through here and do nothing.
            InputEvent::Copy { shift: false } if self.view_state() == ViewState::Clip => {
                self.send_sequencer(SequencerCommand::CopyNotes);
            }
            InputEvent::Cut { shift: false } if self.view_state() == ViewState::Clip => {
                self.send_sequencer(SequencerCommand::CutNotes);
            }

            // ⌘/Ctrl+D in the clip view duplicates the selected notes, the
            // note twin of the Arranger's Duplicate Clips. The Arranger's
            // ⌘/Ctrl+D and Shift+⌘/Ctrl+D are handled by
            // Display::forward_input_event before they reach here, since they
            // need the view-local time selection. Bare `D` is spare.
            InputEvent::KeyPressed {
                key: Key::D,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && matches!(self.clip_context(), ClipContext::ClipWithSelection) =>
            {
                self.send_sequencer(SequencerCommand::DuplicateSelectedEvents);
            }

            // Shift+Tab, and a plain Tab the view didn't take as a pure focus
            // move (the clip panel hidden or maximized, so the other pane is
            // off screen).
            InputEvent::KeyPressed { key, modifiers }
                if *key == Key::Tab && (modifiers.shift || is_pane_focus_key(*key, *modifiers)) =>
            {
                self.handle_shift_tab();
            }

            // `[`/`]` move the start/end edge of a clip to the cursor
            // (`220-capture-without-pending-view.md`): an arrangement trim in
            // the arranger, a start/end marker in the clip view. Any
            // modifier but ⌘/Ctrl, so layouts that type the brackets with
            // ⌥ or ⇧ (Nordic, German) reach them too.
            InputEvent::KeyPressed {
                key: key @ (Key::OpenBracket | Key::CloseBracket),
                modifiers,
            } if !modifiers.command => {
                let edge = if *key == Key::OpenBracket {
                    ClipEdge::Start
                } else {
                    ClipEdge::End
                };
                if matches!(
                    self.clip_context(),
                    ClipContext::Arranger | ClipContext::Clip | ClipContext::ClipWithSelection
                ) {
                    self.send_sequencer(SequencerCommand::SetClipEdgeToCursor(edge));
                }
            }

            // Bare `Enter` fits the tempo to the project's only clip, in the
            // arranger and the clip view (`FitTempo`, a no-op once the tempo
            // is set). The overlays (settings modal, dialogs, text fields) consume it upstream,
            // and `Shift+Enter` stays unbound.
            InputEvent::KeyPressed {
                key: Key::Enter,
                modifiers,
            } if !modifiers.shift && !modifiers.command && !modifiers.alt => {
                self.send_sequencer(SequencerCommand::FitTempo);
            }

            InputEvent::KeyPressed {
                key: Key::Delete | Key::Backspace,
                modifiers: _,
            } => self.handle_delete(),

            InputEvent::KeyPressed {
                key: Key::Escape,
                modifiers: _,
            } => self.handle_escape(),

            InputEvent::ConfirmFilename { filename, folder } => {
                self.send_sequencer(SequencerCommand::SaveProject {
                    filename: filename.clone(),
                    folder: folder.clone(),
                });
            }

            InputEvent::ExportClip {
                time_bounds,
                folder,
                name,
            } => {
                self.send_sequencer(SequencerCommand::ExportClip {
                    time_bounds: *time_bounds,
                    folder: folder.clone(),
                    name: name.clone(),
                });
            }

            InputEvent::ImportMidiClip { clip, name, target } => {
                self.send_sequencer(SequencerCommand::ImportMidiClip {
                    clip: clip.clone(),
                    name: name.clone(),
                    target: *target,
                });
            }

            InputEvent::ProjectAction {
                action,
                discard_changes,
            } => {
                self.send_sequencer(SequencerCommand::ProjectAction {
                    action: action.clone(),
                    discard_changes: *discard_changes,
                });
            }

            InputEvent::ApplyStagedProject => {
                self.send_sequencer(SequencerCommand::ApplyStagedProject);
            }

            InputEvent::SetTrackOutput { track, output } => {
                self.send_sequencer(SequencerCommand::SetTrackOutput {
                    track: *track,
                    output: output.clone(),
                });
            }

            InputEvent::CaptureTrackInstrumentState { slot, state } => {
                self.send_sequencer(SequencerCommand::SetTrackInstrumentState {
                    slot: *slot,
                    state: state.clone(),
                });
            }

            InputEvent::AddTrack { track_idx } => {
                self.send_sequencer(SequencerCommand::AddTrack {
                    track_idx: *track_idx,
                });
            }

            InputEvent::RemoveTrack { track_idx } => {
                self.send_sequencer(SequencerCommand::RemoveTrack {
                    track_idx: *track_idx,
                });
            }

            InputEvent::RenameTrack { track_idx, name } => {
                self.send_sequencer(SequencerCommand::RenameTrack {
                    track_idx: *track_idx,
                    name: name.clone(),
                });
            }

            InputEvent::ConfirmMidiPorts { in_port, out_port } => {
                update_settings("MIDI ports", |settings| {
                    settings.midi_in_port = (!in_port.is_empty()).then(|| in_port.clone());
                    settings.midi_out_port = (!out_port.is_empty()).then(|| out_port.clone());
                });
            }

            InputEvent::SetMidiOutOffset { out_offset_ms } => {
                let offset = clamp_midi_out_offset_ms(*out_offset_ms);
                // Publish before persisting: the `"midiout"` thread reads this
                // per message, so the new offset applies to the very next note
                // whether or not the write to disk succeeds.
                self.midi_out_offset_ms.store(offset, Ordering::Relaxed);
                update_settings("MIDI output offset", |settings| {
                    settings.midi_out_offset_ms = offset;
                });
            }

            InputEvent::SaveTheme { theme_index } => {
                update_settings("theme", |settings| settings.theme_index = *theme_index);
            }

            InputEvent::KeyPressed {
                key: Key::K,
                modifiers: _,
            } => self.send_transport(TransportCommand::ToggleMetronomeMute),

            // Bare `T` (⌘/Ctrl+T is add track, bound in `Display`).
            InputEvent::KeyPressed {
                key: Key::T,
                modifiers,
            } if !modifiers.command => self.send_sequencer(SequencerCommand::TapTempo),

            InputEvent::FocusPane { pane } => {
                self.send_sequencer(SequencerCommand::FocusPane(*pane));
            }

            // Routed by the pane the click landed in, not by `view_state()`:
            // a click in the unfocused pane sends `FocusPane` just before
            // this, and the focus may not have reached the sequencer yet.
            InputEvent::MouseClickedTicks {
                pane,
                tick_x,
                event_id,
                track_idx,
                performance_lane_hit,
            } => match pane {
                Pane::Arranger => {
                    self.select_arranger_row(*track_idx, *performance_lane_hit);
                    self.place_cursor(Pane::Arranger, *tick_x);
                }
                Pane::Clip => {
                    self.place_cursor(Pane::Clip, *tick_x);
                    self.send_sequencer(SequencerCommand::SelectClipEvent(*event_id));
                }
            },

            InputEvent::SelectTrack { track_idx } => {
                self.send_sequencer(SequencerCommand::SelectTrackAt(*track_idx));
            }

            InputEvent::TrackHeaderClicked {
                track_idx,
                performance_lane_hit,
            } => self.select_arranger_row(*track_idx, *performance_lane_hit),

            // Sole dispatch site for ⌘/Ctrl+L (the view binds the key so it can
            // resolve the time selection it owns). A time selection arrives
            // pre-resolved and goes straight to the transport, which moves the
            // region to it (re-enabling looping) or — if the region already sits
            // there — toggles looping. With no selection the press is a bare
            // loop toggle: the region band stays put so the next press re-arms
            // it in place. The selected clip is deliberately *not* an operand
            // here — the arranger nearly always has one selected, so a clip
            // fallback would swallow the toggle.
            InputEvent::SetRegionToTimeSelectionOrToggleLoop { time_bounds } => match time_bounds {
                Some((start, end)) => {
                    self.send_transport(TransportCommand::SetRegionOrToggleLoop {
                        start: *start,
                        end: *end,
                    });
                }
                None => self.send_transport(TransportCommand::ToggleLoop),
            },

            // Sole dispatch site for Delete/Backspace in the Arranger — same
            // reasoning as ⌘/Ctrl+L: the view resolves the time selection it owns
            // and passes it down. Marquee-only, like `MuteClipsInSelection`:
            // the view never sends this without a real time selection.
            InputEvent::RemoveClipsInSelection { rect } => {
                self.send_sequencer(SequencerCommand::RemoveClips { rect: *rect });
            }
            InputEvent::InsertEmptyClip { time_bounds } => {
                self.send_sequencer(SequencerCommand::InsertEmptyClip {
                    time_bounds: *time_bounds,
                });
            }
            InputEvent::SplitClipsInTimeSelectionOrClip { time_bounds } => {
                self.send_sequencer(SequencerCommand::SplitClips {
                    time_bounds: *time_bounds,
                });
            }
            InputEvent::MuteClipsInSelection { rect } => {
                self.send_sequencer(SequencerCommand::MuteClipsInRange { rect: *rect });
            }
            InputEvent::InsertSilenceInSelection { start, end } => {
                self.send_sequencer(SequencerCommand::InsertSilenceInRange {
                    start: *start,
                    end: *end,
                });
            }
            InputEvent::DeleteTimeInSelection { start, end } => {
                self.send_sequencer(SequencerCommand::DeleteTimeInRange {
                    start: *start,
                    end: *end,
                });
            }
            InputEvent::DuplicateTimeInSelection { start, end } => {
                self.send_sequencer(SequencerCommand::DuplicateTimeInRange {
                    start: *start,
                    end: *end,
                });
            }
            InputEvent::DuplicateClipsInSelection { rect } => {
                self.send_sequencer(SequencerCommand::DuplicateClips { rect: *rect });
            }
            InputEvent::MergeClipsInSelection { rect } => {
                self.send_sequencer(SequencerCommand::MergeClips { rect: *rect });
            }
            InputEvent::CopyClipsInSelection { start, end } => {
                self.send_sequencer(SequencerCommand::CopyClips {
                    start: *start,
                    end: *end,
                });
            }
            InputEvent::CutClipsInSelection { start, end } => {
                self.send_sequencer(SequencerCommand::CutClips {
                    start: *start,
                    end: *end,
                });
            }
            InputEvent::CopyClipsInSelectionScoped { rect } => {
                self.send_sequencer(SequencerCommand::CopyClipsScoped { rect: *rect });
            }
            InputEvent::CutClipsInSelectionScoped { rect } => {
                self.send_sequencer(SequencerCommand::CutClipsScoped { rect: *rect });
            }

            InputEvent::SelectEventsInRect {
                tick_min,
                tick_max,
                note_min,
                note_max,
            } => {
                self.send_sequencer(SequencerCommand::SelectEventsInRect {
                    tick_min: *tick_min,
                    tick_max: *tick_max,
                    note_min: *note_min,
                    note_max: *note_max,
                });
            }

            InputEvent::DragEventsVelocity {
                event_ids,
                nudge,
                drag_id,
            } => {
                self.send_sequencer(SequencerCommand::DragEventsVelocity {
                    event_ids: event_ids.clone(),
                    nudge: *nudge,
                    drag_id: *drag_id,
                });
            }

            InputEvent::InsertNote {
                tick,
                length,
                pitch,
                velocity,
            } => {
                self.send_sequencer(SequencerCommand::InsertNote {
                    tick: *tick,
                    length: *length,
                    pitch: *pitch,
                    velocity: *velocity,
                });
            }

            InputEvent::DragNotes {
                event_ids,
                drag,
                drag_id,
            } => {
                self.send_sequencer(SequencerCommand::DragNotes {
                    event_ids: event_ids.clone(),
                    drag: *drag,
                    drag_id: *drag_id,
                });
            }

            InputEvent::PreviewNote { note, velocity } => {
                self.send_sequencer(SequencerCommand::PreviewNote {
                    note: *note,
                    velocity: *velocity,
                });
            }

            InputEvent::ResizeSelectedClipRegionEnd {
                target_tick,
                drag_id,
            } => {
                self.send_sequencer(SequencerCommand::ResizeSelectedClipRegionEnd {
                    target_tick: *target_tick,
                    drag_id: *drag_id,
                });
            }

            InputEvent::ResizeSelectedClipRegionStart {
                target_tick,
                drag_id,
            } => {
                self.send_sequencer(SequencerCommand::ResizeSelectedClipRegionStart {
                    target_tick: *target_tick,
                    drag_id: *drag_id,
                });
            }

            InputEvent::MoveCursorByGrid { step_ticks } => match self.view_state() {
                ViewState::Arranger => {
                    self.send_transport(TransportCommand::MoveCursor { ticks: *step_ticks });
                }
                // The view-resolved step goes straight to the grid nudge.
                ViewState::Clip => {
                    self.send_sequencer(SequencerCommand::NudgeClipCursorByGrid {
                        step_ticks: *step_ticks,
                    });
                }
            },

            // The arranger's pre-resolved gestures below are not re-gated on
            // `view_state()`: the view only produces them from the arranger
            // (a press in the arranger pane, or a key with the arranger
            // focused). A press in the arranger while the docked clip view has
            // the keyboard sends `FocusPane` first, and the focus may not have
            // reached the sequencer when these arrive — a focus check here
            // dropped the clip-header press (`SelectClipSpan`), so `⌘L` found
            // no marquee. See `archive/210-docked-clip-panel.md`.
            //
            // `⌥←`/`⌥→` clip-edge jump and timeline clicks — the same commands
            // a lane click sends, minus the track select (arranger) or the
            // note selection (clip view).
            InputEvent::SetCursorTick { pane, tick } => self.place_cursor(*pane, *tick),

            InputEvent::SelectClipSpan { track_idx, clip_id } => {
                self.send_sequencer(SequencerCommand::SelectClipSpan {
                    track_idx: *track_idx,
                    clip_id: *clip_id,
                });
            }

            InputEvent::MoveClip {
                track_idx,
                clip_id,
                to_track_idx,
                to_start_tick,
            } => {
                self.send_sequencer(SequencerCommand::MoveClip {
                    track_idx: *track_idx,
                    clip_id: *clip_id,
                    to_track_idx: *to_track_idx,
                    to_start_tick: *to_start_tick,
                });
            }

            InputEvent::MoveRange {
                rect,
                delta_ticks,
                delta_tracks,
            } => {
                self.send_sequencer(SequencerCommand::MoveRange {
                    rect: *rect,
                    delta_ticks: *delta_ticks,
                    delta_tracks: *delta_tracks,
                });
            }

            InputEvent::SetTrackVolume {
                track_idx,
                volume_db,
            } => {
                self.send_sequencer(SequencerCommand::SetTrackVolume {
                    track_idx: *track_idx,
                    volume_db: *volume_db,
                });
            }

            InputEvent::SetTempo { tempo_us, drag_id } => {
                self.send_sequencer(SequencerCommand::SetTempo {
                    tempo_us: *tempo_us,
                    gesture: drag_id.map(TempoGesture::Drag),
                });
            }

            InputEvent::SetMeter { meter } => {
                self.send_sequencer(SequencerCommand::SetMeter { meter: *meter });
            }

            InputEvent::SetTrackPan { track_idx, pan } => {
                self.send_sequencer(SequencerCommand::SetTrackPan {
                    track_idx: *track_idx,
                    pan: *pan,
                });
            }

            InputEvent::ToggleTrackMute { track_idx } => {
                self.send_sequencer(SequencerCommand::ToggleTrackMute {
                    track_idx: *track_idx,
                });
            }

            InputEvent::ToggleTrackSolo { track_idx } => {
                self.send_sequencer(SequencerCommand::ToggleTrackSolo {
                    track_idx: *track_idx,
                });
            }
            _ => {}
        }
    }

    // --- Input routing ---
    /// Selects the arranger row a click landed on: the performance lane, or
    /// track `track_idx`. The selection half of a lane or header click.
    fn select_arranger_row(&self, track_idx: Option<usize>, performance_lane_hit: bool) {
        if performance_lane_hit {
            self.send_sequencer(SequencerCommand::SelectPerformanceLane);
        } else if let Some(idx) = track_idx {
            self.send_sequencer(SequencerCommand::SelectTrackAt(idx));
        }
    }

    /// Places `pane`'s cursor at `tick` — the arranger's transport cursor
    /// (re-syncing the lead clip) or the lead clip's own cursor. The cursor
    /// half of a lane click, and all of a `SetCursorTick`.
    fn place_cursor(&self, pane: Pane, tick: i32) {
        match pane {
            Pane::Arranger => {
                self.send_transport(TransportCommand::SetCursorAndSelectClip { tick });
            }
            Pane::Clip => self.send_sequencer(SequencerCommand::CommitClipClickByTicks(tick)),
        }
    }

    /// `Left`/`Right` (+ Shift/Cmd): transport cursor, clip cursor, or event
    /// nudge, per view.
    fn handle_left_right_keys(&self, direction: i32, modifiers: &KeyModifiers) {
        match self.clip_context() {
            // No Arranger ←/→ ever arrives here: `Display::forward_input_event`
            // consumes every chord — plain steps the cursor by the zoom-adaptive
            // snap grid (`MoveCursorByGrid`, resolved view-side), ⇧ drags the
            // time selection, ⌥ jumps to clip edges, ⌘ nudges the marquee.
            ClipContext::Arranger => {}
            // ⌘ and ⌥ with nothing selected: unbound. They used to fall
            // through to the cursor step below with the modifier ignored — a
            // leftover duplicate of plain ←/→ at a different (fixed 16th)
            // step. ⌘ is the fine nudge once notes are selected, and ⌥←/→ is
            // kept free for a future jump between note edges (the arranger's
            // ⌥ clip-edge jump).
            ClipContext::Clip if modifiers.command || modifiers.alt => {}
            ClipContext::Clip => self.handle_clip_cursor_movement(direction, modifiers),
            // Position: L/R nudges the selected events at the grid step;
            // Cmd/Ctrl+L/R nudges by a much finer step. Shift+L/R nudges
            // event length instead of position (moved off +/-, see
            // `handle_plus_minus_keys`).
            ClipContext::ClipWithSelection if modifiers.shift => self.send_sequencer(
                SequencerCommand::NudgeSelectedEventsLength(direction * GRID_NUDGE_TICKS),
            ),
            ClipContext::ClipWithSelection if modifiers.command => self.send_sequencer(
                SequencerCommand::NudgeSelectedEvents(direction * FINE_NUDGE_TICKS),
            ),
            ClipContext::ClipWithSelection => self.send_sequencer(
                SequencerCommand::NudgeSelectedEvents(direction * GRID_NUDGE_TICKS),
            ),
        }
    }

    /// `Up`/`Down` (+ Shift): track cycle in the Arranger, cursor-to-start/end or transpose in a clip.
    fn handle_prev_next_keys(&self, direction: i32, modifiers: &KeyModifiers) {
        match self.clip_context() {
            // ⇧↑/↓ never arrives here in the Arranger: `Display::forward_input_event`
            // consumes it to drag the view-local time selection's track span.
            ClipContext::Arranger if direction < 0 => {
                self.send_sequencer(SequencerCommand::SelectPrevTrack);
            }
            ClipContext::Arranger => {
                self.send_sequencer(SequencerCommand::SelectNextTrack);
            }
            ClipContext::Clip if modifiers.shift && direction < 0 => {
                self.send_sequencer(SequencerCommand::MoveClipCursorToStart);
            }
            ClipContext::Clip if modifiers.shift && direction > 0 => {
                self.send_sequencer(SequencerCommand::MoveClipCursorToEnd);
            }
            // Pitch: Up increases, Down decreases (`direction` is -1 for Up,
            // +1 for Down, so it's negated here) — a semitone normally, an
            // octave with Shift.
            ClipContext::ClipWithSelection => {
                let semitones = if modifiers.shift { 12 } else { 1 };
                self.send_sequencer(SequencerCommand::TransposeSelectedEvents(
                    -direction * semitones,
                ));
            }
            _ => {}
        }
    }

    /// `+`/`-` reaching the handler: `⌥=`/`⌥-` clip stretch in `Clip`.
    fn handle_plus_minus_keys(&self, direction: i32, modifiers: &KeyModifiers) {
        match self.clip_context() {
            ClipContext::Arranger => {}
            // ⌥=/⌥- stretches the whole clip a bar longer/shorter — Logic's
            // Option-drag time-stretch. A whole-clip op, so the event
            // selection is irrelevant and it works in both views. Plain and
            // Shift+ +/- never arrive here from these views: they zoom,
            // consumed view-side in `Display::forward_input_event`. (Shift+-
            // used to clear the selection; that is Esc's job alone now.)
            ClipContext::Clip | ClipContext::ClipWithSelection => {
                // Retimes the notes to fill the new length
                // (`RetimeClipEdit::rescale_selected`).
                if modifiers.alt && !modifiers.command {
                    self.send_sequencer(SequencerCommand::RescaleSelectedClipTempo(direction));
                }
            }
        }
    }

    /// `⌘/Ctrl+A` in `Clip`: selects every `NoteOn` in the open clip (and
    /// the workflow publishes the selection, so the arrows then edit). No-op
    /// elsewhere.
    fn handle_select_all(&self) {
        match self.clip_context() {
            ClipContext::Clip | ClipContext::ClipWithSelection => {
                self.send_sequencer(SequencerCommand::SelectAllEvents);
            }
            ClipContext::Arranger => {}
        }
    }

    // --- Clip cursor ---
    /// Steps the clip cursor one grid line — the 16th here, since the view
    /// pre-resolves plain `←`/`→` to its zoom-adaptive snap
    /// (`MoveCursorByGrid`) — or a raw 16th with Shift.
    fn handle_clip_cursor_movement(&self, direction: i32, modifiers: &KeyModifiers) {
        let ticks = direction * sixteenth_straight_ticks();
        let cmd = if modifiers.shift {
            SequencerCommand::NudgeClipCursor(ticks)
        } else {
            SequencerCommand::NudgeClipCursorByGrid { step_ticks: ticks }
        };
        self.send_sequencer(cmd);
    }

    // --- Live REC, Commit and navigation ---
    /// `R` in the Arranger: start/stop live recording (no-op elsewhere).
    fn handle_toggle_live_recording(&self) {
        match self.clip_context() {
            ClipContext::Arranger => self.send_sequencer(SequencerCommand::ToggleLiveRecording),
            ClipContext::Clip | ClipContext::ClipWithSelection => {}
        }
    }

    /// `Shift+Tab` (and plain `Tab` with the clip panel hidden or
    /// maximized): toggles between the arranger and the clip view. Enters
    /// the selected clip from `Arranger` (`EnterClip`, moved off bare
    /// `Enter`); from `Clip` it exits to `Arranger` (`ExitClip`; edits there
    /// are already committed). Moved off
    /// `Shift+Enter`, which is now unbound everywhere — the Ableton-style
    /// chord is what a keyboard user reaches for.
    fn handle_shift_tab(&self) {
        match self.clip_context() {
            ClipContext::Arranger => self.send_sequencer(SequencerCommand::EnterClip),
            ClipContext::Clip | ClipContext::ClipWithSelection => {
                self.send_sequencer(SequencerCommand::ExitClip);
            }
        }
    }

    /// `Delete` in `Clip` with notes selected: delete them (the Arranger case is view-resolved).
    fn handle_delete(&self) {
        match self.clip_context() {
            // Handled by Display::forward_input_event before it ever reaches
            // here — it needs the view-local time selection.
            ClipContext::Arranger => {}
            ClipContext::Clip => {}
            ClipContext::ClipWithSelection => {
                self.send_sequencer(SequencerCommand::DeleteSelectedEvents);
            }
        }
    }

    /// `M`: toggles mute on the selected events in `Clip`
    /// (`MuteSelectedEventsEdit`) and nothing anywhere else. The Arranger
    /// never reaches here — its `M` is marquee-only and resolved upstream in
    /// `Display::forward_input_event` (`MuteClipsInSelection`), so a bare
    /// `M` with no marquee is consumed there. The old whole-clip toggle
    /// (`MuteSelectedClipsEdit`) is gone: a clip is only ever muted through a
    /// tick range.
    fn handle_mute(&self) {
        if matches!(self.clip_context(), ClipContext::ClipWithSelection) {
            self.send_sequencer(SequencerCommand::MuteSelectedEvents);
        }
    }

    /// ESC deselects every selected event in `Clip` (a no-op with
    /// nothing selected; `Shift+Tab` remains the way back to `Arranger`).
    fn handle_escape(&self) {
        match self.clip_context() {
            ClipContext::Clip | ClipContext::ClipWithSelection => {
                self.send_sequencer(SequencerCommand::ClearEventSelection);
            }
            ClipContext::Arranger => {}
        }
    }

    // --- Utility ---
    /// The [`ClipContext`] the current view's bindings run in: the view,
    /// with `Clip` split by the event-selection mirror.
    fn clip_context(&self) -> ClipContext {
        match self.view_state() {
            ViewState::Arranger => ClipContext::Arranger,
            ViewState::Clip if self.has_event_selection() => ClipContext::ClipWithSelection,
            ViewState::Clip => ClipContext::Clip,
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use crate::core::event_handlers::test_harness::harness;
    use crate::core::input_event::{InputEvent, KeyModifiers};
    use crate::core::sequencer::SequencerCommand;

    /// `\` and `/` are both the commit key, with or without modifiers (on a
    /// layout where `\` takes a chord, it arrives with them).
    #[test]
    fn backslash_and_slash_both_commit() {
        let h = harness();
        let chord = KeyModifiers {
            shift: true,
            alt: true,
            ..KeyModifiers::default()
        };
        for (key, modifiers) in [
            (Key::Backslash, KeyModifiers::default()),
            (Key::Slash, KeyModifiers::default()),
            (Key::Backslash, chord),
        ] {
            h.handlers
                .handle_input_event(&InputEvent::KeyPressed { key, modifiers });
            assert!(
                matches!(
                    h.sequencer_commands.try_recv(),
                    Ok(SequencerCommand::Commit)
                ),
                "{key:?} should commit"
            );
        }
    }
}
