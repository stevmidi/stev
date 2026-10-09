//! `EventHandlers` workflows for the track list itself: the view's mirror of
//! it (`UiEvent::TracksChanged`), and adding / removing a track — the
//! refusals, and the fan-out after `AddTrackEdit` / `RemoveTrackEdit` (or
//! the undo of either). A rename (`RenameTrackEdit`) only re-sends the
//! mirror. See `050-undo-redo.md` § Track add / remove and
//! `020-views-and-state.md`.

use undo::Record;

use crate::{
    core::{
        config::MAX_TRACKS,
        sequencer::{AddTrackEdit, RemoveTrackEdit, SequencerEdit},
    },
    models::track::{InstrumentRef, TrackOutput, TrackShift},
    view::display::{TrackLane, TrackRoute},
};

use super::*;

impl EventHandlers {
    /// Sends `Display` the whole track list — each track's slot, output,
    /// colour and name — with the add / remove that just happened, if any
    /// (`shift`). After a project load / new-project, every `SetTrackOutput`,
    /// every track add / remove and every rename.
    pub(super) fn emit_tracks(&self, sequencer: &Sequencer, shift: Option<TrackShift>) {
        let tracks = sequencer
            .tracks()
            .iter()
            .map(|track| TrackLane {
                slot: track.slot(),
                route: match track.output() {
                    TrackOutput::MidiOut { channel } => TrackRoute::MidiOut { channel: *channel },
                    TrackOutput::Instrument(r) => TrackRoute::Instrument {
                        name: r.display_name.clone(),
                    },
                },
                color: track.color_slot(),
                name: track.name().map(str::to_owned),
            })
            .collect();
        self.ui_event_tx
            .send(UiEvent::TracksChanged { tracks, shift })
            .ok();
    }

    /// ⌘T / the `+` row: records an `AddTrackEdit` at `track_idx` (after the
    /// selected track when `None`). Refused, with a footer message, at
    /// `MAX_TRACKS` and during a live take (whose session names its track by
    /// position).
    pub(super) fn add_track_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        track_idx: Option<usize>,
    ) {
        if sequencer.is_recording() {
            self.send_status_ui_event("Stop recording to add a track".to_owned());
            return;
        }
        let edit = AddTrackEdit::new(sequencer, track_idx);
        if edit.is_none() {
            self.send_status_ui_event(format!("{MAX_TRACKS} tracks is the most a project holds"));
            return;
        }
        self.record_edit(sequencer, undo_record, edit);
    }

    /// Delete in track header focus / the output menu's `Delete track`: records a `RemoveTrackEdit` for
    /// `track_idx` (the selected track when `None`). Refused, with a footer
    /// message, on the last remaining track and during a live take.
    pub(super) fn remove_track_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        track_idx: Option<usize>,
    ) {
        if sequencer.is_recording() {
            self.send_status_ui_event("Stop recording to delete a track".to_owned());
            return;
        }
        let edit = RemoveTrackEdit::new(sequencer, track_idx);
        if edit.is_none() {
            if sequencer.tracks().len() <= 1 {
                self.send_status_ui_event("A project keeps at least one track".to_owned());
            }
            return;
        }
        self.record_edit(sequencer, undo_record, edit);
    }

    /// A track is in the arrangement at `track_idx` (`EditResult::TrackAdded`):
    /// the view's mirror first (with the shift, so its positional state —
    /// the selection included — follows), then the track's clip shapes (a
    /// restored track has some), its plugin reload into `slot`, and last its
    /// selection.
    pub(super) fn track_added_workflow(
        &self,
        sequencer: &mut Sequencer,
        track_idx: usize,
        track_id: Uuid,
        slot: usize,
        instrument: Option<InstrumentRef>,
    ) {
        self.emit_tracks(sequencer, Some(TrackShift::Inserted(track_idx)));
        let clips: Vec<_> = sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .map(|clip| ClipMetadata::from_clip(track_idx, clip))
            .collect();
        self.send_clips_added_ui_event(&clips);
        if let Some(instrument) = instrument {
            self.ui_event_tx
                .send(UiEvent::TrackInstrumentRestored {
                    slot,
                    track_id,
                    instrument,
                })
                .ok();
        }
        self.select_track_workflow(sequencer, track_idx);
    }

    /// The track at `track_idx` left the arrangement
    /// (`EditResult::TrackRemoved`): out of the clip view if it showed one of
    /// its clips, the view's mirror (with the shift — the view drops the
    /// track's clip shapes and the marquee, and moves its selection), its
    /// plugin in `slot` torn down. If it was the selected track, the one that
    /// took its place is selected (the one above when it was the last);
    /// otherwise the selection stays on its track.
    pub(super) fn track_removed_workflow(
        &self,
        sequencer: &mut Sequencer,
        track_idx: usize,
        track_id: Uuid,
        slot: usize,
    ) {
        let was_selected = sequencer.selected_track_id() == Some(track_id);
        if was_selected && self.view_state() == ViewState::Clip {
            self.exit_clip_workflow(sequencer);
        }
        self.emit_tracks(sequencer, Some(TrackShift::Removed(track_idx)));
        self.ui_event_tx
            .send(UiEvent::TrackInstrumentRemoved { slot, track_id })
            .ok();
        if was_selected {
            let select_idx = track_idx.min(sequencer.tracks().len() - 1);
            self.select_track_workflow(sequencer, select_idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use undo::Record;

    use crate::core::config::MAX_TRACKS;
    use crate::core::event_handlers::test_harness::{Harness, harness, run_command};
    use crate::core::sequencer::SequencerCommand;
    use crate::core::sequencer::test_support::stub_instrument;
    use crate::models::track::TrackShift;
    use crate::view::display::UiEvent;

    /// Everything the handlers have sent the view so far.
    fn ui_events(h: &Harness) -> Vec<UiEvent> {
        h.ui_events.try_iter().collect()
    }

    /// The footer message among `events`, if any.
    fn status(events: &[UiEvent]) -> Option<&str> {
        events.iter().find_map(|event| match event {
            UiEvent::Status { message } => Some(message.as_str()),
            _ => None,
        })
    }

    /// Where in `events` the first one `pick` matches sits.
    fn position(events: &[UiEvent], pick: impl Fn(&UiEvent) -> bool) -> usize {
        events.iter().position(pick).expect("event sent")
    }

    #[test]
    fn add_mirrors_the_tracks_with_the_shift_before_selecting_the_new_track() {
        let mut h = harness();
        let mut record = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );

        let events = ui_events(&h);
        let mirror = position(
            &events,
            |e| matches!(e, UiEvent::TracksChanged { tracks, shift: Some(TrackShift::Inserted(1)) } if tracks.len() == 5),
        );
        let selected = position(&events, |e| {
            matches!(e, UiEvent::TrackSelected { track_idx: 1 })
        });
        assert!(mirror < selected, "the view's mirror must grow first");
        assert_eq!(h.sequencer.selected_track_index(), Some(1));
        assert_eq!(record.len(), 1);
    }

    #[test]
    fn a_rename_mirrors_the_new_name_without_a_shift_and_undo_takes_it_back() {
        let mut h = harness();
        let mut record = Record::new();
        let rename = || SequencerCommand::RenameTrack {
            track_idx: 2,
            name: Some("Pads".to_owned()),
        };
        run_command(&mut h, &mut record, rename());

        let named = |events: &[UiEvent], name: Option<&str>| {
            events.iter().any(|e| {
                matches!(e, UiEvent::TracksChanged { tracks, shift: None } if tracks[2].name.as_deref() == name)
            })
        };
        assert!(named(&ui_events(&h), Some("Pads")));

        // The same name again (the field doesn't check): nothing sent.
        run_command(&mut h, &mut record, rename());
        assert!(ui_events(&h).is_empty());

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(named(&ui_events(&h), None));
    }

    #[test]
    fn add_at_the_cap_and_remove_of_the_last_track_are_refused_with_a_message() {
        let mut h = harness();
        let mut record = Record::new();
        h.sequencer.set_track_count(MAX_TRACKS);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AddTrack { track_idx: None },
        );
        let events = ui_events(&h);
        assert!(status(&events).is_some());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, UiEvent::TracksChanged { .. }))
        );

        h.sequencer.set_track_count(1);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::RemoveTrack { track_idx: None },
        );
        assert!(status(&ui_events(&h)).is_some());
        assert_eq!(h.sequencer.tracks().len(), 1);
        assert!(record.is_empty());
    }

    #[test]
    fn deleting_the_selected_last_track_selects_the_one_above() {
        let mut h = harness();
        let mut record = Record::new();
        let last = h.sequencer.track_id_by_index(3);
        h.sequencer.select_track(last);

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::RemoveTrack { track_idx: None },
        );

        assert_eq!(h.sequencer.selected_track_index(), Some(2));
        assert!(
            ui_events(&h)
                .iter()
                .any(|e| matches!(e, UiEvent::TrackSelected { track_idx: 2 }))
        );
    }

    #[test]
    fn deleting_another_track_keeps_the_selection_on_its_track() {
        let mut h = harness();
        let mut record = Record::new();
        let selected = h.sequencer.track_id_by_index(3);
        h.sequencer.select_track(selected);

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::RemoveTrack { track_idx: Some(1) },
        );

        assert_eq!(h.sequencer.selected_track_id(), selected);
        // It moved up one, which the view follows from the shift alone.
        let events = ui_events(&h);
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::TracksChanged {
                shift: Some(TrackShift::Removed(1)),
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, UiEvent::TrackSelected { .. }))
        );
    }

    #[test]
    fn removing_a_plugin_track_tears_its_plugin_down_and_undo_reloads_it() {
        let mut h = harness();
        let mut record = Record::new();
        h.sequencer.set_track_output(2, stub_instrument(vec![7]));
        let removed_id = h.sequencer.track_id_by_index(2).unwrap();
        h.sequencer.select_track(Some(removed_id));

        // Delete in header focus: the selected track.
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::RemoveTrack { track_idx: None },
        );
        let events = ui_events(&h);
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::TracksChanged { tracks, shift: Some(TrackShift::Removed(2)) } if tracks.len() == 3
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::TrackInstrumentRemoved { slot: 2, track_id } if *track_id == removed_id
        )));
        // The track that took its place is selected.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::TrackSelected { track_idx: 2 }))
        );

        run_command(&mut h, &mut record, SequencerCommand::Undo);
        let events = ui_events(&h);
        let mirror = position(&events, |e| {
            matches!(
                e,
                UiEvent::TracksChanged {
                    shift: Some(TrackShift::Inserted(2)),
                    ..
                }
            )
        });
        let reload = position(
            &events,
            |e| matches!(e, UiEvent::TrackInstrumentRestored { slot: 2, track_id, instrument } if *track_id == removed_id && instrument.state == vec![7]),
        );
        assert!(mirror < reload, "the plugin reloads into a mirrored track");
        assert_eq!(h.sequencer.track_id_by_index(2), Some(removed_id));
    }
}
