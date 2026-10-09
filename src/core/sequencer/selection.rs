//! Track, clip and event selection on the `Sequencer`, plus the note queries
//! that read the current selection.
//!
//! The three [`Selection`](crate::models::selection::Selection)s
//! (`track_selection`, `clip_selection`, and the per-clip event selection) are
//! model state, never undoable. Selecting a track also re-arms live MIDI input
//! ([`arm_selected_track`](Sequencer::arm_selected_track)).

use std::sync::atomic::Ordering;

use uuid::Uuid;

use crate::core::{config::MARQUEE_AUDITION_MAX_NOTES, input_event::TimeSelectionRect};

use crate::models::{
    clip::Clip,
    event::{Event, EventType},
    track::{Track, TrackOutput},
};

use super::Sequencer;

impl Sequencer {
    // --- Track selection ---

    /// Selects a track by id (`None` clears), re-arms live input, and returns
    /// the previously selected id.
    pub(crate) fn select_track(&mut self, id: Option<Uuid>) -> Option<Uuid> {
        let old_id = self.track_selection.select(id);
        self.arm_selected_track();
        old_id
    }

    /// Id of the selected track, if any.
    pub(crate) fn selected_track_id(&self) -> Option<Uuid> {
        self.track_selection.selected_id()
    }

    /// Index of the selected track in `tracks`, if any.
    pub(crate) fn selected_track_index(&self) -> Option<usize> {
        let track_id = self.track_selection.selected_id()?;
        self.tracks.iter().position(|t| t.id() == track_id)
    }

    /// Id of the track at `idx`, if in range.
    pub(crate) fn track_id_by_index(&self, idx: usize) -> Option<Uuid> {
        self.tracks.get(idx).map(|t| t.id())
    }

    // --- Track output ---

    /// Re-routes `track` and re-arms live input, which follows the selected
    /// track's channel. No-op on an out-of-range index.
    pub(crate) fn set_track_output(&mut self, track: usize, output: TrackOutput) {
        if let Some(track) = self.tracks.get_mut(track) {
            track.set_output(output);
        }
        self.arm_selected_track();
    }

    /// Stores the latest plugin-state blob on the instrument of the track in
    /// engine slot `slot` so the next project save persists the active preset.
    /// No-op if no track holds the slot or it is not routed to an instrument.
    /// macOS plugin host — see `130-plugin-host.md`.
    pub(crate) fn set_slot_instrument_state(&mut self, slot: usize, state: Vec<u8>) {
        if let Some(TrackOutput::Instrument(r)) = self
            .tracks
            .iter_mut()
            .find(|t| t.slot() == slot)
            .map(|t| t.output_mut())
        {
            r.state = state;
        }
    }

    // --- Clip selection ---

    /// Selects a clip by id (`None` clears); returns the previous lead id.
    pub(crate) fn select_clip(&mut self, id: Option<Uuid>) -> Option<Uuid> {
        self.clip_selection.select(id)
    }

    /// Id of the lead-selected clip, if any.
    pub(crate) fn selected_clip_id(&self) -> Option<Uuid> {
        self.clip_selection.selected_id()
    }

    /// `(track index, clip id)` of every clip overlapping the marquee's tick
    /// range on its tracks (`Track::find_clip_ids_in`), track by track. Tracks
    /// past the last one are skipped.
    pub(crate) fn clip_ids_in(
        &self,
        rect: TimeSelectionRect,
    ) -> impl Iterator<Item = (usize, Uuid)> + '_ {
        (rect.track_start..=rect.track_end).flat_map(move |track_idx| {
            self.tracks()
                .get(track_idx)
                .map(|track| track.find_clip_ids_in(rect.start, rect.end))
                .unwrap_or_default()
                .into_iter()
                .map(move |id| (track_idx, id))
        })
    }

    /// Clears the clip selection, returning the ids that were selected.
    pub(crate) fn clear_clip_selection(&mut self) -> Vec<Uuid> {
        self.clip_selection.clear_selection()
    }

    /// Id of the clip under the cursor on the selected track, if any.
    pub(crate) fn find_selected_track_clip_id_at_cursor(&self) -> Option<Uuid> {
        self.selected_track()
            .and_then(|track| track.find_clip_id_at(self.cursor_tick()))
    }

    /// Selected clip's region start, in event ticks.
    pub(crate) fn selected_clip_region_start(&self) -> Option<i32> {
        self.selected_clip().map(|c| c.region().start())
    }

    /// Selected clip's region end, in event ticks.
    pub(crate) fn selected_clip_region_end(&self) -> Option<i32> {
        self.selected_clip().map(|c| c.region().end())
    }

    // --- Event selection ---

    /// Selects one event in the open clip (`None` clears); returns the previous
    /// lead id.
    pub(crate) fn select_event(&mut self, id: Option<Uuid>) -> Option<Uuid> {
        let selected_clip = self.selected_clip_mut()?;
        selected_clip.select_event(id)
    }

    /// Selects every `NoteOn` in the open clip; returns the newly added ids.
    pub(crate) fn select_all_events(&mut self) -> Option<Vec<Uuid>> {
        let selected_clip = self.selected_clip_mut()?;
        Some(selected_clip.select_all_events())
    }

    /// Clears the open clip's event selection, returning the deselected ids.
    pub(crate) fn clear_event_selection(&mut self) -> Option<Vec<Uuid>> {
        let selected_clip = self.selected_clip_mut()?;
        Some(selected_clip.clear_event_selection())
    }

    /// Recomputes the open clip's event selection from a marquee rectangle;
    /// returns `(newly deselected, newly selected)`.
    pub(crate) fn select_events_in_rect(
        &mut self,
        tick_min: i32,
        tick_max: i32,
        note_min: u8,
        note_max: u8,
    ) -> Option<(Vec<Uuid>, Vec<Uuid>)> {
        let selected_clip = self.selected_clip_mut()?;
        Some(selected_clip.select_events_in_rect(tick_min, tick_max, note_min, note_max))
    }

    /// Lead-selected event id in the open clip, if any.
    pub(crate) fn selected_event_id(&self) -> Option<Uuid> {
        self.selected_clip()?.selected_event_id()
    }

    // --- Note queries ---

    /// Returns `(channel, note, velocity)` for the first (earliest-tick) selected NoteOn event.
    pub(crate) fn first_selected_note_on(&self) -> Option<(u8, u8, u8)> {
        let track = self.selected_track()?;
        let clip = self.selected_clip()?;
        let selected_ids = clip.selected_event_ids();
        let event = clip
            .events()
            .iter()
            .filter(|e| selected_ids.contains(&e.id()) && e.event_type() == Some(EventType::NoteOn))
            .min_by_key(|e| e.tick())?;
        note_on_tuple(track, event)
    }

    /// Returns `(channel, note, velocity)` for each `NoteOn` among `ids` that a
    /// marquee update should audition — [`Clip::audition_note_ons`] capped at
    /// `MARQUEE_AUDITION_MAX_NOTES`.
    pub(crate) fn marquee_audition_note_ons(&self, ids: &[Uuid]) -> Vec<(u8, u8, u8)> {
        let (Some(track), Some(clip)) = (self.selected_track(), self.selected_clip()) else {
            return Vec::new();
        };
        if ids.is_empty() {
            return Vec::new();
        }
        clip.audition_note_ons(ids, MARQUEE_AUDITION_MAX_NOTES)
            .into_iter()
            .filter_map(|event| note_on_tuple(track, event))
            .collect()
    }

    /// Returns `(channel, note, velocity)` for the most recently selected NoteOn event.
    pub(crate) fn selected_note_on(&self) -> Option<(u8, u8, u8)> {
        let track = self.selected_track()?;
        let clip = self.selected_clip()?;
        let event_id = self.selected_event_id()?;
        let event = clip
            .events()
            .iter()
            .find(|e| e.id() == event_id && e.event_type() == Some(EventType::NoteOn))?;
        note_on_tuple(track, event)
    }

    // --- Private helpers ---

    /// The selected [`Track`], if any.
    pub(super) fn selected_track(&self) -> Option<&Track> {
        self.tracks.get(self.selected_track_index()?)
    }

    /// Mutable access to the selected track.
    pub(super) fn selected_track_mut(&mut self) -> Option<&mut Track> {
        let idx = self.selected_track_index()?;
        self.tracks.get_mut(idx)
    }

    /// The lead-selected [`Clip`] on the selected track, if any.
    pub(crate) fn selected_clip(&self) -> Option<&Clip> {
        let clip_id = self.clip_selection.selected_id()?;
        self.selected_track()
            .and_then(|track| track.get_clip_by_id(clip_id))
    }

    /// Mutable access to the lead-selected clip. Not for moving its events:
    /// that goes through [`edit_clip_events`](Self::edit_clip_events), or a
    /// note sounding under a running playhead can hang.
    pub(super) fn selected_clip_mut(&mut self) -> Option<&mut Clip> {
        let clip_id = self.clip_selection.selected_id()?;
        self.selected_track_mut()
            .and_then(|track| track.get_clip_by_id_mut(clip_id))
    }

    /// Updates `arm_channel` to reflect the currently selected track,
    /// so `MidiInputForwarder` immediately routes live input correctly.
    /// Called on every `select_track()` and `set_track_output()`.
    pub(super) fn arm_selected_track(&self) {
        if let Some(track) = self.selected_track() {
            let channel = match track.output() {
                TrackOutput::MidiOut { channel } => *channel,
                TrackOutput::Instrument(_) => 0,
            };
            self.arm_channel.store(channel, Ordering::Relaxed);
        }
    }
}

/// `(channel, note, velocity)` for auditioning a selected `NoteOn` on
/// `track` — velocity defaulted to 100 when missing and floored to 1, so the
/// preview is never a note-off.
fn note_on_tuple(track: &Track, event: &Event) -> Option<(u8, u8, u8)> {
    Some((
        track.midi_channel(),
        event.note_number()?,
        event.velocity().unwrap_or(100).max(1),
    ))
}

#[cfg(test)]
mod tests {
    use crate::models::track::TrackOutput;

    use super::super::test_support::test_sequencer;
    use super::*;

    #[test]
    fn set_track_output_routes_any_track_and_arms_the_selected_ones_channel() {
        let mut seq = test_sequencer();
        seq.select_track(seq.track_id_by_index(0));
        seq.set_track_output(0, TrackOutput::MidiOut { channel: 3 });
        assert_eq!(seq.arm_channel.load(Ordering::Relaxed), 3);

        // Routing another track leaves the armed channel with the selected one.
        seq.set_track_output(2, TrackOutput::MidiOut { channel: 9 });
        assert!(matches!(
            seq.tracks()[2].output(),
            TrackOutput::MidiOut { channel: 9 }
        ));
        assert_eq!(seq.arm_channel.load(Ordering::Relaxed), 3);

        // Out of range: nothing changes.
        seq.set_track_output(99, TrackOutput::MidiOut { channel: 5 });
        assert_eq!(seq.arm_channel.load(Ordering::Relaxed), 3);
    }
}
