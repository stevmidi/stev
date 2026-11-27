//! `EventHandlers` workflows for the arrangement's markers
//! (`600-markers.md`): the view's mirror of them (`UiEvent::MarkersChanged`)
//! and a marker's recall. Setting one is an edit (`SetMarkerEdit`, recorded
//! in `handle_sequencer_command`); the time edits that carry them re-send
//! the mirror from their `handle_edit_result` arms.

use crate::models::performance_lane::phase_offset;

use super::*;

impl EventHandlers {
    /// Sends `Display` the markers as they now are: after every edit that
    /// can change them and a project load. Nine entries at most.
    pub(crate) fn emit_markers(&self, sequencer: &Sequencer) {
        self.ui_event_tx
            .send(UiEvent::MarkersChanged {
                markers: sequencer.markers().clone(),
            })
            .ok();
        self.request_repaint();
    }

    /// `1`–`9`, or a click on a flag: recalls marker `number`
    /// (`600-markers.md` decision 4). Stopped, the arranger cursor moves to
    /// it, as any cursor move: the lane's hand and the lead clip follow, and
    /// the view follows the cursor. Running, playback jumps to it by the
    /// lane's phase jump (`550-phase-jumps.md`): the marker plus playback's
    /// offset from the nearest beat, through the same transport path as a
    /// lane press, so the beat never breaks; the cursor stays. The loop is
    /// not consulted: a jump past its end plays on (`Transport::tick` wraps
    /// only from inside the region, decision 5). An unset marker moves
    /// nothing and says so.
    pub(super) fn recall_marker_workflow(
        &self,
        number: u8,
        transport: &mut Transport,
        sequencer: &mut Sequencer,
    ) {
        let Some(tick) = sequencer.markers().get(number).map(|marker| marker.tick) else {
            self.send_status_ui_event(format!("No marker {number}"));
            return;
        };
        if transport.is_running() {
            let offset = phase_offset(sequencer.meter(), sequencer.playback_tick());
            transport.jump_and_chase(tick + offset);
        } else {
            self.set_cursor_workflow(tick, transport, sequencer);
        }
        self.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::event_handlers::test_harness::{
        Harness, harness, muted_metronome, run_command, statuses, transport, transport_with_events,
    };
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::transport::{Transport, TransportCommand};
    use crate::models::markers::MarkerChange;
    use crate::view::display::UiEvent;

    /// Runs `RecallMarker { number }` through the handlers.
    fn recall(h: &mut Harness, transport: &mut Transport, number: u8) {
        h.handlers.handle_transport_command(
            &TransportCommand::RecallMarker { number },
            transport,
            &mut h.sequencer,
            &muted_metronome(),
        );
    }

    /// Stopped, a recall moves the cursor to the marker and leaves playback
    /// parked there with it.
    #[test]
    fn a_recall_while_stopped_moves_the_cursor() {
        let mut h = harness();
        let mut transport = transport(&h);
        let mut record: Record<UndoStep> = Record::new();
        h.sequencer.set_cursor_tick(3840);
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetMarker { number: 2 },
        );
        h.sequencer.set_cursor_tick(0);

        recall(&mut h, &mut transport, 2);
        assert_eq!(h.sequencer.cursor_tick(), 3840);
        assert!(!transport.is_running());
    }

    /// Running, a recall jumps playback to the marker plus its offset from
    /// the nearest beat, and the cursor stays where it was.
    #[test]
    fn a_recall_while_running_jumps_playback_on_the_beat() {
        let mut h = harness();
        let mut transport = transport(&h);
        h.sequencer.markers_mut().set(1, 7680);
        h.sequencer.set_cursor_tick(960);
        transport.start();
        let beat = Meter::FOUR_FOUR.beat_ticks();
        // 10 ticks past beat 2.
        h.playback_tick.store(beat + 10, Ordering::Relaxed);

        recall(&mut h, &mut transport, 1);
        assert_eq!(h.playback_tick.load(Ordering::Relaxed), 7680 + 10);
        assert_eq!(h.sequencer.cursor_tick(), 960);
        assert!(transport.is_running());
    }

    #[test]
    fn recalling_an_unset_marker_moves_nothing_and_says_so() {
        let mut h = harness();
        let mut transport = transport(&h);
        h.sequencer.set_cursor_tick(480);

        recall(&mut h, &mut transport, 4);
        assert_eq!(h.sequencer.cursor_tick(), 480);
        assert_eq!(statuses(&h), vec!["No marker 4".to_owned()]);
    }

    /// `⌘n` sets marker `n` at the cursor as one undo step, and the undo
    /// takes it away again.
    #[test]
    fn setting_a_marker_is_undoable() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        h.sequencer.set_cursor_tick(1920);

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::SetMarker { number: 3 },
        );
        assert_eq!(h.sequencer.markers().get(3).map(|m| m.tick), Some(1920));
        run_command(&mut h, &mut record, SequencerCommand::Undo);
        assert!(h.sequencer.markers().is_empty());
        run_command(&mut h, &mut record, SequencerCommand::Redo);
        assert_eq!(h.sequencer.markers().get(3).map(|m| m.tick), Some(1920));
    }

    /// A strip change the view already made in its mirror, which makes no
    /// edit here (the mirror was behind), still sends the list back, so the
    /// view's copy can't be left wrong; one that edits sends it too.
    #[test]
    fn a_strip_change_always_sends_the_markers_back() {
        let mut h = harness();
        let mut record: Record<UndoStep> = Record::new();
        h.sequencer.markers_mut().set(1, 960);
        for (change, steps) in [(MarkerChange::Move(960), 0), (MarkerChange::Move(1920), 1)] {
            run_command(
                &mut h,
                &mut record,
                SequencerCommand::ChangeMarker { number: 1, change },
            );
            assert_eq!(record.len(), steps);
            let sent: Vec<_> = h
                .ui_events
                .try_iter()
                .filter_map(|event| match event {
                    UiEvent::MarkersChanged { markers } => Some(markers.placed()),
                    _ => None,
                })
                .collect();
            assert_eq!(sent.last(), Some(&h.sequencer.markers().placed()));
        }
    }

    /// A recall in the middle of a running take is a jump like a lane
    /// press: the take survives it, and the `/` after it still lands the
    /// notes as one clip, one undo step.
    #[test]
    fn a_recall_mid_take_leaves_the_take_to_commit() {
        let mut h = harness();
        let (mut transport, events) = transport_with_events(&h);
        let bar = Meter::FOUR_FOUR.bar_ticks();
        h.region.0.store(0, Ordering::Relaxed);
        h.region.1.store(bar * 2, Ordering::Relaxed);
        h.sequencer.markers_mut().set(1, bar);
        transport.start();
        for i in 0..4 {
            let t = 300 + i * 960;
            h.sequencer.handle_midi_input(&[0x90, 60, 100], t);
            h.sequencer.handle_midi_input(&[0x80, 60, 0], t + 480);
        }
        h.playback_tick.store(3900, Ordering::Relaxed);

        recall(&mut h, &mut transport, 1);
        for event in events.try_iter() {
            h.handlers
                .handle_transport_event(&event, &mut h.sequencer, &transport);
        }
        let mut record: Record<UndoStep> = Record::new();
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::Commit { frame: None },
        );

        assert_eq!(record.len(), 1);
        let clips = h.sequencer.tracks()[0].clips();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].events().len(), 8, "four notes, on and off");
    }
}
