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
            transport.set_cursor_tick(tick);
            // A cursor move places the lane's hand (`570` § 2).
            sequencer.place_performance_lane_hand();
            self.sync_clip_selection_to_cursor_workflow(sequencer);
        }
        self.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use undo::Record;

    use crate::core::event_handlers::test_harness::{
        Harness, harness, muted_metronome, run_command, statuses, transport,
    };
    use crate::core::sequencer::{SequencerCommand, UndoStep};
    use crate::core::time::Meter;
    use crate::core::transport::{Transport, TransportCommand};

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
}
