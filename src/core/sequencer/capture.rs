//! Committing the live capture buffer into clips.
//!
//! Every incoming note is fed into `capture_clip` (a rolling buffer) as it
//! arrives. `build_committed_capture_clip` freezes a slice of it into a new
//! clip at the cursor (placed on the track — undoably — by `CommitClipEdit`);
//! `build_running_capture_insert` picks the slice `InsertCaptureEdit` inserts
//! into an existing clip, from either pane. The
//! window that gets frozen is derived from the loop region — see
//! `100-running-capture.md` for the invariants.

use std::time::Instant;

use uuid::Uuid;

use crate::{
    core::{
        config,
        midi::message::{midi3, parse_controller},
        time,
    },
    models::{
        clip::Clip,
        event::{Event, EventType},
        track::TrackOutput,
    },
};

use super::Sequencer;

/// A take's notes ready to insert into an existing clip — what
/// [`Sequencer::build_running_capture_insert`] and a take session's Enter
/// ([`Sequencer::finish_take_framing`]) hand `InsertCaptureEdit`.
pub(crate) struct CaptureInsert {
    /// Track the target clip is on.
    pub(crate) track_idx: usize,
    /// The clip the take goes into.
    pub(crate) clip_id: Uuid,
    /// The notes and wheel moves, already in the clip's event ticks.
    pub(crate) events: Vec<Event>,
    /// Where the take's wheel span ends, in the clip's event ticks: the
    /// wheels it moves latch to here, and the clip's old curve resumes
    /// ([`Clip::land_take`], which keeps it inside the clip's window).
    pub(crate) wheel_end: i32,
}

/// The capture buffer framed as `/` would frame it now
/// ([`Sequencer::capture_take`]), for the agent's `get_capture`.
pub(crate) struct CaptureTake {
    /// The take: its window is what `/` would keep, laid at tick 0, so
    /// [`Clip::played_events`] gives its notes in ticks from the take's start.
    pub(crate) clip: Clip,
    /// Where the take's start sits in the project.
    pub(crate) place: TakePlace,
    /// When the last note-on was captured, if it came in live.
    pub(crate) last_note_at: Option<Instant>,
}

/// Where a [`CaptureTake`] starts in the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TakePlace {
    /// Played looping: the take is the loop's last pass, starting at the
    /// loop region's start.
    LoopPass,
    /// Played running but not looping: the take starts on the bar line at
    /// this project tick.
    Linear(i32),
    /// Played stopped: no place in the project yet. The take starts where
    /// phrase detection puts the phrase.
    Stopped,
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

impl RunningCaptureTarget {
    /// Whether `capture` is a take this target lands: a new clip needs a
    /// note, an insert also takes wheel moves alone (`370-wheel-capture.md`
    /// § Decisions 3).
    fn lands(self, capture: &Clip) -> bool {
        capture.has_notes()
            || matches!(self, Self::InsertFromArranger) && capture.has_controller_moves()
    }
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
        let anchor_tick = self.capture_anchor_tick(cursor_tick);
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

    /// Routes one live-input message into the capture buffer at `tick`, the
    /// `clock_tick` it landed on — its arrangement *position*. Returns
    /// whether a note-on kicked the selected track's meter
    /// ([`kick_live_meter`](Self::kick_live_meter)).
    pub(crate) fn handle_midi_input(&mut self, message: &[u8], tick: i32) -> bool {
        // Track which live keys are down so a transport stop does not release a
        // note the player is holding on an instrument track. See `130-plugin-host.md`.
        let live_target = self.live_instrument_target();
        self.instrument_notes.observe_live(live_target, message);
        // Likewise a wheel or a knob the player moves: the armed track's
        // synth is now where the player put it, not where a clip did.
        if let Some(track) = self.selected_track_mut() {
            track.hand_controller_to_player(message);
        }

        // Remember how far the capture clock's numbering sits from playback's,
        // so a commit can move the cursor into it (`capture_anchor_tick`). The
        // clock only matches playback's *phase* — it may be whole loops ahead —
        // and the sequencer doesn't hold `clock_tick`, so this input, stamped
        // with the clock's position, is the one place both are in hand at
        // once. Without it a take outside the loop (a linear take, windowed
        // from the anchor itself) commits nothing. Any message while running,
        // but never stopped: playback is parked at the cursor while the clock
        // keeps counting in, so the difference would not be whole loops.
        if self.is_running() {
            self.capture_clock_offset = Some(tick - self.playback_tick());
            if let Some(start_tick) = self.capture_held_start.take() {
                self.place_held_notes(tick, start_tick);
            }
        } else if self.capture_held_start.take().is_some() {
            // Input while stopped after a take that never got any: the held
            // notes, the buffer's only events, have no clock tick to land on
            // and were released anyway.
            self.capture_clip = Clip::new();
        }
        if is_note_on(message) {
            self.capture_last_note_at = Some(Instant::now());
        }
        if let Some((wheel, value)) = parse_controller(message)
            && let Some(i) = wheel.wheel_index()
        {
            self.live_wheels[i] = (Some(value) != wheel.rest()).then(|| midi3(message));
        }

        self.add_midi_event_to_capture(message, tick);
        self.kick_live_meter(message)
    }

    /// Starts the capture of a running take from `start_tick`, where playback
    /// starts: empties the buffer, keeping what the player holds into the
    /// take — a chord pressed before the transport started would otherwise
    /// lose its note-ons and commit nothing. Each wheel off its rest lands
    /// on `start_tick`, then the notes still down: the first-pressed on
    /// `start_tick`, each other one after it by the gap it was pressed with,
    /// at most [`HELD_NOTE_SPREAD_TICKS`](config::HELD_NOTE_SPREAD_TICKS).
    /// Each note keeps its real release, which arrives once playing. They are
    /// in cursor ticks until the take's first input while running
    /// ([`place_held_notes`](Self::place_held_notes)). See
    /// `100-running-capture.md` § Notes held into a take.
    pub(crate) fn begin_running_take(&mut self, start_tick: i32) {
        let held = self.capture_clip.open_note_ons();
        let first_on = held.iter().map(Event::tick).min().unwrap_or(0);

        self.reset_capture();
        for wheel in self.live_wheels.into_iter().flatten() {
            self.capture_clip
                .add_event(Event::from_midi_with_tick(&wheel, start_tick));
        }
        for mut note in held {
            let spread = (note.tick() - first_on).clamp(0, config::HELD_NOTE_SPREAD_TICKS);
            note.set_tick(start_tick + spread);
            note.set_length(0);
            self.capture_clip.add_event(note);
        }
        self.capture_held_start = (!self.capture_clip.events().is_empty()).then_some(start_tick);
        self.ticks_into_take = 0;
    }

    /// On the take's first input while running, stamped `tick`: moves what
    /// [`begin_running_take`](Self::begin_running_take) carried from
    /// `start_tick` — the buffer's only events — into the clock's numbering.
    /// The clock stood [`ticks_into_take`](Self::ticks_into_take) earlier at
    /// the start, so the shift is exact however often the loop has wrapped
    /// since — the latest `capture_clock_offset` grows a loop with every
    /// wrap. Rounded to whole loops, dropping input latency, as
    /// [`capture_clock_shift`](Self::capture_clock_shift) does.
    fn place_held_notes(&mut self, tick: i32, start_tick: i32) {
        let clock_at_start = tick - self.ticks_into_take;
        let shift = time::snap_to_grid(clock_at_start - start_tick, self.capture_loop_length());
        let held = std::mem::replace(&mut self.capture_clip, Clip::new());
        for event in held.events() {
            let mut event = event.clone();
            event.set_tick(event.tick() + shift);
            self.capture_clip.add_event(event);
        }
    }

    /// Kicks the selected track's meter for a live note-on, as `tick` does for
    /// its clips: the keyboard plays the selected track. Not on a plugin track
    /// during an audio export, where the keyboard reaches no plugin
    /// (`130-plugin-host.md` § Audio export). Returns whether it kicked one,
    /// so [`handle_midi_input`](Self::handle_midi_input)'s
    /// caller can wake a stopped UI.
    fn kick_live_meter(&self, message: &[u8]) -> bool {
        if !is_note_on(message) {
            return false;
        }
        let Some(track) = self.selected_track() else {
            return false;
        };
        if self.audio_export.is_some() && matches!(track.output(), TrackOutput::Instrument(_)) {
            return false;
        }
        self.track_mix
            .mark_note_on(track.slot(), time::monotonic_nanos(), message[2]);
        true
    }

    /// The note pairs of `capture` (a crop, rebased to 0) whose onsets fall
    /// in `[0, keep_end_tick)` and the wheel moves inside it, shifted by
    /// `event_offset_ticks`: what a capture insert lands in its target clip
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

    /// The running `/` with a lead clip: the notes and wheel moves the live
    /// capture lands in the selected clip, from the arranger or the clip view alike, since
    /// the clip view doesn't loop the transport on its clip
    /// (`archive/210-docked-clip-panel.md`). The pure half of the insert;
    /// [`InsertCaptureEdit`] adds the notes and clears the buffer. `None` on a
    /// buffer with neither notes nor wheel moves, or nothing that lands in
    /// the clip, so a no-op insert never enters the undo record. A take of
    /// wheel moves alone lands too, framed by the pass holding its last move
    /// (`370-wheel-capture.md` § Decisions 3–4).
    ///
    /// An overdub: everything lands where it was heard, wherever the cursors
    /// are. Anchored on the first arrangement tick of the clip that playback
    /// goes through ([`overdub_anchor`](Self::overdub_anchor)): the clip's
    /// start, or the loop's start when the loop starts inside the clip. The
    /// clip cursor plays no part — anchoring on it dropped everything played
    /// at a phase before it, so a cursor left near the clip's end lost the
    /// whole take (removed 2026-10-31, `100-running-capture.md` § Anchor
    /// contract). The window is pinned at the anchor's phase in the arranger
    /// loop, in the pass holding the take's last note-on (or last controller
    /// move), so a note lands at the region phase it was played at.
    ///
    /// After the crop window is computed and rebased to 0, a `clip_offset` of
    /// `anchor - clip.start_tick()` converts from anchor-relative ticks into
    /// clip-local ticks (0-based from `clip.start_tick()`). Notes the shift
    /// would push past `clip.region_length()` — past the clip's span inside a
    /// longer loop — are discarded, never shifted into the clip.
    ///
    /// The wheels latch to the end of the loop pass the window starts in,
    /// not the window's: a one-bar loop over a four-bar clip hands back to
    /// the clip's own curve at the loop's end, and a wheel move after it
    /// belongs to another pass (`370-wheel-capture.md`). Not looping, they
    /// latch to the window's end. Either way never past the clip's end.
    ///
    /// [`InsertCaptureEdit`]: crate::core::sequencer::edit::InsertCaptureEdit
    pub(crate) fn build_running_capture_insert(&self) -> Option<CaptureInsert> {
        if !RunningCaptureTarget::InsertFromArranger.lands(&self.capture_clip) {
            return None;
        }

        let track_idx = self.selected_track_index()?;
        let clip = self.selected_clip()?;
        let clip_id = clip.id();
        let clip_start_tick = clip.start_tick();
        let clip_region_start = clip.region().start();
        let region_length = clip.region_length().max(time::min_clip_length_ticks());
        let anchor = self.overdub_anchor(clip_start_tick, region_length);

        // The crop window is the target clip's length, pinned at the anchor's
        // phase in the arranger loop (independent of the clip).
        let capture = self.normalize_running_capture(
            self.capture_anchor_tick(anchor),
            region_length,
            RunningCaptureTarget::InsertFromArranger,
        )?;

        // Shift from anchor-relative space into the clip's event ticks: the
        // clip's first arrangement tick is event tick `region.start` (non-zero
        // once its left edge was dragged in), then the anchor's offset into
        // the clip. Onsets the shift would push past the clip end — phases
        // before the anchor, played after the wrap — are dropped, not parked
        // out of region. The crop above rebased the window to 0 and closed
        // every note inside it, so the whole room keeps every note; it also
        // keeps a wheel moved after the last note-off (a bend easing back).
        let clip_offset = anchor - clip_start_tick;
        let room = region_length - clip_offset;
        let insert_at = clip_region_start + clip_offset;
        // `land_take` keeps the wheel span inside the clip.
        let wheel_room = if self.playback_is_looping() {
            let loop_len = self.capture_loop_length();
            loop_len - (anchor - self.region_start()).rem_euclid(loop_len)
        } else {
            room
        };

        let events = Self::capture_events_to_insert(&capture, insert_at, room);
        (!events.is_empty()).then_some(CaptureInsert {
            track_idx,
            clip_id,
            events,
            wheel_end: insert_at + wheel_room,
        })
    }

    /// Where a running insert into a clip spanning `clip_start` for `length`
    /// ticks is anchored: the first of its arrangement ticks playback goes
    /// through — the loop's start when looping and the loop starts inside
    /// the clip, otherwise the clip's start (a loop starting before it
    /// reaches its start; not looping, playback runs through it from there).
    fn overdub_anchor(&self, clip_start: i32, length: i32) -> i32 {
        let loop_start = self.region_start();
        if self.playback_is_looping() && (clip_start..clip_start + length).contains(&loop_start) {
            loop_start
        } else {
            clip_start
        }
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
    ///   of a wrap anticipate the downbeat after it: they belong to the next
    ///   pass (the pass grid is read a tolerance early) and are relocated to
    ///   its phase 0 before the fold — inside the region when the anchor is
    ///   at phase 0, retained pre-roll otherwise (never teleported to a
    ///   mid-loop cursor);
    /// - linear: everything from the earliest note-on's bar (relative to the
    ///   anchor) through the content end, the region starting at the anchor.
    ///
    /// A silent region with retained pre-roll is still a clip: the user drags
    /// the left edge to reveal it.
    ///
    /// Every target carries the wheels off their rest where the kept span
    /// starts onto its start ([`Clip::carry_wheels_onto`]): a wheel set in an
    /// earlier pass, or before the transport started, and held, is part of
    /// the take (`370-wheel-capture.md` § Decisions 7). A new clip also holds
    /// each knob's start value there ([`Clip::hold_cc_start_values`]), so the
    /// stretch before a turn plays where the knob stood; an insert leaves
    /// that to [`Clip::land_take`], which knows the clip's own data.
    ///
    /// **The insert targets** select a window of `region_length` ticks pinned
    /// at `anchor_tick`'s phase ([`Self::calculate_running_region`]) and crop
    /// to it; `InsertFromArranger` relocates wrap-anticipating notes to the
    /// *loop* start first (kept by the crop only when that is the anchor).
    ///
    /// Either way the result's tick origin is `0`. Returns `None` when nothing
    /// survives: no note for a new clip, neither a note nor a wheel move for
    /// an insert, whose pass a take of wheel moves alone is framed by
    /// ([`Self::calculate_running_region`]).
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
                // A note-on within the tolerance of a wrap anticipates the
                // next pass's downbeat, so the pass grid is read that much
                // early: the anticipating note selects the pass it starts
                // and is relocated onto its downbeat (held length intact)
                // before the fold, never dragging the previous pass along.
                let tolerance = config::LATE_NOTE_TOLERANCE_TICKS;
                let (pass_start, pass_end) = Self::calculate_running_region(
                    &capture,
                    self.region_start() - tolerance,
                    loop_len,
                    Some(loop_len),
                );
                let (pass_start, pass_end) = (pass_start + tolerance, pass_end + tolerance);
                capture.relocate_late_notes_to_region_start(pass_start, pass_end, tolerance);
                capture.carry_wheels_onto(pass_start);
                capture.hold_cc_start_values(pass_start);
                capture.fold_into_loop(pass_start, pass_end, self.region_start(), loop_len);

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
                capture.carry_wheels_onto(retained_start);
                capture.hold_cc_start_values(retained_start);

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
                capture.carry_wheels_onto(region_start_tick);
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

        target.lands(&capture).then_some(capture)
    }

    /// The capture buffer as `/` would frame it now, for the agent's
    /// `get_capture` (`360-mcp-server.md`): `None` without a note. Stopped,
    /// the window phrase detection gives the stopped `/`
    /// ([`build_stopped_capture_clip`](Self::build_stopped_capture_clip),
    /// before its bar rounding); running, the clip the running `/` builds
    /// ([`normalize_running_capture`](Self::normalize_running_capture)),
    /// its window widened over everything it keeps: the whole last pass
    /// when looping, the pre-roll before the cursor when not. Pure.
    pub(crate) fn capture_take(&self) -> Option<CaptureTake> {
        if !self.capture_clip.has_notes() {
            return None;
        }
        let (mut clip, place) = if self.is_running() {
            let anchor = self.capture_anchor_tick(self.cursor_tick());
            // The length only sizes a linear take's window: a looping one is
            // widened to the whole pass below.
            let mut clip = self.normalize_running_capture(
                anchor,
                self.linear_content_length(anchor),
                RunningCaptureTarget::NewClip,
            )?;
            let (window_start, window_end) = (clip.region().start(), clip.region().end());
            let place = if self.playback_is_looping() {
                clip.region_mut()
                    .set_region(Some(0), Some(self.capture_loop_length()));
                TakePlace::LoopPass
            } else {
                clip.region_mut().set_region(Some(0), Some(window_end));
                TakePlace::Linear(self.cursor_tick() - window_start)
            };
            (clip, place)
        } else {
            (self.build_detected_phrase_clip(), TakePlace::Stopped)
        };
        clip.set_start_tick(0);
        Some(CaptureTake {
            clip,
            place,
            last_note_at: self.capture_last_note_at,
        })
    }

    /// `tick` (a cursor) in the capture clock's numbering — the anchor a
    /// commit windows the capture buffer from. The events are stamped in
    /// `clock_tick` space, which agrees with the cursor's only in *phase*:
    /// enough for a looping take (its pass index absorbs whole loops), not
    /// for a linear one, whose window starts at the anchor itself — a take
    /// played outside the loop used to commit nothing. See
    /// `100-running-capture.md` § Anchor contract.
    fn capture_anchor_tick(&self, tick: i32) -> i32 {
        tick + self.capture_clock_shift()
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
    use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

    use crossbeam_channel::Receiver;

    use super::*;
    use crate::{
        core::{
            config,
            midi::{message::Controller, out_queue::MidiOutMessage},
            sequencer::edit::{
                CommitClipEdit, EditResult, InsertCaptureEdit, ResizeClipEdit, SequencerEdit,
            },
            sequencer::test_support::{
                bend_at, cc_at, clip_at, instrument_track_0, mod_at, note_off, note_on, rect,
                sequencer_with_outputs, test_sequencer, wheel_moves,
            },
            time::{self, Meter},
            view_state::ViewState,
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

    // --- capture_take ---

    /// The note-on ticks the take sounds, from its start.
    fn take_note_ons(seq: &Sequencer) -> Vec<i32> {
        seq.capture_take()
            .unwrap()
            .clip
            .played_events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(Event::tick)
            .collect()
    }

    #[test]
    fn capture_take_is_none_on_an_empty_buffer() {
        assert!(make_sequencer(0, 3840).capture_take().is_none());
    }

    #[test]
    fn capture_take_while_looping_is_the_last_pass_from_the_loop_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let loop_len = bar * 2;
        let mut seq = make_sequencer(bar * 4, loop_len);
        seq.running.store(true, Ordering::Relaxed);
        for cycle in 0..3 {
            let on = bar * 4 + cycle * loop_len + 600 + cycle * 10;
            seq.capture_clip.add_event(note_on(on));
            seq.capture_clip.add_event(note_off(on + 300));
        }
        let take = seq.capture_take().unwrap();
        assert_eq!(take.place, TakePlace::LoopPass);
        assert_eq!(take_note_ons(&seq), vec![620]);
    }

    #[test]
    fn capture_take_without_looping_is_the_commit_from_its_pre_roll() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.running.store(true, Ordering::Relaxed);
        seq.loop_enabled.store(false, Ordering::Relaxed);
        seq.playback_tick.store(bar * 5, Ordering::Relaxed);
        // The cursor a bar after the first note: that bar is pre-roll.
        seq.cursor_tick.store(bar * 6, Ordering::Relaxed);
        seq.capture_clip.add_event(note_on(bar * 5 + 100));
        seq.capture_clip.add_event(note_off(bar * 5 + 400));
        seq.capture_clip.add_event(note_on(bar * 6 + 960));
        seq.capture_clip.add_event(note_off(bar * 6 + 1200));
        let take = seq.capture_take().unwrap();
        assert_eq!(take.place, TakePlace::Linear(bar * 5));
        assert_eq!(take_note_ons(&seq), vec![100, bar + 960]);
    }

    #[test]
    fn capture_take_while_stopped_is_the_detected_phrase_and_knows_when() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        assert!(!seq.is_running());
        seq.handle_midi_input(&[0x90, 60, 100], bar + 960);
        seq.handle_midi_input(&[0x80, 60, 0], bar + 1400);
        let take = seq.capture_take().unwrap();
        assert_eq!(take.place, TakePlace::Stopped);
        assert!(take.last_note_at.is_some());
        let ons = take_note_ons(&seq);
        assert_eq!(ons.len(), 1);
        assert!(ons[0] >= 0 && ons[0] < bar, "{ons:?}");

        seq.reset_capture();
        assert!(seq.capture_take().is_none());
        assert!(seq.capture_last_note_at.is_none());
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
    /// not played, never teleported to the cursor. It starts the next pass,
    /// so the previous pass's bar-1 note is not carried along.
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
        assert!(region_note_on_ticks(clip).is_empty());
        assert_eq!(last_clip_note_on_ticks(&seq), vec![0]);
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
        assert_eq!(ons, vec![0]);
    }

    /// The note from the private edge-case note: a chord played a hair ahead
    /// of the wrap, meant for bar 1. It starts the next pass — the commit is
    /// the chord on the downbeat alone, not the previous pass's noodle with
    /// the chord on top.
    #[test]
    fn commit_clip_to_track_early_chord_starts_the_next_pass() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        // The previous pass's noodle, bars 0 and 1.
        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        // The chord, a little early, held into the next pass.
        for (note, on) in [(60, bar * 2 - 40), (64, bar * 2 - 30), (67, bar * 2 - 20)] {
            seq.capture_clip
                .add_event(Event::new(on, 0, vec![0x90, note, 100]));
        }
        for note in [60, 64, 67] {
            seq.capture_clip
                .add_event(Event::new(bar * 2 + 400, 0, vec![0x80, note, 0]));
        }

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        assert_eq!(region_note_on_ticks(clip), vec![0, 10, 20]);
        assert_eq!(last_clip_note_on_ticks(&seq), vec![0, 10, 20]);
    }

    /// A chord rolled across the wrap — some notes just before it, the rest
    /// just after — stays whole on the downbeat: the pre-wrap notes belong to
    /// the pass the post-wrap ones select, not to the one before.
    #[test]
    fn commit_clip_to_track_chord_rolled_across_the_wrap_stays_whole() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        for (note, on) in [(60, bar * 2 - 30), (64, bar * 2 - 10), (67, bar * 2 + 10)] {
            seq.capture_clip
                .add_event(Event::new(on, 0, vec![0x90, note, 100]));
            seq.capture_clip
                .add_event(Event::new(bar * 2 + 400, 0, vec![0x80, note, 0]));
        }

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        let notes: Vec<(i32, u8)> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.note_number().unwrap()))
            .collect();
        assert_eq!(notes, vec![(0, 60), (10, 67), (20, 64)]);
    }

    /// An early first note held into bar 1 keeps its held length — it is
    /// moved onto the downbeat before the fold, not closed at the loop end
    /// and then moved.
    #[test]
    fn commit_clip_to_track_early_held_note_keeps_its_length() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.capture_clip.add_event(note_on(bar * 2 - 50));
        seq.capture_clip.add_event(note_off(bar * 2 + 400));

        seq.commit_clip_to_track().unwrap();

        let clip = last_clip(&seq);
        let edges: Vec<(i32, bool)> = clip
            .events()
            .iter()
            .map(|e| (e.tick(), e.event_type() == Some(EventType::NoteOn)))
            .collect();
        assert_eq!(edges, vec![(0, true), (450, false)]);
    }

    /// A note just outside the tolerance is a pickup at the end of the pass
    /// it was played in: it stays where it was played, with that pass.
    #[test]
    fn commit_clip_to_track_note_outside_the_tolerance_stays_in_its_pass() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        let pickup = bar * 2 - config::LATE_NOTE_TOLERANCE_TICKS - 1;

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(pickup));
        seq.capture_clip.add_event(note_off(pickup + 10));

        seq.commit_clip_to_track().unwrap();

        assert_eq!(last_clip_note_on_ticks(&seq), vec![600, pickup]);
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

    /// An arranger cursor move to `tick`: the lead clip's cursor follows it,
    /// as `sync_clip_selection_to_cursor_workflow` does in the app.
    fn move_cursor(seq: &mut Sequencer, tick: i32) {
        seq.cursor_tick.store(tick, Ordering::Relaxed);
        seq.sync_selected_clip_cursor_with_arranger();
    }

    /// The NoteOn ticks of `seq`'s selected clip, sorted.
    fn selected_clip_note_on_ticks(seq: &Sequencer) -> Vec<i32> {
        note_on_ticks(seq.selected_clip().unwrap())
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
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![(700, vec![0xE0, 0x00, 0x60]), (950, vec![0xB0, 0x01, 70])]
        );
    }

    /// A 2-bar loop at 0 and a 2-bar lead clip there holding `curve`, with
    /// a note at 100–300 and `moves` in the capture buffer.
    fn take_over_curve(curve: Vec<Event>, moves: &[Event]) -> Sequencer {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        seq.selected_clip_mut().unwrap().restore_events(curve);
        let mut buffer = vec![note_on(100), note_off(300)];
        buffer.extend_from_slice(moves);
        buffer.sort_by_key(Event::tick);
        for event in buffer {
            seq.capture_clip.add_event(event);
        }
        seq
    }

    /// A second pass over a clip that already has a curve replaces it from
    /// the take's first move on instead of interleaving the two, and ⌘Z
    /// brings the old curve back.
    #[test]
    fn a_take_over_a_curve_replaces_it_instead_of_interleaving() {
        let curve = vec![mod_at(0, 20), mod_at(500, 40), mod_at(1000, 60)];
        let mut seq = take_over_curve(curve.clone(), &[mod_at(400, 90), mod_at(800, 100)]);
        let mut record = Record::new();

        let edit = InsertCaptureEdit::from_running_capture(&seq).unwrap();
        record.edit(&mut seq, SequencerEdit::InsertCapture(edit));

        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (400, vec![0xB0, 0x01, 90]),
                (800, vec![0xB0, 0x01, 100]),
            ]
        );
        record.undo(&mut seq);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            wheel_moves(&curve)
        );
    }

    /// A take of notes alone leaves both of the clip's curves as they were.
    #[test]
    fn a_notes_only_take_leaves_both_wheels() {
        let curve = vec![bend_at(0, 0x60), mod_at(200, 40), bend_at(600, 0x40)];
        let mut seq = take_over_curve(curve.clone(), &[]);

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![100]);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            wheel_moves(&curve)
        );
    }

    /// Riding the mod wheel replaces the mod curve and leaves the bends.
    #[test]
    fn riding_one_wheel_leaves_the_other() {
        let curve = vec![bend_at(0, 0x60), mod_at(200, 40), bend_at(600, 0x40)];
        let mut seq = take_over_curve(curve, &[mod_at(150, 90)]);

        seq.insert_running_capture().unwrap();

        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xE0, 0x00, 0x60]),
                (150, vec![0xB0, 0x01, 90]),
                (600, vec![0xE0, 0x00, 0x40]),
            ]
        );
    }

    /// A one-bar loop over a four-bar clip: the wheel latches to the loop's
    /// end, where the clip's old curve resumes, not to the clip's end; a
    /// move after the wrap belongs to another pass and stays out
    /// (`370-wheel-capture.md` § Open question).
    #[test]
    fn a_short_loop_latches_the_wheel_to_the_pass_end() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar * 2, bar);
        add_and_select_clip(&mut seq, 0, bar * 4);
        let sweep: Vec<_> = (0..4).map(|i| mod_at(i * bar, 20 + i as u8 * 20)).collect();
        seq.selected_clip_mut().unwrap().restore_events(sweep);
        move_cursor(&mut seq, bar * 2);
        for event in [
            note_on(bar * 2 + 100),
            note_off(bar * 2 + 300),
            mod_at(bar * 2 + 960, 100),
            mod_at(bar * 3 + 100, 0),
        ] {
            seq.capture_clip.add_event(event);
        }

        seq.insert_running_capture().unwrap();

        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (bar, vec![0xB0, 0x01, 40]),
                (bar * 2, vec![0xB0, 0x01, 60]),
                (bar * 2 + 960, vec![0xB0, 0x01, 100]),
                (bar * 3, vec![0xB0, 0x01, 80]),
            ]
        );
    }

    /// A wheel set in an earlier pass and held counts as a move at the
    /// start of the pass the take lands from.
    #[test]
    fn a_wheel_held_from_an_earlier_pass_lands_from_the_window_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = take_over_curve(vec![mod_at(0, 20)], &[]);
        for event in [
            mod_at(500, 70),
            note_on(bar * 2 + 100),
            note_off(bar * 2 + 300),
        ] {
            seq.capture_clip.add_event(event);
        }

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![100]);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![(0, vec![0xB0, 0x01, 70])]
        );
    }

    /// Without a lead clip a take is its notes: a wheel wiggled with nothing
    /// played commits no clip, running or stopped, and stopped it opens no
    /// take session into a lead clip either (`370-wheel-capture.md`
    /// § Decisions 2–3).
    #[test]
    fn a_capture_of_wheel_moves_alone_commits_no_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        for tick in [100, 400, 700] {
            seq.capture_clip.add_event(mod_at(tick, 90));
        }

        assert!(seq.build_committed_capture_clip().is_none());
        assert!(seq.build_stopped_capture_clip().is_none());
        add_and_select_clip(&mut seq, 0, bar * 2);
        assert!(!seq.open_take_framing(ViewState::Clip));
    }

    /// Running with a lead clip, a take of wheel moves alone lands in it by
    /// the latch rule, and ⌘Z brings the old curve back.
    #[test]
    fn a_running_take_of_wheel_moves_alone_lands_in_the_lead_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        let curve = vec![mod_at(0, 20), mod_at(bar, 40)];
        seq.selected_clip_mut()
            .unwrap()
            .restore_events(curve.clone());
        for event in [mod_at(400, 90), mod_at(700, 100)] {
            seq.capture_clip.add_event(event);
        }
        let mut record = Record::new();

        let edit = InsertCaptureEdit::from_running_capture(&seq).unwrap();
        record.edit(&mut seq, SequencerEdit::InsertCapture(edit));

        assert!(selected_clip_note_on_ticks(&seq).is_empty());
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (400, vec![0xB0, 0x01, 90]),
                (700, vec![0xB0, 0x01, 100])
            ]
        );
        assert!(seq.capture_clip.events().is_empty(), "buffer consumed");

        record.undo(&mut seq);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            wheel_moves(&curve)
        );
    }

    /// A wheel-only take over a loop played several times lands the pass
    /// holding its last move, carrying the value an earlier pass left the
    /// wheel at onto its start.
    #[test]
    fn a_wheel_only_take_lands_the_pass_holding_its_last_move() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar);
        add_and_select_clip(&mut seq, 0, bar);
        for event in [mod_at(300, 90), mod_at(bar * 2 + 600, 30)] {
            seq.capture_clip.add_event(event);
        }

        seq.insert_running_capture().unwrap();

        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![(0, vec![0xB0, 0x01, 90]), (600, vec![0xB0, 0x01, 30])]
        );
    }

    /// A knob turned with notes lands by the wheels' latch rule, and the
    /// other knobs' curves and the wheels are left as they were
    /// (`390-knob-panel.md` decision 9).
    #[test]
    fn a_knob_take_with_notes_lands_and_leaves_the_other_controllers() {
        let curve = vec![
            cc_at(0, 14, 10),
            cc_at(0, 15, 40),
            mod_at(200, 50),
            cc_at(1000, 14, 60),
        ];
        let mut seq = take_over_curve(curve, &[cc_at(400, 14, 90), cc_at(800, 14, 100)]);

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![100]);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 14, 10]),
                (0, vec![0xB0, 15, 40]),
                (200, vec![0xB0, 0x01, 50]),
                (400, vec![0xB0, 14, 90]),
                (800, vec![0xB0, 14, 100]),
            ]
        );
    }

    /// Running with a lead clip, a take of a knob alone lands in it like a
    /// wheel-only one, and ⌘Z brings the old curve back.
    #[test]
    fn a_running_take_of_a_knob_alone_lands_in_the_lead_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        let curve = vec![cc_at(0, 14, 20), cc_at(bar, 14, 40)];
        seq.selected_clip_mut()
            .unwrap()
            .restore_events(curve.clone());
        for event in [cc_at(400, 14, 90), cc_at(700, 14, 100)] {
            seq.capture_clip.add_event(event);
        }
        let mut record = Record::new();

        let edit = InsertCaptureEdit::from_running_capture(&seq).unwrap();
        record.edit(&mut seq, SequencerEdit::InsertCapture(edit));

        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 14, 20]),
                (400, vec![0xB0, 14, 90]),
                (700, vec![0xB0, 14, 100])
            ]
        );
        record.undo(&mut seq);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            wheel_moves(&curve)
        );
    }

    /// A knob turned with nothing played commits nothing stopped, as a
    /// wheel-only take doesn't, and opens no take session into a lead clip.
    #[test]
    fn a_stopped_take_of_a_knob_alone_commits_nothing() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        for tick in [100, 400, 700] {
            seq.capture_clip.add_event(cc_at(tick, 14, 90));
        }

        assert!(seq.build_committed_capture_clip().is_none());
        assert!(seq.build_stopped_capture_clip().is_none());
        add_and_select_clip(&mut seq, 0, bar * 2);
        assert!(!seq.open_take_framing(ViewState::Clip));
    }

    /// A new clip from a pass where a knob was turned mid-way plays the
    /// value the turn started from before it, so the loop's wrap doesn't
    /// leave the knob where the pass ended (`390-knob-panel.md` decision 9).
    #[test]
    fn a_new_clip_holds_where_a_knob_stood_before_its_turn() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        for event in [
            note_on(100),
            note_off(300),
            cc_at(bar, 14, 20),
            cc_at(bar, 14, 30),
            cc_at(bar + 400, 14, 90),
        ] {
            seq.capture_clip.add_event(event);
        }

        let (_, clip) = seq.build_committed_capture_clip().unwrap();

        assert_eq!(
            wheel_moves(clip.events()),
            vec![
                (0, vec![0xB0, 14, 20]),
                (bar, vec![0xB0, 14, 30]),
                (bar + 400, vec![0xB0, 14, 90]),
            ]
        );
    }

    /// A knob's CC has no rest to be off: where the player left it is never
    /// carried into a take, as a wheel off its rest is.
    #[test]
    fn a_turned_knob_is_not_carried_into_the_take() {
        assert!(take_after_wheel(&[[0xB0, 14, 64]]).is_empty());
    }

    /// The capture buffer takes a wheel move off the input like a note.
    #[test]
    fn a_wheel_move_reaches_the_capture_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);

        seq.handle_midi_input(&[0xE0, 0x00, 0x50], 300);

        let ticks = |clip: &Clip| -> Vec<i32> { clip.events().iter().map(Event::tick).collect() };
        assert_eq!(ticks(&seq.capture_clip), vec![300]);
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
        move_cursor(&mut seq, clip_start);

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

        move_cursor(&mut seq, clip_start);
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

        move_cursor(&mut seq, clip_start);

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

    /// An overdub lands the last pass where it was heard, wherever the
    /// cursors are: the cursors a bar into a 2-bar clip, the post-wrap note
    /// at a phase before them lands at its phase, and the pass before it
    /// (one note) is not the take.
    #[test]
    fn insert_running_capture_lands_notes_before_the_cursor_phase() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);
        move_cursor(&mut seq, clip_start + bar);

        seq.capture_clip.add_event(note_on(clip_start + bar + 600));
        seq.capture_clip.add_event(note_off(clip_start + bar + 900));
        seq.capture_clip
            .add_event(note_on(clip_start + bar * 2 + 100));
        seq.capture_clip
            .add_event(note_off(clip_start + bar * 2 + 400));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![100]);
    }

    /// Regression (knobs.stev, 2026-10-31): a clip cursor left a few ticks
    /// inside the clip's end — where Enter's tempo fit could leave it after
    /// `]` — anchored every take there, and everything played before it was
    /// dropped: a knob turn landed nothing. The clip cursor plays no part.
    #[test]
    fn insert_running_capture_ignores_a_clip_cursor_near_the_end() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        seq.set_selected_clip_cursor_tick(bar * 2 - 16);
        for event in [
            note_on(600),
            note_off(900),
            cc_at(bar + 100, 14, 30),
            cc_at(bar + 200, 14, 60),
        ] {
            seq.capture_clip.add_event(event);
        }

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);
        assert_eq!(
            wheel_moves(seq.selected_clip().unwrap().events()),
            vec![
                (0, vec![0xB0, 14, 30]),
                (bar + 100, vec![0xB0, 14, 30]),
                (bar + 200, vec![0xB0, 14, 60])
            ],
            "the turn's start value held from the window's start (`390` decision 9)"
        );
    }

    /// A loop starting inside the clip: the take lands from the loop's
    /// start, where playback goes through the clip, not from the clip's.
    #[test]
    fn insert_running_capture_into_a_clip_longer_than_the_loop_lands_at_the_loop() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(bar * 2, bar); // loop over bar 3
        add_and_select_clip(&mut seq, 0, bar * 4);
        seq.set_selected_clip_cursor_tick(0);
        seq.capture_clip.add_event(note_on(bar * 2 + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![bar * 2 + 600]);
    }

    /// A 2-bar clip inside a 4-bar arranger loop, cursor at the clip start:
    /// notes played past the clip's span (bars 2-3 of the same pass) are
    /// dropped, not slid back into the clip.
    #[test]
    fn insert_running_capture_drops_notes_played_past_the_clip_span() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 4);
        add_and_select_clip(&mut seq, 0, bar * 2);
        move_cursor(&mut seq, 0);

        seq.capture_clip.add_event(note_on(600));
        seq.capture_clip.add_event(note_off(900));
        seq.capture_clip.add_event(note_on(bar * 3 + 600));
        seq.capture_clip.add_event(note_off(bar * 3 + 900));

        seq.insert_running_capture().unwrap();

        assert_eq!(selected_clip_note_on_ticks(&seq), vec![600]);
    }

    /// Regression: the insert anchored on the arranger cursor, which `⌥Space`
    /// leaves where it was. A 4-bar clip under a 4-bar loop, the arranger
    /// cursor at bar 3, the take played from the clip cursor at bar 2 with
    /// `⌥Space`: its notes in bars 2–3 sat at phases before the arranger
    /// cursor and were all dropped. Neither cursor anchors the take now.
    #[test]
    fn insert_running_capture_ignores_the_arranger_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 4);
        add_and_select_clip(&mut seq, 0, bar * 4);
        move_cursor(&mut seq, bar * 2);
        seq.set_selected_clip_cursor_tick(bar);

        seq.capture_clip.add_event(note_on(bar + 600));
        seq.capture_clip.add_event(note_off(bar + 900));
        seq.capture_clip.add_event(note_on(bar * 2 + 600));
        seq.capture_clip.add_event(note_off(bar * 2 + 900));

        seq.insert_running_capture().unwrap();

        assert_eq!(
            selected_clip_note_on_ticks(&seq),
            vec![bar + 600, bar * 2 + 600]
        );
    }

    /// Returns `None` on empty capture buffer.
    #[test]
    fn insert_running_capture_returns_none_on_empty_capture() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        move_cursor(&mut seq, clip_start);

        assert!(seq.insert_running_capture().is_none());
    }

    /// Capture buffer is cleared after a successful commit.
    #[test]
    fn insert_running_capture_clears_capture_buffer() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let clip_start = bar * 3;
        let mut seq = make_sequencer(clip_start, bar * 2);
        add_and_select_clip(&mut seq, clip_start, bar * 2);

        move_cursor(&mut seq, clip_start);

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
        move_cursor(&mut seq, clip_start);
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
            input_at(
                seq,
                message,
                playback + offset,
                playback + offset + clock_ahead,
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
        move_cursor(&mut seq, clip_start);

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

    // --- notes held into a running take ---

    /// Feeds `message` through the real input dispatch, stamped `clock`, with
    /// playback at `playback`.
    fn input_at(seq: &mut Sequencer, message: [u8; 3], playback: i32, clock: i32) {
        seq.playback_tick.store(playback, Ordering::Relaxed);
        seq.handle_midi_input(&message, clock);
    }

    /// Presses `notes` while stopped, at their clock ticks, then starts the
    /// transport from the cursor as Space does.
    fn hold_then_start(seq: &mut Sequencer, notes: &[(u8, i32)]) {
        let cursor = seq.cursor_tick();
        for &(note, clock) in notes {
            input_at(seq, [0x90, note, 100], cursor, clock);
        }
        seq.begin_running_take(cursor);
        seq.running.store(true, Ordering::Relaxed);
    }

    /// `(tick, note, length)` of each note-on in `clip`, sorted.
    fn notes_of(clip: &Clip) -> Vec<(i32, u8, i32)> {
        let mut notes: Vec<_> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.note_number().unwrap(), e.end_tick() - e.tick()))
            .collect();
        notes.sort_unstable();
        notes
    }

    /// `(tick, note)` of each note-on in `clip`, sorted.
    fn ons_of(clip: &Clip) -> Vec<(i32, u8)> {
        notes_of(clip)
            .into_iter()
            .map(|(tick, note, _)| (tick, note))
            .collect()
    }

    /// Regression: a chord held before the transport started lost its
    /// note-ons (the start emptied the buffer), so a commit before the first
    /// wrap left it out. It lands on the cursor, rolled as it was pressed,
    /// each note held as long as it really was.
    #[test]
    fn a_chord_held_before_the_start_commits_at_the_cursor_with_its_spread() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        hold_then_start(&mut seq, &[(60, 50_000), (64, 50_030), (67, 50_060)]);

        // Five loops ahead, plus input latency.
        let ahead = bar * 2 * 5 + 2;
        seq.ticks_into_take = 1000;
        for note in [60, 64, 67] {
            input_at(&mut seq, [0x80, note, 0], 1000, 1000 + ahead);
        }
        input_at(&mut seq, [0x90, 62, 100], 1200, 1200 + ahead);
        input_at(&mut seq, [0x80, 62, 0], 1400, 1400 + ahead);

        seq.commit_clip_to_track().unwrap();

        // The chord's start drops the input latency; its releases and the
        // played note keep it, as every played note does.
        assert_eq!(
            notes_of(last_clip(&seq)),
            vec![(0, 60, 1002), (30, 64, 972), (60, 67, 942), (1202, 62, 200)]
        );
    }

    /// A note added long after the first lands at most a quarter after it,
    /// and a chord still held at the commit, with no input since the start,
    /// commits too.
    #[test]
    fn held_notes_spread_at_most_a_quarter_and_commit_while_still_held() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        hold_then_start(&mut seq, &[(60, 50_000), (64, 50_000 + time::PPQN * 3)]);

        seq.commit_clip_to_track().unwrap();

        assert_eq!(
            ons_of(last_clip(&seq)),
            vec![(0, 60), (config::HELD_NOTE_SPREAD_TICKS, 64)]
        );
    }

    /// Only what is still held at the start is carried: a note released
    /// before it stays out of the take.
    #[test]
    fn a_note_released_before_the_start_is_not_held_into_the_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        input_at(&mut seq, [0x90, 60, 100], 0, 50_000);
        input_at(&mut seq, [0x80, 60, 0], 0, 50_100);
        hold_then_start(&mut seq, &[(64, 50_200)]);

        seq.commit_clip_to_track().unwrap();

        assert_eq!(ons_of(last_clip(&seq)), vec![(0, 64)]);
    }

    /// The held chord belongs to the first pass: once the loop has wrapped
    /// and a later pass holds the last note, the commit keeps that pass
    /// alone, as for any note played in an earlier pass.
    #[test]
    fn a_wrap_leaves_the_held_chord_in_the_first_pass() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        hold_then_start(&mut seq, &[(60, 50_000)]);

        // The clock three loops ahead at the start, one more after the wrap.
        let ahead = bar * 2 * 3;
        seq.ticks_into_take = 500;
        input_at(&mut seq, [0x80, 60, 0], 500, 500 + ahead);
        let wrapped = bar * 2 + 200;
        input_at(&mut seq, [0x90, 62, 100], 200, wrapped + ahead);
        input_at(&mut seq, [0x80, 62, 0], 400, wrapped + 200 + ahead);

        seq.commit_clip_to_track().unwrap();

        assert_eq!(notes_of(last_clip(&seq)), vec![(200, 62, 200)]);
    }

    /// A chord held through the wrap, its release the take's first input:
    /// it still starts on the cursor in the first pass — the clock offset at
    /// the release is a loop more than at the start, and the chord is placed
    /// by the start's.
    #[test]
    fn a_chord_released_after_the_wrap_still_starts_the_first_pass() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let loop_len = bar * 2;
        let mut seq = make_sequencer(0, loop_len);
        hold_then_start(&mut seq, &[(60, 50_000)]);

        let ahead = loop_len * 3;
        seq.ticks_into_take = loop_len + 300;
        input_at(&mut seq, [0x80, 60, 0], 300, loop_len + 300 + ahead);

        seq.commit_clip_to_track().unwrap();

        // Held across the wrap, so closed at the loop's end.
        assert_eq!(notes_of(last_clip(&seq)), vec![(0, 60, loop_len - 1)]);
    }

    /// A linear take (loop off, cursor past the region) starts its held
    /// chord on the cursor too, the clip placed there.
    #[test]
    fn a_linear_take_starts_its_held_chord_on_the_cursor() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        seq.loop_enabled.store(false, Ordering::Relaxed);
        move_cursor(&mut seq, bar * 4);
        hold_then_start(&mut seq, &[(60, 50_000), (64, 50_020)]);
        assert!(!seq.playback_is_looping(), "the take must be linear");

        let ahead = bar * 2 * 7;
        seq.ticks_into_take = 900;
        for note in [60, 64] {
            input_at(
                &mut seq,
                [0x80, note, 0],
                bar * 4 + 900,
                bar * 4 + 900 + ahead,
            );
        }

        let meta = seq.commit_clip_to_track().unwrap();

        assert_eq!(meta.start_tick, bar * 4);
        assert_eq!(notes_of(last_clip(&seq)), vec![(0, 60, 900), (20, 64, 880)]);
    }

    /// The wheel moves of a take started after the wheel `moves`, made
    /// while stopped before an earlier reset emptied the buffer.
    fn take_after_wheel(moves: &[[u8; 3]]) -> Vec<(i32, Vec<u8>)> {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        for &message in moves {
            input_at(&mut seq, message, 0, 40_000);
        }
        seq.reset_capture();
        hold_then_start(&mut seq, &[]);
        seq.ticks_into_take = 300;
        input_at(&mut seq, [0x90, 60, 100], 300, 300);
        input_at(&mut seq, [0x80, 60, 0], 500, 500);
        seq.commit_clip_to_track().unwrap();
        wheel_moves(last_clip(&seq).events())
    }

    /// The mod wheel the player left off its rest — even before a reset
    /// emptied the buffer — starts the take on the cursor; one back at rest
    /// adds nothing.
    #[test]
    fn a_mod_wheel_off_its_rest_is_carried_into_the_take() {
        assert_eq!(
            take_after_wheel(&[[0xB0, 1, 64]]),
            vec![(0, Controller::MOD_WHEEL.message(64))]
        );
        assert!(take_after_wheel(&[[0xB0, 1, 64], [0xB0, 1, 0]]).is_empty());
    }

    /// The bend's twin: a bend held into the take starts it on the cursor,
    /// alongside a held mod wheel; one back at centre adds nothing.
    #[test]
    fn a_bend_off_its_rest_is_carried_into_the_take() {
        assert_eq!(
            take_after_wheel(&[[0xE0, 0, 0x60], [0xB0, 1, 64]]),
            vec![
                (0, Controller::PitchBend.message(0x3000)),
                (0, Controller::MOD_WHEEL.message(64)),
            ]
        );
        assert!(take_after_wheel(&[[0xE0, 0, 0x60], [0xE0, 0, 0x40]]).is_empty());
    }

    /// The held notes commit with a running `/` into a selected clip too.
    #[test]
    fn insert_running_capture_keeps_a_chord_held_before_the_start() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_sequencer(0, bar * 2);
        add_and_select_clip(&mut seq, 0, bar * 2);
        hold_then_start(&mut seq, &[(60, 50_000), (64, 50_040)]);
        seq.ticks_into_take = 600;
        for note in [60, 64] {
            input_at(&mut seq, [0x80, note, 0], 600, 600 + bar * 2 * 4);
        }

        seq.insert_running_capture().unwrap();

        assert_eq!(
            ons_of(seq.selected_clip().unwrap()),
            vec![(0, 60), (40, 64)]
        );
    }

    #[test]
    fn is_note_on_treats_velocity_zero_as_a_note_off() {
        assert!(is_note_on(&[0x93, 60, 1]));
        assert!(!is_note_on(&[0x90, 60, 0]));
        assert!(!is_note_on(&[0x80, 60, 64]));
        assert!(!is_note_on(&[0xB0, 7, 100]));
        assert!(!is_note_on(&[0x90]));
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

    // --- Track meter ---

    #[test]
    fn a_live_note_on_kicks_the_selected_tracks_meter_with_its_velocity() {
        let mut seq = test_sequencer();
        let track1 = seq.track_id_by_index(1);
        seq.select_track(track1);
        let slot = seq.tracks()[1].slot();

        assert!(!seq.kick_live_meter(&[0x80, 60, 0]));
        // A velocity-0 note-on is a note-off.
        assert!(!seq.kick_live_meter(&[0x90, 60, 0]));
        assert!(!seq.kick_live_meter(&[0xE0, 0x00, 0x60]));
        assert_eq!(seq.track_mix.last_note_on(slot), None);

        assert!(seq.kick_live_meter(&[0x90, 60, 90]));
        assert_eq!(seq.track_mix.last_note_on(slot).map(|(_, v)| v), Some(90));
        let other = seq.tracks()[0].slot();
        assert_eq!(seq.track_mix.last_note_on(other), None);
    }

    #[test]
    fn during_an_audio_export_a_live_note_kicks_no_plugin_tracks_meter() {
        // The keyboard reaches no plugin during a run; MIDI out still plays.
        let mut seq = test_sequencer();
        instrument_track_0(&mut seq);
        seq.tracks_mut()[0].add_clip(&clip_at(0, 480));
        let plan = seq.audio_export_plan(Some(rect(0, 480, 0, 0))).unwrap();
        seq.begin_audio_export(plan, PathBuf::new());

        let track0 = seq.track_id_by_index(0);
        seq.select_track(track0);
        assert!(!seq.kick_live_meter(&[0x90, 60, 100]));

        let track1 = seq.track_id_by_index(1);
        seq.select_track(track1);
        assert!(seq.kick_live_meter(&[0x90, 60, 100]));
    }
}
