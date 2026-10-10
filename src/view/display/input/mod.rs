//! `Display`'s per-frame input handling: `forward_input_event` — the dispatch
//! loop that resolves anything view-local (scroll, cursor placement, the drag
//! rectangles, the `⌘/Ctrl+L` / `Delete` operand) against `Display` state and
//! forwards the rest to `EventHandlers`. `gestures.rs` holds the drag state
//! machines; `modal.rs` the settings modal's keymap and mouse, `help.rs` the
//! help overlay's. See
//! `020-views-and-state.md`.

use egui::Key;

use crate::core::config::ZOOM_KEY_STEP;
use crate::core::input_event::KeyModifiers;
use crate::core::project::{ProjectAction, midi_file_path};
use crate::core::time::PPQN;

mod gestures;
mod help;
mod midi_drag;
mod modal;
mod plugin_drag;

pub(super) use midi_drag::DragPointer;

use self::gestures::past_drag_threshold;
use super::browser::BrowserPress;
use super::help_overlay::is_help_key;
use super::settings_modal::is_settings_chord;

use super::*;

/// What a key does while the track-header column has the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum TrackHeaderKey {
    /// Remove the selected track.
    RemoveTrack,
    /// Give the keyboard back to the lanes.
    Leave {
        /// False when the key still goes on to do its usual work.
        consumed: bool,
    },
}

/// The track-header column's keymap: plain Delete/Backspace removes the
/// selected track, Esc leaves, Shift+Tab leaves and still shows/hides the
/// clip panel. `None` — every other key, a modified Delete included — falls
/// through to the arranger.
fn track_header_key(key: Key, modifiers: KeyModifiers) -> Option<TrackHeaderKey> {
    let plain = !modifiers.command && !modifiers.alt && !modifiers.shift;
    match key {
        Key::Delete | Key::Backspace if plain => Some(TrackHeaderKey::RemoveTrack),
        Key::Escape => Some(TrackHeaderKey::Leave { consumed: true }),
        Key::Tab if modifiers.shift => Some(TrackHeaderKey::Leave { consumed: false }),
        _ => None,
    }
}

impl Display {
    /// Whether `(x, y)` is inside the drawable canvas.
    fn is_mouse_inside_window(&self, x: f32, y: f32) -> bool {
        self.render.canvas_rect.contains(egui::pos2(x, y))
    }

    /// Whether `(x, y)` is inside the active pane: its lanes and its timeline
    /// strip above them, but not the gap between the panes.
    fn is_mouse_inside_pane(&self, x: f32, y: f32) -> bool {
        let pane = self.pane_rect();
        self.is_mouse_inside_window(x, y) && y >= pane.min.y && y <= pane.max.y
    }

    /// Whether `(x, y)` is inside the active pane's clip-lane content area
    /// (below its timeline strip).
    fn is_mouse_inside_clip_content(&self, x: f32, y: f32) -> bool {
        self.is_mouse_inside_pane(x, y) && y >= self.pane_rect().min.y + Self::TIMELINE_H
    }

    /// Whether `(x, y)` is where the active pane's clicks act on time: its
    /// content area, and not over the track headers / piano key column or
    /// the right padding (`content_x_on_screen`) — a tick there is scrolled
    /// out of view. Every time-placing gesture gates on this; an arranger
    /// track-header press is handled before it (`TrackHeaderClicked`).
    fn is_mouse_inside_grid(&self, x: f32, y: f32) -> bool {
        self.is_mouse_inside_clip_content(x, y) && self.content_x_on_screen(x)
    }

    /// Whether `(x, y)` is on the active pane's timeline strip.
    fn is_mouse_on_timeline_strip(&self, x: f32, y: f32) -> bool {
        self.is_mouse_inside_pane(x, y) && !self.is_mouse_inside_clip_content(x, y)
    }

    /// Sends the event `make` builds from the arranger marquee, if there is
    /// one; with none the key is consumed and nothing is sent — the
    /// marquee-only range commands. Returns whether processing should
    /// continue.
    fn send_for_time_selection(&self, make: impl FnOnce(TimeSelectionRect) -> InputEvent) -> bool {
        self.gesture
            .time_selection
            .is_none_or(|rect| self.input_event_tx.send(make(rect)).is_ok())
    }

    /// `-1` for ←, `1` for →.
    fn horizontal_direction(key: Key) -> i32 {
        if key == Key::ArrowLeft { -1 } else { 1 }
    }

    /// A press in the active pane (`forward_input_event` scopes it with
    /// `in_pane`): on the timeline strip, cursor placement alone
    /// (`SetCursorTick`); in the lanes, the arranger's header buttons, mix bars, Shift+click
    /// extend, clip edge / band drags; the clip view's ⌘-click velocity drag;
    /// then, in either, cursor placement plus the time-selection or event
    /// marquee anchor, sent on as `MouseClickedTicks`.
    fn handle_mouse_click(&mut self, x: f32, y: f32, modifiers: KeyModifiers) -> bool {
        let arranger = self.active_pane() == Pane::Arranger;
        let is_clip_events_view = self.active_pane() == Pane::Clip;

        // A press anywhere but the track-header column (its empty area,
        // buttons, bars, the `+` row) takes the keyboard back from the
        // headers: a click in the lanes or the timeline strip, or in the
        // clip pane.
        let in_header_column =
            arranger && self.is_mouse_inside_clip_content(x, y) && x < self.content_origin_x();
        if !in_header_column {
            self.key_focus = KeyFocus::Pane;
        }

        // The timeline strip places the pane's cursor and nothing else: no
        // track, clip or note selection, no marquee, no drag. Where the
        // cursor line shows, a click puts the cursor on it.
        if self.is_mouse_on_timeline_strip(x, y) {
            let pane = self.active_pane();
            if self.content_x_on_screen(x) {
                let tick = self.snapped_tick_at(x);
                self.gesture.hover_cursor = Some((tick, pane));
                self.input_event_tx
                    .send(InputEvent::SetCursorTick { pane, tick })
                    .ok();
            }
            return true;
        }

        // The `+` row under the last lane adds a track at the end.
        if arranger && self.add_track_row_at(x, y) {
            self.input_event_tx
                .send(InputEvent::AddTrack {
                    track_idx: Some(self.track_count()),
                })
                .ok();
            return true;
        }

        // Track-header S/M/output buttons and volume/pan bars take priority
        // over every other arranger gesture — most specific first.
        // Pressing one deliberately does not select the track or move
        // the cursor: it is not a navigation gesture.
        if arranger && let Some((track_idx, button)) = self.track_button_at(x, y) {
            let event = match button {
                TrackButton::Solo => InputEvent::ToggleTrackSolo { track_idx },
                TrackButton::Mute => InputEvent::ToggleTrackMute { track_idx },
                TrackButton::Output => {
                    self.open_output_menu(track_idx);
                    return true;
                }
            };
            self.input_event_tx.send(event).ok();
            return true;
        }

        // A ⌘/Ctrl-click resets the bar to neutral without starting a drag.
        if arranger && let Some((track_idx, param)) = self.track_mix_bar_at(x, y) {
            if modifiers.command {
                let event = match param {
                    TrackMixParam::Volume => InputEvent::SetTrackVolume {
                        track_idx,
                        volume_db: 0.0,
                    },
                    TrackMixParam::Pan => InputEvent::SetTrackPan {
                        track_idx,
                        pan: 0.0,
                    },
                };
                self.input_event_tx.send(event).ok();
            } else {
                self.begin_track_mix_drag(track_idx, param, y);
            }
            return true;
        }

        // A press on a track header (left of the timeline) selects that
        // row and nothing else: no time lies under it, so the cursor stays
        // put and no marquee starts. Ahead of the Shift+click arm, which
        // would otherwise extend the marquee to the clamped left-edge tick.
        // A track's header also takes the keyboard (Delete removes the
        // track); the performance lane's doesn't.
        if in_header_column {
            let performance_lane_hit = self.performance_lane_hit_at(y);
            let track_idx = (!performance_lane_hit)
                .then(|| self.track_idx_at(y))
                .flatten();
            self.key_focus = if track_idx.is_some() {
                KeyFocus::TrackHeaders
            } else {
                KeyFocus::Pane
            };
            self.input_event_tx
                .send(InputEvent::TrackHeaderClicked {
                    track_idx,
                    performance_lane_hit,
                })
                .ok();
            return true;
        }

        // Shift+click extends the marquee from its anchor (the
        // cursor / selected track) to the click — a one-click drag.
        // Checked ahead of the edge and band hit-tests so a
        // Shift+click on a clip extends the selection instead of
        // arming a resize or move drag, and it never reaches the
        // cursor-placement logic below: the cursor is the anchor and
        // must not move. Consumed here; nothing is sent.
        if modifiers.shift && arranger && self.is_mouse_inside_clip_content(x, y) {
            self.extend_time_selection_to_click(x, y);
            return true;
        }

        if arranger && let Some(hit) = self.clip_edge_at(x, y) {
            self.begin_clip_resize_drag(hit);
            return true;
        }

        // The band behind the edges: press = select + marquee the
        // clip, hold-and-drag = move it. Replaces the marquee drag a
        // press here used to start — the body below the band, and
        // empty lane, still start one.
        if arranger && let Some((track_idx, clip_id)) = self.clip_band_at(x, y) {
            self.begin_clip_move_drag(track_idx, clip_id, x, y);
            return true;
        }

        if modifiers.command
            && is_clip_events_view
            && self.is_mouse_inside_grid(x, y)
            && let Some(hit_id) = self.event_id_at(x, y)
        {
            self.begin_velocity_drag(y, hit_id);
            return true;
        }

        // A plain press on a note: its body moves it, an edge resizes it.
        if is_clip_events_view
            && self.is_mouse_inside_grid(x, y)
            && let Some((event_id, part)) = self.note_hit_at(x, y)
        {
            self.begin_note_drag(event_id, part, x, y);
            return true;
        }

        // The octave legend left of the keys: zoom (horizontal) and scroll
        // (vertical) the piano roll's rows.
        if is_clip_events_view && self.is_on_octave_legend(x, y) {
            self.begin_key_zoom_drag(y);
            return true;
        }

        if !self.is_mouse_inside_grid(x, y) {
            return true;
        }
        let tick_x = self.snapped_tick_at(x);

        self.gesture.hover_cursor = Some((tick_x, self.active_pane()));
        self.begin_time_selection(tick_x, y);
        self.begin_event_marquee(x, y);

        let event_id = is_clip_events_view
            .then(|| self.event_id_at(x, y))
            .flatten();
        let performance_lane_hit = arranger && self.performance_lane_hit_at(y);
        let track_idx = (arranger && !performance_lane_hit)
            .then(|| self.track_idx_at(y))
            .flatten();

        self.input_event_tx
            .send(InputEvent::MouseClickedTicks {
                pane: self.active_pane(),
                tick_x,
                event_id,
                track_idx,
                performance_lane_hit,
            })
            .is_ok()
    }

    /// The pane pointer input at `(x, y)` runs in: with the button held, the
    /// one the press landed in, so a drag never changes pane mid-gesture;
    /// otherwise the one under the pointer.
    fn pointer_pane(&self, x: f32, y: f32) -> Pane {
        self.gesture
            .press_pane
            .or_else(|| self.pane_at(x, y))
            .unwrap_or_else(|| self.focused_pane())
    }

    /// Re-derives every hover — the cursor line, the note / clip edge / band
    /// / mix-bar / button / octave-legend / BPM chip hovers — at pointer
    /// `(x, y)`. Run
    /// on each `MouseMoved`, and on the release of a drag that held the
    /// pointer still (the octave-legend zoom), after which no move comes.
    fn refresh_hovers(&mut self, x: f32, y: f32, modifiers: KeyModifiers) {
        let pane = self.pointer_pane(x, y);
        self.in_pane(pane, |display| {
            display.update_hover_cursor_tick(x, y);
            display.update_note_hover(x, y, modifiers.command);
            display.update_key_zoom_hover(x, y);
            display.update_clip_resize_hover(x, y);
            display.update_clip_move_hover(x, y);
            display.update_track_mix_hover(x, y);
            display.update_track_button_hover(x, y);
        });
        self.update_tempo_hover(x, y);
    }

    /// Velocity of a note drawn by double-click until a resize sets one.
    const DRAWN_NOTE_VELOCITY: i32 = 100;

    /// A double-click in the clip pane: on empty piano-roll grid, a note at
    /// that row, starting on the grid line the hover cursor line shows
    /// (`snapped_tick_at`, the nearest visible line), at the last-used length
    /// and velocity (`GestureState::last_note`; until a resize sets them, a
    /// beat at [`DRAWN_NOTE_VELOCITY`](Self::DRAWN_NOTE_VELOCITY)) (`InsertNote`). It
    /// arrives on the second press, right after that press's own
    /// `MouseClicked` started an event marquee — dropped here, so holding and
    /// moving after the insert can't marquee the new note's selection away.
    /// Anywhere else — on a note, the key column, the timeline strip, the
    /// velocity panel — nothing; the two clicks it is made of did their own
    /// work.
    fn handle_double_click(&mut self, x: f32, y: f32) {
        if self.view_state() != ViewState::Clip
            || !self.is_mouse_inside_grid(x, y)
            || self.event_id_at(x, y).is_some()
        {
            return;
        }
        let Some(pitch) = self.note_row_at_screen_y(y) else {
            return;
        };
        let tick = self.snapped_tick_at(x);

        let (length, velocity) = self
            .gesture
            .last_note
            .unwrap_or((PPQN, Self::DRAWN_NOTE_VELOCITY));
        self.clear_event_marquee();
        self.input_event_tx
            .send(InputEvent::InsertNote {
                tick,
                length,
                pitch: i32::from(pitch),
                velocity,
            })
            .ok();
    }

    // --- Event handlers ---
    /// Polls input and routes each event: theme / project chords first, then
    /// the modal keymap, the browser panel's or the track headers' keys while
    /// they have the keyboard, or the main `forward_input_event` dispatch. An
    /// open project dialog gets first look at every event
    /// (`handle_project_dialog_input_event`), then an open rename field
    /// (`handle_track_rename_input_event`,
    /// `handle_browser_rename_input_event`), then an open output menu
    /// (`handle_output_menu_input_event`).
    pub(super) fn handle_input_events(&mut self, ctx: &egui::Context) {
        self.input_poller.poll(ctx, self.browser_width());
        // The clip view took the keyboard from the arranger: an open rename
        // field closes, and the header focus goes with it, so coming back
        // lands in the lanes. The settings modal, an overlay, takes neither:
        // the header focus is still there when it closes.
        let in_clip_view = self.view_state().is_clip_view();
        let left_arranger = in_clip_view && !self.input_was_clip_view;
        self.drop_stale_track_rename(left_arranger);
        self.drop_stale_browser_rename();
        if left_arranger && self.key_focus == KeyFocus::TrackHeaders {
            self.key_focus = KeyFocus::Pane;
        }
        self.input_was_clip_view = in_clip_view;
        let mut events = self.input_poller.take_input_events();
        for event in events.drain(..) {
            let ok = self.handle_project_dialog_input_event(&event)
                || self.handle_track_rename_input_event(&event)
                || self.handle_browser_rename_input_event(&event)
                || self.handle_tempo_field_input_event(&event)
                || self.handle_meter_field_input_event(&event)
                || self.handle_output_menu_input_event(&event)
                || self.handle_theme_input_event(&event)
                || self.handle_project_input_event(&event)
                || self.handle_overlay_input_event(&event)
                || match self.key_focus {
                    KeyFocus::Browser => self.handle_browser_key(&event),
                    KeyFocus::TrackHeaders if self.track_headers_have_keyboard() => {
                        self.handle_track_header_key(&event)
                    }
                    _ => false,
                }
                || self.forward_input_event(event, ctx);

            if !ok {
                break;
            }
        }
        self.input_poller.recycle_input_events(events);
    }

    /// The track-header column's keys while it has the keyboard: plain
    /// Delete/Backspace removes the selected track (`RemoveTrack`), Esc gives
    /// the keyboard back to the lanes. Shift+Tab gives it back too and still
    /// does its usual work. Returns whether it consumed `event`; everything
    /// else — ↑/↓ moving the track selection, Space, every chord — falls
    /// through to the arranger as usual.
    fn handle_track_header_key(&mut self, event: &InputEvent) -> bool {
        let InputEvent::KeyPressed { key, modifiers } = event else {
            return false;
        };
        match track_header_key(*key, *modifiers) {
            Some(TrackHeaderKey::RemoveTrack) => {
                self.input_event_tx
                    .send(InputEvent::RemoveTrack { track_idx: None })
                    .ok();
                true
            }
            Some(TrackHeaderKey::Leave { consumed }) => {
                self.key_focus = KeyFocus::Pane;
                consumed
            }
            None => false,
        }
    }

    /// Consumes ⇧F5 / ⇧F6 to step the theme — applied and saved, as in the
    /// settings modal's Appearance tab; returns whether it handled the event.
    fn handle_theme_input_event(&mut self, input_event: &InputEvent) -> bool {
        let direction = match input_event {
            InputEvent::KeyPressed {
                key: Key::F5,
                modifiers,
            } if modifiers.shift => Some(-1),
            InputEvent::KeyPressed {
                key: Key::F6,
                modifiers,
            } if modifiers.shift => Some(1),
            _ => None,
        };

        let Some(direction) = direction else {
            return false;
        };
        self.step_theme(direction);
        true
    }

    /// Consumes the project chords; returns whether it handled the event.
    /// ⌘/Ctrl+S saves at once under the project's name, or opens Save As for
    /// a project never saved; ⌘/Ctrl+⇧+S always opens Save As
    /// (`project_dialog.rs`). ⌘/Ctrl+N asks for a new project, through the
    /// unsaved-changes check.
    fn handle_project_input_event(&mut self, input_event: &InputEvent) -> bool {
        let InputEvent::KeyPressed { key, modifiers } = input_event else {
            return false;
        };
        if !modifiers.command {
            return false;
        }
        match key {
            Key::S if modifiers.shift => self.open_save_as(None),
            Key::S => self.save_project(None),
            Key::N => self.request_project_action(ProjectAction::New),
            _ => return false,
        }
        true
    }

    /// Routes `event` to the modal overlay that is up, which takes every
    /// event; returns whether one was up to take it.
    fn handle_overlay_input_event(&mut self, event: &InputEvent) -> bool {
        match self.overlay {
            Some(Overlay::Settings) => self.handle_settings_input_event(event),
            Some(Overlay::Help) => self.handle_help_input_event(event),
            None => return false,
        }
        true
    }

    /// If a modal overlay is up and `event` is a key press, routes it to the
    /// overlay's keymap and returns `true` — used by the CLAP key guard to
    /// feed modal keys from a plugin-focused window.
    pub(crate) fn try_consume_as_modal(&mut self, event: &InputEvent) -> bool {
        matches!(event, InputEvent::KeyPressed { .. }) && self.handle_overlay_input_event(event)
    }

    /// The main input dispatch: resolves anything view-local and forwards the
    /// rest to `EventHandlers`. Returns whether processing should continue.
    ///
    /// Governing rule for what gets intercepted below vs. sent through
    /// unchanged: pre-resolve a `KeyPressed`/mouse event here only when it
    /// needs view-local state (e.g. `time_selection`, marquee/drag anchors)
    /// as an operand that `EventHandlers::handle_input_event` has no access
    /// to. Everything else — the large majority of keys — passes through as
    /// a raw `KeyPressed` for the handler to dispatch on `view_state`/
    /// `clip_context` itself. When adding a new binding, only reach for a
    /// purpose-built `InputEvent` variant (and an arm here) if it fails that
    /// test; otherwise let it fall through to the catch-all `other` arm.
    fn forward_input_event(&mut self, input_event: InputEvent, ctx: &egui::Context) -> bool {
        match input_event {
            // A keyboard-armed marquee nudge (`⌘/Ctrl+←`/`→`, ⌘/Ctrl held)
            // owns the keyboard the way it owns the pointer: every other key
            // is consumed as a no-op until `MoveModifierReleased` commits it
            // or Esc cancels it. ⌘/Ctrl is held for the drag's whole life, so
            // every key here is a ⌘/Ctrl chord — `⌘↑` is a chord with no
            // binding, not "release ⌘, then ↑" — and letting it fall through
            // to the plain `↑`/`↓` track cycling changed the selected track
            // under a live ghost, wiping the marquee tint via
            // `sync_time_selection_to_cursor` while the frozen drag rect
            // still committed on the original tracks. First arm so nothing
            // below can act mid-nudge either — and with ⌘/Ctrl held that
            // now includes every real `⌘/Ctrl+X` command (`⌘S`, `⌘Z`,
            // `⌘D`, `⌘L`, ...), which is right: none of them should fire
            // under a live ghost. `⌘⇧←` is refused by the nudge arm
            // (`!shift`) and would otherwise reach the marquee-resize arm.
            InputEvent::KeyPressed { key, modifiers }
                if key != Key::Escape
                    && (modifiers.shift
                        || modifiers.alt
                        || !matches!(key, Key::ArrowLeft | Key::ArrowRight))
                    && self
                        .gesture
                        .clip_move_drag
                        .is_some_and(|drag| drag.via_keyboard) =>
            {
                true
            }
            // ⌘/Ctrl+, opens the settings modal over the arranger or the clip
            // view, from any key focus (`settings_modal.rs`); inside it the
            // modal keymap closes it again. View-local: the view it returns
            // to and the tab it opens on are the view's.
            InputEvent::KeyPressed { key, modifiers } if is_settings_chord(key, modifiers) => {
                self.open_settings();
                true
            }
            // `?` opens the help overlay, from any key focus
            // (`help_overlay.rs`); inside it its own keymap closes it again.
            InputEvent::KeyPressed { key, modifiers } if is_help_key(key, modifiers) => {
                self.open_help();
                true
            }
            // ⌘⌥B (Ableton's browser toggle) shows and focuses the browser
            // panel, or hides it; ⌘O shows it with the Projects tree in focus.
            // View-local layout state, like ⌘⌥E below.
            InputEvent::KeyPressed {
                key: Key::B,
                modifiers,
            } if modifiers.command && modifiers.alt && !modifiers.shift => {
                self.toggle_browser();
                true
            }
            InputEvent::KeyPressed {
                key: Key::O,
                modifiers,
            } if modifiers.command && !modifiers.alt && !modifiers.shift => {
                self.show_browser();
                true
            }
            // ⌘⌥E — the clip panel between maximized and docked below the
            // arranger (`archive/210-docked-clip-panel.md`). View-local layout state.
            // With the panel hidden it only flips the size it will show at.
            // Ahead of the ⌘E split arm, which refuses ⌥ too.
            InputEvent::KeyPressed {
                key: Key::E,
                modifiers,
            } if modifiers.command && modifiers.alt && !modifiers.shift => {
                self.toggle_clip_panel_size();
                true
            }
            // Shift+Tab shows and hides the clip panel. From a docked arranger
            // with the keyboard, the panel is already showing, so it hides
            // here — the handler's `EnterClip` would show it again. Every
            // other case (show, or hide from the clip view) is the handler's.
            InputEvent::KeyPressed {
                key: Key::Tab,
                modifiers,
            } if modifiers.shift
                && self.view_state() == ViewState::Arranger
                && self.render.clip_panel.visible =>
            {
                self.render.clip_panel.visible = false;
                true
            }
            // `v` — show/hide the selected track's hosted CLAP plugin editor.
            // View-local (owns the `!Send` editors), so it never reaches
            // `EventHandlers`. Only plain `v`; `⌘V` arrives as `InputEvent::Paste`.
            #[cfg(target_os = "macos")]
            InputEvent::KeyPressed {
                key: Key::V,
                modifiers,
            } if !modifiers.command && self.view_state() == ViewState::Arranger => {
                self.toggle_instrument_editor(self.selected_track_idx);
                true
            }
            // Two-finger trackpad / wheel scroll. View-local (mutates the
            // panes' scroll), consumed in every view so it never reaches the
            // channel.
            InputEvent::TimelineScroll {
                delta_x,
                delta_y,
                pointer_x,
                pointer_y,
            } => {
                if pointer_x.is_some_and(Self::is_over_browser) {
                    self.scroll_browser(delta_y);
                } else {
                    self.scroll_timeline_by(delta_x, delta_y, pointer_y);
                }
                true
            }
            // ⌘/Ctrl+wheel / pinch zoom, anchored on the pointer. View-local
            // and consumed in every view, exactly like `TimelineScroll`; a
            // no-op outside the arranger and `Clip`.
            InputEvent::TimelineZoom {
                factor,
                pointer_x,
                pointer_y,
            } => {
                self.zoom_timeline_by(factor, pointer_x.zip(pointer_y));
                true
            }
            // `+`/`=` and `-` zoom the arranger or clip view around the cursor
            // (Ableton, Bitwig). View-local (the scale is `Display` state), so
            // bound here. Shift is
            // allowed because `+` is Shift+`=` on many layouts. ⌘/Ctrl is
            // refused: ⌘+/⌘- belong to egui's UI-scale zoom. ⌥ is refused:
            // `⌥=`/`⌥-` stretch the clip, in the handler.
            InputEvent::KeyPressed {
                key: key @ (Key::Plus | Key::Equals | Key::Minus),
                modifiers,
            } if !modifiers.command
                && !modifiers.alt
                && (self.view_state() == ViewState::Arranger
                    || self.is_pane_visible(Pane::Clip)) =>
            {
                let factor = if key == Key::Minus {
                    1.0 / ZOOM_KEY_STEP
                } else {
                    ZOOM_KEY_STEP
                };
                self.zoom_timeline_by(factor, None);
                true
            }
            // Hover and drags run in the pane the pointer is over — or, with
            // the button held, the one the press landed in, so a drag never
            // changes pane mid-gesture.
            InputEvent::MouseMoved { x, y, modifiers } => {
                // The octave-legend zoom drag and the BPM chip drag own the
                // pointer: no cursor line, hover or other gesture follows it.
                // They run on `PointerMotion`, not positions, which stop at
                // the window edge.
                if self.pointer_held_drag() {
                    return true;
                }
                let pane = self.pointer_pane(x, y);
                self.in_pane(pane, |display| {
                    display.extend_time_selection(x, y);
                    display.extend_event_marquee(x, y);
                    display.extend_velocity_drag(y);
                    display.extend_note_drag(x, y);
                    display.extend_clip_resize_drag(x);
                    display.extend_clip_move_drag(x, y);
                    display.extend_track_mix_drag(y, modifiers.shift);
                });
                self.refresh_hovers(x, y, modifiers);
                // A `.mid` or a Plugins row pressed in the browser shows it is
                // being dragged once the pointer moves off the press point,
                // and becomes the drag proper once it leaves the panel.
                if let Some(press) = &mut self.browser.press {
                    press.dragging |=
                        past_drag_threshold(press.origin, (x, y), Self::DRAG_THRESHOLD_PX);
                }
                if !Self::is_over_browser(x)
                    && let Some(BrowserPress { item, .. }) = self.browser.press.take()
                {
                    if let BrowserItem::Plugin(plugin) = item {
                        self.begin_plugin_drag(plugin);
                    } else if let BrowserItem::MidiFile { folder, name } = item
                        && self.gesture.midi_drag.is_none()
                    {
                        self.begin_midi_drag(&midi_file_path(folder.as_deref(), &name));
                    }
                }
                self.update_midi_drag_target(x, y);
                self.update_plugin_drag_target(x, y);
                true
            }
            // A `.mid` from the file manager: the ghost appears as it comes
            // over the window and goes if it leaves; the drop imports it. A
            // drop with no hover before it (a platform that reports none)
            // lands where the pointer is.
            InputEvent::FileDragEntered { path } => {
                self.begin_midi_drag(&path);
                true
            }
            InputEvent::FileDragLeft => {
                self.gesture.midi_drag = None;
                true
            }
            InputEvent::FileDropped { path } => {
                if self.gesture.midi_drag.is_none() {
                    self.begin_midi_drag(&path);
                }
                self.finish_midi_drag();
                true
            }
            // Raw motion with the button held: the octave-legend zoom drag
            // and the BPM chip drag, which keep going past the window edge.
            // Consumed in every view.
            InputEvent::PointerMotion { dx, dy } => {
                self.in_pane(Pane::Clip, |display| display.extend_key_zoom_drag(dx, dy));
                if self.tempo_chip.dragging() {
                    let fine = ctx.input(|i| i.modifiers.shift);
                    self.extend_tempo_drag(dy, fine);
                }
                true
            }
            // A press goes to the pane it lands in. In the pane without the
            // keyboard it first moves the focus there (`FocusPane`, ahead of
            // the press's own events on the same channel), so the keys that
            // follow act on what was clicked. Focusing the clip pane drops
            // the arranger's time selection, as entering the clip view does.
            InputEvent::MouseClicked { x, y, modifiers } => {
                // Left of the canvas is the browser panel; a press anywhere
                // else takes the keyboard back from it.
                if Self::is_over_browser(x) {
                    self.handle_browser_click(x, y, false);
                    return true;
                }
                if self.key_focus == KeyFocus::Browser {
                    self.key_focus = KeyFocus::Pane;
                }
                // The header's `?` chip opens the help overlay; a press
                // anywhere while it is up closes it again.
                if self.is_on_help_chip(x, y) {
                    self.open_help();
                    return true;
                }
                // The header's BPM chip: a press starts its tempo drag, and
                // takes the keyboard from the track headers, so Esc reaches
                // the drag.
                if self.tempo_chip.contains(x, y) {
                    self.key_focus = KeyFocus::Pane;
                    self.begin_tempo_drag();
                    return true;
                }
                // The meter chip: only its double-click does anything, but a
                // press on it is the header's, not a pane's.
                if self.meter_chip.contains(x, y) {
                    self.key_focus = KeyFocus::Pane;
                    return true;
                }
                let Some(pane) = self.pane_at(x, y) else {
                    return true;
                };
                if pane != self.focused_pane() {
                    self.input_event_tx
                        .send(InputEvent::FocusPane { pane })
                        .ok();
                    if pane == Pane::Clip {
                        self.clear_time_selection();
                    }
                }
                self.gesture.press_pane = Some(pane);
                self.in_pane(pane, |display| display.handle_mouse_click(x, y, modifiers))
            }
            InputEvent::MouseDoubleClicked { x, y } => {
                if self.tempo_chip.contains(x, y) {
                    self.open_tempo_field();
                } else if self.meter_chip.contains(x, y) {
                    self.open_meter_field();
                } else if Self::is_over_browser(x) {
                    self.handle_browser_click(x, y, true);
                } else if self.pane_at(x, y) == Some(Pane::Clip) {
                    self.in_pane(Pane::Clip, |display| display.handle_double_click(x, y));
                } else if let Some(track_idx) =
                    self.in_pane(Pane::Arranger, |display| display.track_name_at(x, y))
                {
                    // Its first press already selected the track and gave
                    // the header column the keyboard.
                    self.open_track_rename(track_idx);
                }
                true
            }
            InputEvent::MouseReleased => {
                self.browser.press = None;
                self.finish_midi_drag();
                self.finish_plugin_drag();
                self.gesture.time_selection_anchor = None;
                self.clear_event_marquee();
                self.gesture.velocity_drag = None;
                if self.gesture.note_drag.is_some() {
                    self.in_pane(Pane::Clip, Self::finish_note_drag);
                }
                self.gesture.clip_resize_drag = None;
                // A keyboard-armed nudge is committed by `MoveModifierReleased`
                // only — the mirror of that arm's `via_keyboard` guard, so a
                // click while ⌘/Ctrl is held can't commit (or discard) it.
                if self
                    .gesture
                    .clip_move_drag
                    .is_some_and(|drag| !drag.via_keyboard)
                {
                    self.in_pane(Pane::Arranger, Self::finish_clip_move_drag);
                }
                self.gesture.track_mix_drag = None;
                self.gesture.press_pane = None;
                // A drag that held the pointer froze the hovers, and no
                // `MouseMoved` follows a release the pointer didn't move for:
                // refresh them here, or the magnifying glass (or, released
                // over the grid off macOS, the cursor line) waits for a move.
                let pointer_was_held = self.pointer_held_drag();
                self.gesture.key_zoom_drag = None;
                self.finish_tempo_drag();
                if pointer_was_held && let Some(pos) = self.canvas_pointer() {
                    let modifiers = ctx.input(|i| KeyModifiers::from(i.modifiers));
                    self.refresh_hovers(pos.x, pos.y, modifiers);
                }
                true
            }
            // Plain ←/→ — step the cursor one visible grid line, in the
            // arranger and in `Clip` with nothing selected (with notes
            // selected ←/→ nudge them, in the handler). Bound here because
            // the step is the zoom-adaptive snap grid, view-local state; the
            // move itself is the handler's (`MoveCursorByGrid`).
            InputEvent::KeyPressed {
                key: key @ (Key::ArrowLeft | Key::ArrowRight),
                modifiers,
            } if !modifiers.shift
                && !modifiers.command
                && !modifiers.alt
                && !(self.view_state().is_clip_view() && self.has_event_selection()) =>
            {
                self.input_event_tx
                    .send(InputEvent::MoveCursorByGrid {
                        step_ticks: Self::horizontal_direction(key) * self.cursor_grid_ticks(),
                    })
                    .is_ok()
            }
            // Bare `Z` — zoom to fit (the arranger's marquee; the selected
            // notes in `Clip`) — and bare `X` — step back to the
            // framing the last `Z` replaced: Ableton's pair, exactly.
            // View-local (the scale, scroll and history are `Display` state).
            // Only the bare key: `⌘/Ctrl+Z` is undo and must fall through to
            // the handler, and `⌘/Ctrl+X` arrives as `InputEvent::Cut`, never
            // as a `KeyPressed`.
            InputEvent::KeyPressed {
                key: key @ (Key::Z | Key::X),
                modifiers,
            } if !modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && (self.view_state() == ViewState::Arranger
                    || self.is_pane_visible(Pane::Clip)) =>
            {
                if key == Key::Z {
                    self.zoom_timeline_to_fit();
                } else {
                    self.zoom_timeline_back();
                }
                true
            }
            // ⌥←/⌥→ — coarse cursor navigation: jump to the next clip edge
            // (start or end, selected track) in that direction, the
            // text-editor "word jump". Bound here because the edges come
            // from the view-owned clip shapes. The cursor move itself goes
            // through the handler (`SetCursorTick`), the same path as a
            // lane click, so the marquee collapses and the clip selection
            // re-syncs. No edge that way ⇒ no-op.
            //
            // ⇧⌥←/⇧⌥→ — the "select a word" twin of the jump: the same
            // gesture as ⇧←/⇧→ (free edge moves, cursor-anchored, view-local)
            // with the step being "to the next clip edge" instead of one
            // grid step. From a clip's start one press marquees exactly the
            // clip — the band-press end state, so `L`/`⌘D`/Delete behave
            // identically afterwards.
            InputEvent::KeyPressed {
                key: key @ (Key::ArrowLeft | Key::ArrowRight),
                modifiers,
            } if modifiers.alt
                && !modifiers.command
                && self.view_state() == ViewState::Arranger =>
            {
                let direction = Self::horizontal_direction(key);
                if modifiers.shift {
                    self.extend_time_selection_to_clip_edge(direction);
                } else {
                    self.jump_cursor_to_clip_edge(direction);
                }
                true
            }
            // `⇧⌘/Ctrl+←`/`→` is deliberately unbound and consumed: the ⇧
            // nudge below must not pick it up (a stray ⌘ during a nudge would
            // silently keep nudging), and it must not fall through to the
            // handler's plain cursor step either.
            InputEvent::KeyPressed {
                key: Key::ArrowLeft | Key::ArrowRight,
                modifiers,
            } if modifiers.shift
                && modifiers.command
                && self.view_state() == ViewState::Arranger =>
            {
                true
            }
            // Keyboard twin of a drag: bound here for the same reason ⌘/Ctrl+L is,
            // the operand (the time selection) is view-local state.
            InputEvent::KeyPressed {
                key: key @ (Key::ArrowLeft | Key::ArrowRight),
                modifiers,
            } if modifiers.shift && self.view_state() == ViewState::Arranger => {
                self.nudge_time_selection_edge(Self::horizontal_direction(key));
                true
            }
            // ⌘/Ctrl+←/→ — the keyboard twin of the marquee band drag, but
            // horizontal-only: arms (or extends) a `ClipMoveDrag` ghost one
            // grid step at a time, exactly like a mouse drag except driven
            // by repeated presses instead of `MouseMoved` — see
            // `nudge_clip_move_drag`. Nothing is sent to the sequencer here;
            // the gesture commits on `MoveModifierReleased` below (`020-views-
            // and-state.md`). No vertical twin: shifting a selection across
            // tracks is a heavier gesture than "nudge it a little" and stays
            // mouse-only. ⌘/Ctrl rather than ⌥ because the move is the rarer,
            // heavier gesture; ⌥ is the conventional "coarser cursor step"
            // modifier and is reserved for cursor navigation. Only the exact
            // chord arms a drag — a stray ⇧ or ⌥ on top refuses rather than
            // starting a destructive gesture by accident.
            InputEvent::KeyPressed {
                key: key @ (Key::ArrowLeft | Key::ArrowRight),
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && self.view_state() == ViewState::Arranger =>
            {
                self.nudge_clip_move_drag(Self::horizontal_direction(key));
                true
            }
            // Commits a keyboard-armed marquee nudge the moment ⌘/Ctrl
            // goes up — the single commit point for `⌘/Ctrl+←`/`→`, exactly
            // mirroring `MouseReleased` for the mouse drag: same
            // `finish_clip_move_drag`, same `InputEvent::MoveRange`, same
            // no-op-if-nothing-moved guard. A no-op when `clip_move_drag` is
            // `None` or belongs to a mouse drag (`via_keyboard` false) —
            // `MoveModifierReleased` fires on every ⌘/Ctrl-up regardless of
            // context.
            //
            // `request_repaint()` here is load-bearing, not decorative: the
            // commit only *sends* `InputEvent::MoveRange` — the sequencer
            // thread applies it and mails back the `ClipRemoved`/`ClipAdded`/
            // `TimeSelectionSet` `UiEvent`s that actually move the drawn
            // shapes and the marquee, and `Display` only drains those on its
            // *next* frame. The mouse-driven commit (`MouseReleased`) has the
            // same round trip but it's invisible there because the mouse
            // almost always keeps moving right after a release, which
            // already forces the next frame (`020-views-and-state.md`'s
            // "next `MouseMoved` picks it up instead"). Releasing ⌘/Ctrl alone
            // has no such follow-up: with the pointer stationary and no
            // other input pending, egui's reactive loop would otherwise sit
            // idle with the ghost gone and the content still drawn at its
            // pre-move position — reading as "the move was cancelled" —
            // until some unrelated later input happened to wake it. Same
            // idiom as every other background-round-trip wake in this
            // codebase (`EventHandlers::request_repaint`), just called
            // directly on the `ctx` already in hand here instead of through
            // the cross-thread `repaint_ctx` — this runs on the UI thread
            // inside a live frame already.
            InputEvent::MoveModifierReleased => {
                if self
                    .gesture
                    .clip_move_drag
                    .is_some_and(|drag| drag.via_keyboard)
                {
                    self.finish_clip_move_drag();
                    ctx.request_repaint();
                }
                true
            }
            // Keyboard twin of a vertical drag: same rationale as ⇧←/⇧→
            // above, just extending the track span instead of the tick
            // range. Also view-local (the operand is `time_selection`), so
            // it never reaches `input_handler.rs` either — see
            // `handle_prev_next_keys`.
            InputEvent::KeyPressed {
                key: key @ (Key::ArrowUp | Key::ArrowDown),
                modifiers,
            } if modifiers.shift && self.view_state() == ViewState::Arranger => {
                let direction = if key == Key::ArrowUp { -1 } else { 1 };
                self.nudge_time_selection_track(direction);
                true
            }
            // ⌘/Ctrl+A — Select All: marquees every clip in the arranger
            // at once (the hull of every clip on both axes), the same
            // gesture as dragging corner to corner over the whole
            // arrangement — see `select_all_clips`. View-local like
            // Shift+click: the operand (`render.clip_shapes`) and the
            // result (`time_selection`) both live here, and nothing is sent
            // so the cursor and selected track stay put. Only the exact
            // chord — a stray ⇧ or ⌥ on top refuses rather than selecting
            // everything by accident.
            InputEvent::KeyPressed {
                key: Key::A,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && self.view_state() == ViewState::Arranger =>
            {
                self.select_all_clips();
                true
            }
            // Esc mid note drag cancels it: the notes go back where they
            // were and the gesture leaves no undo step. Ahead of every other
            // Esc binding.
            InputEvent::KeyPressed {
                key: Key::Escape,
                modifiers: _,
            } if self.gesture.note_drag.is_some() => {
                self.cancel_note_drag();
                true
            }
            // Esc mid BPM chip drag: the tempo goes back, no undo step.
            InputEvent::KeyPressed {
                key: Key::Escape,
                modifiers: _,
            } if self.tempo_chip.dragging() => {
                self.cancel_tempo_drag();
                true
            }
            // Esc cancels the marquee, mid-drag or already committed — the
            // standard "cancel this gesture" chord (Excel, Photoshop, VS Code).
            // `clear_time_selection` is all that's needed: the committed cursor
            // (and `selected_track_idx`) never left the drag's anchor tick/track
            // in the first place — `begin_time_selection` commits it there via
            // `MouseClickedTicks` at press time, and nothing since has moved it
            // — so dropping `time_selection` alone makes `draw_selected_track_cursor`
            // fall back to that anchor. Also drops the drag anchor so a still-held
            // mouse button doesn't resume extending a selection.
            InputEvent::KeyPressed {
                key: Key::Escape,
                modifiers: _,
            } if self.view_state() == ViewState::Arranger => {
                self.clear_time_selection();
                // Also abandons a live band drag: the ghost vanishes and
                // nothing is committed.
                self.clear_clip_move_drag();
                true
            }
            // ⌘/Ctrl+L — resolves the region operand the view owns and leaves
            // the precedence rule to the handler; a collapsed selection yields
            // `None`, which the handler turns into a bare loop toggle. Bare `L`
            // is deliberately unbound (it used to match any modifiers).
            InputEvent::KeyPressed {
                key: Key::L,
                modifiers,
            } if modifiers.command && self.view_state() == ViewState::Arranger => self
                .input_event_tx
                .send(InputEvent::SetRegionToTimeSelectionOrToggleLoop {
                    time_bounds: self.gesture.time_selection.map(|r| (r.start, r.end)),
                })
                .is_ok(),
            // Shift+⌘/Ctrl+D — Ableton "Duplicate Time", the global form:
            // insert time at the selection end (pushing every track's later
            // clips right), drop the copy into the gap on every track, then
            // advance the selection so repeated presses chain. Bypasses the
            // marquee's track range, like Shift+⌘/Ctrl+C/X. Selection-only,
            // like `I` — nothing is sent without a real time selection.
            InputEvent::KeyPressed {
                key: Key::D,
                modifiers,
            } if modifiers.command
                && modifiers.shift
                && self.view_state() == ViewState::Arranger =>
            {
                self.send_for_time_selection(|rect| InputEvent::DuplicateTimeInSelection {
                    start: rect.start,
                    end: rect.end,
                })
            }
            // Plain ⌘/Ctrl+D — "Duplicate Clips": paste a copy of the marquee
            // rectangle flush after itself on the marqueed tracks only,
            // overwriting (carving) whatever is already there. Nothing
            // shifts, no time is inserted. Marquee-only like above. Bare `D`
            // (no modifier) has no Arranger binding — ⌘D/Shift+⌘D cover
            // Duplicate entirely.
            InputEvent::KeyPressed {
                key: Key::D,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && self.view_state() == ViewState::Arranger =>
            {
                self.send_for_time_selection(|rect| InputEvent::DuplicateClipsInSelection { rect })
            }
            // ⌘/Ctrl+T — add an empty MIDI track after the selected one
            // (Ableton's chord). Exact chord, like ⌘/Ctrl+J.
            InputEvent::KeyPressed {
                key: Key::T,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && self.view_state() == ViewState::Arranger =>
            {
                self.input_event_tx
                    .send(InputEvent::AddTrack { track_idx: None })
                    .is_ok()
            }
            // ⌘/Ctrl+R — rename the selected track (Ableton's chord): the
            // rename field over its header's name. Exact chord; bound here,
            // ahead of bare `R` (record), which ⌘ no longer fires.
            InputEvent::KeyPressed {
                key: Key::R,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && self.view_state() == ViewState::Arranger =>
            {
                self.open_track_rename(self.selected_track_idx);
                true
            }
            // ⌘/Ctrl+J — "Merge Clips" (Ableton's Consolidate): bake the
            // marquee into one clip per marqueed track. Marquee-only like
            // ⌘/Ctrl+D; exact chord, a stray modifier refuses.
            InputEvent::KeyPressed {
                key: Key::J,
                modifiers,
            } if modifiers.command
                && !modifiers.shift
                && !modifiers.alt
                && self.view_state() == ViewState::Arranger =>
            {
                self.send_for_time_selection(|rect| InputEvent::MergeClipsInSelection { rect })
            }
            // The exact opposite of ⌘/Ctrl+I: same no-single-clip-fallback
            // shape (a duration-driven op is meaningless without a real time
            // selection), but closes the gap instead of opening one. Bound to
            // ⌘/Ctrl+Delete rather than bare Delete/Backspace, for the same
            // reason ⌘/Ctrl+I moved off bare `I` — matched ahead of the plain
            // Delete/Backspace arm below, which has no modifier guard and
            // would otherwise catch this chord first.
            InputEvent::KeyPressed {
                key: Key::Delete,
                modifiers,
            } if modifiers.command && self.view_state() == ViewState::Arranger => self
                .send_for_time_selection(|rect| InputEvent::DeleteTimeInSelection {
                    start: rect.start,
                    end: rect.end,
                }),
            // Marquee-only, like `Q` below: the old selected-clip fallback
            // (delete a clip just by clicking it) is gone, so with no time
            // selection the press is consumed and nothing is sent.
            InputEvent::KeyPressed {
                key: Key::Delete | Key::Backspace,
                modifiers: _,
            } if self.view_state() == ViewState::Arranger => {
                self.send_for_time_selection(|rect| InputEvent::RemoveClipsInSelection { rect })
            }
            // ⌘/Ctrl+⇧+E — "Export MIDI Clip" (Ableton's chord): the lead
            // clip to a `.mid` next to the project. In both views; the
            // arranger's marquee rides along so an export over several
            // clips is refused, never resolved to one of them. Ahead of the
            // ⌘E split arm, which refuses ⇧.
            InputEvent::KeyPressed {
                key: Key::E,
                modifiers,
            } if modifiers.command && modifiers.shift && !modifiers.alt => {
                let time_bounds = self
                    .gesture
                    .time_selection
                    .filter(|_| self.view_state() == ViewState::Arranger);
                let folder = self.project.project_current_folder.clone();
                let name = self
                    .project
                    .project_current_name
                    .as_deref()
                    .or(folder.as_deref())
                    .unwrap_or("project")
                    .to_owned();
                self.input_event_tx
                    .send(InputEvent::ExportClip {
                        time_bounds,
                        folder,
                        name,
                    })
                    .is_ok()
            }
            // Ableton-style split at the cursor: same shape as Delete above,
            // the view resolves the time selection it owns and passes it down.
            // Bound to ⌘/Ctrl+E (not bare `E`) for consistency with the other
            // now-marquee-scoped range keys. ⌥ is refused: ⌘⌥E is the clip
            // panel size (it used to fire the split too); so is ⇧, the
            // clip export above.
            InputEvent::KeyPressed {
                key: Key::E,
                modifiers,
            } if modifiers.command
                && !modifiers.alt
                && !modifiers.shift
                && self.view_state() == ViewState::Arranger =>
            {
                self.input_event_tx
                    .send(InputEvent::SplitClipsInTimeSelectionOrClip {
                        time_bounds: self.gesture.time_selection,
                    })
                    .is_ok()
            }
            // Ableton's "Insert MIDI Clip" (⇧⌘/Ctrl+M): an empty clip over
            // the marquee, or one bar at the cursor with none. Ahead of the
            // bare `M` arm below, which matches any modifiers. Plain ⌘M is
            // left to the OS (macOS minimize).
            InputEvent::KeyPressed {
                key: Key::M,
                modifiers,
            } if modifiers.command
                && modifiers.shift
                && self.view_state() == ViewState::Arranger =>
            {
                self.input_event_tx
                    .send(InputEvent::InsertEmptyClip {
                        time_bounds: self.gesture.time_selection,
                    })
                    .is_ok()
            }
            // Ableton-style "Deactivate Time Selection": marquee-only, same
            // shape as Delete above — intercepted here (Arranger only) ahead
            // of the bare `Key::M` arm in `input_handler.rs`, which toggles
            // the selected events' mute inside `Clip`. With no time
            // selection the press is consumed.
            InputEvent::KeyPressed {
                key: Key::M,
                modifiers: _,
            } if self.view_state() == ViewState::Arranger => {
                self.send_for_time_selection(|rect| InputEvent::MuteClipsInSelection { rect })
            }
            // Ableton/Bitwig-style "Insert Silence": same resolved-operand
            // shape as Delete/⌘E above, but with no single-clip fallback —
            // insert only makes sense with a real duration to move things
            // by, so nothing is sent (the key press is just consumed) when
            // there's no active time selection. Bound to ⌘/Ctrl+I (not bare
            // `I`), for the same reason Split moved off bare `E`.
            InputEvent::KeyPressed {
                key: Key::I,
                modifiers,
            } if modifiers.command && self.view_state() == ViewState::Arranger => self
                .send_for_time_selection(|rect| InputEvent::InsertSilenceInSelection {
                    start: rect.start,
                    end: rect.end,
                }),
            // Shift+⌘/Ctrl+C copy-time-selection: arrives as `InputEvent::Copy`
            // (egui-winit swallows the raw `C` key for the copy chord; `shift`
            // is sampled separately since the bare chord event carries no
            // modifier payload — see `input_event.rs`). Bypasses the marquee's
            // track range, like Shift+⌘/Ctrl+D — always every track.
            // Marquee-only like Delete/Backspace and Cut: with no time
            // selection the chord is consumed and nothing is sent.
            InputEvent::Copy { shift: true } if self.view_state() == ViewState::Arranger => self
                .send_for_time_selection(|rect| InputEvent::CopyClipsInSelection {
                    start: rect.start,
                    end: rect.end,
                }),
            // Plain ⌘/Ctrl+C: forwards the marquee whole (track-range-scoped)
            // — see `CopyClipsInSelectionScoped`. Marquee-only as above.
            InputEvent::Copy { shift: false } if self.view_state() == ViewState::Arranger => {
                self.send_for_time_selection(|rect| InputEvent::CopyClipsInSelectionScoped { rect })
            }
            // Shift+⌘/Ctrl+X cut-time-selection: arrives as `InputEvent::Cut`
            // (egui-winit swallows the raw `X` key for the cut chord). Same
            // marquee-bypassing story as Shift+⌘/Ctrl+C above — but, unlike
            // copy, marquee-only like Delete/Backspace: cut deletes, so with
            // no time selection the press is consumed and nothing is sent.
            InputEvent::Cut { shift: true } if self.view_state() == ViewState::Arranger => self
                .send_for_time_selection(|rect| InputEvent::CutClipsInSelection {
                    start: rect.start,
                    end: rect.end,
                }),
            // Plain ⌘/Ctrl+X: forwards the marquee whole (track-range-scoped)
            // — see `CutClipsInSelectionScoped`. Marquee-only for the same
            // reason as Shift+⌘/Ctrl+X above.
            InputEvent::Cut { shift: false } if self.view_state() == ViewState::Arranger => {
                self.send_for_time_selection(|rect| InputEvent::CutClipsInSelectionScoped { rect })
            }
            other => self.input_event_tx.send(other).is_ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{TrackHeaderKey, track_header_key};
    use crate::core::input_event::KeyModifiers;

    fn mods(command: bool, shift: bool, alt: bool) -> KeyModifiers {
        KeyModifiers {
            shift,
            command,
            alt,
            ctrl: false,
        }
    }

    #[test]
    fn plain_delete_and_backspace_remove_the_track() {
        for key in [Key::Delete, Key::Backspace] {
            assert_eq!(
                track_header_key(key, KeyModifiers::default()),
                Some(TrackHeaderKey::RemoveTrack)
            );
        }
    }

    /// ⌘⌫ no longer deletes a track, and ⌘+forward-Delete stays Delete Time.
    #[test]
    fn a_modified_delete_falls_through() {
        for key in [Key::Delete, Key::Backspace] {
            assert_eq!(track_header_key(key, mods(true, false, false)), None);
            assert_eq!(track_header_key(key, mods(false, true, false)), None);
            assert_eq!(track_header_key(key, mods(false, false, true)), None);
        }
    }

    #[test]
    fn escape_leaves_and_shift_tab_leaves_and_passes_on() {
        assert_eq!(
            track_header_key(Key::Escape, KeyModifiers::default()),
            Some(TrackHeaderKey::Leave { consumed: true })
        );
        assert_eq!(
            track_header_key(Key::Tab, mods(false, true, false)),
            Some(TrackHeaderKey::Leave { consumed: false })
        );
        assert_eq!(track_header_key(Key::Tab, KeyModifiers::default()), None);
    }

    #[test]
    fn arrows_and_space_fall_through() {
        for key in [Key::ArrowUp, Key::ArrowDown, Key::Space] {
            assert_eq!(track_header_key(key, KeyModifiers::default()), None);
        }
    }
}
