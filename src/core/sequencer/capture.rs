//! Committing the live capture buffer into clips.
//!
//! Every incoming note is fed into `capture_clip` (a rolling buffer) as it
//! arrives. `build_committed_capture_clip` freezes a slice of it into a new
//! clip at the cursor (placed on the track — undoably — by `CommitClipEdit`);
//! `build_running_capture_insert` picks the slice `InsertCaptureEdit` inserts
//! into an existing clip, from either pane. The
//! window that gets frozen is derived from the loop region — see
//! `100-running-capture.md` for the invariants, and `090-live-recording.md`
//! for how this differs from the dedicated live-record path.

use uuid::Uuid;

use crate::{
    core::{config, midi::input::InputTicks, time},
    models::{
        clip::Clip,
        event::{Event, EventType},
    },
};

use super::Sequencer;

/// A take's notes ready to insert into an existing clip — what the insert
/// builders ([`Sequencer::build_running_capture_insert`],
/// [`Sequencer::build_stopped_capture_insert`]) hand `InsertCaptureEdit`.
pub(crate) struct CaptureInsert {
    /// Track the target clip is on.
    pub(crate) track_idx: usize,
    /// The clip the take goes into.
    pub(crate) clip_id: Uuid,
    /// The notes, already in the clip's event ticks.
    pub(crate) events: Vec<Event>,
}

/// Whether a running-capture commit produces a fresh clip or is inserted into
/// an existing one — see [`normalize_running_capture`](Sequencer::normalize_running_capture).
#[derive(Clone, Copy)]
enum RunningCaptureTarget {
    /// Commit into a new standalone clip: the whole last pass is kept, the
    /// region is the slice from the cursor.
    NewClip,
    /// Insert into the selected clip from the arranger: the pinned window,
    /// with wrap-anticipating notes relocated to the loop start.
    InsertFromArranger,
}

impl Sequencer {
    /// Builds the clip a running-capture commit would place — the pure half of
    /// the commit, returning `(track index, positioned clip)` without touching
    /// the track or the capture buffer. `None` when the capture holds no note or
    /// the clip wouldn't fit before the next clip on the track, so a no-op
    /// commit never enters the undo record. [`CommitClipEdit`] does the add
    /// and clears the buffer.
    ///
    /// [`CommitClipEdit`]: crate::core::sequencer::edit::CommitClipEdit
    pub(crate) fn build_committed_capture_clip(&self) -> Option<(usize, Clip)> {
        if !self.capture_clip.has_notes() {
            return None;
        }

        let cursor_tick = self.cursor_tick();
        let min_len = time::min_clip_length_ticks();
        let track_idx = self.selected_track_index()?;
        let track = self.tracks.get(track_idx)?;

        // Space available before the next clip on this track — the committed
        // clip must not overlap it (the placeholder used to shrink itself here).
        let available = track
            .next_clip_start_after(cursor_tick)
            .map(|next_start| next_start - cursor_tick)
            .unwrap_or(i32::MAX);

        // Region (playable) length — the clip itself keeps the whole last
        // pass, see `normalize_running_capture`.
        //   Playback wrapping: the remainder of the current loop cycle from
        //   where the cursor sits — cursor at the loop start gives a full
        //   cycle, cursor a bar in gives one bar less. Clamped to `available`,
        //   floored to `min_clip_length_ticks` — a beat, the shortest a clip can
        //   be (a collapsed loop region can be zero-width).
        //   Linear (loop off, or a start after the region end that never
        //   wraps): the content played, last note-on rounded up to the next
        //   bar, likewise clamped and floored.
        // See `100-running-capture.md`.
        let anchor_tick = self.capture_anchor_tick();
        let region_length = if self.playback_is_looping() {
            let loop_len = self.capture_loop_length();
            let phase = (cursor_tick - self.region_start()).rem_euclid(loop_len);
            (loop_len - phase).min(available).max(min_len)
        } else {
            self.linear_content_length(anchor_tick)
                .min(available)
                .max(min_len)
        };

        let mut clip = self.normalize_running_capture(
            anchor_tick,
            region_length,
            RunningCaptureTarget::NewClip,
        )?;
        clip.detect_and_store_swing();

        clip.set_start_tick(cursor_tick);

        // A gap under the minimum clip length: the floor above made the clip wider than
        // the space, and `Track::add_clip` would refuse it.
        track.fits(&clip).then_some((track_idx, clip))
    }

    /// Routes one live-input message to both recorders, each in its own tick
    /// space — running capture needs the arrangement *position*, live recording
    /// needs *elapsed* time a seek can't move. See [`InputTicks`].
    pub(crate) fn handle_midi_input_dispatch(&mut self, message: &[u8], ticks: InputTicks) {
        // Track which live keys are down so a transport stop does not release a
        // note the player is holding on an instrument track. See `130-plugin-host.md`.
        let live_target = self.live_instrument_target();
        self.instrument_notes.observe_live(live_target, message);
        // Likewise a wheel the player moves: the armed track's synth is now
        // where the player put it, not where a clip did.
        if let Some(track) = self.selected_track_mut() {
            track.hand_wheel_to_player(message);
        }

        // Remember how far the capture clock's numbering sits from playback's,
        // so a commit can move the cursor into it (`capture_anchor_tick`). The
        // clock only matches playback's *phase* — it may be whole loops ahead —
        // and the sequencer doesn't hold `clock_tick`, so this input, stamped
        // with the clock's position, is the one place both are in hand at
        // once. Without it a take outside the loop (a linear take, windowed
        // from the anchor itself) commits nothing. Note-ons while running only:
        // stopped, playback is parked at the cursor while the clock keeps
        // counting in, so the difference would not be whole loops.
        if self.is_running() && is_note_on(message) {
            self.capture_clock_offset = Some(ticks.position - self.playback_tick());
        }

        self.add_midi_event_to_capture(message, ticks.position);
        self.add_midi_event_to_live_rec(message, ticks.elapsed);
    }

    /// The note pairs of `capture` (a crop, rebased to 0) whose onsets fall
    /// in `[0, keep_end_tick)` and the wheel moves inside it, shifted by
    /// `event_offset_ticks`: what a capture insert adds to its target clip
    /// (`InsertCaptureEdit`).
    pub(super) fn capture_events_to_insert(
        capture: &Clip,
        event_offset_ticks: i32,
        keep_end_tick: i32,
    ) -> Vec<Event> {
        let mut events = capture.cloned_events_in_range(0, keep_end_tick);
        for event in &mut events {
            event.set_tick(event.tick() + event_offset_ticks);
        }
        events
    }

    /// The running `/` with a lead clip: the notes the live capture inserts
    /// into the selected clip, from the arranger or the clip view alike, since
    /// the clip view doesn't loop the transport on its clip
    /// (`archive/210-docked-clip-panel.md`). The pure half of the insert;
    /// [`InsertCaptureEdit`] adds the notes and clears the buffer. `None` on an
    /// empty buffer, a cursor off the selected clip, or nothing that lands in
    /// the clip, so a no-op insert never enters the undo record.
    ///
    /// Uses `cursor_tick` as the window anchor — the same coordinate space as
    /// `clock_tick` (which tracks `playback_tick` relative to `region_start`).
    /// The window is pinned at the cursor's phase in the arranger loop, so a
    /// note lands at the region phase it was played at whatever the cursor's
    /// offset into the clip.
    ///
    /// After the crop window is computed and rebased to 0, a `clip_offset` of
    /// `cursor_tick - clip.start_tick()` converts from cursor-relative ticks into
    /// clip-local ticks (0-based from `clip.start_tick()`). When the cursor is at
    /// the clip start this is 0. Notes the shift would push past
    /// `clip.region_length()` — phases before the cursor, played after the loop
    /// wrapped, or past the clip's span inside a longer loop — are discarded,
    /// never shifted into the clip.
    ///
    /// [`InsertCaptureEdit`]: crate::core::sequencer::edit::InsertCaptureEdit
    pub(crate) fn build_running_capture_insert(&self) -> Option<CaptureInsert> {
        if !self.capture_clip.has_notes() {
            return None;
        }

        let cursor_tick = self.cursor_tick();
        let track_idx = self.selected_track_index()?;

        let (clip_id, clip_start_tick, clip_region_start, region_length) = {
            let clip = self.selected_clip()?;
            // Guard: cursor must be on the selected clip.
            if cursor_tick < clip.start_tick() || cursor_tick >= clip.end_tick() {
                return None;
            }
            (
                clip.id(),
                clip.start_tick(),
                clip.region().start(),
                clip.region_length().max(time::min_clip_length_ticks()),
            )
        };

        // Anchor on cursor_tick; the crop window is the target clip's length,
        // pinned at the cursor's phase in the arranger loop (independent of
        // the clip) — both share clock_tick space.
        let capture = self.normalize_running_capture(
            self.capture_anchor_tick(),
            region_length,
            RunningCaptureTarget::InsertFromArranger,
        )?;

        // Shift from cursor-relative space into the clip's event ticks: the
        // clip's first arrangement tick is event tick `region.start` (non-zero
        // once its left edge was dragged in), then the cursor's offset into
        // the clip. Onsets the shift would push past the clip end — phases
        // before the cursor, played after the wrap — are dropped, not parked
        // out of region. The crop above rebased the window to 0 and closed
        // every note inside it, so the whole room keeps every note; it also
        // keeps a wheel moved after the last note-off (a bend easing back).
        let clip_offset = cursor_tick - clip_start_tick;
        let room = region_length - clip_offset;

        let events =
            Self::capture_events_to_insert(&capture, clip_region_start + clip_offset, room);
        (!events.is_empty()).then_some(CaptureInsert {
            track_idx,
            clip_id,
            events,
        })
    }

    /// The played span of a linear take from `cursor_tick`: the last note-on
    /// rounded up to the next bar, at least one bar.
    fn linear_content_length(&self, cursor_tick: i32) -> i32 {
        let last_on = Self::last_inserted_event_tick(self.capture_clip.events(), EventType::NoteOn);
        let meter = self.meter();
        meter
            .next_bar_boundary_after(last_on - cursor_tick)
            .max(meter.bar_ticks())
    }

    /// Converts a live MIDI capture into a finalized, clip-local form ready for
    /// commit.
    ///
    /// The raw capture buffer (`self.capture_clip`) holds events in absolute
    /// transport ticks. Whether the take is *looping* or *linear* follows
    /// [`Self::playback_is_looping`] — the transport's own wrap condition, so a
    /// start after the region end is linear even with the loop flag on. In clip
    /// view the transport loop is the clip's own bounds.
    ///
    /// **`NewClip`** keeps the take as played and lets the region do the hiding:
    ///
    /// - looping: the *last pass only* — the **region-grid** pass holding the
    ///   last note-on ("the last wrap" as the loop shows it, whatever the
    ///   cursor: [`Self::calculate_running_region`] anchored on `region_start`),
    ///   kept whole via [`Clip::fold_into_loop`]. Phases the pass has not
    ///   reached yet stay empty: a take is one noodle per wrap, and the commit
    ///   is the last one, never topped up from the pass before. The region is
    ///   the slice from the anchor's phase, `[phase, phase + region_length)`;
    ///   the phases before it — the start of that same wrap — sit before
    ///   `region.start`, anything past a clamped window sits after
    ///   `region.end`, both there for the header-band edge drags to reveal. A
    ///   cursor accidentally a bar in therefore costs nothing: the whole wrap
    ///   is in the clip and the left edge brings the first bar back.
    ///   Notes within [`LATE_NOTE_TOLERANCE_TICKS`](config::LATE_NOTE_TOLERANCE_TICKS)
    ///   of the loop end anticipate the wrap's downbeat and are relocated to
    ///   phase 0 — inside the region when the anchor is at phase 0, retained
    ///   pre-roll otherwise (never teleported to a mid-loop cursor);
    /// - linear: everything from the earliest note-on's bar (relative to the
    ///   anchor) through the content end, the region starting at the anchor.
    ///
    /// A silent region with retained pre-roll is still a clip: the user drags
    /// the left edge to reveal it.
    ///
    /// **The insert targets** select a window of `region_length` ticks pinned
    /// at `anchor_tick`'s phase ([`Self::calculate_running_region`]) and crop
    /// to it; `InsertFromArranger` relocates wrap-anticipating notes to the
    /// *loop* start first (kept by the crop only when that is the anchor).
    ///
    /// Either way the result's tick origin is `0`. Returns `None` when nothing
    /// survives.
    fn normalize_running_capture(
        &self,
        anchor_tick: i32,
        region_length: i32,
        target: RunningCaptureTarget,
    ) -> Option<Clip> {
        let mut capture = self.capture_clip.clone();
        let bar = self.meter().bar_ticks();

        let transport_loop = self
            .playback_is_looping()
            .then(|| self.capture_loop_length());

        match (target, transport_loop) {
            (RunningCaptureTarget::NewClip, Some(loop_len)) => {
                // The *region-grid* pass holding the last note-on — "the last
                // wrap" as the loop shows it, whatever the cursor — kept whole:
                // the slice from the cursor is the region, the rest is pre-roll
                // for the left edge, so a misplaced cursor is a drag away.
                let (pass_start, pass_end) = Self::calculate_running_region(
                    &capture,
                    self.region_start(),
                    loop_len,
                    Some(loop_len),
                );
                capture.fold_into_loop(pass_start, pass_end, self.region_start(), loop_len);
                capture.relocate_late_notes_to_region_start(
                    0,
                    loop_len,
                    config::LATE_NOTE_TOLERANCE_TICKS,
                );

                let phase = (anchor_tick - self.region_start()).rem_euclid(loop_len);
                capture
                    .region_mut()
                    .set_region(Some(phase), Some(phase + region_length));
            }
            (RunningCaptureTarget::NewClip, None) => {
                // Whole bars of material before the anchor (a cursor moved
                // later mid-take), kept before `region.start`.
                let first_on = Self::first_inserted_event_tick(capture.events(), EventType::NoteOn);
                let before_anchor = (anchor_tick - first_on).max(0);
                let pre_roll = (before_anchor + bar - 1) / bar * bar;
                let retained_start = anchor_tick - pre_roll;
                let retained_end = anchor_tick + self.linear_content_length(anchor_tick);

                capture
                    .region_mut()
                    .set_region(Some(retained_start), Some(retained_end));
                capture.sort_events_by_tick();
                capture.calculate_note_lengths();
                capture.crop(retained_end - retained_start);
                capture
                    .region_mut()
                    .set_region(Some(pre_roll), Some(pre_roll + region_length));
            }
            (insert, _) => {
                let (region_start_tick, region_end_tick) = Self::calculate_running_region(
                    &capture,
                    anchor_tick,
                    region_length,
                    transport_loop,
                );

                capture
                    .region_mut()
                    .set_region(Some(region_start_tick), Some(region_end_tick));
                capture.sort_events_by_tick();
                capture.calculate_note_lengths();

                if let (RunningCaptureTarget::InsertFromArranger, Some(loop_len)) =
                    (insert, transport_loop)
                {
                    // The loop pass the window sits in — the window starts at
                    // the anchor's phase into it.
                    let loop_start_tick = region_start_tick
                        - (anchor_tick - self.region_start()).rem_euclid(loop_len);
                    capture.relocate_late_notes_to_region_start(
                        loop_start_tick,
                        loop_start_tick + loop_len,
                        config::LATE_NOTE_TOLERANCE_TICKS,
                    );
                }

                capture.crop(region_length);
            }
        }

        if !capture.has_notes() {
            return None;
        }
        Some(capture)
    }

    /// The cursor in the capture clock's numbering — the anchor a commit
    /// windows the capture buffer from. The events are stamped in
    /// `clock_tick` space, which agrees with the cursor's only in *phase*:
    /// enough for a looping take (its pass index absorbs whole loops), not
    /// for a linear one, whose window starts at the anchor itself — a take
    /// played outside the loop used to commit nothing. See
    /// `100-running-capture.md` § Anchor contract.
    fn capture_anchor_tick(&self) -> i32 {
        self.cursor_tick() + self.capture_clock_shift()
    }

    /// The whole-loop shift from cursor space into the capture clock's: the
    /// recorded `capture_clock_offset` rounded to the nearest multiple of the
    /// loop length, dropping the few ticks of input latency in it. `0` with
    /// no offset recorded (nothing captured while running).
    fn capture_clock_shift(&self) -> i32 {
        self.capture_clock_offset.map_or(0, |offset| {
            time::snap_to_grid(offset, self.capture_loop_length())
        })
    }

    /// The loop region's length, floored to `min_clip_length_ticks` (a beat)
    /// so a collapsed, zero-width region still gives a usable loop modulus.
    fn capture_loop_length(&self) -> i32 {
        (self.region_end() - self.region_start()).max(time::min_clip_length_ticks())
    }

    /// Appends one incoming MIDI message to the rolling capture buffer at
    /// `tick` (capture-position space).
    fn add_midi_event_to_capture(&mut self, midi_message: &[u8], tick: i32) {
        let event = Event::from_midi_with_tick(midi_message, tick);
        self.capture_clip.add_event(event);
    }
}

/// Whether `message` is a note-on (status `0x9n` with a non-zero velocity —
/// velocity 0 is a note-off by convention).
fn is_note_on(message: &[u8]) -> bool {
    matches!(message, [status, _, velocity, ..] if status & 0xF0 == 0x90 && *velocity > 0)
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::Ordering, time::Instant};

    use crossbeam_channel::Receiver;

    use super::*;
    use crate::{
        core::{
            config,
            midi::out_queue::MidiOutMessage,
            sequencer::edit::{
                CommitClipEdit, EditResult, InsertCaptureEdit, ResizeClipEdit, SequencerEdit,
            },
            sequencer::test_support::{clip_at, note_off, note_on, sequencer_with_outputs},
            time::{self, Meter},
        },
        metadata::clip_metadata::ClipMetadata,
        models::{
            clip::{Clip, ClipBounds},
            event::Event,
        },
    };
    use undo::{Edit, Record};
    use uuid::Uuid;

    // --- Helpers ---

    impl Sequencer {
        /// The old one-shot commit, kept for the tests below: builds the
        /// clip, places it and clears the buffer through the real
        /// [`CommitClipEdit`] path, returning the placed clip's metadata.
        fn commit_clip_to_track(&mut self) -> Option<ClipMetadata> {
            let mut edit = SequencerEdit::CommitClip(CommitClipEdit::from_running_capture(self)?);
            match edit.edit(self) {
                EditResult::ClipCommitted { clip } => Some(clip),
                _ => None,
            }
        }

        /// The edge drag's left-edge trim of the selected clip to `target`,
        /// through the real [`ResizeClipEdit`] path. Returns whether it moved.
        fn resize_selected_clip_region_start_to_tick(&mut self, target: i32) -> bool {
            let after = self
                .selected_clip_id()
                .and_then(|id| Some((id, self.clip_start_trimmed_to(id, target)?)));
            self.apply_resize(after)
        }

        /// The edge drag's right-edge trim, like
        /// [`resize_selected_clip_region_start_to_tick`](Self::resize_selected_clip_region_start_to_tick).
        fn resize_selected_clip_region_end_to_tick(&mut self, target: i32) -> bool {
            let after = self
                .selected_clip_id()
                .and_then(|id| Some((id, self.clip_end_trimmed_to(id, target)?)));
            self.apply_resize(after)
        }

        /// The running `/` with a lead clip, through the real
        /// [`InsertCaptureEdit`] path. `None` when nothing was inserted.
        fn insert_running_capture(&mut self) -> Option<()> {
            let edit = InsertCaptureEdit::from_running_capture(self)?;
            SequencerEdit::InsertCapture(edit).edit(self);
            Some(())
        }

        /// Applies `after` to its clip as a [`ResizeClipEdit`].
        fn apply_resize(&mut self, after: Option<(Uuid, ClipBounds)>) -> bool {
            let Some(edit) =
                after.and_then(|(id, bounds)| ResizeClipEdit::for_clip(self, id, bounds, None))
            else {
                return false;
            };
            SequencerEdit::ResizeClip(edit).edit(self);
            true
        }
    }

    /// Constructs a minimal Sequencer with no transport thread, with the
    /// transport region initialised to `[region_start, region_start + region_len]`
    /// and playback parked at `region_start` — so the loop flag (on) means
    /// playback *is* wrapping. A test modelling a linear take flips the flag
    /// or parks `playback_tick` outside the region.
    fn make_sequencer(region_start: i32, region_len: i32) -> Sequencer {
        make_sequencer_with_midi_out(region_start, region_len).0
    }

    /// [`make_sequencer`], keeping the MIDI-out receiver so a test can watch
    /// what playback actually emits.
    fn make_sequencer_with_midi_out(
        region_start: i32,
        region_len: i32,
    ) -> (Sequencer, Receiver<MidiOutMessage>) {
        let (mut seq, midi_out_rx, _plugin_midi_rx) = sequencer_with_outputs(true);
        seq.playback_tick.store(region_start, Ordering::Relaxed);
        seq.set_global_region(region_start, region_start + region_len);
        // Select track 0 (always present after construction).
        let track_id = seq.track_id_by_index(0).unwrap();
        seq.select_track(Some(track_id));
        (seq, midi_out_rx)
    }

    /// Runs playback from the track position `from` up to (not including)
    /// `until`, one `Sequencer::tick` per tick, returning every emitted
    /// `(tick, status, note)`.
    fn play_ticks(
        seq: &mut Sequencer,
        midi_out_rx: &Receiver<MidiOutMessage>,
        from: i32,
        until: i32,
    ) -> Vec<(i32, u8, u8)> {
        let mut out = Vec::new();
        for t in from..until {
            seq.tick(Instant::now());
            while let Ok(msg) = midi_out_rx.try_recv() {
                out.push((t, msg.bytes[0] & 0xF0, msg.bytes[1]));
            }
        }
        out
    }

    /// Adds a committed clip at `start_tick` with `region_len` to the selected
    /// track, selects it, and returns its id.
    fn add_and_select_clip(seq: &mut Sequencer, start_tick: i32, region_len: i32) -> Uuid {
        let clip = clip_at(start_tick, region_len);
        let track = seq.selected_track_mut().unwrap();
        track.add_clip(&clip);
        let id = clip.id();
        seq.select_clip(Some(id));
        id
    }

    // --- normalize_running_capture ---

    /// A bar-1 note in the capture buffer lands in bar 1 of the normalized clip.
    #[test]
    fn normalize_running_capture_bar1_note_stays_in_bar1() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        let result = seq
            .normalize_running_capture(0, bar * 2, RunningCaptureTarget::InsertFromArranger)
            .unwrap();

        let on_tick = result
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();

        assert!(
            on_tick < bar,
            "bar-1 note must stay in bar 1, got tick {on_tick}"
        );
    }

    /// A bar-2 note in the capture buffer lands in bar 2 of the normalized clip.
    #[test]
    fn normalize_running_capture_bar2_note_lands_in_bar2() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        let note_tick = bar + 600; // bar 2
        seq.capture_clip.add_event(note_on(note_tick));
        seq.capture_clip.add_event(note_off(note_tick + 300));

        let result = seq
            .normalize_running_capture(0, bar * 2, RunningCaptureTarget::InsertFromArranger)
            .unwrap();

        let on_tick = result
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();

        assert!(
            on_tick >= bar && on_tick < bar * 2,
            "bar-2 note must land in bar 2, got tick {on_tick}"
        );
    }

    /// An empty capture buffer produces `None`.
    #[test]
    fn normalize_running_capture_returns_none_on_empty_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let seq = make_sequencer(0, bar * 2);
        assert!(
            seq.normalize_running_capture(0, bar * 2, RunningCaptureTarget::InsertFromArranger)
                .is_none()
        );
    }

    /// After crop, all event ticks are within `[0, region_length)`.
    #[test]
    fn normalize_running_capture_events_are_within_region_length() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let region_len = bar * 2;
        let mut seq = make_sequencer(0, region_len);

        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));

        let result = seq
            .normalize_running_capture(0, region_len, RunningCaptureTarget::InsertFromArranger)
            .unwrap();

        for event in result.events() {
            assert!(
                event.tick() >= 0 && event.tick() < region_len,
                "event tick {} must be within [0, {region_len})",
                event.tick()
            );
        }
    }

    // --- commit_clip_to_track ---

    /// The new clip is placed at `cursor_tick` in the arrangement.
    #[test]
    fn commit_clip_to_track_places_clip_at_cursor_tick() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar * 2, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 2 + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar * 2, "clip must start at cursor_tick");
    }

    /// The builder is the pure half of the commit: it positions the clip but
    /// leaves the track and the capture buffer exactly as they were.
    #[test]
    fn build_committed_capture_clip_does_not_touch_the_track_or_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        let (track_idx, clip) = seq.build_committed_capture_clip().unwrap();

        assert_eq!(track_idx, seq.selected_track_index().unwrap());
        assert_eq!(clip.start_tick(), 0);
        assert!(seq.selected_track().unwrap().clips().is_empty());
        assert_eq!(seq.capture_clip.events().len(), 2);
    }

    /// All events in the committed clip have ticks within one loop, `[0, L)`
    /// — the clip's event space is the loop's phase circle.
    #[test]
    fn commit_clip_to_track_events_are_within_one_loop() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar * 2 + bar + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + bar + 900));

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        for event in clip.events() {
            assert!(
                event.tick() >= 0 && event.tick() < bar * 2,
                "event tick {} must be within [0, L)",
                event.tick()
            );
        }
    }

    /// Capture buffer is cleared after a successful commit.
    #[test]
    fn commit_clip_to_track_clears_capture_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        seq.commit_clip_to_track().unwrap();

        assert!(
            seq.capture_clip.events().is_empty(),
            "capture buffer must be empty after commit"
        );
    }

    /// Improvising over several loop wraps and then committing must yield only
    /// the notes from the last pass — not the whole take.
    ///
    /// Regression guard for the `AlignToPlayback` bug: capture events are
    /// stamped with the free-running `clock_tick`, so each cycle of a take sits
    /// a whole region length further along. When a loop wrap wrongly dragged
    /// the clock back to `region_start`, every cycle was stamped into the same
    /// span and all of them landed inside the crop window. Here the stamps are
    /// what a correctly free-running clock produces, and the commit must keep
    /// only the last pass.
    #[test]
    fn commit_clip_to_track_keeps_only_the_last_loop_of_a_multi_cycle_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let loop_len = bar * 2;
        let mut seq = make_sequencer(0, loop_len);

        // Three passes over the loop, one note each at the same phase.
        for cycle in 0..3 {
            let on = cycle * loop_len + 600;
            seq.capture_clip.add_event(note_on(on));
            seq.capture_clip.add_event(note_off(on + 300));
        }

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        let note_ons = note_on_ticks(clip);

        assert_eq!(
            note_ons,
            vec![600],
            "only the last loop of the take may be committed, got {note_ons:?}"
        );
    }

    /// The same take committed from a loop the clock has free-run far into
    /// still keeps one cycle — the window follows the loop index rather than
    /// assuming the capture buffer starts at the region.
    #[test]
    fn commit_clip_to_track_keeps_one_cycle_many_loops_into_a_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let loop_len = bar * 2;
        let mut seq = make_sequencer(0, loop_len);

        for cycle in 0..12 {
            // Two notes per pass, one per bar, so a window that swallowed a
            // neighbouring cycle would show up as four note-ons, not two.
            for beat in [600, bar + 600] {
                let on = cycle * loop_len + beat;
                seq.capture_clip.add_event(note_on(on));
                seq.capture_clip.add_event(note_off(on + 300));
            }
        }

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        let note_ons = note_on_ticks(clip);

        assert_eq!(
            note_ons,
            vec![600, bar + 600],
            "twelve passes must still commit a single cycle, got {note_ons:?}"
        );
    }

    /// Returns `None` and does not panic when capture buffer is empty.
    #[test]
    fn commit_clip_to_track_returns_none_on_empty_capture() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        assert!(seq.commit_clip_to_track().is_none());
    }

    /// With looping on and the cursor at the loop start, the committed clip is
    /// exactly the loop region length — regardless of how little was played.
    #[test]
    fn commit_clip_to_track_loop_enabled_clip_matches_loop_region_length() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 3);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        assert_eq!(clip.region_length(), bar * 3);
    }

    /// Looping, cursor a bar into a 2-bar loop: the clip fills the *remainder*
    /// of the loop cycle, not the whole loop.
    #[test]
    fn commit_clip_to_track_looping_sizes_to_remainder_of_loop_from_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar, bar * 2); // loop [bar, bar*3]
        seq.cursor_tick.store(bar * 2, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 300));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar * 2);
        let clip = seq
            .selected_track()
            .unwrap()
            .get_clip_by_id(meta.clip_id)
            .unwrap();
        assert_eq!(clip.region_length(), bar);
    }

    /// A committed clip shrinks to the gap before the next clip on the track
    /// instead of failing to place (what the placeholder's auto-shrink did).
    #[test]
    fn commit_clip_to_track_shrinks_to_fit_before_next_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar, bar * 2); // loop [bar, bar*3]
        add_and_select_clip(&mut seq, bar * 2, bar); // existing clip [bar*2, bar*3]
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar + 100));
        seq.capture_clip.add_event(note_off(bar + 300));

        let meta = seq
            .commit_clip_to_track()
            .expect("commit must place a clip, not no-op");

        assert_eq!(meta.start_tick, bar);
        let clip = seq
            .selected_track()
            .unwrap()
            .get_clip_by_id(meta.clip_id)
            .unwrap();
        assert_eq!(clip.region_length(), bar);
    }

    /// With looping off, the clip runs from the cursor to the bar boundary after
    /// the last note-on.
    #[test]
    fn commit_clip_to_track_loop_disabled_is_content_sized_to_next_bar() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 8);
        seq.loop_enabled.store(false, Ordering::Relaxed);

        // Cursor at 0; last note-on lands in bar 3.
        seq.capture_clip.add_event(note_on(bar * 2 + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        assert_eq!(clip.region_length(), bar * 3);
    }

    /// A collapsed (zero-width) loop region must not panic the commit — the
    /// window is floored to one bar. Regression for `crop(0)` -> divide by zero
    /// in `Region::snap_to_grid`.
    #[test]
    fn commit_clip_to_track_zero_width_loop_region_does_not_panic() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar * 4, 0); // region [bar*4, bar*4]
        seq.cursor_tick.store(bar * 4, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 4 + 100));
        seq.capture_clip.add_event(note_off(bar * 4 + 300));

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        assert_eq!(clip.region_length(), bar);
    }

    /// Looping, cursor on the last beat of a 2-bar loop: the remainder is one
    /// beat and that is the clip — the minimum clip length, not a bar, is the
    /// floor. (A bar floor used to push the clip past the loop end.)
    #[test]
    fn commit_clip_to_track_looping_remainder_can_be_shorter_than_a_bar() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let min_len = time::min_clip_length_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar * 2 - min_len, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 2 - min_len + 100));
        seq.capture_clip
            .add_event(note_off(bar * 2 - min_len + 300));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar * 2 - min_len);
        assert_eq!(meta.end_tick, bar * 2);
        assert_eq!(
            region_note_on_ticks(last_clip(&seq)),
            vec![bar * 2 - min_len + 100]
        );
    }

    /// A gap of one minimum clip length before the next clip is enough to
    /// commit into (it used to no-op below a bar); half of one still is not.
    #[test]
    fn commit_clip_to_track_fits_a_gap_of_the_minimum_clip_length() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let min_len = time::min_clip_length_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, min_len, bar);

        seq.capture_clip.add_event(note_on(100));
        seq.capture_clip.add_event(note_off(200));

        let meta = seq.commit_clip_to_track().expect("a one-step gap fits");
        assert_eq!(meta.end_tick, min_len);

        // Half a step: floored to a full step, which no longer fits → no-op.
        let mut seq2 = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq2, min_len / 2, bar);
        seq2.capture_clip.add_event(note_on(100));
        seq2.capture_clip.add_event(note_off(200));
        assert!(seq2.commit_clip_to_track().is_none());
    }

    /// With looping off, a single short note still yields a one-bar clip.
    #[test]
    fn commit_clip_to_track_loop_disabled_short_note_gets_one_bar_floor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 8);
        seq.loop_enabled.store(false, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(100));
        seq.capture_clip.add_event(note_off(200));

        seq.commit_clip_to_track().unwrap();

        let track = seq.selected_track().unwrap();
        let clip = track.clips().last().unwrap();
        assert_eq!(clip.region_length(), bar);
    }

    /// The one-bar floor is a bar of the project's meter.
    #[test]
    fn commit_clip_to_track_loop_disabled_floor_is_a_bar_of_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let mut seq = make_sequencer(0, three_four.bar_ticks() * 8);
        seq.set_meter(three_four);
        seq.loop_enabled.store(false, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(100));
        seq.capture_clip.add_event(note_off(200));
        seq.commit_clip_to_track().unwrap();

        let clip = seq.selected_track().unwrap().clips().last().unwrap();
        assert_eq!(clip.region_length(), three_four.bar_ticks());
    }

    /// All NoteOn ticks of `clip`, sorted — retained pre-roll / post-window
    /// material included.
    fn note_on_ticks(clip: &Clip) -> Vec<i32> {
        let mut ticks: Vec<i32> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(Event::tick)
            .collect();
        ticks.sort_unstable();
        ticks
    }

    /// [`note_on_ticks`] of the selected track's last clip.
    fn last_clip_note_on_ticks(seq: &Sequencer) -> Vec<i32> {
        note_on_ticks(last_clip(seq))
    }

    /// The NoteOn ticks of `clip` that fall inside its region — what plays.
    fn region_note_on_ticks(clip: &Clip) -> Vec<i32> {
        let region = clip.region();
        note_on_ticks(clip)
            .into_iter()
            .filter(|&t| t >= region.start() && t < region.end())
            .collect()
    }

    /// The selected track's last clip.
    fn last_clip(seq: &Sequencer) -> &Clip {
        seq.selected_track().unwrap().clips().last().unwrap()
    }

    /// Cursor a bar into a 2-bar loop; a take covering the whole wrap (bar 0
    /// then bar 1 of the same region pass), committed at its end. The region
    /// is the remainder from the cursor — the bar-1 note plays, at its region
    /// phase — and the bar-0 note is retained *before* the region for the
    /// left edge to reveal, not shifted into the clip and not dropped.
    #[test]
    fn commit_clip_to_track_mid_loop_cursor_keeps_events_at_their_region_phase() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        // Second wrap: bar 0, then bar 1.
        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 400));
        seq.capture_clip.add_event(note_on(bar * 3 + 600));
        seq.capture_clip.add_event(note_off(bar * 3 + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar);
        assert_eq!(meta.end_tick - meta.start_tick, bar);
        let clip = last_clip(&seq);
        assert_eq!(clip.region().start(), bar);
        assert_eq!(region_note_on_ticks(clip), vec![bar + 600]);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![100, bar + 600]);
    }

    /// The pass is the *region-grid* wrap, not "from the cursor": with the
    /// cursor a bar in, a note played after the wrap belongs to the next
    /// pass, and committing right after it keeps that pass alone — the
    /// previous wrap's bar-1 note is not carried along.
    #[test]
    fn commit_clip_to_track_post_wrap_note_selects_the_next_region_pass() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 400));

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        assert!(region_note_on_ticks(clip).is_empty());
        assert_eq!(last_clip_note_on_ticks(&seq), vec![100]);
    }

    /// Cursor on beat 2 (mid-bar) of a 2-bar loop, notes right after the
    /// cursor and in bar 2: every note lands at the region phase it was
    /// played at — nothing shifts by the cursor's offset into the bar.
    #[test]
    fn commit_clip_to_track_mid_bar_cursor_keeps_events_at_their_region_phase() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let beat = time::beats_to_ticks(1.0);
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(beat, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(beat + 100));
        seq.capture_clip.add_event(note_off(beat + 300));
        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, beat);
        let clip = last_clip(&seq);
        assert_eq!(clip.region().start(), beat);
        let arrangement_ticks: Vec<i32> = region_note_on_ticks(clip)
            .iter()
            .map(|&t| clip.start_tick() + t - clip.region().start())
            .collect();
        assert_eq!(
            arrangement_ticks,
            vec![beat + 100, bar + 600],
            "clip start + (event tick - region start) must equal the played tick"
        );
    }

    /// Loop off, three bars played, only one bar free before the next clip:
    /// the region holds the *first* bar from the cursor, not the last bar of
    /// content slid into place; the rest is retained past the region end.
    #[test]
    fn commit_clip_to_track_loop_disabled_available_clamp_keeps_the_first_bar() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 8);
        seq.loop_enabled.store(false, Ordering::Relaxed);
        add_and_select_clip(&mut seq, bar, bar); // next clip at bar 1

        for b in 0..3 {
            seq.capture_clip.add_event(note_on(bar * b + 600));
            seq.capture_clip.add_event(note_off(bar * b + 900));
        }

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, 0);
        assert_eq!(meta.end_tick, bar);
        let clip = seq
            .selected_track()
            .unwrap()
            .get_clip_by_id(meta.clip_id)
            .unwrap();
        assert_eq!(region_note_on_ticks(clip), vec![600]);
        assert_eq!(
            note_on_ticks(clip),
            vec![600, bar + 600, bar * 2 + 600],
            "material past the region end is retained for the right edge"
        );
    }

    /// A note played a hair ahead of the loop wrap anticipates the loop's
    /// downbeat. With the cursor a bar in, that downbeat is not the clip's
    /// start, so the note lands at phase 0 *before* the region — retained,
    /// not played, never teleported to the cursor.
    #[test]
    fn commit_clip_to_track_late_note_is_not_relocated_to_a_mid_loop_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        seq.capture_clip.add_event(note_on(bar * 2 - 50));
        seq.capture_clip.add_event(note_off(bar * 2 + 100));

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        assert_eq!(clip.region().start(), bar);
        assert_eq!(region_note_on_ticks(clip), vec![bar + 600]);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![0, bar + 600]);
    }

    /// Cursor at the loop start but the window clamped to one bar by a next
    /// clip: a note ahead of the wrap still anticipates the clip's own
    /// downbeat and is relocated to it — the relocation keys on the loop
    /// end, not the (shorter) window end.
    #[test]
    fn commit_clip_to_track_late_note_relocates_to_clip_start_when_window_is_clamped() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, bar, bar); // next clip at bar 1

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar * 2 - 50));
        seq.capture_clip.add_event(note_off(bar * 2 + 100));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.end_tick, bar);
        let clip = seq
            .selected_track()
            .unwrap()
            .get_clip_by_id(meta.clip_id)
            .unwrap();
        let ons = note_on_ticks(clip);
        assert_eq!(ons, vec![0, 600]);
    }

    /// A linear take has no wrap to anticipate: a note near the content end
    /// stays where it was played instead of being relocated to tick 0.
    #[test]
    fn commit_clip_to_track_loop_disabled_does_not_relocate_a_note_near_the_content_end() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 8);
        seq.loop_enabled.store(false, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar - 50));
        seq.capture_clip.add_event(note_off(bar - 10));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.end_tick, bar);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![bar - 50]);
    }

    /// Loop flag on but the cursor — and playback — start after the region
    /// end: the transport never wraps, so the take is linear and content-sized
    /// even though more than a loop length was played.
    #[test]
    fn commit_clip_to_track_start_after_the_region_is_a_linear_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar * 4, Ordering::Relaxed);
        seq.playback_tick.store(bar * 4, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 4 + 600));
        seq.capture_clip.add_event(note_off(bar * 4 + 900));
        seq.capture_clip.add_event(note_on(bar * 6 + 600));
        seq.capture_clip.add_event(note_off(bar * 6 + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar * 4);
        assert_eq!(meta.end_tick, bar * 7);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![600, bar * 2 + 600]);
    }

    /// Cursor before the region: playback runs in and then loops, so at
    /// commit the take is loop-shaped. The clip sits at the cursor and holds
    /// the last pass at its region phase; the lead-in and earlier pass go.
    #[test]
    fn commit_clip_to_track_start_before_the_region_commits_the_last_pass_at_its_phase() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar * 2, bar * 2); // loop [2b, 4b)
        seq.cursor_tick.store(0, Ordering::Relaxed);
        seq.playback_tick.store(bar * 2 + 700, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(600)); // lead-in
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar * 2 + 600)); // pass 0
        seq.capture_clip.add_event(note_off(bar * 2 + 900));
        seq.capture_clip.add_event(note_on(bar * 4 + 600)); // pass 1
        seq.capture_clip.add_event(note_off(bar * 4 + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, 0);
        assert_eq!(meta.end_tick, bar * 2);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![600]);
    }

    /// The cursor was left on bar 2 by mistake and a full-wrap take was
    /// committed: the bar-1 notes are retained before the region, and the
    /// existing left-edge drag brings them back — trimming the clip start to
    /// the loop start moves `start_tick` and `region.start` together and
    /// every note is at its played arrangement tick.
    #[test]
    fn commit_clip_to_track_retains_pre_cursor_notes_the_left_edge_can_reveal() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 400));
        seq.capture_clip.add_event(note_on(bar * 3 + 600));
        seq.capture_clip.add_event(note_off(bar * 3 + 900));

        let meta = seq.commit_clip_to_track().unwrap();
        seq.select_clip(Some(meta.clip_id));

        assert!(seq.resize_selected_clip_region_start_to_tick(0));

        let clip = seq.selected_clip().unwrap();
        assert_eq!(clip.start_tick(), 0);
        assert_eq!(clip.region().start(), 0);
        assert_eq!(clip.end_tick(), bar * 2, "the end stays put");
        let arrangement_ticks: Vec<i32> = region_note_on_ticks(clip)
            .iter()
            .map(|&t| clip.start_tick() + t - clip.region().start())
            .collect();
        assert_eq!(arrangement_ticks, vec![100, bar + 600]);
    }

    /// Every note at a phase before the cursor: the region is silent but the
    /// clip is still committed, holding them as pre-roll — the case that used
    /// to commit nothing at all.
    #[test]
    fn commit_clip_to_track_commits_a_silent_region_when_all_notes_precede_the_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        // Post-wrap, bar 0 of the next pass only.
        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 400));
        seq.capture_clip.add_event(note_on(bar * 2 + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));

        let meta = seq
            .commit_clip_to_track()
            .expect("a take with only pre-cursor notes must still commit");

        assert_eq!(meta.start_tick, bar);
        assert_eq!(meta.end_tick, bar * 2);
        let clip = last_clip(&seq);
        assert_eq!(clip.region().start(), bar);
        assert!(region_note_on_ticks(clip).is_empty());
        assert_eq!(last_clip_note_on_ticks(&seq), vec![100, 600]);
    }

    /// Committing mid-pass keeps the last pass only: the new pass's bar 0
    /// (pitch 62) is the take, and the previous pass's bar 1 (pitch 60) must
    /// not linger in the phases this pass has not reached yet. (A first cut
    /// kept "the most recent note at each phase" instead; the user found the
    /// leftovers from the second-last noodle wrong for the try-ideas-per-wrap
    /// workflow, 2026-09-19.)
    #[test]
    fn commit_clip_to_track_mid_pass_commit_keeps_the_last_pass_only() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        seq.capture_clip
            .add_event(Event::new(bar * 2 + 600, 0, vec![0x90, 62, 100]));
        seq.capture_clip
            .add_event(Event::new(bar * 2 + 900, 0, vec![0x80, 62, 0]));

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        let notes: Vec<(i32, u8)> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.note_number().unwrap()))
            .collect();
        assert_eq!(notes, vec![(600, 62)]);
    }

    /// A note held across the wrap is closed at the loop end — never left
    /// with its off folded to before its on. (Its onset sits outside the
    /// late-note tolerance, so no relocation is in play.)
    #[test]
    fn commit_clip_to_track_note_held_across_the_wrap_is_closed_at_the_loop_end() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        let on_tick = bar * 2 - config::LATE_NOTE_TOLERANCE_TICKS * 2;

        seq.capture_clip.add_event(note_on(on_tick));
        seq.capture_clip.add_event(note_off(bar * 2 + 300));

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        let edges: Vec<(i32, bool)> = clip
            .events()
            .iter()
            .map(|e| (e.tick(), e.event_type() == Some(EventType::NoteOn)))
            .collect();
        assert_eq!(edges, vec![(on_tick, true), (bar * 2 - 1, false)]);
    }

    /// Loop off, cursor moved a bar later than where the take began: the
    /// bar before the cursor is retained as pre-roll and the left edge
    /// reveals it.
    #[test]
    fn commit_clip_to_track_linear_take_retains_notes_before_a_moved_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 8);
        seq.loop_enabled.store(false, Ordering::Relaxed);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar);
        assert_eq!(meta.end_tick, bar * 2);
        let clip = last_clip(&seq);
        assert_eq!(clip.region().start(), bar);
        assert_eq!(region_note_on_ticks(clip), vec![bar + 600]);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![600, bar + 600]);

        seq.select_clip(Some(meta.clip_id));
        assert!(seq.resize_selected_clip_region_start_to_tick(0));
        let clip = seq.selected_clip().unwrap();
        assert_eq!(clip.start_tick(), 0);
        assert_eq!(region_note_on_ticks(clip), vec![600, bar + 600]);
    }

    /// End to end, the way the arranger does it: commit with the cursor a bar
    /// in, play through a wrap, and drag the left edge back to the loop start
    /// while playback is in the gap before the clip. The revealed bar must
    /// sound at its own ticks in *this* wrap, and a note sounding through a
    /// later drag must still get its note-off.
    #[test]
    fn left_edge_drag_during_playback_keeps_phase_and_releases_notes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut seq, midi_out_rx) = make_sequencer_with_midi_out(0, bar * 2);
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        // Second wrap of the take: bar 0 (pitch 60, held), bar 1 (pitch 62).
        seq.capture_clip.add_event(note_on(bar * 2 + 100));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));
        seq.capture_clip
            .add_event(Event::new(bar * 3 + 600, 0, vec![0x90, 62, 100]));
        seq.capture_clip
            .add_event(Event::new(bar * 3 + 900, 0, vec![0x80, 62, 0]));
        let meta = seq.commit_clip_to_track().unwrap();
        seq.select_clip(Some(meta.clip_id));
        seq.running.store(true, Ordering::Relaxed);

        let whole_wrap = vec![
            (100, 0x90, 60),
            (900, 0x80, 60),
            (bar + 600, 0x90, 62),
            (bar + 900, 0x80, 62),
        ];

        // Loop wrap onto the region start: the clip at bar 1 is ahead.
        seq.reset_to_tick(0);
        assert!(play_ticks(&mut seq, &midi_out_rx, 0, 50).is_empty());

        // Drag the left edge back to the loop start mid-gap.
        assert!(seq.resize_selected_clip_region_start_to_tick(0));
        assert_eq!(
            play_ticks(&mut seq, &midi_out_rx, 50, bar * 2),
            whole_wrap,
            "revealed bar must sound at its own ticks in the current wrap"
        );

        // Next wrap; while pitch 60 is sounding, a real drag's first snapped
        // target overshoots *past* the playhead before heading back.
        seq.reset_to_tick(0);
        assert_eq!(
            play_ticks(&mut seq, &midi_out_rx, 0, 500),
            vec![(100, 0x90, 60)]
        );
        assert!(seq.resize_selected_clip_region_start_to_tick(bar));
        assert_eq!(
            play_ticks(&mut seq, &midi_out_rx, 500, 550),
            vec![(501, 0x80, 60)],
            "the sounding note must be released when the clip leaves the playhead"
        );
        assert!(seq.resize_selected_clip_region_start_to_tick(0));
        assert_eq!(
            play_ticks(&mut seq, &midi_out_rx, 550, bar * 2),
            // The stored off of the already-released note still passes by —
            // a redundant off, never a stuck note — then bar 1 in phase.
            vec![
                (900, 0x80, 60),
                (bar + 600, 0x90, 62),
                (bar + 900, 0x80, 62)
            ]
        );

        // And the wrap after that plays the whole revealed clip in phase.
        seq.reset_to_tick(0);
        assert_eq!(play_ticks(&mut seq, &midi_out_rx, 0, bar * 2), whole_wrap);
    }

    // --- clip edge resize clamps (the `Sequencer` fixture lives here) ---

    /// Either edge can shrink a clip down to the minimum clip length (a
    /// beat) but never below it — even though a zoomed-in arranger snaps the
    /// drag on a finer grid.
    #[test]
    fn edge_resize_clamps_clip_length_to_the_minimum() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let min_len = time::min_clip_length_ticks();
        let mut seq = make_sequencer(0, bar * 4);
        let clip_id = add_and_select_clip(&mut seq, bar, bar * 2);

        // Right edge: down to the minimum, not further.
        assert!(seq.resize_selected_clip_region_end_to_tick(bar + min_len));
        assert_eq!(seq.selected_clip().unwrap().region_length(), min_len);
        assert!(!seq.resize_selected_clip_region_end_to_tick(bar + min_len / 2));
        assert_eq!(seq.selected_clip().unwrap().region_length(), min_len);

        // Back out, then the left edge: up to `end - min_len`, not further.
        assert!(seq.resize_selected_clip_region_end_to_tick(bar * 3));
        assert!(seq.resize_selected_clip_region_start_to_tick(bar * 3 - min_len / 2));
        let clip = seq
            .selected_track()
            .unwrap()
            .get_clip_by_id(clip_id)
            .unwrap();
        assert_eq!(clip.start_tick(), bar * 3 - min_len);
        assert_eq!(clip.region_length(), min_len);
        assert_eq!(clip.end_tick(), bar * 3, "the end stays put");
    }

    // --- insert_running_capture ---

    /// The NoteOn ticks of `seq`'s selected clip, sorted.
    fn selected_clip_note_on_ticks(seq: &Sequencer) -> Vec<i32> {
        note_on_ticks(seq.selected_clip().unwrap())
    }

    /// The wheel events of `seq`'s selected clip as `(tick, message)`.
    fn selected_clip_wheel_moves(seq: &Sequencer) -> Vec<(i32, Vec<u8>)> {
        seq.selected_clip()
            .unwrap()
            .events()
            .iter()
            .filter(|e| e.event_type().is_none())
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect()
    }

    /// A running `/` into the lead clip brings the take's wheel moves along
    /// with its notes, as a new clip keeps them.
    #[test]
    fn insert_running_capture_carries_the_wheels_played_with_the_notes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip
            .add_event(Event::new(700, 0, vec![0xE0, 0x00, 0x60]));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip
            .add_event(Event::new(950, 0, vec![0xB0, 0x01, 70]));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);
        assert_eq!(
            selected_clip_wheel_moves(&seq),
            vec![(700, vec![0xE0, 0x00, 0x60]), (950, vec![0xB0, 0x01, 70])]
        );
    }

    /// A take is its notes: a wheel wiggled with nothing played commits no
    /// clip and inserts nothing, running or stopped.
    #[test]
    fn a_capture_of_wheel_moves_alone_commits_nothing() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        for tick in [100, 400, 700] {
            seq.capture_clip
                .add_event(Event::new(tick, 0, vec![0xB0, 0x01, 90]));
        }

        assert!(seq.build_committed_capture_clip().is_none());
        assert!(seq.build_stopped_capture_clip().is_none());
        add_and_select_clip(&mut seq, 0, bar * 2);
        assert!(seq.build_running_capture_insert().is_none());
        assert!(seq.build_stopped_capture_insert().is_none());
    }

    /// Both recorders take a wheel move off the input like a note.
    #[test]
    fn a_wheel_move_reaches_both_recorders() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.start_live_recording(0).unwrap();

        seq.handle_midi_input_dispatch(
            &[0xE0, 0x00, 0x50],
            InputTicks {
                position: 300,
                elapsed: 300,
            },
        );

        let ticks = |clip: &Clip| -> Vec<i32> { clip.events().iter().map(Event::tick).collect() };
        assert_eq!(ticks(&seq.capture_clip), vec![300]);
        assert_eq!(ticks(&seq.live_rec_clip), vec![300]);
    }

    /// Regression: committing used to select the note at the clip cursor
    /// whenever the cursor fell inside the inserted span — invisibly, since
    /// nothing published it. A commit never changes the selection: with
    /// nothing selected before, nothing is selected after, not even the new
    /// notes. The captured note sits exactly on the cursor (tick 0), the one
    /// case the old code fired in.
    #[test]
    fn insert_running_capture_selects_nothing_when_nothing_was_selected() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);

        seq.capture_clip.add_event(note_on(0));
        seq.capture_clip.add_event(note_off(300));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![0]);
        assert!(seq.selected_clip().unwrap().selected_event_ids().is_empty());
    }

    /// Regression: the same commit used to *replace* an existing selection
    /// with the note at the cursor. Whatever was selected before a commit
    /// stays selected after it, and only that.
    #[test]
    fn insert_running_capture_keeps_an_existing_selection() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        let existing = note_on(bar);
        let existing_id = existing.id();
        {
            let clip = seq.selected_clip_mut().unwrap();
            clip.add_event(existing);
            clip.add_event(note_off(bar + 300));
            clip.sort_events_by_tick();
            clip.calculate_note_lengths();
            clip.select_event(Some(existing_id));
        }

        seq.capture_clip.add_event(note_on(0));
        seq.capture_clip.add_event(note_off(300));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![0, bar]);
        assert_eq!(
            seq.selected_clip().unwrap().selected_event_ids(),
            vec![existing_id]
        );
    }

    /// Cursor at clip start: a bar-1 note is inserted at bar 1 (clip_offset = 0).
    #[test]
    fn insert_running_capture_bar1_note_at_clip_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3; // clip at bar 4
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        // Cursor at clip start — clip_offset will be 0.
        seq.cursor_tick.store(clip_start, Ordering::Relaxed);

        // Note played at clock_tick == clip_start + 600 (bar 1 of the clip).
        seq.capture_clip.add_event(note_on(clip_start + 600));
        seq.capture_clip.add_event(note_off(clip_start + 900));

        seq.insert_running_capture().unwrap();

        let clip = seq.selected_clip().unwrap();
        let on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();

        assert!(
            on_tick < bar,
            "bar-1 note must be in bar 1 of clip, got tick {on_tick}"
        );
    }

    /// Regression: a clip whose left edge was dragged in keeps its events but
    /// starts its region past them (non-destructive trim), so its first
    /// arrangement tick is event tick `region.start`, not 0. The commit used to
    /// shift notes by the cursor's offset from the clip start alone, landing
    /// them *before* the region — kept, but never played or drawn.
    #[test]
    fn insert_running_capture_lands_inside_a_left_trimmed_region() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        // Was bar 2..4 (region 0..2 bars); the left edge dragged in a bar
        // leaves bar 3..4 with region 1..2 bars.
        let clip_start = bar * 2;
        let mut seq = make_sequencer(clip_start, bar);
        add_and_select_clip(&mut seq, bar, bar * 2);
        assert!(seq.resize_selected_clip_region_start_to_tick(clip_start));
        assert_eq!(seq.selected_clip_region_start(), Some(bar));

        seq.cursor_tick.store(clip_start, Ordering::Relaxed);
        seq.capture_clip.add_event(note_on(clip_start + 600));
        seq.capture_clip.add_event(note_off(clip_start + 900));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![bar + 600]);
    }

    /// Cursor at clip start: a bar-2 note is inserted at bar 2 of a 2-bar clip.
    #[test]
    fn insert_running_capture_bar2_note_at_clip_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3; // clip at bar 4
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        seq.cursor_tick.store(clip_start, Ordering::Relaxed);

        let note_tick = clip_start + bar + 600; // bar 2 of the clip
        seq.capture_clip.add_event(note_on(note_tick));
        seq.capture_clip.add_event(note_off(note_tick + 300));

        seq.insert_running_capture().unwrap();

        let clip = seq.selected_clip().unwrap();
        let on_tick = clip
            .events()
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .unwrap()
            .tick();

        assert!(
            on_tick >= bar && on_tick < bar * 2,
            "bar-2 note must be in bar 2 of clip, got tick {on_tick}"
        );
    }

    /// Cursor a bar into a 2-bar clip, loop wrapped: the post-wrap bar-1 note
    /// is at a phase before the cursor and must be dropped, not inserted past
    /// the clip end where it would resurface on a later region extend.
    #[test]
    fn insert_running_capture_drops_post_wrap_notes_before_cursor_phase() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);
        seq.cursor_tick.store(clip_start + bar, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(clip_start + bar + 600));
        seq.capture_clip.add_event(note_off(clip_start + bar + 900));
        seq.capture_clip
            .add_event(note_on(clip_start + bar * 2 + 100));
        seq.capture_clip
            .add_event(note_off(clip_start + bar * 2 + 400));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![bar + 600]);
        let clip = seq.selected_clip().unwrap();
        assert!(
            clip.events().iter().all(|e| e.tick() < bar * 2),
            "no event may be parked past the clip end"
        );
    }

    /// A 2-bar clip inside a 4-bar arranger loop, cursor at the clip start:
    /// notes played past the clip's span (bars 2-3 of the same pass) are
    /// dropped, not slid back into the clip.
    #[test]
    fn insert_running_capture_drops_notes_played_past_the_clip_span() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 4);
        add_and_select_clip(&mut seq, 0, bar * 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar * 3 + 600));
        seq.capture_clip.add_event(note_off(bar * 3 + 900));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);
    }

    /// Returns `None` when cursor is before the clip start.
    #[test]
    fn insert_running_capture_returns_none_when_cursor_before_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 2;
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        // Cursor is before the clip.
        seq.cursor_tick.store(clip_start - 1, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        assert!(
            seq.insert_running_capture().is_none(),
            "must return None when cursor is before the clip"
        );
    }

    /// Returns `None` when cursor is at or after the clip end.
    #[test]
    fn insert_running_capture_returns_none_when_cursor_after_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 2;
        let mut seq = make_sequencer(0, bar * 4);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        // Cursor is at the clip end (half-open — not on the clip).
        seq.cursor_tick
            .store(clip_start + bar * 2, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));

        assert!(
            seq.insert_running_capture().is_none(),
            "must return None when cursor is at or after clip end"
        );
    }

    /// Returns `None` on empty capture buffer.
    #[test]
    fn insert_running_capture_returns_none_on_empty_capture() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        seq.cursor_tick.store(clip_start, Ordering::Relaxed);

        assert!(seq.insert_running_capture().is_none());
    }

    /// Capture buffer is cleared after a successful commit.
    #[test]
    fn insert_running_capture_clears_capture_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        seq.cursor_tick.store(clip_start, Ordering::Relaxed);

        seq.capture_clip.add_event(note_on(clip_start + 600));
        seq.capture_clip.add_event(note_off(clip_start + 900));

        seq.insert_running_capture().unwrap();

        assert!(
            seq.capture_clip.events().is_empty(),
            "capture buffer must be empty after commit"
        );
    }

    /// The running insert is one undo step: undo takes the notes back out,
    /// redo puts the same ones back, and the buffer stays cleared.
    #[test]
    fn insert_running_capture_is_undoable() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);
        seq.cursor_tick.store(clip_start, Ordering::Relaxed);
        seq.capture_clip.add_event(note_on(clip_start + 600));
        seq.capture_clip.add_event(note_off(clip_start + 900));

        let mut record: Record<SequencerEdit> = Record::new();
        let edit = InsertCaptureEdit::from_running_capture(&seq).unwrap();
        record.edit(&mut seq, SequencerEdit::InsertCapture(edit));
        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);

        record.undo(&mut seq);
        assert!(seq.selected_clip().unwrap().events().is_empty());
        assert!(seq.capture_clip.events().is_empty());

        record.redo(&mut seq);
        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);
    }

    // --- a take outside the loop: the clock's numbering vs. the cursor's ---

    /// Feeds one note (on at `playback + 600`, off 300 later) through the real
    /// input dispatch while running, stamping its `position` `clock_ahead`
    /// ticks ahead of playback — the free-running clock after some loop
    /// passes, which only ever matches playback's phase.
    fn play_note_with_clock_ahead(seq: &mut Sequencer, playback: i32, clock_ahead: i32) {
        seq.running.store(true, Ordering::Relaxed);
        for (offset, message) in [(600, [0x90, 60, 100]), (900, [0x80, 60, 0])] {
            seq.playback_tick
                .store(playback + offset, Ordering::Relaxed);
            seq.handle_midi_input_dispatch(
                &message,
                InputTicks {
                    position: playback + offset + clock_ahead,
                    elapsed: 0,
                },
            );
        }
    }

    /// Regression: a take into a clip outside the loop region — a linear
    /// take — committed nothing. The clock sat whole loops ahead of the
    /// cursor, and a linear window starts at the anchor itself, so it missed
    /// every note. Since the clip view stopped looping the transport on its
    /// clip, this is any take in a clip the loop doesn't cover.
    #[test]
    fn insert_running_capture_outside_the_loop_finds_the_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2); // loop [0, 2b)
        let clip_start = bar * 2; // the clip at bar 3..5, past the loop
        add_and_select_clip(&mut seq, clip_start, bar * 2);
        seq.cursor_tick.store(clip_start, Ordering::Relaxed);

        // Seven loop passes ahead, plus three ticks of input latency.
        play_note_with_clock_ahead(&mut seq, clip_start, bar * 2 * 7 + 3);
        assert!(!seq.playback_is_looping(), "the take must be linear");

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![603]);
    }

    /// The same for a new clip: a linear take outside the loop is placed and
    /// content-sized from the cursor, not from wherever the clock's numbering
    /// happens to be.
    #[test]
    fn commit_clip_to_track_outside_the_loop_finds_the_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2); // loop [0, 2b)
        seq.cursor_tick.store(bar * 4, Ordering::Relaxed);

        play_note_with_clock_ahead(&mut seq, bar * 4, bar * 2 * 5 - 2);

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!((meta.start_tick, meta.end_tick), (bar * 4, bar * 5));
        assert_eq!(last_clip_note_on_ticks(&seq), vec![598]);
    }

    /// Clearing the buffer forgets the offset: the next take measures its own.
    #[test]
    fn reset_capture_forgets_the_clock_offset() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        play_note_with_clock_ahead(&mut seq, bar * 4, bar * 2 * 3);
        assert_eq!(seq.capture_clock_shift(), bar * 2 * 3);

        seq.reset_capture();

        assert_eq!(seq.capture_clock_shift(), 0);
    }

    #[test]
    fn is_note_on_treats_velocity_zero_as_a_note_off() {
        assert!(is_note_on(&[0x93, 60, 1]));
        assert!(!is_note_on(&[0x90, 60, 0]));
        assert!(!is_note_on(&[0x80, 60, 64]));
        assert!(!is_note_on(&[0xB0, 7, 100]));
        assert!(!is_note_on(&[0x90]));
    }

    // --- live recording vs. a clock reposition ---

    /// Recorded notes are placed from the odometer, so a `clock_tick`
    /// reposition mid-take cannot move them.
    ///
    /// This is what the odometer in `150-clock-position-sync.md` exists for. Before
    /// the split, one tick fed both recorders: a region edit or an
    /// arranger↔clip-view switch while recording snapped that tick onto
    /// playback, and every note after the snap landed at the wrong clip-local
    /// position — corrupting the take, not just its bounds. Here the `position`
    /// values are deliberately incoherent; only `elapsed` may be consulted.
    #[test]
    fn live_rec_note_placement_survives_a_clock_position_snap() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.start_live_recording(5_000).unwrap();

        // First note, one beat into the take.
        seq.handle_midi_input_dispatch(
            &[0x90, 60, 100],
            InputTicks {
                position: 900_000,
                elapsed: 5_480,
            },
        );
        // A reposition lands here — `position` collapses to a small value while
        // the odometer keeps counting.
        seq.handle_midi_input_dispatch(
            &[0x90, 62, 100],
            InputTicks {
                position: 7,
                elapsed: 5_960,
            },
        );

        let ticks: Vec<i32> = seq
            .live_rec_clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.tick())
            .collect();

        assert_eq!(
            ticks,
            vec![480, 960],
            "recorded notes must be placed from the odometer alone, got {ticks:?}"
        );
    }

    /// The running-capture side of the same dispatch keeps taking `position`,
    /// so the two recorders genuinely read different coordinates.
    #[test]
    fn running_capture_still_stamps_the_position_coordinate() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.handle_midi_input_dispatch(
            &[0x90, 60, 100],
            InputTicks {
                position: 900_000,
                elapsed: 5_480,
            },
        );

        let ticks: Vec<i32> = seq.capture_clip.events().iter().map(|e| e.tick()).collect();

        assert_eq!(ticks, vec![900_000]);
    }

    /// A take auto-ends on odometer distance, and nothing else moving can
    /// trigger it early.
    #[test]
    fn should_end_live_recording_fires_on_odometer_distance() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.elapsed_ticks.store(5_000, Ordering::Relaxed);
        seq.start_live_recording(5_000).unwrap();

        // `rec_start_tick` is playback 0 and the region ends at 2 bars, so the
        // take runs for 2 bars of odometer time.
        assert!(!seq.should_end_live_recording());

        // Playback and the cursor moving under it change nothing.
        seq.playback_tick.store(bar * 2, Ordering::Relaxed);
        seq.cursor_tick.store(bar * 4, Ordering::Relaxed);
        assert!(!seq.should_end_live_recording());

        seq.elapsed_ticks
            .store(5_000 + bar * 2 - 1, Ordering::Relaxed);
        assert!(!seq.should_end_live_recording());

        seq.elapsed_ticks.store(5_000 + bar * 2, Ordering::Relaxed);
        assert!(seq.should_end_live_recording());
    }

    // --- capture_events_to_insert ---

    /// Positive offset shifts events correctly into clip-local space.
    #[test]
    fn capture_events_to_insert_positive_offset_shifts_into_clip_space() {
        let bar = Meter::FOUR_FOUR.bar_ticks();

        // Capture: note-on at 600, note-off at 900 (both relative to region_start after crop).
        let mut capture = Clip::new();
        capture.add_event(note_on(600));
        capture.add_event(note_off(900));
        capture.sort_events_by_tick();
        capture.calculate_note_lengths();

        // clip_offset = region_start (bar) - clip.start_tick (0) = bar
        let clip_offset = bar;
        let events = Sequencer::capture_events_to_insert(&capture, clip_offset, bar * 2);

        let on_tick = events
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .expect("NoteOn must be present")
            .tick();

        assert_eq!(
            on_tick,
            600 + bar,
            "NoteOn must be shifted by clip_offset into clip-local space"
        );
    }

    /// Zero offset (region_start == clip.start_tick()) must leave event ticks unchanged.
    #[test]
    fn capture_events_to_insert_zero_offset_preserves_ticks() {
        let bar = Meter::FOUR_FOUR.bar_ticks();

        let mut capture = Clip::new();
        capture.add_event(note_on(600));
        capture.add_event(note_off(900));
        capture.sort_events_by_tick();
        capture.calculate_note_lengths();

        let events = Sequencer::capture_events_to_insert(&capture, 0, bar * 2);

        let on_tick = events
            .iter()
            .find(|e| e.event_type() == Some(EventType::NoteOn))
            .expect("NoteOn must be present")
            .tick();

        assert_eq!(on_tick, 600, "zero offset must not shift event ticks");
    }
}
