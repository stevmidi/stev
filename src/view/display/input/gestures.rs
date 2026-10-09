//! The `Display` drag state machines — one `begin_*` / `extend_*` pair per
//! gesture: arranger time selection, event marquee, ⌘/Ctrl+drag velocity, note
//! move / resize, clip edge resize, clip band move, track-header volume/pan.
//! Each `Some`
//! drag-state field doubles as the drag-active flag. See
//! `020-views-and-state.md`.

use egui::Pos2;

use crate::core::audio::mix::{db_from_fader_pos, fader_pos_from_db};
use crate::models::clip::{NoteBounds, NoteDrag};

use super::*;

impl Display {
    /// Below this screen-space distance from the press point, a press is
    /// still just a press, not a drag — for the event marquee
    /// (`extend_event_marquee`), the clip band move (`extend_clip_move_drag`)
    /// and a browser row pressed for a drag (`BrowserPress::dragging`) alike.
    /// See [`past_drag_threshold`].
    pub(super) const DRAG_THRESHOLD_PX: f32 = 3.0;
    /// Screen-space pixels of vertical drag per unit of velocity change —
    /// see `extend_velocity_drag`. Tunable to taste; full range (1..=127) is
    /// currently about 380px.
    const VELOCITY_DRAG_PX_PER_UNIT: f32 = 3.0;

    /// Anchors a new time selection drag at `tick_x`/`y`, dropping any existing
    /// range. Nothing is stored until the pointer actually moves — a press
    /// alone leaves the selection collapsed, which is simply the cursor. The
    /// anchor track resolves via `track_idx_at`, clamped to track 0 when the
    /// press landed on the performance-lane row (mirrors `track_idx_at`'s own
    /// bottom-edge clamp — there's no "no track" state to carry forward).
    /// Arranger only.
    pub(super) fn begin_time_selection(&mut self, tick_x: i32, y: f32) {
        if self.active_pane() != Pane::Arranger {
            return;
        }

        self.gesture.time_selection_anchor = Some((tick_x, self.track_idx_at(y).unwrap_or(0)));
        self.gesture.time_selection = None;
    }

    /// Extends an in-progress time selection to the pointer's snapped tick and
    /// current track row, or collapses it back to `None` when the pointer
    /// returns to *both* the tick and track anchor. A pure vertical drag
    /// (tick unchanged, track row moved) is kept as a real, zero-tick-width
    /// selection rather than collapsing — this is what lets a marquee select
    /// "these tracks, at this instant" for `⌘/Ctrl+E` (Split) without also
    /// carving out a time range. The content-editing ops (`Delete`/`M`/
    /// `⌘C`/`⌘X`/`⌘D`) already guard `end <= start` at their own
    /// construction, so a zero-width range harmlessly no-ops there instead of
    /// touching any content.
    ///
    /// Deliberately not gated on `is_mouse_inside_clip_content` — once a drag has
    /// started, wandering out of the clip lanes vertically must not cancel it.
    /// When the pointer leaves the track lanes mid-drag (e.g. up over the
    /// timeline/performance-lane row), the track row falls back to the
    /// *anchor* track rather than 0, so a drag that briefly dips out doesn't
    /// snap the span back down to track 0.
    pub(super) fn extend_time_selection(&mut self, x: f32, y: f32) {
        let Some((anchor_tick, anchor_track)) = self.gesture.time_selection_anchor else {
            return;
        };
        let tick_x = self.snapped_tick_at(x);
        let track_idx = self.track_idx_at(y).unwrap_or(anchor_track);

        let collapsed = tick_x == anchor_tick && track_idx == anchor_track;
        self.gesture.time_selection = (!collapsed)
            .then(|| TimeSelectionRect::spanning(anchor_tick, tick_x, anchor_track, track_idx));
    }

    /// Shift+click: the one-click marquee — extends the selection to the
    /// click, exactly as if the pointer had been dragged there. Arranger
    /// only, lanes only.
    ///
    /// The anchor is the current `time_selection` itself, or the cursor tick
    /// × `selected_track_idx` when there is none — never anything stored, so
    /// a selection built by a band press, a drag, the keyboard nudges or an
    /// earlier Shift+click all extend identically. A click *outside* the
    /// selection on an axis extends it to the click (the hull); a click
    /// *inside* moves the edge that isn't sitting on the cursor / selected
    /// track, or the nearer edge when neither is — see
    /// `shift_click_selection`. Nothing is sent — the cursor and selected
    /// track must stay put, otherwise `sync_time_selection_to_cursor` would
    /// collapse the range a frame later. Mirrors the plain-click split on
    /// the click side: a click on a clip's header band means the whole clip
    /// (its span is the target, so the clip ends up covered whole); a click
    /// in a body or an empty lane means that point, snapped to the cursor
    /// grid. The track edge is the clicked row either way, falling back to
    /// the selected track off the lanes like `extend_time_selection`.
    /// Collapsing onto the cursor on both axes clears the selection, same
    /// rule as the drags.
    pub(super) fn extend_time_selection_to_click(&mut self, x: f32, y: f32) {
        let point_tick = self.snapped_tick_at(x);

        let cursor_tick = self.cursor_tick.load(Ordering::Relaxed);
        let selected_track = self.selected_track_idx;
        let track_idx = self.track_idx_at(y).unwrap_or(selected_track);

        let span = self
            .clip_band_at(x, y)
            .and_then(|(band_track_idx, clip_id)| self.clip_shape(band_track_idx, clip_id))
            .map_or((point_tick, point_tick), |shape| {
                (shape.start_tick(), shape.end_tick())
            });

        self.gesture.time_selection = shift_click_selection(
            self.gesture.time_selection,
            cursor_tick,
            selected_track,
            span,
            track_idx,
        );
    }

    /// Anchors a new event marquee-select drag at the screen-space press
    /// point. Clip only. Nothing is selected here — `MouseClicked`'s
    /// existing hit-test/`SelectClipEvent` already handles a plain click;
    /// this only starts tracking in case the press turns into a drag.
    pub(super) fn begin_event_marquee(&mut self, x: f32, y: f32) {
        if self.active_pane() != Pane::Clip {
            return;
        }

        self.gesture.event_marquee_anchor = Some(MarqueeAnchor {
            x,
            y,
            tick: self.screen_x_to_tick(x),
            row: self.note_area_row_at(y),
            last_sent: None,
        });
        self.gesture.event_marquee_rect = None;
    }

    /// Extends an in-progress event marquee to the pointer's current
    /// position and pushes the resulting rectangle to the sequencer as
    /// `InputEvent::SelectEventsInRect` so `Clip::event_selection` stays in
    /// sync with the box as it grows/shrinks. Unlike the arranger's time
    /// selection, event selection is model state (it drives nudge/delete/
    /// etc.), so it can't stay purely view-local like `extend_time_selection`.
    ///
    /// Gated on a small pixel threshold before the first update: a plain
    /// click almost always produces a sub-pixel `MouseMoved` even with no
    /// intentional drag, and the rectangle's overlap test uses different
    /// boundary semantics than `event_id_at`'s click hit-test — sending an
    /// update on that jitter could immediately clobber the click's own
    /// `SelectClipEvent` selection. Once the threshold is crossed the drag is
    /// latched (`event_marquee_rect` is `Some`), so coming back near the
    /// anchor afterward keeps updating normally rather than freezing.
    ///
    /// Deliberately not gated on `is_mouse_inside_clip_content`, for the same
    /// reason as `extend_time_selection`: once a drag has started, wandering
    /// out of the clip lanes vertically must not cancel it. No grid snap on
    /// either axis for the drawn box — `event_marquee_rect` keeps unsnapped
    /// rows so it tracks the pointer smoothly; `note_min`/`note_max` below
    /// are only used for the hit-test sent to the sequencer, not for
    /// rendering. The anchor is held in content space, so it stays on its
    /// notes if the user wheel-scrolls under a held marquee; the pointer's end
    /// is held to the visible rows. The marquee never edge-auto-scrolls (see
    /// `auto_scroll_note_area`).
    pub(super) fn extend_event_marquee(&mut self, x: f32, y: f32) {
        let Some(anchor) = self.gesture.event_marquee_anchor else {
            return;
        };

        if self.gesture.event_marquee_rect.is_none()
            && !past_drag_threshold((anchor.x, anchor.y), (x, y), Self::DRAG_THRESHOLD_PX)
        {
            return;
        }

        let anchor_tick = anchor.tick;
        let anchor_note = self.note_at_row(anchor.row);
        let tick = self.screen_x_to_tick(x);
        let row = self.note_area_row_at(y);
        let note = self.note_at_row(row);

        let tick_min = anchor_tick.min(tick);
        let tick_max = anchor_tick.max(tick);
        let note_min = anchor_note.min(note);
        let note_max = anchor_note.max(note);

        self.gesture.event_marquee_rect =
            Some((tick_min, tick_max, anchor.row.min(row), anchor.row.max(row)));

        let hit_test = (tick_min, tick_max, note_min, note_max);
        if anchor.last_sent == Some(hit_test) {
            return;
        }
        if let Some(anchor) = self.gesture.event_marquee_anchor.as_mut() {
            anchor.last_sent = Some(hit_test);
        }
        self.input_event_tx
            .send(InputEvent::SelectEventsInRect {
                tick_min,
                tick_max,
                note_min,
                note_max,
            })
            .ok();
    }

    /// Anchors a new ⌘/Ctrl+drag velocity gesture at the screen-space press
    /// point. Called only once `MouseClicked` has already confirmed the
    /// modifier was held and `hit_id` is a real hit — this method just
    /// resolves the target set and stores the drag state.
    ///
    /// Target resolution mirrors Ableton ([`drag_targets`](Self::drag_targets)):
    /// the whole selection when the pressed note is in it, otherwise only the
    /// pressed note, with the real selection left untouched.
    pub(super) fn begin_velocity_drag(&mut self, y: f32, hit_id: Uuid) {
        let (_, target_event_ids) = self.drag_targets(hit_id);

        let drag_id = self.gesture.take_drag_id();

        self.gesture.velocity_drag = Some(VelocityDrag {
            anchor_y: y,
            drag_id,
            target_event_ids,
            last_total_nudge: 0,
        });
    }

    /// Extends an in-progress velocity drag: recomputes the total nudge from
    /// the anchor (like `extend_time_selection`/`extend_event_marquee`, not
    /// an incremental accumulation, so there's no drift) and sends only the
    /// delta since the last update. Only vertical movement matters — `x` is
    /// ignored, matching Ableton. Not gated on `is_mouse_inside_clip_content`:
    /// once the drag has started, wandering out of the clip lanes must not
    /// cancel it.
    pub(super) fn extend_velocity_drag(&mut self, y: f32) {
        let Some(drag) = self.gesture.velocity_drag.as_mut() else {
            return;
        };

        let total_nudge = ((drag.anchor_y - y) / Self::VELOCITY_DRAG_PX_PER_UNIT).round() as i32;
        let delta = total_nudge - drag.last_total_nudge;
        if delta == 0 {
            return;
        }
        drag.last_total_nudge = total_nudge;

        self.input_event_tx
            .send(InputEvent::DragEventsVelocity {
                event_ids: drag.target_event_ids.clone(),
                nudge: delta,
                drag_id: drag.drag_id,
            })
            .ok();
    }

    /// Begins a note move / resize drag, decided at press time by
    /// `note_hit_at` — a plain press on a note (⌘/Ctrl is the velocity drag):
    /// the body moves, an edge resizes. Targets follow
    /// [`drag_targets`](Self::drag_targets), like the velocity drag.
    ///
    /// A press on an unselected note is sent at once as the ordinary click
    /// (`MouseClickedTicks`: select it alone, audition, cursor to the snapped
    /// tick). A press on a selected note sends nothing, so the group stays
    /// selected to move; `finish_note_drag` sends the held-back click if the
    /// press never becomes a drag. No event marquee starts — dragging from a
    /// note moves it.
    pub(super) fn begin_note_drag(&mut self, pressed_id: Uuid, part: NotePart, x: f32, y: f32) {
        let tick_x = self.snapped_tick_at(x);
        let Some(pressed) = self.event_shape(pressed_id) else {
            return;
        };
        let (pressed_velocity, press_note) = (pressed.velocity(), pressed.note_number());

        let (pressed_is_selected, target_ids) = self.drag_targets(pressed_id);
        let deferred_click_tick = if pressed_is_selected {
            Some(tick_x)
        } else {
            self.send_note_click(pressed_id, tick_x);
            None
        };

        // Every target has a shape: `drag_targets` only names shaped notes.
        let origin = target_ids
            .into_iter()
            .filter_map(|id| {
                let shape = self.event_shape(id)?;
                Some((
                    id,
                    (shape.start_tick(), shape.end_tick(), shape.note_number()),
                ))
            })
            .collect();
        let drag_id = self.gesture.take_drag_id();

        self.gesture.note_hover = None;
        self.gesture.note_drag = Some(NoteMouseDrag {
            pressed_id,
            origin,
            drag_id,
            pressed_velocity,
            press_tick: self.screen_x_to_tick(x),
            press_note,
            press_x: x,
            press_y: y,
            deferred_click_tick,
            dragging: false,
            drag: note_drag_for(part),
            auditioned_pitch: press_note,
        });
    }

    /// The ordinary click on note `event_id` in the clip pane: cursor to
    /// `tick_x`, select the note alone and audition it.
    fn send_note_click(&mut self, event_id: Uuid, tick_x: i32) {
        self.gesture.hover_cursor = Some((tick_x, Pane::Clip));
        self.input_event_tx
            .send(InputEvent::MouseClickedTicks {
                pane: Pane::Clip,
                tick_x,
                event_id: Some(event_id),
                track_idx: None,
                performance_lane_hit: false,
            })
            .ok();
    }

    /// Extends an in-progress note drag: latches `dragging` once the pointer
    /// has moved past `DRAG_THRESHOLD_PX`, then recomputes the drag from the
    /// press each move (never accumulates): the time delta is the pointer's
    /// tick offset from the press, rounded to whole visible grid steps
    /// (`snap_to_grid` on the offset — so a recorded note keeps its feel, and
    /// a pure pitch move never shifts it in time); a move's pitch delta is the note
    /// row under the pointer (clamped to the visible range) minus the press
    /// row. Each change is applied live: one `InputEvent::DragNotes` with the
    /// whole drag (the sequencer merges a gesture's steps into one undo
    /// step), plus an audition each time the pressed note's pitch changes.
    /// Not gated on `is_mouse_inside_clip_content`, same rationale as the
    /// other drags.
    pub(super) fn extend_note_drag(&mut self, x: f32, y: f32) {
        let Some(drag) = self.gesture.note_drag.as_ref() else {
            return;
        };
        let grid_ticks = self.cursor_grid_ticks();
        if !drag.dragging
            && !past_drag_threshold(
                (drag.press_x, drag.press_y),
                (x, y),
                Self::DRAG_THRESHOLD_PX,
            )
        {
            return;
        }

        let delta_ticks = snap_to_grid(self.screen_x_to_tick(x) - drag.press_tick, grid_ticks);
        let delta_pitch = i32::from(self.note_at_screen_y(y)) - i32::from(drag.press_note);
        let next = drag.drag.with_deltas(delta_ticks, delta_pitch);
        if drag.dragging && next == drag.drag {
            return;
        }

        let (pressed_id, auditioned_pitch) = (drag.pressed_id, drag.auditioned_pitch);
        let pitch = self
            .note_drag_preview(&drag.origin, next)
            .into_iter()
            .find(|&(id, ..)| id == pressed_id)
            .map_or(auditioned_pitch, |(.., (_, _, pitch))| pitch);
        let Some(drag) = self.gesture.note_drag.as_mut() else {
            return;
        };
        drag.dragging = true;
        if next != drag.drag {
            drag.drag = next;
            self.input_event_tx
                .send(InputEvent::DragNotes {
                    event_ids: drag.target_ids(),
                    drag: next,
                    drag_id: drag.drag_id,
                })
                .ok();
        }
        if pitch != auditioned_pitch {
            drag.auditioned_pitch = pitch;
            let velocity = drag.pressed_velocity;
            self.input_event_tx
                .send(InputEvent::PreviewNote {
                    note: pitch,
                    velocity,
                })
                .ok();
        }
    }

    /// Per-frame edge auto-scroll for a note move drag. While one is held
    /// with the pointer past the note area's top or bottom edge, scrolls the
    /// view toward it (`edge_scroll_speed` × `dt`, faster the further out)
    /// and re-extends the drag at the same pointer, so the dragged pitch
    /// follows the row now at the edge. The event marquee deliberately
    /// doesn't scroll (as in Ableton): the view sliding under a sweep is
    /// distracting while its notes audition, and it would select notes the
    /// user never saw. The scroll belongs to the user's gesture, not
    /// the app, and stays where it ended. A resize has no pitch, so it
    /// doesn't scroll. `pointer` is egui's latest pointer position. Returns
    /// whether the view moved, so the frame can ask for the next one while
    /// the pointer holds still. Clip pane only.
    pub(in crate::view::display) fn auto_scroll_note_area(
        &mut self,
        dt: f32,
        pointer: Option<Pos2>,
    ) -> bool {
        if !self
            .gesture
            .note_drag
            .as_ref()
            .is_some_and(|drag| drag.dragging && !drag.is_resize())
        {
            return false;
        }
        let Some(Pos2 { x, y }) = pointer else {
            return false;
        };
        let overshoot = self.note_area_edge_overshoot(y);
        if overshoot == 0.0 {
            return false;
        }
        let before = self.render.clip_centre_row;
        self.scroll_note_area_by(overshoot.signum() * edge_scroll_speed(overshoot.abs()) * dt);
        if self.render.clip_centre_row == before {
            return false;
        }
        self.extend_note_drag(x, y);
        true
    }

    /// While a mouse marquee or clip band drag holds the pointer past the top
    /// or bottom of the arranger's lane viewport, scrolls the lanes toward
    /// it (the piano roll's speed curve, `edge_scroll_speed`) and re-extends
    /// the drag at the pointer held to the edge lane — so a drag across
    /// tracks follows into lanes scrolled out of view. Returns whether the
    /// view moved, so the frame can ask for the next one while the pointer
    /// holds still. Arranger pane only.
    pub(in crate::view::display) fn auto_scroll_arranger_lanes(
        &mut self,
        dt: f32,
        pointer: Option<Pos2>,
    ) -> bool {
        let marquee = self.gesture.time_selection_anchor.is_some();
        let band = self
            .gesture
            .clip_move_drag
            .is_some_and(|drag| drag.dragging && !drag.via_keyboard);
        if !(marquee || band) {
            return false;
        }
        let Some(Pos2 { x, y }) = pointer else {
            return false;
        };
        let layout = self.arranger_layout();
        let overshoot = layout.edge_overshoot(y);
        if overshoot == 0.0 || layout.viewport_h < 1.0 {
            return false;
        }
        self.scroll_arranger_lanes_by(overshoot.signum() * edge_scroll_speed(overshoot.abs()) * dt);
        if self.arranger_layout().scroll_y == layout.scroll_y {
            return false;
        }
        let edge_y = y.clamp(layout.lanes_top, layout.lanes_top + layout.viewport_h - 1.0);
        if marquee {
            self.extend_time_selection(x, edge_y);
        } else {
            self.extend_clip_move_drag(x, edge_y);
        }
        true
    }

    /// Release of a note drag. The drag is already applied (each move sent
    /// its step); release moves the shapes to where the preview showed them,
    /// in case the last step's edit hasn't come back yet, and a resize makes
    /// the pressed note's new length (and its velocity) the last-used one the
    /// next double-click draws. A press that never became a drag sends the
    /// click `begin_note_drag` held back, if it held one.
    pub(super) fn finish_note_drag(&mut self) {
        let Some(drag) = self.gesture.note_drag.take() else {
            return;
        };
        if !drag.dragging {
            if let Some(tick_x) = drag.deferred_click_tick {
                self.send_note_click(drag.pressed_id, tick_x);
            }
            return;
        }

        let preview = self.note_drag_preview(&drag.origin, drag.drag);
        if preview.iter().all(|(_, before, after)| before == after) {
            return;
        }
        if drag.is_resize()
            && let Some(&(.., (start, end, _))) =
                preview.iter().find(|&&(id, ..)| id == drag.pressed_id)
        {
            self.gesture.last_note = Some((end - start, i32::from(drag.pressed_velocity)));
        }
        // Show the notes where they land now, in case the last step's
        // `EventsUpdated` is still on its way — without this they could flash
        // back for the frames the round-trip takes.
        for (id, _, (start, end, note)) in preview {
            if let Some(shape) = self
                .render
                .event_shapes
                .iter_mut()
                .find(|shape| shape.matches(id))
            {
                shape.set_span(start, end, note);
            }
        }
    }

    /// Esc mid note drag: sends the drag back to nothing, which puts the
    /// notes back and drops the gesture's undo step, and ends the drag.
    pub(super) fn cancel_note_drag(&mut self) {
        let Some(drag) = self.gesture.note_drag.take() else {
            return;
        };
        if !drag.drag.is_noop() {
            self.input_event_tx
                .send(InputEvent::DragNotes {
                    event_ids: drag.target_ids(),
                    drag: drag.drag.with_deltas(0, 0),
                    drag_id: drag.drag_id,
                })
                .ok();
        }
    }

    /// Each dragged note as `(id, (start, end, pitch) before, after)`:
    /// `origin` (the spans at the press) and where `drag` puts them, clamped
    /// into the clip window by [`NoteDrag::dragged_spans`] — the model's own
    /// rule, so the preview `draw_clip_view` paints is where the step lands
    /// (same-pitch trims aside).
    pub(in crate::view::display) fn note_drag_preview(
        &self,
        origin: &[(Uuid, NoteBounds)],
        drag: NoteDrag,
    ) -> Vec<(Uuid, NoteBounds, NoteBounds)> {
        let before: Vec<NoteBounds> = origin.iter().map(|&(_, bounds)| bounds).collect();
        let after = drag.dragged_spans(&before, self.region_bounds_for_render());
        origin
            .iter()
            .zip(after)
            .map(|(&(id, before), after)| (id, before, after))
            .collect()
    }

    /// Updates which part of a note (if any) is under the pointer, for the
    /// resize cursor on an edge. A no-op while a note drag is live; `None`
    /// outside the clip pane's grid, with ⌘/Ctrl held (a press would be the
    /// velocity drag) and during the other clip-pane drags.
    pub(super) fn update_note_hover(&mut self, x: f32, y: f32, command: bool) {
        if self.gesture.note_drag.is_some() {
            return;
        }
        self.gesture.note_hover = (self.active_pane() == Pane::Clip
            && !command
            && self.gesture.velocity_drag.is_none()
            && self.gesture.event_marquee_anchor.is_none()
            && self.is_mouse_inside_grid(x, y))
        .then(|| self.note_hit_at(x, y))
        .flatten()
        .map(|(_, part)| part);
    }

    /// Begins a clip edge drag-resize gesture, decided at press time by
    /// `clip_edge_at` — a distinct gesture, like the velocity drag above, so
    /// the click is not forwarded as a normal `MouseClickedTicks`.
    ///
    /// First selects the clip exactly as a plain click there would
    /// (`SelectTrackAt` + `SetCursorAndSelectClip`, via a synthesized
    /// `MouseClickedTicks`), using the clip's own `start_tick` rather than
    /// the raw press position — that's always inside the clip regardless of
    /// which edge was grabbed, sidestepping the exclusive-end hit-test edge
    /// case. The subsequent drag then drives the "selected clip" resize
    /// commands.
    pub(super) fn begin_clip_resize_drag(&mut self, hit: ClipResizeDrag) {
        let start_tick = self
            .clip_shape(hit.track_idx, hit.clip_id)
            .map_or(0, |shape| shape.start_tick());

        self.input_event_tx
            .send(InputEvent::MouseClickedTicks {
                pane: Pane::Arranger,
                tick_x: start_tick,
                event_id: None,
                track_idx: Some(hit.track_idx),
                performance_lane_hit: false,
            })
            .ok();

        self.gesture.clip_resize_hover = None;
        self.gesture.clip_resize_drag = Some(hit);
        self.gesture.clip_resize_drag_id = self.gesture.take_drag_id();
    }

    /// Extends an in-progress clip edge drag: recomputes the pointer's
    /// grid-snapped absolute tick (same `snap_to_grid` + `cursor_grid_ticks`
    /// path as cursor placement) and sends it as the new target, tagged with
    /// the drag's id so the whole drag is one undo step. Not gated on
    /// `is_mouse_inside_clip_content`, same rationale as the other drags.
    pub(super) fn extend_clip_resize_drag(&mut self, x: f32) {
        let Some(drag) = self.gesture.clip_resize_drag else {
            return;
        };
        let target_tick = self.snapped_tick_at(x);
        let drag_id = self.gesture.clip_resize_drag_id;
        let event = match drag.edge {
            ClipResizeEdge::End => InputEvent::ResizeSelectedClipRegionEnd {
                target_tick,
                drag_id,
            },
            ClipResizeEdge::Start => InputEvent::ResizeSelectedClipRegionStart {
                target_tick,
                drag_id,
            },
        };
        self.input_event_tx.send(event).ok();
    }

    /// Updates which clip edge (if any) is under the pointer, for the resize
    /// cursor icon and edge glyph. A no-op while a drag is already in
    /// progress — the drag's own target is what matters then, not hover.
    pub(super) fn update_clip_resize_hover(&mut self, x: f32, y: f32) {
        if self.gesture.clip_resize_drag.is_some() {
            return;
        }
        self.gesture.clip_resize_hover = self
            .is_mouse_inside_clip_content(x, y)
            .then(|| self.clip_edge_at(x, y))
            .flatten();
    }

    /// Begins a clip band gesture, decided at press time by `clip_band_at`
    /// (after `clip_edge_at` has had first refusal). What the gesture drags
    /// is decided here too, by [`marquee_under_press`]: a press landing
    /// *inside* an active marquee that covers this clip's track drags
    /// everything inside that marquee (`ClipMoveKind::Marquee`) and leaves
    /// the selection exactly as it is — nothing is sent. Any other press is
    /// sent as `InputEvent::SelectClipSpan` — select this clip and marquee
    /// exactly its span, with the cursor on its start, discarding whatever
    /// marquee was active — and drags the whole clip (`ClipMoveKind::Clip`).
    /// Either way a not-yet-dragging `ClipMoveDrag` is armed in case the
    /// press turns into a move; a press that never crosses the drag
    /// threshold does only the select (or nothing). The click is *not*
    /// forwarded as `MouseClickedTicks`, same as the edge resize.
    ///
    /// On a whole-clip press the hover cursor line jumps to the clip start
    /// immediately rather than a frame later when the sequencer's cursor
    /// move lands.
    pub(super) fn begin_clip_move_drag(&mut self, track_idx: usize, clip_id: Uuid, x: f32, y: f32) {
        let Some((clip_start, clip_end)) = self
            .clip_shape(track_idx, clip_id)
            .map(|shape| (shape.start_tick(), shape.end_tick()))
        else {
            return;
        };

        let press_tick = self.screen_x_to_tick(x);
        let (kind, start_tick, end_tick, track_start, track_end) =
            match marquee_under_press(self.gesture.time_selection, track_idx, press_tick) {
                Some(sel) => (
                    ClipMoveKind::Marquee,
                    sel.start,
                    sel.end,
                    sel.track_start,
                    sel.track_end,
                ),
                None => {
                    self.input_event_tx
                        .send(InputEvent::SelectClipSpan { track_idx, clip_id })
                        .ok();
                    self.gesture.hover_cursor = Some((clip_start, self.active_pane()));
                    (
                        ClipMoveKind::Clip(clip_id),
                        clip_start,
                        clip_end,
                        track_idx,
                        track_idx,
                    )
                }
            };

        self.gesture.clip_move_hover = None;
        self.gesture.clip_move_drag = Some(ClipMoveDrag {
            kind,
            via_keyboard: false,
            pressed_clip_id: Some(clip_id),
            start_tick,
            end_tick,
            track_start,
            track_end,
            press_track_idx: track_idx,
            grab_offset_ticks: press_tick - start_tick,
            dragging: false,
            target_start_tick: start_tick,
            delta_tracks: 0,
            press_x: x,
            press_y: y,
            last_x: x,
            last_y: y,
        });
    }

    /// `⌘/Ctrl+←`/`→`: arms or extends a keyboard-driven marquee nudge, one grid
    /// step per press — the keyboard sibling of `begin_clip_move_drag` +
    /// `extend_clip_move_drag`, sharing the exact same `ClipMoveDrag` /ghost
    /// / `finish_clip_move_drag` machinery so the preview and commit are
    /// identical to a mouse drag. No-op when there's no real-tick-width
    /// marquee, or when a *mouse* drag already owns `clip_move_drag` (an
    /// unrelated in-progress band drag must not be stomped on by a stray
    /// `⌘/Ctrl+←`/`→`).
    ///
    /// The first press arms a new drag straight into `dragging: true` — a
    /// keypress is already a deliberate act, unlike a mouse press that could
    /// still just be a click, so there's no threshold to cross. Each further
    /// press while the same drag is live nudges `target_start_tick` by one
    /// more grid step from wherever the ghost already sits (accumulating,
    /// unlike the mouse path's "recompute from the live pointer" rule —
    /// there is no pointer here), clamped `>= 0`. Horizontal-only:
    /// `delta_tracks` stays `0` for the drag's whole life.
    ///
    /// Committing on every press (an earlier version of this) carved into
    /// whatever the ghost passed over at each intermediate step instead of
    /// just its final resting position — the same destructive-preview
    /// problem a ghost avoids for the mouse drag. Committing once on
    /// `MoveModifierReleased` (`finish_clip_move_drag`, called from `mod.rs`)
    /// fixes that the same way.
    pub(super) fn nudge_clip_move_drag(&mut self, direction: i32) {
        let grid_ticks = self.cursor_grid_ticks();
        let step = direction * grid_ticks;

        match self.gesture.clip_move_drag {
            Some(drag) if drag.via_keyboard => {
                self.gesture.clip_move_drag = Some(ClipMoveDrag {
                    target_start_tick: nudged_ghost_start(drag.target_start_tick, step),
                    ..drag
                });
            }
            Some(_) => {
                // A mouse drag owns the gesture; leave it alone.
            }
            None => {
                let Some(rect) = self.gesture.time_selection.filter(|r| r.has_tick_range()) else {
                    return;
                };
                self.gesture.clip_move_hover = None;
                self.gesture.clip_move_drag = Some(ClipMoveDrag {
                    kind: ClipMoveKind::Marquee,
                    via_keyboard: true,
                    pressed_clip_id: None,
                    start_tick: rect.start,
                    end_tick: rect.end,
                    track_start: rect.track_start,
                    track_end: rect.track_end,
                    press_track_idx: rect.track_start,
                    grab_offset_ticks: 0,
                    dragging: true,
                    target_start_tick: nudged_ghost_start(rect.start, step),
                    delta_tracks: 0,
                    press_x: 0.0,
                    press_y: 0.0,
                    last_x: 0.0,
                    last_y: 0.0,
                });
            }
        }
    }

    /// Extends an in-progress clip band drag: latches `dragging` once the
    /// pointer has moved past `DRAG_THRESHOLD_PX`, then recomputes
    /// the ghost's absolute target from the pointer each move (never
    /// accumulates — the drift rule every extender follows): the start tick
    /// is the pointer tick minus the grab offset, snapped to the arranger
    /// grid like cursor placement and clamped to `>= 0`; the lane delta is
    /// the lane under the pointer minus the press lane, clamped so the whole
    /// block stays on the lanes (`clamped_track_delta`), with the pointer
    /// off the lanes counting as the press lane (same wander rule as
    /// `extend_time_selection`). **Sends nothing** — the ghost is
    /// view-local until release.
    ///
    /// A keyboard-armed drag (`via_keyboard`, `⌘/Ctrl+←`/`→`) is left alone: the
    /// pointer only ever drives a drag the *button* started, and a nudge
    /// has no button held — it is armed straight into `dragging`, so without
    /// this guard the first stray pointer motion would skip the threshold
    /// and yank the ghost to wherever the mouse happens to be. The keyboard
    /// owns that drag until `MoveModifierReleased` commits it or Esc cancels it.
    pub(super) fn extend_clip_move_drag(&mut self, x: f32, y: f32) {
        let Some(drag) = self.gesture.clip_move_drag else {
            return;
        };
        if drag.via_keyboard {
            return;
        }
        let grid_ticks = self.cursor_grid_ticks();

        if !drag.dragging
            && !past_drag_threshold(
                (drag.press_x, drag.press_y),
                (x, y),
                Self::DRAG_THRESHOLD_PX,
            )
        {
            self.gesture.clip_move_drag = Some(ClipMoveDrag {
                last_x: x,
                last_y: y,
                ..drag
            });
            return;
        }

        let target_start_tick =
            snapped_move_start(self.screen_x_to_tick(x), drag.grab_offset_ticks, grid_ticks);
        let lane = self.track_idx_at(y).unwrap_or(drag.press_track_idx);
        let delta_tracks = clamped_track_delta(
            lane as i32 - drag.press_track_idx as i32,
            drag.track_start,
            drag.track_end,
            self.track_count(),
        );

        self.gesture.clip_move_drag = Some(ClipMoveDrag {
            dragging: true,
            target_start_tick,
            delta_tracks,
            last_x: x,
            last_y: y,
            ..drag
        });
    }

    /// Release of a clip band drag: the single commit point. Sends one
    /// `InputEvent::MoveClip` (whole-clip drag) or `InputEvent::MoveRange`
    /// (marquee drag) when the ghost actually sits somewhere new (past the
    /// drag threshold *and* off the block's own position); a drag returned
    /// home (crossed the threshold, ended back at zero net delta) sends
    /// nothing either — a completed drag, just a no-op one, same as a
    /// whole-clip drag returning home. A *plain click* — never crossed the
    /// threshold at all — is different for a `Marquee` drag specifically:
    /// nothing was selected at press time (unlike a whole-clip press, which
    /// already fired `SelectClipSpan` there), so this is the one chance to
    /// give a click inside the marquee its ordinary meaning — reselect the
    /// pressed clip and re-marquee its span, exactly as a click outside the
    /// marquee already does. `!drag.dragging` is exactly "never crossed the
    /// threshold" (it only ever flips true inside `extend_clip_move_drag`/
    /// `nudge_clip_move_drag`, alongside the first change to
    /// `target_start_tick`/`delta_tracks`, so it implies `!has_moved()` too
    /// — the two checks below are mutually exclusive, not overlapping).
    /// Drops the drag state either way, then re-runs the band hover test at
    /// the last pointer position — `MouseReleased` carries none, and the
    /// hover was cleared at press — so a plain click goes closed hand → open
    /// hand rather than falling back to the arrow until the pointer next
    /// moves. (After a real move the moved shape isn't under the pointer
    /// until the sequencer's `ClipAdded` lands, so the next `MouseMoved`
    /// picks it up instead.)
    pub(super) fn finish_clip_move_drag(&mut self) {
        let Some(drag) = self.gesture.clip_move_drag.take() else {
            return;
        };
        if drag.dragging && drag.has_moved() {
            let event = match drag.kind {
                ClipMoveKind::Clip(clip_id) => InputEvent::MoveClip {
                    track_idx: drag.track_start,
                    clip_id,
                    to_track_idx: (drag.track_start as i32 + drag.delta_tracks) as usize,
                    to_start_tick: drag.target_start_tick,
                },
                ClipMoveKind::Marquee => InputEvent::MoveRange {
                    rect: TimeSelectionRect {
                        start: drag.start_tick,
                        end: drag.end_tick,
                        track_start: drag.track_start,
                        track_end: drag.track_end,
                    },
                    delta_ticks: drag.delta_ticks(),
                    delta_tracks: drag.delta_tracks,
                },
            };
            self.input_event_tx.send(event).ok();
        } else if !drag.dragging
            && drag.kind == ClipMoveKind::Marquee
            && let Some(clip_id) = drag.pressed_clip_id
        {
            self.input_event_tx
                .send(InputEvent::SelectClipSpan {
                    track_idx: drag.press_track_idx,
                    clip_id,
                })
                .ok();
        }
        self.update_clip_move_hover(drag.last_x, drag.last_y);
    }

    /// Updates which clip band (if any) is under the pointer, for the grab
    /// cursor icon. A no-op while a *mouse* clip drag is in progress (a
    /// keyboard nudge doesn't own the pointer, so hover keeps working over
    /// it), and an edge hit wins over the band behind it, mirroring the
    /// press order.
    pub(super) fn update_clip_move_hover(&mut self, x: f32, y: f32) {
        if self
            .gesture
            .clip_move_drag
            .is_some_and(|drag| !drag.via_keyboard)
            || self.gesture.clip_resize_drag.is_some()
        {
            return;
        }
        self.gesture.clip_move_hover = (self.is_mouse_inside_clip_content(x, y)
            && self.clip_edge_at(x, y).is_none())
        .then(|| self.clip_band_at(x, y))
        .flatten();
    }

    /// Keyboard equivalent of dragging a time selection edge: ⇧← and ⇧→ move the
    /// free edge one grid step in that direction, exactly as if the pointer were
    /// dragged there. Arranger only.
    ///
    /// Needs no stored tick anchor, because a selection is always
    /// cursor-anchored: a drag places the cursor at its own anchor, and any
    /// later cursor move collapses the range (`sync_time_selection_to_cursor`).
    /// So the edge sitting on the cursor *is* the anchor and the other one is
    /// free; with no selection, the cursor anchors a fresh gesture. Collapsing
    /// back onto the anchor clears the selection **only if the track span is
    /// also back at its own anchor** (`self.selected_track_idx`) — exactly
    /// `extend_time_selection`'s collapse rule (both axes must return to the
    /// anchor), so nudging the tick edge back to the cursor after a `⇧↑`/`⇧↓`
    /// track-range extension leaves that track range intact instead of
    /// wiping it out; only a selection with no independent track extension
    /// actually collapses.
    ///
    /// The track span has no keyboard-driven anchor to extend, so it's left
    /// untouched when extending an existing selection (mouse- or
    /// keyboard-originated); starting a fresh selection defaults it to just
    /// the currently selected track.
    pub(super) fn nudge_time_selection_edge(&mut self, direction: i32) {
        let grid_ticks = self.cursor_grid_ticks();

        let (anchor_tick, free_tick) = self.tick_selection_edges();
        let next_tick = (free_tick + direction * grid_ticks).max(0);
        self.set_nudged_tick_selection(anchor_tick, next_tick);
    }

    /// `⌥←`/`⌥→`: places the cursor on the nearest clip edge strictly beyond
    /// it in `direction`, on the selected track — see [`next_clip_edge`].
    /// Sends `InputEvent::SetCursorTick`; the cursor is not moved locally
    /// (the transport owns it, and `sync_time_selection_to_cursor` collapses
    /// any marquee once the move lands, exactly as after a lane click).
    /// No-op when there is no edge that way.
    pub(super) fn jump_cursor_to_clip_edge(&mut self, direction: i32) {
        let from = self.cursor_tick.load(Ordering::Relaxed);
        if let Some(tick) = self.next_clip_edge_on_selected_track(from, direction) {
            self.input_event_tx
                .send(InputEvent::SetCursorTick {
                    pane: Pane::Arranger,
                    tick,
                })
                .ok();
        }
    }

    /// `⇧⌥←`/`⇧⌥→`: [`nudge_time_selection_edge`](Self::nudge_time_selection_edge)
    /// with the step being "to the next clip edge" instead of one grid step
    /// — same cursor anchor, same free-edge rule, same both-axes collapse
    /// rule, same untouched track span. The free edge moves to the nearest
    /// clip edge strictly beyond *it* (not beyond the cursor), so repeated
    /// presses walk edge to edge. No edge that way ⇒ no-op, selection kept.
    pub(super) fn extend_time_selection_to_clip_edge(&mut self, direction: i32) {
        let (anchor_tick, free_tick) = self.tick_selection_edges();
        let Some(next_tick) = self.next_clip_edge_on_selected_track(free_tick, direction) else {
            return;
        };
        self.set_nudged_tick_selection(anchor_tick, next_tick);
    }

    /// The tick edges of the keyboard selection gestures: `(anchor, free)` —
    /// the cursor, and the selection's other edge (the cursor too with no
    /// selection, or one not sitting on it).
    fn tick_selection_edges(&self) -> (i32, i32) {
        let anchor_tick = self.cursor_tick.load(Ordering::Relaxed);
        let free_tick = match self.gesture.time_selection {
            Some(rect) if rect.start == anchor_tick => rect.end,
            Some(rect) if rect.end == anchor_tick => rect.start,
            _ => anchor_tick,
        };
        (anchor_tick, free_tick)
    }

    /// Moves the selection's free tick edge to `next_tick`
    /// ([`nudged_tick_selection`]), keeping its track span — the selected
    /// track alone with no selection.
    fn set_nudged_tick_selection(&mut self, anchor_tick: i32, next_tick: i32) {
        let anchor_track = self.selected_track_idx;
        let (track_start, track_end) = match self.gesture.time_selection {
            Some(rect) => (rect.track_start, rect.track_end),
            None => (anchor_track, anchor_track),
        };
        self.gesture.time_selection =
            nudged_tick_selection(anchor_tick, anchor_track, next_tick, track_start, track_end);
    }

    /// The selected track's clip starts and ends — plus tick 0, so a leftward
    /// jump from before the first clip still has somewhere to land — fed to
    /// [`next_clip_edge`]. Every shape on the track counts, the one being
    /// recorded included (its live end is a real edge).
    fn next_clip_edge_on_selected_track(&self, from: i32, direction: i32) -> Option<i32> {
        let track_idx = self.selected_track_idx;
        let edges = self
            .render
            .clip_shapes
            .iter()
            .filter(|shape| shape.track_idx() == track_idx)
            .flat_map(|shape| [shape.start_tick(), shape.end_tick()])
            .chain(std::iter::once(0));
        next_clip_edge(edges, from, direction)
    }

    /// Keyboard equivalent of dragging a time selection's track span: ⇧↑ and
    /// ⇧↓ move the free track edge one row in that direction, exactly as if
    /// the pointer were dragged there vertically. Arranger only. Mirrors
    /// `nudge_time_selection_edge` with track and tick roles swapped:
    /// `selected_track_idx` is the anchor (a track selection is always
    /// anchored to it, the same way a tick range is anchored to the cursor —
    /// see `sync_time_selection_to_cursor`), so the row sitting on it *is*
    /// the anchor and the other one is free; with no selection, the selected
    /// track anchors a fresh gesture.
    ///
    /// The tick span has no keyboard-driven anchor to extend here, so it's
    /// left untouched when extending an existing selection; starting a fresh
    /// selection defaults it to the current cursor tick (zero tick width) —
    /// the track-only counterpart of a purely vertical mouse drag. Mirrors
    /// `nudge_time_selection_edge`'s collapse rule: nudging the track edge
    /// back onto its anchor clears the selection only if the tick span is
    /// *also* back at the cursor, so an independent `⇧←`/`⇧→` tick extension
    /// survives a `⇧↑`/`⇧↓` track edge returning home.
    pub(super) fn nudge_time_selection_track(&mut self, direction: i32) {
        let anchor_track = self.selected_track_idx;
        let anchor_tick = self.cursor_tick.load(Ordering::Relaxed);
        let free_track = match self.gesture.time_selection {
            Some(rect) if rect.track_start == anchor_track => rect.track_end,
            Some(rect) if rect.track_end == anchor_track => rect.track_start,
            _ => anchor_track,
        };
        let (start, end) = match self.gesture.time_selection {
            Some(rect) => (rect.start, rect.end),
            None => (anchor_tick, anchor_tick),
        };

        let next_track =
            (free_track as i32 + direction).clamp(0, self.track_count() as i32 - 1) as usize;
        self.gesture.time_selection =
            nudged_track_selection(anchor_tick, anchor_track, start, end, next_track);
        self.in_pane(Pane::Arranger, |display| display.reveal_track(next_track));
    }

    /// `⌘/Ctrl+A`: marquees every clip in the arranger at once — the
    /// selection becomes the hull of every clip shape on both axes (earliest
    /// start → latest end, lowest → highest track holding a clip), exactly
    /// the rect a mouse drag from the first clip's top-left corner to the
    /// last clip's bottom-right corner would build. Arranger only.
    ///
    /// View-local like Shift+click, and for the same reason: nothing is
    /// sent, so the cursor and `selected_track_idx` stay exactly where they
    /// are and `sync_time_selection_to_cursor` leaves the fresh rect alone.
    /// The hull is *not* snapped to the cursor grid — clip edges are the
    /// points, as for `⇧⌥←`/`⇧⌥→`. Every shape counts, the one being
    /// recorded included (its live end is a real edge). No clips ⇒ no-op,
    /// selection kept — there is nothing to select.
    pub(super) fn select_all_clips(&mut self) {
        let shapes = self
            .render
            .clip_shapes
            .iter()
            .map(|shape| (shape.track_idx(), shape.start_tick(), shape.end_tick()));
        if let Some(rect) = clip_hull_selection(shapes) {
            self.gesture.time_selection = Some(rect);
        }
    }

    /// Screen-space vertical drag distance that spans the whole travel of a
    /// track-header bar (`0.0..=1.0` fader position for volume, `-1.0..=1.0`
    /// for pan). Holding Shift divides the rate by this factor for fine
    /// adjustment.
    const TRACK_MIX_DRAG_PX_FULL_TRAVEL: f32 = 240.0;
    /// Divisor applied to the drag ratio while a fine-adjust modifier is held.
    const TRACK_MIX_FINE_DIVISOR: f32 = 5.0;

    /// Anchors a new track-header volume/pan drag. Called only once
    /// `MouseClicked` has confirmed the press landed on a bar (and was not a
    /// ⌘/Ctrl-modified reset click). Records the parameter's value at press
    /// time so `extend_track_mix_drag` can recompute an absolute value each
    /// move rather than accumulating deltas — the drift rule every other
    /// extender follows. Arranger only.
    pub(super) fn begin_track_mix_drag(&mut self, track_idx: usize, param: TrackMixParam, y: f32) {
        let (anchor_value, last_sent) = match param {
            TrackMixParam::Volume => {
                let db = self.track_volume_db(track_idx);
                (fader_pos_from_db(db), db)
            }
            TrackMixParam::Pan => {
                let pan = self.track_pan(track_idx);
                (pan, pan)
            }
        };
        self.gesture.track_mix_hover = None;
        self.gesture.track_mix_drag = Some(TrackMixDrag {
            track_idx,
            param,
            anchor_y: y,
            anchor_value,
            last_sent,
        });
    }

    /// Extends an in-progress track-header drag: recomputes the parameter's
    /// absolute value from the anchor (never accumulates), quantises it
    /// (0.1 dB / 1 pan unit) and sends it only when it changed since the last
    /// value sent. `shift` engages fine mode. Not gated on
    /// `is_mouse_inside_clip_content` — wandering off the bar must not cancel
    /// a live drag.
    pub(super) fn extend_track_mix_drag(&mut self, y: f32, shift: bool) {
        let Some(drag) = self.gesture.track_mix_drag.as_mut() else {
            return;
        };

        let rate = if shift {
            1.0 / Self::TRACK_MIX_FINE_DIVISOR
        } else {
            1.0
        };
        let delta = (drag.anchor_y - y) / Self::TRACK_MIX_DRAG_PX_FULL_TRAVEL * rate;

        let (value, event) = match drag.param {
            TrackMixParam::Volume => {
                let pos = (drag.anchor_value + delta).clamp(0.0, 1.0);
                let db = (db_from_fader_pos(pos) * 10.0).round() / 10.0;
                (
                    db,
                    InputEvent::SetTrackVolume {
                        track_idx: drag.track_idx,
                        volume_db: db,
                    },
                )
            }
            TrackMixParam::Pan => {
                // Pan travels −1..1 (range 2) over the same pixel distance.
                let raw = (drag.anchor_value + delta * 2.0).clamp(-1.0, 1.0);
                let pan = (raw * 50.0).round() / 50.0;
                (
                    pan,
                    InputEvent::SetTrackPan {
                        track_idx: drag.track_idx,
                        pan,
                    },
                )
            }
        };

        if (value - drag.last_sent).abs() < f32::EPSILON {
            return;
        }
        drag.last_sent = value;
        self.input_event_tx.send(event).ok();
    }

    /// Updates which track-header bar (if any) is under the pointer, for the
    /// resize cursor icon. A no-op while a drag is already in progress.
    pub(super) fn update_track_mix_hover(&mut self, x: f32, y: f32) {
        if self.gesture.track_mix_drag.is_some() {
            return;
        }
        self.gesture.track_mix_hover = self.track_mix_bar_at(x, y);
    }

    /// Updates which track-header S/M button (if any) is under the pointer, for
    /// the `PointingHand` cursor icon. A no-op while a mix-bar drag is running.
    pub(super) fn update_track_button_hover(&mut self, x: f32, y: f32) {
        if self.gesture.track_mix_drag.is_some() {
            return;
        }
        self.gesture.track_button_hover = self.track_button_at(x, y);
    }

    /// Sets `hover_cursor` — where the cursor line is drawn — to the pointer
    /// position snapped to the current view's grid, or `None` when the
    /// pointer is outside the pane's time area: its lanes and timeline strip
    /// between the track-header / piano gutter and the right padding
    /// (`content_x_on_screen`). Over the gutter the x would clamp to a tick
    /// and pin the line at the left edge, so there is no line there at all
    /// (the Logic / Ableton / Bitwig convention). Does not touch the
    /// committed transport/clip cursor — that only moves on click (see
    /// `MouseClickedTicks` handling). A no-op while a time-selection drag is
    /// in progress — the cursor line stays pinned at the drag anchor rather
    /// than sliding along with the pointer.
    pub(super) fn update_hover_cursor_tick(&mut self, x: f32, y: f32) {
        if self.gesture.time_selection_anchor.is_some() {
            return;
        }
        self.gesture.hover_cursor = (self.is_mouse_inside_pane(x, y)
            && self.content_x_on_screen(x))
        .then(|| self.snapped_tick_at(x))
        .map(|tick| (tick, self.active_pane()));
    }
}

/// Pure core of [`Display::begin_clip_move_drag`]'s what-to-drag decision:
/// the marquee itself when a band press at `press_tick` on `track_idx` lands
/// inside an active, real-tick-width marquee (`time_selection`) whose track
/// range covers that track — the press then drags everything inside it and
/// keeps it. `None` for any other press (no marquee, a zero-width one, the
/// press outside its tick range, or its track range not covering the clip):
/// the press then falls back to "discard the marquee, select + marquee the
/// whole clip, drag the whole clip".
fn marquee_under_press(
    time_selection: Option<TimeSelectionRect>,
    track_idx: usize,
    press_tick: i32,
) -> Option<TimeSelectionRect> {
    time_selection.filter(|sel| {
        sel.has_tick_range()
            && track_idx >= sel.track_start
            && track_idx <= sel.track_end
            && press_tick >= sel.start
            && press_tick < sel.end
    })
}

/// Pure core of [`Display::extend_clip_move_drag`]'s lane delta: `delta`
/// clamped so a block spanning lanes `track_start..=track_end` stays within
/// `0..track_count` after the shift.
fn clamped_track_delta(
    delta: i32,
    track_start: usize,
    track_end: usize,
    track_count: usize,
) -> i32 {
    delta.clamp(
        -(track_start as i32),
        track_count as i32 - 1 - track_end as i32,
    )
}

/// Pure core of [`Display::nudge_clip_move_drag`]'s accumulation step:
/// `current + step`, clamped `>= 0` — the ghost never nudges before the
/// timeline origin, same floor `snapped_move_start` applies to the mouse
/// drag. Accumulates from the ghost's *current* position rather than
/// re-deriving from a live pointer tick, since a keyboard nudge has none.
fn nudged_ghost_start(current: i32, step: i32) -> i32 {
    (current + step).max(0)
}

/// Pure core of [`Display::extend_clip_move_drag`]: where a dragged clip's
/// start lands for a pointer at `pointer_tick`, keeping the press point's
/// `grab_offset` into the clip, snapped to `grid_ticks` and never before the
/// timeline origin.
fn snapped_move_start(pointer_tick: i32, grab_offset: i32, grid_ticks: i32) -> i32 {
    snap_to_grid(pointer_tick - grab_offset, grid_ticks).max(0)
}

/// The model edit a press on `part` starts, before any movement: the body
/// moves, an edge resizes.
fn note_drag_for(part: NotePart) -> NoteDrag {
    match part {
        NotePart::Body => NoteDrag::Move {
            delta_ticks: 0,
            delta_pitch: 0,
        },
        NotePart::Start => NoteDrag::ResizeStart { delta_ticks: 0 },
        NotePart::End => NoteDrag::ResizeEnd { delta_ticks: 0 },
    }
}

/// Edge auto-scroll speed just past the note area's edge, in points per
/// second — a crawl of about five rows a second at full row height.
const EDGE_SCROLL_MIN_SPEED: f32 = 150.0;

/// How much faster the edge auto-scroll goes per point the pointer is past
/// the edge, in points per second.
const EDGE_SCROLL_GAIN: f32 = 15.0;

/// The edge auto-scroll's top speed, in points per second.
const EDGE_SCROLL_MAX_SPEED: f32 = 3000.0;

/// How fast the piano roll auto-scrolls with the pointer `overshoot` points
/// past the note area's edge, in points per second: a crawl just past it,
/// faster the further out, capped — `Display::auto_scroll_note_area`.
fn edge_scroll_speed(overshoot: f32) -> f32 {
    (EDGE_SCROLL_MIN_SPEED + overshoot * EDGE_SCROLL_GAIN).min(EDGE_SCROLL_MAX_SPEED)
}

/// Whether the pointer at `to` has moved at least `threshold` screen pixels
/// from the press point `from` on either axis — the latch that tells a drag
/// from a click's jitter.
pub(super) fn past_drag_threshold(from: (f32, f32), to: (f32, f32), threshold: f32) -> bool {
    (to.0 - from.0).abs() >= threshold || (to.1 - from.1).abs() >= threshold
}

/// Pure core of [`Display::extend_time_selection_to_click`]: the selection a
/// Shift+click builds from the current one (`selection`, or the cursor tick ×
/// `selected_track` when `None`) and the clicked `span` on `track_idx`.
/// `span` is `(start, end)` — a clip's own span for a header-band click, or
/// `(tick, tick)` for a point. Each axis goes through [`extended_edges`]
/// with the cursor / selected track as its pinned edge. Collapses to `None`
/// only when the result has no width on either axis — the same both-axes
/// rule as [`Display::extend_time_selection`].
fn shift_click_selection(
    selection: Option<TimeSelectionRect>,
    cursor_tick: i32,
    selected_track: usize,
    span: (i32, i32),
    track_idx: usize,
) -> Option<TimeSelectionRect> {
    let (start, end, track_start, track_end) = match selection {
        Some(rect) => (rect.start, rect.end, rect.track_start, rect.track_end),
        None => (cursor_tick, cursor_tick, selected_track, selected_track),
    };
    let (start, end) = extended_edges(start, end, cursor_tick, span);
    let (track_start, track_end) = extended_edges(
        track_start as i32,
        track_end as i32,
        selected_track as i32,
        (track_idx as i32, track_idx as i32),
    );
    let collapsed = start == end && track_start == track_end;
    (!collapsed).then_some(TimeSelectionRect {
        start,
        end,
        track_start: track_start as usize,
        track_end: track_end as usize,
    })
}

/// One axis of [`shift_click_selection`]: where the edges `start..=end` land
/// after a Shift+click on `span`. A span reaching *outside* on either side
/// extends the edges to cover it — the hull, so a clip on either end is
/// always covered whole and a band press or a drag over the same clip extend
/// identically. A span *inside* moves one edge onto it: the edge that is
/// not sitting on `pinned` (the cursor / selected track — the anchor of
/// every drag and keyboard nudge, so a click on the free side shrinks from
/// the free edge exactly as `⇧←`/`⇧→` would), or the nearer edge when
/// `pinned` is on neither (a selection already extended past its anchor on
/// both sides).
fn extended_edges(start: i32, end: i32, pinned: i32, span: (i32, i32)) -> (i32, i32) {
    let (span_start, span_end) = span;
    if span_start < start || span_end > end {
        return (start.min(span_start), end.max(span_end));
    }
    let move_start = if start == pinned {
        false
    } else if end == pinned {
        true
    } else {
        span_start - start <= end - span_end
    };
    if move_start {
        (span_start, end)
    } else {
        (start, span_end)
    }
}

/// Pure core of [`Display::nudge_time_selection_edge`]: computes the next
/// selection when the tick edge moves to `next_tick`, given the anchor tick/
/// track and the track span already in play (the anchor track on both sides
/// when there is no existing selection). Collapses to `None` only when the
/// tick edge lands back on `anchor_tick` **and** the track span is itself
/// just the anchor track — the same both-axes rule
/// [`Display::extend_time_selection`] applies to a mouse drag, so an
/// independent `⇧↑`/`⇧↓` track extension survives the tick edge returning
/// home.
fn nudged_tick_selection(
    anchor_tick: i32,
    anchor_track: usize,
    next_tick: i32,
    track_start: usize,
    track_end: usize,
) -> Option<TimeSelectionRect> {
    let collapsed =
        next_tick == anchor_tick && track_start == anchor_track && track_end == anchor_track;
    (!collapsed)
        .then(|| TimeSelectionRect::spanning(anchor_tick, next_tick, track_start, track_end))
}

/// Pure core of [`Display::jump_cursor_to_clip_edge`] /
/// [`Display::extend_time_selection_to_clip_edge`]: the nearest edge
/// *strictly* beyond `from` in `direction` (`> 0` rightward, otherwise
/// leftward), or `None` when there is none. Strictness is what makes
/// repeated presses walk: an edge exactly at `from` is where we already are.
/// `edges` needs no ordering or de-duplication.
fn next_clip_edge(edges: impl IntoIterator<Item = i32>, from: i32, direction: i32) -> Option<i32> {
    if direction > 0 {
        edges.into_iter().filter(|&edge| edge > from).min()
    } else {
        edges.into_iter().filter(|&edge| edge < from).max()
    }
}

/// Pure core of [`Display::nudge_time_selection_track`]: the track-axis twin
/// of [`nudged_tick_selection`], with the tick span already in play (the
/// cursor tick on both sides when there is no existing selection) and the
/// track edge moving to `next_track`.
fn nudged_track_selection(
    anchor_tick: i32,
    anchor_track: usize,
    start: i32,
    end: i32,
    next_track: usize,
) -> Option<TimeSelectionRect> {
    let collapsed = next_track == anchor_track && start == anchor_tick && end == anchor_tick;
    (!collapsed).then(|| TimeSelectionRect::spanning(start, end, anchor_track, next_track))
}

/// Pure core of [`Display::select_all_clips`]: the bounding rect of every
/// `(track_idx, start_tick, end_tick)` clip on both axes, or `None` when there
/// are no clips at all.
fn clip_hull_selection(
    clips: impl IntoIterator<Item = (usize, i32, i32)>,
) -> Option<TimeSelectionRect> {
    clips
        .into_iter()
        .map(|(track_idx, start, end)| TimeSelectionRect {
            start,
            end,
            track_start: track_idx,
            track_end: track_idx,
        })
        .reduce(|hull, clip| TimeSelectionRect {
            start: hull.start.min(clip.start),
            end: hull.end.max(clip.end),
            track_start: hull.track_start.min(clip.track_start),
            track_end: hull.track_end.max(clip.track_end),
        })
}

#[cfg(test)]
mod tests {
    use super::{
        EDGE_SCROLL_MAX_SPEED, EDGE_SCROLL_MIN_SPEED, TimeSelectionRect, clamped_track_delta,
        clip_hull_selection, edge_scroll_speed, extended_edges, marquee_under_press,
        next_clip_edge, nudged_ghost_start, nudged_tick_selection, nudged_track_selection,
        past_drag_threshold, shift_click_selection, snapped_move_start,
    };

    fn sel(
        start: i32,
        end: i32,
        track_start: usize,
        track_end: usize,
    ) -> Option<TimeSelectionRect> {
        Some(TimeSelectionRect {
            start,
            end,
            track_start,
            track_end,
        })
    }

    #[test]
    fn a_press_inside_the_marquee_drags_the_marquee() {
        let hit = marquee_under_press(sel(1000, 2000, 0, 2), 1, 1500).unwrap();
        assert_eq!(
            (hit.start, hit.end, hit.track_start, hit.track_end),
            (1000, 2000, 0, 2)
        );
    }

    #[test]
    fn a_press_outside_the_marquee_ticks_falls_back_to_the_clip() {
        assert!(marquee_under_press(sel(1000, 2000, 1, 1), 1, 500).is_none());
        assert!(marquee_under_press(sel(1000, 2000, 1, 1), 1, 2000).is_none()); // end is exclusive
    }

    #[test]
    fn a_press_without_a_covering_real_marquee_falls_back_to_the_clip() {
        assert!(marquee_under_press(None, 1, 1500).is_none());
        assert!(marquee_under_press(sel(1500, 1500, 1, 1), 1, 1500).is_none()); // zero width
        assert!(marquee_under_press(sel(1000, 2000, 2, 3), 1, 1500).is_none()); // other tracks
    }

    #[test]
    fn ghost_nudge_accumulates_from_the_current_position() {
        assert_eq!(nudged_ghost_start(1000, 480), 1480);
        assert_eq!(nudged_ghost_start(1000, -480), 520);
    }

    #[test]
    fn ghost_nudge_never_goes_before_the_origin() {
        assert_eq!(nudged_ghost_start(200, -480), 0);
    }

    #[test]
    fn track_delta_keeps_the_block_on_the_lanes() {
        // Block on lanes 1..=2 of 8: can go up 1, down 5.
        assert_eq!(clamped_track_delta(-3, 1, 2, 8), -1);
        assert_eq!(clamped_track_delta(9, 1, 2, 8), 5);
        assert_eq!(clamped_track_delta(2, 1, 2, 8), 2);
        assert_eq!(clamped_track_delta(0, 0, 7, 8), 0);
    }

    #[test]
    fn move_start_keeps_the_grab_offset_and_snaps() {
        // Grabbed 100 ticks into the clip; pointer at 1030 → raw start 930 →
        // snapped to the 480 grid = 960.
        assert_eq!(snapped_move_start(1030, 100, 480), 960);
    }

    #[test]
    fn edge_scroll_speeds_up_with_distance_and_caps() {
        assert_eq!(edge_scroll_speed(0.0), EDGE_SCROLL_MIN_SPEED);
        assert!(edge_scroll_speed(40.0) > edge_scroll_speed(10.0));
        assert_eq!(edge_scroll_speed(1e6), EDGE_SCROLL_MAX_SPEED);
    }

    #[test]
    fn drag_threshold_is_crossed_on_either_axis() {
        assert!(!past_drag_threshold((10.0, 10.0), (12.9, 7.1), 3.0));
        assert!(past_drag_threshold((10.0, 10.0), (13.0, 10.0), 3.0));
        assert!(past_drag_threshold((10.0, 10.0), (10.0, 7.0), 3.0));
    }

    #[test]
    fn move_start_never_goes_before_the_origin() {
        assert_eq!(snapped_move_start(50, 400, 480), 0);
    }

    #[test]
    fn tick_nudge_collapses_only_when_track_is_also_at_anchor() {
        assert!(nudged_tick_selection(100, 2, 100, 2, 2).is_none());
    }

    #[test]
    fn tick_nudge_extends_from_anchor() {
        let rect = nudged_tick_selection(100, 2, 200, 2, 2).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 200, 2, 2)
        );
    }

    #[test]
    fn tick_nudge_survives_an_independent_track_extension() {
        // Track span already covers 0..2 (built by a prior ⇧↑⇧↑); nudging the
        // tick edge back onto the anchor must keep that track span alive with
        // a zero-width tick range, not collapse the whole selection away.
        let rect = nudged_tick_selection(100, 2, 100, 0, 2).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 100, 0, 2)
        );
    }

    #[test]
    fn track_nudge_collapses_only_when_tick_is_also_at_anchor() {
        assert!(nudged_track_selection(100, 2, 100, 100, 2).is_none());
    }

    #[test]
    fn track_nudge_extends_from_anchor() {
        let rect = nudged_track_selection(100, 2, 100, 100, 0).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 100, 0, 2)
        );
    }

    #[test]
    fn track_nudge_survives_an_independent_tick_extension() {
        // Tick range already covers 100..200 (built by a prior ⇧→⇧→); nudging
        // the track edge back onto the anchor must keep that tick range
        // alive with a zero-width track span, not collapse it away.
        let rect = nudged_track_selection(100, 2, 100, 200, 2).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 200, 2, 2)
        );
    }

    #[test]
    fn shift_click_with_no_selection_spans_cursor_to_point_on_both_axes() {
        // Cursor at tick 100 / track 2, click at tick 500 / track 0.
        let rect = shift_click_selection(None, 100, 2, (500, 500), 0).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 500, 0, 2)
        );
        // Clicking before the cursor normalizes the other way round.
        let rect = shift_click_selection(None, 500, 0, (100, 100), 2).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 500, 0, 2)
        );
    }

    #[test]
    fn shift_click_on_a_band_covers_the_whole_clicked_clip() {
        // Cursor before the clip: cursor → clip end.
        let rect = shift_click_selection(None, 100, 0, (1000, 2000), 1).unwrap();
        assert_eq!((rect.start, rect.end), (100, 2000));
        // Cursor after the clip: clip start → cursor.
        let rect = shift_click_selection(None, 3000, 0, (1000, 2000), 1).unwrap();
        assert_eq!((rect.start, rect.end), (1000, 3000));
        // Cursor inside the clip: the clip's own span.
        let rect = shift_click_selection(None, 1500, 0, (1000, 2000), 1).unwrap();
        assert_eq!((rect.start, rect.end), (1000, 2000));
    }

    #[test]
    fn shift_click_outside_a_selection_extends_it_whichever_way_it_was_built() {
        // Regression: a clip (1000..2000) selected with the cursor on its
        // start — the identical end state of a band press *and* of a
        // left-to-right drag — extended left to 400 must keep the clip
        // inside: `[400, 2000]`, not `[400, 1000]`.
        let clip = sel(1000, 2000, 1, 1);
        let rect = shift_click_selection(clip, 1000, 1, (400, 400), 1).unwrap();
        assert_eq!((rect.start, rect.end), (400, 2000));
        // Same to the right: grows from the far edge.
        let rect = shift_click_selection(clip, 1000, 1, (2600, 2600), 1).unwrap();
        assert_eq!((rect.start, rect.end), (1000, 2600));
        // Band-to-band, left: the hull of both clips, on both tracks.
        let rect = shift_click_selection(clip, 1000, 1, (200, 600), 0).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (200, 2000, 0, 1)
        );
    }

    #[test]
    fn shift_click_inside_a_selection_moves_the_free_edge() {
        // Cursor pinned on the start: a click inside pulls the end back,
        // exactly as ⇧← would.
        let rect = shift_click_selection(sel(1000, 3000, 1, 1), 1000, 1, (2000, 2000), 1).unwrap();
        assert_eq!((rect.start, rect.end), (1000, 2000));
        // Cursor pinned on the end: the start moves instead.
        let rect = shift_click_selection(sel(1000, 3000, 1, 1), 3000, 1, (2000, 2000), 1).unwrap();
        assert_eq!((rect.start, rect.end), (2000, 3000));
        // Cursor on neither edge (already extended past it both ways): the
        // nearer edge moves.
        let rect = shift_click_selection(sel(0, 4000, 1, 1), 2000, 1, (3500, 3500), 1).unwrap();
        assert_eq!((rect.start, rect.end), (0, 3500));
        let rect = shift_click_selection(sel(0, 4000, 1, 1), 2000, 1, (500, 500), 1).unwrap();
        assert_eq!((rect.start, rect.end), (500, 4000));
    }

    #[test]
    fn shift_click_track_axis_follows_the_same_rule() {
        // Selected track 2 pinned, span 2..=4: a click on track 3 (inside)
        // pulls the free edge in; a click on track 0 (outside) extends.
        let rect = shift_click_selection(sel(0, 1000, 2, 4), 0, 2, (1000, 1000), 3).unwrap();
        assert_eq!((rect.track_start, rect.track_end), (2, 3));
        let rect = shift_click_selection(sel(0, 1000, 2, 4), 0, 2, (1000, 1000), 0).unwrap();
        assert_eq!((rect.track_start, rect.track_end), (0, 4));
    }

    #[test]
    fn shift_click_collapses_only_with_no_width_on_either_axis() {
        assert!(shift_click_selection(None, 100, 2, (100, 100), 2).is_none());
        // Same tick, other track: a zero-tick-width track selection.
        let rect = shift_click_selection(None, 100, 2, (100, 100), 0).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (100, 100, 0, 2)
        );
        // A click exactly on the pinned edge pulls the free edge home —
        // collapsing, exactly as ⇧← nudged all the way back would.
        assert!(shift_click_selection(sel(1000, 2000, 1, 1), 1000, 1, (1000, 1000), 1).is_none());
        // ...unless the track span keeps width: the click on the free
        // track 0 (selected track 1 pinned) leaves a zero-tick-width
        // two-track selection.
        let rect = shift_click_selection(sel(1000, 2000, 0, 1), 1000, 1, (1000, 1000), 0).unwrap();
        assert_eq!(
            (rect.start, rect.end, rect.track_start, rect.track_end),
            (1000, 1000, 0, 1)
        );
    }

    #[test]
    fn extended_edges_hull_wins_over_the_inside_rule() {
        // A span straddling both edges covers everything.
        assert_eq!(extended_edges(1000, 2000, 1000, (500, 2500)), (500, 2500));
        // A zero-width selection at the pin extends either way.
        assert_eq!(extended_edges(100, 100, 100, (300, 300)), (100, 300));
        assert_eq!(extended_edges(100, 100, 100, (0, 0)), (0, 100));
    }

    // Two clips on one track: [100, 200) and [300, 400), plus the tick-0
    // floor the caller chains on — unordered on purpose.
    const EDGES: [i32; 5] = [300, 0, 200, 400, 100];

    #[test]
    fn next_clip_edge_rightward_picks_nearest_greater_edge() {
        assert_eq!(next_clip_edge(EDGES, 0, 1), Some(100));
        assert_eq!(next_clip_edge(EDGES, 150, 1), Some(200));
        // From a clip's end, the next edge is the following clip's start —
        // the gap is a step of its own.
        assert_eq!(next_clip_edge(EDGES, 200, 1), Some(300));
    }

    #[test]
    fn next_clip_edge_leftward_picks_nearest_smaller_edge() {
        assert_eq!(next_clip_edge(EDGES, 350, -1), Some(300));
        assert_eq!(next_clip_edge(EDGES, 300, -1), Some(200));
        // Before the first clip, the tick-0 floor is still a landing spot.
        assert_eq!(next_clip_edge(EDGES, 50, -1), Some(0));
    }

    #[test]
    fn next_clip_edge_is_strict_so_repeated_presses_walk() {
        // Sitting exactly on an edge never returns that same edge.
        assert_eq!(next_clip_edge(EDGES, 100, 1), Some(200));
        assert_eq!(next_clip_edge(EDGES, 100, -1), Some(0));
    }

    #[test]
    fn next_clip_edge_is_none_past_the_last_edge() {
        assert_eq!(next_clip_edge(EDGES, 400, 1), None);
        assert_eq!(next_clip_edge(EDGES, 0, -1), None);
        assert_eq!(next_clip_edge([], 10, 1), None);
    }

    #[test]
    fn extend_to_clip_edge_from_clip_start_selects_exactly_the_clip() {
        // Cursor on the clip start (anchor), no selection: the free edge is
        // the cursor itself, so one rightward press lands on the clip's end —
        // the band-press end state.
        let next = next_clip_edge(EDGES, 100, 1).unwrap();
        let rect = nudged_tick_selection(100, 2, next, 2, 2).unwrap();
        assert_eq!((rect.start, rect.end), (100, 200));
        // A second press walks the free edge to the next clip's start.
        let next = next_clip_edge(EDGES, rect.end, 1).unwrap();
        let rect = nudged_tick_selection(100, 2, next, 2, 2).unwrap();
        assert_eq!((rect.start, rect.end), (100, 300));
        // Walking back home collapses, same rule as the grid nudge.
        assert!(nudged_tick_selection(100, 2, 100, 2, 2).is_none());
    }

    #[test]
    fn clip_hull_spans_every_clip_on_both_axes() {
        // Unordered on purpose: the hull must not depend on shape order.
        let clips = [(3, 800, 1200), (1, 100, 300), (2, 200, 900)];
        let rect = clip_hull_selection(clips).unwrap();
        assert_eq!((rect.start, rect.end), (100, 1200));
        assert_eq!((rect.track_start, rect.track_end), (1, 3));
    }

    #[test]
    fn clip_hull_of_one_clip_is_that_clip() {
        let rect = clip_hull_selection([(4, 500, 700)]).unwrap();
        assert_eq!((rect.start, rect.end), (500, 700));
        assert_eq!((rect.track_start, rect.track_end), (4, 4));
    }

    #[test]
    fn clip_hull_skips_empty_tracks_outside_the_content() {
        // Tracks 0 and 7 hold nothing: the hull hugs the clips, it doesn't
        // reach for the lane edges.
        let rect = clip_hull_selection([(2, 0, 100), (5, 50, 60)]).unwrap();
        assert_eq!((rect.track_start, rect.track_end), (2, 5));
    }

    #[test]
    fn clip_hull_is_none_with_no_clips() {
        assert!(clip_hull_selection([]).is_none());
    }
}
