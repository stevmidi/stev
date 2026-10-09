//! A live-recording take: its start and end, and the notes fed into
//! `live_rec_clip` as they arrive (from the MIDI input handler in
//! `capture.rs`), each one refreshing the thumbnail the arranger draws over the
//! growing clip. A take measures
//! its length from the transport *odometer* (`elapsed_ticks`), not the position
//! clock, so a loop wrap during recording doesn't corrupt it — see
//! `150-clock-position-sync.md` and `090-live-recording.md`.

use std::{collections::HashMap, sync::atomic::Ordering};

use crate::{
    core::{
        sequencer::{
            Sequencer,
            state::{LiveRecResult, LiveRecSession},
        },
        shared_atomics::LiveRecState,
    },
    metadata::clip_metadata::{ClipMetadata, pitch_frac},
    models::{
        clip::Clip,
        event::{Event, EventType},
    },
};

impl Sequencer {
    /// `elapsed_tick` is the odometer value the take begins at; every recorded
    /// note is placed relative to it. Never a position — see
    /// [`LiveRecSession::elapsed_start_tick`].
    pub(crate) fn start_live_recording(&mut self, elapsed_tick: i32) -> Option<ClipMetadata> {
        let track_idx = self.selected_track_index()?;
        let start_tick = self.playback_tick();

        if let Ok(mut snapshot) = self.live_rec_state.thumbnail_snapshot.lock() {
            snapshot.clear();
        }

        // Share the odometer start with the renderer
        self.live_rec_state
            .elapsed_start_tick
            .store(elapsed_tick, Ordering::Relaxed);

        self.live_rec_session = Some(LiveRecSession {
            track_idx,
            rec_start_tick: start_tick,
            elapsed_start_tick: elapsed_tick,
        });

        Some(ClipMetadata {
            track_idx,
            clip_id: self.live_rec_clip.id(),
            start_tick,
            end_tick: start_tick,
            note_thumbnails: Vec::new(),
            muted: false,
        })
    }

    /// Finalizes the take: sizes the clip to `[rec_start_tick, end_tick)`
    /// (auto-end tick if one was set, else the current playback tick), pairs
    /// note lengths, detects swing, and adds it to its track. Returns
    /// [`LiveRecResult::Completed`], or `Canceled` if the take was empty or the
    /// clip couldn't be placed.
    pub(crate) fn end_live_recording(&mut self) -> Option<LiveRecResult> {
        let session = self.live_rec_session.as_ref()?;
        let (track_idx, start_tick) = (session.track_idx, session.rec_start_tick);
        let mut clip = std::mem::replace(&mut self.live_rec_clip, Clip::new());
        let end_tick = self
            .auto_end_tick
            .take()
            .unwrap_or_else(|| self.playback_tick());
        let rec_length = end_tick - start_tick;

        if rec_length <= 0 {
            self.reset_live_recording();
            return Some(LiveRecResult::Canceled {
                track_idx,
                rec_clip_id: clip.id(),
            });
        }

        clip.set_start_tick(start_tick);
        clip.region_mut().set_region(Some(0), Some(rec_length));
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip.detect_and_store_swing();

        if let Some(track) = self.tracks.get_mut(track_idx) {
            if !track.add_clip(&clip) {
                self.reset_live_recording();
                return Some(LiveRecResult::Canceled {
                    track_idx,
                    rec_clip_id: clip.id(),
                });
            }

            self.reset_live_recording();

            return Some(LiveRecResult::Completed {
                clip: ClipMetadata::from_clip(track_idx, &clip),
            });
        }
        None
    }

    /// Clears the recording clip and session back to idle.
    fn reset_live_recording(&mut self) {
        self.live_rec_clip = Clip::new();
        self.live_rec_session = None;
    }

    /// Adds one incoming MIDI message to the take, if one is recording, and
    /// refreshes the thumbnail snapshot on a note-on or note-off. `tick` is in
    /// odometer space ([`InputTicks::elapsed`]) — the take's own start value
    /// subtracted from it gives the clip-local position directly.
    ///
    /// [`InputTicks::elapsed`]: crate::core::midi::input::InputTicks::elapsed
    pub(super) fn add_midi_event_to_live_rec(&mut self, midi_message: &[u8], tick: i32) {
        let Some(session) = &self.live_rec_session else {
            return;
        };
        let local_tick = tick - session.elapsed_start_tick;

        let event = Event::from_midi_with_tick(midi_message, local_tick);
        let (event_type, note_number, velocity) =
            (event.event_type(), event.note_number(), event.velocity());
        self.live_rec_clip.add_event(event);

        match event_type {
            Some(EventType::NoteOn) => {
                self.live_rec_state
                    .last_note_on
                    .store(note_number.unwrap_or(0), Ordering::Relaxed);
                self.live_rec_state
                    .last_note_velocity
                    .store(velocity.unwrap_or(0), Ordering::Relaxed);
            }
            Some(EventType::NoteOff) => {
                self.live_rec_state
                    .last_note_velocity
                    .store(0, Ordering::Relaxed);
            }
            None => return,
        }

        let entries = live_thumbnail_entries(self.live_rec_clip.events(), local_tick);
        if let Ok(mut snapshot) = self.live_rec_state.thumbnail_snapshot.lock() {
            *snapshot = entries;
        }
    }
}

/// The `(start_tick, end_tick, pitch_frac)` thumbnail entries for a take's
/// `events` (clip-local, in arrival order), in one pass. A note-off closes
/// every open onset of its pitch. Of the onsets still open at the end, the
/// last per pitch is the held note — its end is
/// [`LiveRecState::HELD_NOTE_SENTINEL`], so the renderer can stretch it to the
/// playhead — and any earlier retriggers end at `current_tick`.
fn live_thumbnail_entries(events: &[Event], current_tick: i32) -> Vec<(i32, i32, f32)> {
    let mut open_onsets: HashMap<u8, Vec<i32>> = HashMap::new();
    let mut entries = Vec::new();
    for event in events {
        match (event.event_type(), event.note_number()) {
            (Some(EventType::NoteOn), Some(pitch)) => {
                open_onsets.entry(pitch).or_default().push(event.tick());
            }
            (Some(EventType::NoteOff), Some(pitch)) => {
                if let Some(onsets) = open_onsets.get_mut(&pitch) {
                    let frac = pitch_frac(pitch);
                    entries.extend(onsets.drain(..).map(|start| (start, event.tick(), frac)));
                }
            }
            _ => {}
        }
    }

    for (pitch, onsets) in open_onsets {
        if let Some((&held_start, retriggered)) = onsets.split_last() {
            let frac = pitch_frac(pitch);
            entries.extend(retriggered.iter().map(|&start| (start, current_tick, frac)));
            entries.push((held_start, LiveRecState::HELD_NOTE_SENTINEL, frac));
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use crate::models::event::Event;

    use super::{LiveRecState, live_thumbnail_entries};

    /// `(start_tick, end_tick)` of each entry, sorted, dropping `pitch_frac`.
    fn spans(events: &[Event], current_tick: i32) -> Vec<(i32, i32)> {
        let mut spans: Vec<_> = live_thumbnail_entries(events, current_tick)
            .into_iter()
            .map(|(start, end, _)| (start, end))
            .collect();
        spans.sort_unstable();
        spans
    }

    fn on(pitch: u8, tick: i32) -> Event {
        Event::from_midi_with_tick(&[0x90, pitch, 100], tick)
    }

    fn off(pitch: u8, tick: i32) -> Event {
        Event::from_midi_with_tick(&[0x80, pitch, 0], tick)
    }

    #[test]
    fn a_released_note_spans_to_its_note_off() {
        let events = [on(60, 10), off(60, 50), on(62, 60), off(62, 90)];
        assert_eq!(spans(&events, 100), vec![(10, 50), (60, 90)]);
    }

    #[test]
    fn a_held_note_ends_at_the_sentinel() {
        let events = [on(60, 10), off(60, 50), on(60, 70)];
        assert_eq!(
            spans(&events, 100),
            vec![(10, 50), (70, LiveRecState::HELD_NOTE_SENTINEL)]
        );
    }

    #[test]
    fn a_retriggered_held_note_ends_the_earlier_onset_at_the_current_tick() {
        let events = [on(60, 10), on(60, 30)];
        assert_eq!(
            spans(&events, 100),
            vec![(10, 100), (30, LiveRecState::HELD_NOTE_SENTINEL)]
        );
    }

    #[test]
    fn a_note_off_closes_every_open_onset_of_its_pitch_only() {
        let events = [on(60, 10), on(60, 30), on(64, 35), off(60, 40)];
        assert_eq!(
            spans(&events, 100),
            vec![(10, 40), (30, 40), (35, LiveRecState::HELD_NOTE_SENTINEL)]
        );
    }

    #[test]
    fn a_velocity_zero_note_on_is_a_note_off() {
        let events = [on(60, 10), Event::from_midi_with_tick(&[0x90, 60, 0], 20)];
        assert_eq!(spans(&events, 100), vec![(10, 20)]);
    }

    #[test]
    fn pitch_frac_rises_with_pitch() {
        let entries = live_thumbnail_entries(&[on(40, 0), off(40, 5), on(80, 5), off(80, 9)], 10);
        assert!(entries[0].2 < entries[1].2);
    }
}
