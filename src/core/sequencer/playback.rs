//! The per-tick playback pump and the note-release paths that keep it honest.
//!
//! [`tick`](Sequencer::tick) is called once per `ClockTick` by the
//! `"sequencer"` thread (`core::threads::sequencer_pump`): it steps every
//! track, drops note-ons for inaudible tracks while still letting their
//! note-offs out, and fans events to the `"midiout"` thread — carrying the
//! tick's [`Instant`] so the output offset can be applied, see
//! `160-midi-out-offset.md` — or to the CLAP feed (`130-plugin-host.md`).
//! [`chase_notes`](Sequencer::chase_notes) is its seek-time counterpart.
//!
//! The `release_*` / `reset*` methods are the discontinuity handlers — mute,
//! seek, loop wrap, transport stop — that stop a clip-driven note from hanging
//! in external gear or in a plugin; [`reset_wheels`](Sequencer::reset_wheels)
//! does the same for a wheel a clip left off neutral when the transport stops.
//!
//! [`should_end_live_recording`](Sequencer::should_end_live_recording) lives
//! here too: it is driven by the same pump, one call after every `tick`, and
//! arms the `auto_end_tick` that `live_recording.rs` consumes.

use std::time::{Duration, Instant};

use crate::core::config::PITCH_PREVIEW_MS;
use crate::core::midi::message::midi3;
use crate::core::midi::message::rewrite_channel;
use crate::core::midi::out_queue::MidiOutMessage;
use crate::models::{
    event::EventType,
    track::{Track, TrackOutput},
};

use super::Sequencer;
use super::instrument_event::{ClipInstrumentEvent, EventTime};

impl Sequencer {
    /// Auditions `note` at `velocity` on the selected track's channel, like
    /// [`preview_note`](Self::preview_note) — for a pitch no event holds yet
    /// (a note move drag's preview). No selected track is a no-op.
    pub(crate) fn preview_pitch(&mut self, note: u8, velocity: u8) {
        let channel = self.selected_track().map(Track::midi_channel);
        self.preview_note(channel.map(|channel| (channel, note, velocity.max(1))));
    }

    /// Auditions `note_on` (`(channel, note, velocity)`, as
    /// [`selected_note_on`](Self::selected_note_on) returns it) — see
    /// [`preview_notes`](Self::preview_notes). Used by click-to-select,
    /// transpose, the piano roll's inserted note and
    /// [`preview_pitch`](Self::preview_pitch). `None` is a no-op.
    pub(crate) fn preview_note(&mut self, note_on: Option<(u8, u8, u8)>) {
        self.preview_notes(note_on.as_slice());
    }

    /// Auditions every `(channel, note, velocity)` in `note_ons` together on
    /// the selected track's output for `PITCH_PREVIEW_MS`: the note-ons now,
    /// the note-offs later — from one short-lived thread for a `MidiOut`
    /// track (not `MidiOutMessage::at`, which would add the MIDI-out offset to
    /// it), on the instrument feed as `EventTime::At(now)` /
    /// `At(now + PITCH_PREVIEW_MS)` pairs for a plugin track (never
    /// `Immediate`, see [`EventTime`]). The event marquee's newly joined
    /// notes come through here directly. No selected track is a no-op.
    pub(crate) fn preview_notes(&mut self, note_ons: &[(u8, u8, u8)]) {
        let Some(track_idx) = self.selected_track_index().filter(|_| !note_ons.is_empty()) else {
            return;
        };
        let preview = Duration::from_millis(PITCH_PREVIEW_MS);
        match self.tracks[track_idx].output() {
            TrackOutput::MidiOut { .. } => {
                for &(channel, note, velocity) in note_ons {
                    self.midi_out_tx
                        .send(MidiOutMessage::now(vec![
                            0x90 | (channel & 0x0F),
                            note,
                            velocity,
                        ]))
                        .ok();
                }
                let note_offs: Vec<_> = note_ons
                    .iter()
                    .map(|&(channel, note, _)| vec![0x80 | (channel & 0x0F), note, 0])
                    .collect();
                let tx = self.midi_out_tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(preview);
                    for note_off in note_offs {
                        tx.send(MidiOutMessage::now(note_off)).ok();
                    }
                });
            }
            TrackOutput::Instrument(_) => {
                let slot = self.tracks[track_idx].slot();
                let now = Instant::now();
                for &(_, note, velocity) in note_ons {
                    for (message, when) in [
                        ([0x90, note, velocity], EventTime::At(now)),
                        ([0x80, note, 0], EventTime::At(now + preview)),
                    ] {
                        self.instrument_midi_tx
                            .push(ClipInstrumentEvent {
                                track: slot,
                                message: midi3(&message),
                                when,
                            })
                            .ok();
                    }
                }
            }
        }
    }

    /// Whether the current take has run for a full loop length and should
    /// auto-stop. Sets [`auto_end_tick`](Self::auto_end_tick) as a side effect
    /// when it returns `true`.
    pub(crate) fn should_end_live_recording(&mut self) -> bool {
        let Some(session) = self.live_rec_session else {
            return false;
        };

        let start = session.rec_start_tick;
        let end = self.region_end();
        let length = end - start;

        if self.elapsed_tick() - session.elapsed_start_tick >= length.max(0) {
            self.auto_end_tick = Some(end);
            true
        } else {
            false
        }
    }

    /// `at` is the tick's intended [`Instant`], carried from `Clock` — clip
    /// events for `Instrument` tracks are tagged with it so the CLAP host can
    /// place them at the exact output sample, rather than at block offset 0.
    pub(crate) fn tick(&mut self, at: Instant) {
        // Read audibility once, before the `&mut` loop below.
        let audible = self.track_audibility();
        for (track_idx, track) in self.tracks.iter_mut().enumerate() {
            let track_audible = audible.get(track_idx).copied().unwrap_or(true);
            while let Some(event) = track.tick() {
                // A muted event never sounds at all — both its `NoteOn` and
                // paired `NoteOff` carry the flag (`Clip::toggle_muted_for_selected_events`),
                // so neither one goes out and nothing needs releasing later.
                if event.is_muted() {
                    continue;
                }
                // A muted / non-soloed track drops its note-ons but still emits
                // note-offs — so nothing hangs, and the release queued by
                // `release_track_notes` still gets out. `track.tick()` itself
                // keeps running so the track's playhead stays in sync. Its
                // wheel moves go out too: no note sounds to hear them, and
                // the synth is already where the clip has it on an unmute.
                if !track_audible && matches!(event.event_type(), Some(EventType::NoteOn)) {
                    continue;
                }
                match track.output() {
                    TrackOutput::MidiOut { channel } => {
                        let mut msg = event.into_midi_message();
                        rewrite_channel(&mut msg, *channel);
                        // Carries `at` so the `"midiout"` thread can hold it
                        // for the user's output offset, matching the delay the
                        // audio path already applies to instruments and the
                        // click. See `160-midi-out-offset.md`.
                        self.midi_out_tx.send(MidiOutMessage::at(msg, at)).ok();
                    }
                    TrackOutput::Instrument(_) => {
                        let mut msg = midi3(event.midi_message());
                        rewrite_channel(&mut msg, 0);
                        self.instrument_notes.observe_clip(track.slot(), &msg);
                        self.instrument_midi_tx
                            .push(ClipInstrumentEvent {
                                track: track.slot(),
                                message: msg,
                                when: EventTime::At(at),
                            })
                            .ok();
                    }
                }
            }
        }
    }

    /// After a seek: for every audible clip covering `playback_tick`, re-sends
    /// the `NoteOn`s whose notes are still sounding there, so held notes at the
    /// destination don't go silent. Instrument tracks get `EventTime::Immediate`
    /// (a seek has no future instant); `MidiOut` tracks are sent undelayed.
    pub(crate) fn chase_notes(&mut self, playback_tick: i32) {
        let audible = self.track_audibility();
        for (track, track_audible) in self.tracks.iter().zip(audible) {
            if !track_audible {
                continue;
            }
            let channel = track.midi_channel();
            let is_instrument = matches!(track.output(), TrackOutput::Instrument(_));
            for clip in track.clips() {
                if clip.is_muted() {
                    continue;
                }
                // Strictly inside the clip: landing exactly on its start is
                // `Track::tick`'s own arrival chase (`pending_note_ons`), which
                // fires on the very next tick — doing it here too would sound
                // the note twice for one note-off, sticking it. An ordinary
                // clip has nothing in flight at its start anyway; only the
                // right half of a non-destructive split does.
                if playback_tick <= clip.start_tick() || playback_tick >= clip.end_tick() {
                    continue;
                }
                for event in clip.chased_note_ons_at(playback_tick) {
                    if event.is_muted() {
                        continue;
                    }
                    if is_instrument {
                        let mut msg = midi3(event.midi_message());
                        rewrite_channel(&mut msg, channel);
                        self.instrument_notes.observe_clip(track.slot(), &msg);
                        // A seek — no future position; sound at the next block.
                        self.instrument_midi_tx
                            .push(ClipInstrumentEvent {
                                track: track.slot(),
                                message: msg,
                                when: EventTime::Immediate,
                            })
                            .ok();
                    } else {
                        // A seek, like the `EventTime::Immediate` above — there
                        // is no tick instant to offset from, so it goes now.
                        self.send_midi_out_now(event.into_midi_message(), channel);
                    }
                }
            }
        }
    }

    /// Queues note-offs for whatever `track_idx` currently has sounding from its
    /// clip, so `tick()` flushes them on the next clock tick. The offs travel as
    /// ordinary events through [`tick`](Self::tick)'s output match, so they clear
    /// both the `NoteLogger` (MIDI-out) and `instrument_notes` (CLAP) safety
    /// nets on their own. Called when a track is muted / un-soloed mid-playback.
    pub(crate) fn release_track_notes(&mut self, track_idx: usize) {
        if let Some(track) = self.tracks.get_mut(track_idx) {
            track.release_sounding_notes();
        }
    }

    /// Releases the clip-driven notes every `Instrument` track's plugin
    /// currently has sounding. Clip events sent via `instrument_midi_tx` bypass
    /// the `NoteLogger` note-off safety net on the `"midiout"` thread, so
    /// transport stop / seek would otherwise leave a plugin ringing — and, for
    /// plugins whose on-screen keyboard only clears on a matching note-off (u-he
    /// Repro), showing stuck keys.
    ///
    /// Sends an explicit Note Off (channel 0) for exactly the notes
    /// [`instrument_notes`](Self::instrument_notes) recorded as held from clips —
    /// never a blanket All Notes Off, and never a note the player is currently
    /// holding on the physical keyboard for that track (stopping the transport
    /// mid-improvisation must not cut it). Release tails still ring.
    pub(crate) fn release_instrument_notes(&mut self) {
        for track_idx in 0..self.tracks.len() {
            if matches!(self.tracks[track_idx].output(), TrackOutput::Instrument(_)) {
                self.release_slot_instrument_notes(self.tracks[track_idx].slot());
            }
        }
    }

    /// Sends a Note Off for every clip note engine slot `slot`'s plugin has
    /// sounding (minus a live-held one — see
    /// [`release_instrument_notes`](Self::release_instrument_notes)).
    fn release_slot_instrument_notes(&mut self, slot: usize) {
        for note in self.instrument_notes.release_for(slot) {
            self.push_instrument_now(slot, &[0x80, note, 0x00]);
        }
    }

    /// Silences everything the track at `track_idx` has sounding *now*, for a
    /// track leaving the arrangement: a `MidiOut` track's note-offs go
    /// straight to the `"midiout"` thread (its [`tick`](Self::tick) never runs
    /// again to flush a queued release), a plugin track's go to its slot —
    /// which also clears that slot's `InstrumentNotes`, so the next track to
    /// take the slot inherits nothing. Any wheel its clips moved goes back to
    /// neutral the same way. A no-op while stopped (nothing sounds).
    pub(super) fn silence_leaving_track(&mut self, track_idx: usize) {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return;
        };
        let offs = track.take_sounding_note_offs();
        let slot = track.slot();
        match *track.output() {
            TrackOutput::MidiOut { channel } => {
                for off in offs {
                    self.send_midi_out_now(off.into_midi_message(), channel);
                }
            }
            TrackOutput::Instrument(_) => self.release_slot_instrument_notes(slot),
        }
        self.send_wheel_resets(track_idx);
    }

    /// Puts back to neutral every wheel a track's clips moved — on a
    /// transport stop, where no [`tick`](Self::tick) runs to send a chase. A
    /// wheel the player moved last on the armed track is left where it is
    /// (`Track::hand_wheel_to_player`), as a live-held note is.
    pub(crate) fn reset_wheels(&mut self) {
        for track_idx in 0..self.tracks.len() {
            self.send_wheel_resets(track_idx);
        }
    }

    /// Sends the track at `track_idx` the resets for every wheel its playback
    /// moved ([`Track::take_wheel_resets`]), now, on its own output.
    fn send_wheel_resets(&mut self, track_idx: usize) {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return;
        };
        let resets = track.take_wheel_resets();
        let slot = track.slot();
        match *track.output() {
            TrackOutput::MidiOut { channel } => {
                for reset in resets {
                    self.send_midi_out_now(reset, channel);
                }
            }
            TrackOutput::Instrument(_) => {
                for reset in resets {
                    self.push_instrument_now(slot, &reset);
                }
            }
        }
    }

    /// Makes every track's next seek resend the wheel values it sent
    /// ([`Track::invalidate_wheels`]) — for a jump, which drops the `"midiout"`
    /// delay queue the last of them may still have sat in.
    pub(crate) fn invalidate_wheels(&mut self) {
        for track in &mut self.tracks {
            track.invalidate_wheels();
        }
    }

    /// Sends `msg` on `channel` to MIDI out, undelayed — a release or reset
    /// has no tick instant to offset from.
    fn send_midi_out_now(&self, mut msg: Vec<u8>, channel: u8) {
        rewrite_channel(&mut msg, channel);
        self.midi_out_tx.send(MidiOutMessage::now(msg)).ok();
    }

    /// Pushes `msg` to engine slot `slot`'s plugin for the next block
    /// (`EventTime::Immediate`, behind anything already scheduled there).
    fn push_instrument_now(&mut self, slot: usize, msg: &[u8]) {
        self.instrument_midi_tx
            .push(ClipInstrumentEvent {
                track: slot,
                message: midi3(msg),
                when: EventTime::Immediate,
            })
            .ok();
    }

    /// Re-seeks every track to the current playback tick.
    pub(crate) fn reset(&mut self) {
        self.reset_to_tick(self.playback_tick());
    }

    /// Re-seeks every track to `tick` (each track's wheels then chase what
    /// is in force there — `Track::seek`).
    pub(crate) fn reset_to_tick(&mut self, tick: i32) {
        for track in self.tracks.iter_mut() {
            track.seek(tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossbeam_channel::Receiver;
    use rtrb::Consumer;

    use crate::core::config;
    use crate::core::midi::input::InputTicks;
    use crate::core::midi::out_queue::MidiOutMessage;
    use crate::core::sequencer::ClipInstrumentEvent;
    use crate::core::sequencer::instrument_event::EventTime;
    use crate::core::sequencer::test_support::{clip_at, instrument_track_0, sequencer_at_tempo};
    use crate::models::clip::Clip;
    use crate::models::event::Event;
    use crate::models::track::TrackOutput;

    use super::Sequencer;

    fn drain_plugin_events(rx: &mut Consumer<ClipInstrumentEvent>) -> Vec<(usize, [u8; 3])> {
        std::iter::from_fn(|| rx.pop().ok())
            .map(|e| (e.track, e.message))
            .collect()
    }

    fn sequencer_and_plugin_rx(tempo_us: i32) -> (Sequencer, Consumer<ClipInstrumentEvent>) {
        let (sequencer, _midi_out_rx, plugin_midi_rx) = sequencer_at_tempo(tempo_us);
        (sequencer, plugin_midi_rx)
    }

    /// Same harness, but keeps the MIDI-out receiver alive so the messages a
    /// `MidiOut` track emits can be inspected.
    fn sequencer_and_midi_out_rx(tempo_us: i32) -> (Sequencer, Receiver<MidiOutMessage>) {
        let (sequencer, midi_out_rx, _plugin_midi_rx) = sequencer_at_tempo(tempo_us);
        (sequencer, midi_out_rx)
    }

    #[test]
    fn release_instrument_notes_releases_only_held_notes_on_instrument_tracks() {
        let (mut sequencer, mut rx) = sequencer_and_plugin_rx(config::TEMPO_US_DEFAULT);
        instrument_track_0(&mut sequencer);
        // track 1 stays MidiOut — must not receive anything here.

        // Two clip notes sounding, one already released.
        sequencer.instrument_notes.observe_clip(0, &[0x90, 60, 100]);
        sequencer.instrument_notes.observe_clip(0, &[0x90, 67, 100]);
        sequencer.instrument_notes.observe_clip(0, &[0x80, 60, 0]);

        sequencer.release_instrument_notes();

        let msgs = drain_plugin_events(&mut rx);
        // Exactly one Note Off, for the still-held note (67), on track 0.
        assert_eq!(msgs, vec![(0, [0x80, 67, 0x00])]);

        // Tracking is now clear — a second release sends nothing.
        sequencer.release_instrument_notes();
        assert!(rx.pop().is_err());
    }

    #[test]
    fn release_instrument_notes_spares_a_note_held_live_on_the_armed_track() {
        let (mut sequencer, mut rx) = sequencer_and_plugin_rx(config::TEMPO_US_DEFAULT);
        instrument_track_0(&mut sequencer);
        let track0 = sequencer.track_id_by_index(0);
        sequencer.select_track(track0);

        // A clip note and a live-improvised note, same track, both C4-ish.
        sequencer.instrument_notes.observe_clip(0, &[0x90, 60, 100]);
        sequencer.handle_midi_input_dispatch(
            &[0x90, 64, 100],
            InputTicks {
                position: 0,
                elapsed: 0,
            },
        ); // live E4

        // Also a clip note on E4 — must be spared because E4 is held live.
        sequencer.instrument_notes.observe_clip(0, &[0x90, 64, 100]);

        sequencer.release_instrument_notes();

        let msgs = drain_plugin_events(&mut rx);
        assert_eq!(msgs, vec![(0, [0x80, 60, 0x00])]);
    }

    /// Builds a sequencer whose track 0 is an instrument holding two adjacent
    /// clips — [0,480) opening on note 67, [480,960) opening on note 72 — the
    /// arrangement shape behind the stuck note (loop region on the first clip,
    /// another clip right after it).
    fn sequencer_with_two_adjacent_instrument_clips() -> (Sequencer, Consumer<ClipInstrumentEvent>)
    {
        let (mut sequencer, rx) = sequencer_and_plugin_rx(config::TEMPO_US_DEFAULT);
        instrument_track_0(&mut sequencer);

        for (start, note) in [(0, 67), (480, 72)] {
            let mut clip = clip_at(start, 480);
            clip.add_event(Event::new(0, 0, vec![0x90, note, 100]));
            clip.add_event(Event::new(240, 0, vec![0x80, note, 0]));
            sequencer.tracks_mut()[0].add_clip(&clip);
        }

        (sequencer, rx)
    }

    #[test]
    fn ticking_past_a_clip_end_without_re_anchoring_leaks_the_next_clips_note_on() {
        // The failure mode: on a loop wrap the tick pump kept calling
        // `Sequencer::tick` at the stale position, so the track seeked into the
        // following clip and fired its opening note-on into the plugin.
        let (mut sequencer, mut rx) = sequencer_with_two_adjacent_instrument_clips();
        let at = Instant::now();
        sequencer.reset_to_tick(0);

        // Play the first clip right up to its final tick (479).
        for _ in 0..480 {
            sequencer.tick(at);
        }
        drain_plugin_events(&mut rx);

        // One more tick with no re-anchor: the *second* clip's note-on (72) leaks.
        sequencer.tick(at);
        assert_eq!(drain_plugin_events(&mut rx), vec![(0, [0x90, 72, 100])]);
    }

    #[test]
    fn re_anchoring_to_region_start_on_a_loop_wrap_replays_the_first_clip_not_the_next() {
        let (mut sequencer, mut rx) = sequencer_with_two_adjacent_instrument_clips();
        let at = Instant::now();
        sequencer.reset_to_tick(0);

        for _ in 0..480 {
            sequencer.tick(at);
        }
        // What the tick pump now does synchronously on `TickOutcome::Wrapped`.
        sequencer.reset_to_tick(0);
        drain_plugin_events(&mut rx);

        // The next tick replays the first clip's opening note (67), never the
        // second clip's (72).
        sequencer.tick(at);
        assert_eq!(drain_plugin_events(&mut rx), vec![(0, [0x90, 67, 100])]);
    }

    /// Builds a one-clip `MidiOut` track: note-on at tick 0, note-off at 240.
    fn midi_out_track_with_one_note(sequencer: &mut Sequencer) {
        let mut clip = clip_at(0, 480);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(240, 0, vec![0x80, 60, 0]));
        sequencer.tracks_mut()[0].add_clip(&clip);
    }

    #[test]
    fn clip_events_for_a_midi_out_track_carry_the_tick_instant() {
        // The `"midiout"` thread can only delay a message it can date, so the
        // tick's intended `Instant` has to travel with it. See
        // `160-midi-out-offset.md`.
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        midi_out_track_with_one_note(&mut sequencer);
        sequencer.reset_to_tick(0);

        let at = Instant::now();
        sequencer.tick(at);

        let msg = midi_out_rx.try_recv().expect("note-on reaches MIDI out");
        assert_eq!(msg.bytes, vec![0x90, 60, 100]);
        assert_eq!(msg.at, Some(at));
    }

    #[test]
    fn playing_from_a_split_halfs_start_chases_the_in_flight_note_exactly_once() {
        // Play-from-cursor on the right half of a split (`restart_from_cursor`
        // → `reset_to_tick` + `chase_notes`) must not sound the straddling
        // note twice — once from `chase_notes` and again from `Track::tick`'s
        // own arrival chase — or one note-off later leaves a voice stuck.
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        // The right half `SplitClipsEdit` leaves when a clip is cut at 480
        // through a note (on@0, off@720): full event list, region [480, 960).
        let mut right = Clip::new();
        right.set_start_tick(480);
        right.region_mut().set_region(Some(480), Some(960));
        right.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        right.add_event(Event::new(720, 0, vec![0x80, 60, 0]));
        sequencer.tracks_mut()[0].add_clip(&right);

        sequencer.reset_to_tick(480);
        sequencer.chase_notes(480);
        let at = Instant::now();
        for _ in 0..480 {
            sequencer.tick(at);
        }

        let bytes: Vec<Vec<u8>> = std::iter::from_fn(|| midi_out_rx.try_recv().ok())
            .map(|m| m.bytes)
            .collect();
        assert_eq!(bytes, vec![vec![0x90, 60, 100], vec![0x80, 60, 0]]);
    }

    #[test]
    fn chased_notes_for_a_midi_out_track_are_sent_immediately() {
        // A seek has no tick instant to offset from — the instrument path uses
        // `EventTime::Immediate` here, and MIDI out must match it rather than
        // sitting in the delay queue.
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        midi_out_track_with_one_note(&mut sequencer);

        // Land between the note-on (0) and its note-off (240).
        sequencer.chase_notes(120);

        let msg = midi_out_rx
            .try_recv()
            .expect("chased note-on reaches MIDI out");
        assert_eq!(msg.bytes, vec![0x90, 60, 100]);
        assert_eq!(msg.at, None);
    }

    /// Regression: a preview's note-on went out `Immediate`, which the mixer
    /// queues behind the track's latest pending event — the previous
    /// preview's note-off, `PITCH_PREVIEW_MS` ahead. A quick marquee sweep
    /// heard its notes late and some squeezed to nothing. Each preview must
    /// be a self-contained `At` pair on the wall clock.
    #[test]
    fn back_to_back_previews_are_each_scheduled_at_their_own_time() {
        let (mut sequencer, mut plugin_rx) = sequencer_and_plugin_rx(500_000);
        instrument_track_0(&mut sequencer);
        sequencer.select_track(sequencer.track_id_by_index(0));

        let before = Instant::now();
        sequencer.preview_note(Some((0, 60, 100)));
        sequencer.preview_note(Some((0, 64, 100)));
        let after = Instant::now();

        let events: Vec<_> = std::iter::from_fn(|| plugin_rx.pop().ok()).collect();
        let times: Vec<Instant> = events
            .iter()
            .map(|event| match event.when {
                EventTime::At(at) => at,
                EventTime::Immediate => panic!("preview {:?} sent Immediate", event.message),
            })
            .collect();
        let preview = Duration::from_millis(config::PITCH_PREVIEW_MS);

        assert_eq!(events.len(), 4);
        let [on_60, off_60, on_64, off_64] = times[..] else {
            unreachable!()
        };
        assert!(before <= on_60 && on_64 <= after);
        assert_eq!(off_60, on_60 + preview);
        assert_eq!(off_64, on_64 + preview);
        // The second note-on sounds before the first preview ends, not after.
        assert!(on_64 < off_60);
    }

    /// A clip on track 0 (`MidiOut`, channel 0) bending up at 100 and never
    /// back, with a note so the clip isn't silent.
    fn bent_clip_on_track_0(sequencer: &mut Sequencer) {
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(100, 0, vec![0xE0, 0x00, 0x60]));
        clip.add_event(Event::new(240, 0, vec![0x80, 60, 0]));
        sequencer.tracks_mut()[0].add_clip(&clip);
    }

    fn midi_out_bytes(rx: &Receiver<MidiOutMessage>) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|m| m.bytes)
            .collect()
    }

    #[test]
    fn a_stop_centres_a_bend_a_clip_left_on_its_tracks_channel() {
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        bent_clip_on_track_0(&mut sequencer);
        // A plain note track beside it, which must send nothing.
        midi_out_track_with_one_note(&mut sequencer);
        sequencer.tracks_mut()[0].set_output(TrackOutput::MidiOut { channel: 5 });
        sequencer.reset_to_tick(0);
        let at = Instant::now();
        for _ in 0..200 {
            sequencer.tick(at);
        }
        midi_out_bytes(&midi_out_rx);

        sequencer.reset_wheels();

        let sent: Vec<_> = std::iter::from_fn(|| midi_out_rx.try_recv().ok()).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].bytes, vec![0xE5, 0x00, 0x40]);
        assert_eq!(sent[0].at, None, "a reset goes out now");
        sequencer.reset_wheels();
        assert!(midi_out_rx.try_recv().is_err(), "only once");
    }

    #[test]
    fn a_stop_centres_a_plugin_tracks_bend_on_its_slot() {
        let (mut sequencer, mut rx) = sequencer_and_plugin_rx(config::TEMPO_US_DEFAULT);
        instrument_track_0(&mut sequencer);
        bent_clip_on_track_0(&mut sequencer);
        sequencer.reset_to_tick(0);
        let at = Instant::now();
        for _ in 0..200 {
            sequencer.tick(at);
        }
        drain_plugin_events(&mut rx);

        sequencer.reset_wheels();

        let event = rx.pop().expect("the reset");
        assert_eq!((event.track, event.message), (0, [0xE0, 0x00, 0x40]));
        assert!(matches!(event.when, EventTime::Immediate));
    }

    #[test]
    fn a_stop_leaves_a_wheel_the_player_moved_last_alone() {
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        bent_clip_on_track_0(&mut sequencer);
        sequencer.select_track(sequencer.track_id_by_index(0));
        sequencer.reset_to_tick(0);
        let at = Instant::now();
        for _ in 0..200 {
            sequencer.tick(at);
        }
        let live = InputTicks {
            position: 200,
            elapsed: 200,
        };
        sequencer.handle_midi_input_dispatch(&[0xE0, 0x00, 0x30], live);
        midi_out_bytes(&midi_out_rx);

        sequencer.reset_wheels();

        assert!(midi_out_bytes(&midi_out_rx).is_empty());
    }

    /// Regression guard: with an output offset, a bend's return to centre
    /// can still sit in the `"midiout"` delay queue when a loop wrap drops
    /// it. The wrap's re-seek must send centre again, though the track
    /// already "sent" it.
    #[test]
    fn a_jump_resends_a_wheel_value_the_delay_queue_may_have_dropped() {
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(100, 0, vec![0xE0, 0x00, 0x60]));
        clip.add_event(Event::new(200, 0, vec![0xE0, 0x00, 0x40]));
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.reset_to_tick(0);
        let at = Instant::now();
        for _ in 0..300 {
            sequencer.tick(at);
        }
        midi_out_bytes(&midi_out_rx);

        // What `reanchor_playback` does on the wrap.
        sequencer.invalidate_wheels();
        sequencer.reset_to_tick(0);
        sequencer.tick(at);

        assert_eq!(midi_out_bytes(&midi_out_rx), vec![vec![0xE0, 0x00, 0x40]]);
    }

    #[test]
    fn a_track_leaving_mid_bend_is_centred_at_once() {
        let (mut sequencer, midi_out_rx) = sequencer_and_midi_out_rx(config::TEMPO_US_DEFAULT);
        bent_clip_on_track_0(&mut sequencer);
        sequencer.reset_to_tick(0);
        let at = Instant::now();
        for _ in 0..200 {
            sequencer.tick(at);
        }
        midi_out_bytes(&midi_out_rx);

        sequencer.silence_leaving_track(0);

        // The sounding note's release, then the centre.
        assert_eq!(
            midi_out_bytes(&midi_out_rx),
            vec![vec![0x80, 60, 0], vec![0xE0, 0x00, 0x40]]
        );
    }
}
