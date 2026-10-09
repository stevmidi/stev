//! `handle_ui_events` — drains `ui_event_rx` each frame and applies every
//! `UiEvent` to `Display`'s projection: the shape lists, the open clip view,
//! the modal contents, the selection. See `020-views-and-state.md`.

use std::collections::HashSet;

use crate::models::track::TrackShift;

use super::*;

impl Display {
    /// Applies every pending `UiEvent` from the sequencer.
    pub(in crate::view::display) fn handle_ui_events(&mut self) {
        while let Ok(event) = self.ui_event_rx.try_recv() {
            match event {
                UiEvent::TrackSelected { track_idx } => {
                    self.selected_track_idx = track_idx;
                    self.performance_lane_selected = false;
                    self.in_pane(Pane::Arranger, Self::reveal_selected_track);
                    #[cfg(target_os = "macos")]
                    self.sync_live_instrument_target();
                }
                UiEvent::Status { message } => {
                    self.render.status = Some(StatusMessage::new(message));
                }
                UiEvent::PerformanceLaneSelected => {
                    self.performance_lane_selected = true;
                }
                UiEvent::TimeSelectionSet {
                    start,
                    end,
                    track_span,
                } => {
                    // Shift+⌘/Ctrl+D's chained-selection advance carries
                    // only a tick pair (`DuplicateTimeEdit` doesn't track a
                    // track span) — preserve whatever track range was already
                    // active so repeated presses keep the same scope,
                    // defaulting to the selected track for a fresh chain. The
                    // clip band press and plain ⌘/Ctrl+D (`DuplicateClipsEdit`
                    // freezes its rect) name their span explicitly.
                    let (track_start, track_end) = track_span.unwrap_or_else(|| {
                        self.gesture
                            .time_selection
                            .map(|r| (r.track_start, r.track_end))
                            .unwrap_or((self.selected_track_idx, self.selected_track_idx))
                    });
                    self.gesture.time_selection = Some(TimeSelectionRect {
                        start,
                        end,
                        track_start,
                        track_end,
                    });
                    // Re-latch the collapse watchers: the workflow that sent
                    // this may also have moved the cursor / selected track
                    // (the band press does both), and those already hold
                    // their final values here — `TrackSelected` precedes this
                    // event on the same channel and the cursor atomic was
                    // stored before either was sent. Without this,
                    // `sync_time_selection_to_cursor` (which runs right after
                    // the UI events) would read that move as "cursor moved"
                    // and wipe the selection it was just handed.
                    self.gesture.last_cursor_tick = Some(self.cursor_tick.load(Ordering::Relaxed));
                    self.gesture.last_selected_track_idx = Some(self.selected_track_idx);
                }
                UiEvent::TimeSelectionCleared => {
                    self.clear_time_selection();
                }
                UiEvent::RecordingStarted { clip } => {
                    self.recording_clip_id = Some(clip.clip_id);
                    self.add_clip_shape(clip);
                }
                UiEvent::RecordingCompleted => {
                    self.recording_clip_id = None;
                }
                UiEvent::RecordingCanceled { track_idx, clip_id } => {
                    self.recording_clip_id = None;
                    self.remove_clip_shape(track_idx, clip_id);
                }
                UiEvent::ClipAdded { clip } => {
                    self.add_clip_shape(clip);
                }
                UiEvent::ClipUpdated { clip } => {
                    self.update_clip_shape(clip);
                }
                UiEvent::CaptureInserted {
                    clip_id,
                    pitch_range,
                } => {
                    self.reframe_after_capture(clip_id, pitch_range);
                }
                UiEvent::ClipRetimed { clip_id, retime } => {
                    self.follow_clip_retime(clip_id, retime);
                }
                UiEvent::ClipEntered { clip_view } => {
                    // Shown first: the home framing measures the pane.
                    self.render.clip_panel.visible = true;
                    self.show_lead_clip(clip_view);
                    self.clear_gestures();
                }
                UiEvent::ClipExited => {
                    // Leaving the clip view hides the panel.
                    self.render.clip_panel.visible = false;
                    self.render.event_shapes.clear();
                    // The arranger keeps its own scroll; follow re-arms, so
                    // it pages to the cursor if the clip view moved it off
                    // screen.
                    self.render.clip_scroll_x = 0.;
                    self.rearm_arranger_follow();
                    self.reset_clip_zoom();
                    self.clear_gestures();
                }
                UiEvent::ClipRemoved { clip } => {
                    self.remove_clip_shape(clip.track_idx, clip.clip_id);
                }
                UiEvent::EventSelected { event_id } => {
                    self.set_event_shape_selected(event_id, true);
                }
                UiEvent::EventDeselected { event_id } => {
                    self.set_event_shape_selected(event_id, false);
                }
                UiEvent::LeadClipChanged { clip_view } => {
                    self.show_lead_clip(clip_view);
                    self.clear_event_marquee();
                }
                UiEvent::EventsUpdated { clip_view } => {
                    self.lead_clip_time = Some(ClipTimeAtomics::of(&clip_view));
                    // Refresh arranger thumbnail before events are consumed.
                    let region_start = clip_view.region_start.load(Ordering::Relaxed);
                    let region_end = clip_view.region_end.load(Ordering::Relaxed);
                    let thumbnails =
                        thumbnails_from_event_metadata(&clip_view.events, region_start, region_end);
                    if let Some(shape) = self
                        .render
                        .clip_shapes
                        .iter_mut()
                        .find(|s| s.clip_id() == clip_view.clip_id)
                    {
                        shape.update_note_thumbnails(thumbnails);
                    }
                    self.rebuild_event_shapes(clip_view.events);
                    // No re-fit or scroll here: every note edit and every
                    // `[`/`]` lands here, and the view stays exactly where it
                    // is — only the shading changes.
                    // Gated on the selection mirror, as the old `ClipEdit`
                    // view gated it, so a highlight is never shown that the
                    // keys wouldn't act on. (A capture commit used to select
                    // a note without publishing it; commits no longer touch
                    // the selection at all.)
                    if self.view_state().is_clip_view() && self.has_event_selection() {
                        let selected: HashSet<Uuid> =
                            clip_view.selected_event_ids.into_iter().collect();
                        for shape in &mut self.render.event_shapes {
                            if selected.contains(&shape.event_id()) {
                                shape.set_selected(true);
                            }
                        }
                    }
                }
                UiEvent::ProjectLoaded {
                    clips,
                    filename,
                    folder,
                } => {
                    // A new or loaded project is a fresh start: it lands in
                    // the arranger with no modal overlay up.
                    self.close_overlay();
                    // A loaded project's folder becomes current; a new
                    // project goes where the current folder is.
                    if filename.is_some() {
                        self.set_current_project_folder(folder);
                    }
                    self.project.project_current_name = filename;
                    self.render.clip_shapes.clear();
                    self.render.event_shapes.clear();
                    self.render.arranger_scroll_x = 0.;
                    self.render.clip_scroll_x = 0.;
                    // The zoom itself is kept (a preference), but `X` must not
                    // step back to framings of the previous project.
                    self.render.arranger_zoom_history.clear();
                    self.rearm_arranger_follow();
                    self.reset_clip_zoom();
                    self.lead_clip_time = None;
                    self.clear_gestures();
                    for clip in clips {
                        self.add_clip_shape(clip);
                    }
                }
                UiEvent::UnsavedChanges { action } => self.open_unsaved_prompt(action),
                UiEvent::QuitApproved => self.quit_approved = true,
                UiEvent::MidiPortsRefreshed {
                    in_ports,
                    out_ports,
                } => {
                    self.midi.input.set_ports(in_ports);
                    self.midi.output.set_ports(out_ports);
                }
                UiEvent::TracksChanged { tracks, shift } => {
                    self.tracks = tracks;
                    if let Some(shift) = shift {
                        self.follow_track_shift(shift);
                    }
                    // The selected track's slot may be new (a load).
                    #[cfg(target_os = "macos")]
                    self.sync_live_instrument_target();
                }
                UiEvent::TrackInstrumentRemoved { slot, track_id } => {
                    #[cfg(target_os = "macos")]
                    self.park_track_instrument(slot, track_id);
                    #[cfg(not(target_os = "macos"))]
                    let _ = (slot, track_id);
                }
                UiEvent::TrackInstrumentRestored {
                    slot,
                    track_id,
                    instrument,
                } => {
                    #[cfg(target_os = "macos")]
                    self.restore_slot_instrument(slot, Some(track_id), &instrument);
                    #[cfg(not(target_os = "macos"))]
                    let _ = (slot, track_id, instrument);
                }
                UiEvent::TrackInstrumentsChanged { specs } => {
                    #[cfg(target_os = "macos")]
                    self.sync_instruments_to_tracks(&specs);
                    #[cfg(not(target_os = "macos"))]
                    let _ = specs;
                }
            }
        }
    }
}

impl Display {
    /// A track was added or removed (`UiEvent::TracksChanged`'s `shift`):
    /// moves every positional track index the view holds with it — the clip
    /// shapes (a removed track's go), the selected track and the marquee's
    /// track span on an add (a remove clears the marquee, with every other
    /// gesture). The selection lands where the sequencer puts it — the added
    /// track; after a remove the same track, or the one that took the removed
    /// selected track's place — and the collapse watcher is re-latched to it,
    /// so the edit's own selection move never reads as the user's (an add
    /// keeps the marquee). Any drag in flight, the output menu and an open
    /// rename field are dropped either way: they name a track by position. Engine-side state
    /// (faders, plugins) is keyed by slot and needs nothing.
    fn follow_track_shift(&mut self, shift: TrackShift) {
        self.render.clip_shapes.retain_mut(|shape| {
            let moved = shift.apply(shape.track_idx());
            if let Some(track_idx) = moved {
                shape.set_track_idx(track_idx);
            }
            moved.is_some()
        });
        self.gesture.output_menu = None;
        self.close_track_rename();
        let time_selection = self.gesture.time_selection;
        self.clear_gestures();
        self.selected_track_idx = match shift {
            TrackShift::Inserted(at) => {
                self.gesture.time_selection = time_selection.and_then(|mut rect| {
                    (rect.track_start, rect.track_end) =
                        shift.apply_span((rect.track_start, rect.track_end))?;
                    Some(rect)
                });
                at
            }
            TrackShift::Removed(at) => shift
                .apply(self.selected_track_idx)
                .unwrap_or_else(|| at.min(self.track_count() - 1)),
        };
        self.gesture.last_selected_track_idx = Some(self.selected_track_idx);
    }
}

/// Compute note thumbnail (x_start_frac, x_end_frac, pitch_frac) triples from
/// arranger-side event metadata. Used to refresh arranger clip shapes when
/// events change without a full clip rebuild. Delegates the actual overlap
/// filter/clamp math to `note_thumbnails_from_spans` (`clip_metadata.rs`) —
/// `events` is the piano roll's own unfiltered snapshot (every `NoteOn` on the
/// clip, including a sibling split half's, by design — see `ClipView::from_clip`),
/// so drawing it straight through without that filter is exactly the stale
/// bug that function's doc warns about.
fn thumbnails_from_event_metadata(
    events: &[EventMetadata],
    region_start: i32,
    region_end: i32,
) -> Vec<(f32, f32, f32)> {
    note_thumbnails_from_spans(
        events
            .iter()
            .filter(|e| !e.muted)
            .map(|e| (e.start_tick, e.end_tick, e.note_number)),
        region_start,
        region_end,
    )
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{EventMetadata, thumbnails_from_event_metadata};

    fn note(start_tick: i32, end_tick: i32, note_number: u8) -> EventMetadata {
        EventMetadata {
            id: Uuid::new_v4(),
            start_tick,
            end_tick,
            note_number,
            velocity: 100,
            muted: false,
        }
    }

    /// Regression: the piano-roll snapshot handed to `EventsUpdated`
    /// (`ClipView::from_clip`) includes every `NoteOn` on the clip
    /// unfiltered — including a chord that belongs entirely to the other
    /// half of a split, sitting before this clip's own region start. It must
    /// not draw as a cluster of near-zero-width notes clamped to the left
    /// edge (the bug `note_thumbnails_from_spans` exists to prevent).
    #[test]
    fn thumbnails_from_event_metadata_excludes_a_sibling_splits_chord_before_region_start() {
        let events = vec![
            note(0, 100, 60),   // sibling half, no overlap into [500, 1000)
            note(10, 110, 64),  // sibling half, no overlap
            note(600, 700, 67), // belongs to this region
        ];

        let thumbnails = thumbnails_from_event_metadata(&events, 500, 1000);

        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].0, (600 - 500) as f32 / 500.0);
    }

    #[test]
    fn thumbnails_from_event_metadata_excludes_muted_notes() {
        let mut muted = note(600, 700, 67);
        muted.muted = true;
        let events = vec![note(500, 600, 60), muted];

        let thumbnails = thumbnails_from_event_metadata(&events, 500, 1000);

        assert_eq!(thumbnails.len(), 1);
    }
}
