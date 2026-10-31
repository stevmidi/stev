//! One arrangement track: an ordered, non-overlapping list of [`Clip`]s and the
//! routing that says where its events go.
//!
//! Up to `MAX_TRACKS` of these live in the `Sequencer`. A `Track` owns playback
//! bookkeeping ([`current_clip_idx`](Track::current_clip_idx),
//! [`current_tick`](Track::current_tick)) and two staging queues:
//! [`pending_note_offs`](Track::pending_note_offs), which lets it silence a
//! clip cleanly when it is muted, removed, or simply reaches its own region
//! end mid-note (a non-destructive split/carve/resize can leave a note's
//! `NoteOff` outside a piece's visible region), and `pending_note_ons`, which
//! re-triggers a note that is already sounding when playback arrives at a
//! clip's start — the counterpart case, where the `NoteOn` is hidden just
//! *before* the region. Its [`ControllerTracker`] does the same for the
//! pitch bend and mod wheels, which have no release, and for its knobs' CCs:
//! every [`seek`](Track::seek) and clip-start arrival chases the value in
//! force where playback lands (a wheel's rest in a gap; a knob's CC stays
//! where it was), so a clip end, a loop wrap or a jump never leaves the synth
//! bent. Clips are kept sorted
//! by start tick and rejected on overlap. Pure data — the sequencer drives it
//! one [`tick`](Track::tick) at a time.
//!
//! A track has two indices. Its **position** (where it sits in
//! `Sequencer::tracks`, which lane it draws in) changes when a track above it
//! is added or removed — [`TrackShift`] maps the positional state across that.
//! Its **slot** ([`Track::slot`]) never changes for its lifetime: everything
//! the engine keeps per track (the shared mix atomics, the instrument mixer's
//! voices, `InstrumentNotes`, the tag on every plugin-bound event) is keyed by
//! slot, so adding or removing a track moves nothing on the audio thread. See
//! `050-undo-redo.md` § Track add / remove.

use std::path::PathBuf;

use uuid::Uuid;

use crate::core::config::TRACK_NAME_MAX_CHARS;
use crate::core::midi::message::{Controller, parse_controller};
use crate::models::{
    clip::{Clip, ClipBounds, EventSpaceRetime},
    controllers::{ControllerTracker, ControllerValues},
    event::Event,
    knobs::{KNOB_COUNT, Knob, Knobs, default_knobs},
};

/// Identifies a CLAP instrument plugin for a track: the `.clap` bundle on disk
/// plus the plugin id within it (a bundle can expose several). `display_name` is
/// cached so the UI can label the track without re-scanning the catalog.
///
/// `state` is the plugin's last-persisted CLAP `state` blob (the active preset /
/// knob positions). Empty means "none captured yet" — the plugin loads at its
/// own default. The macOS CLAP host refreshes it from the live editor on every
/// project save and re-applies it after re-instantiating the plugin on load.
/// See `130-plugin-host.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstrumentRef {
    /// Filesystem path to the `.clap` bundle.
    pub(crate) bundle_path: PathBuf,
    /// Plugin id within the bundle (a bundle can expose several).
    pub(crate) plugin_id: String,
    /// Cached human-readable name, so the UI needn't re-scan the catalog.
    pub(crate) display_name: String,
    /// Last-persisted CLAP `state` blob; empty = none captured yet.
    pub(crate) state: Vec<u8>,
}

/// How a track's events are sent to an output.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrackOutput {
    /// Send MIDI bytes to the external MIDI output on the given channel.
    MidiOut {
        /// MIDI channel, 0–15.
        channel: u8,
    },
    /// Route the track's events into a per-track hosted CLAP instrument (macOS).
    /// Clip events reach the audio callback via `instrument_midi_tx` tagged
    /// with the track's slot ([`Track::slot`]). See `130-plugin-host.md`.
    Instrument(InstrumentRef),
}

impl TrackOutput {
    /// The plugin, for an `Instrument` output.
    pub(crate) fn instrument(&self) -> Option<&InstrumentRef> {
        match self {
            TrackOutput::Instrument(instrument) => Some(instrument),
            TrackOutput::MidiOut { .. } => None,
        }
    }
}

/// A track inserted at, or removed from, a position — how every positional
/// track index moves across it (the view's clip shapes and marquee span).
/// Engine state is keyed by [`Track::slot`] and never needs this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackShift {
    /// A track was inserted at this position: it and everything after it
    /// moved down one.
    Inserted(usize),
    /// The track at this position was removed: everything after it moved up
    /// one.
    Removed(usize),
}

impl TrackShift {
    /// Where the track that was at `track_idx` is now — `None` for the
    /// removed track itself.
    pub(crate) fn apply(self, track_idx: usize) -> Option<usize> {
        match self {
            TrackShift::Inserted(at) if track_idx >= at => Some(track_idx + 1),
            TrackShift::Removed(at) if track_idx == at => None,
            TrackShift::Removed(at) if track_idx > at => Some(track_idx - 1),
            _ => Some(track_idx),
        }
    }

    /// An inclusive span `(start, end)` of positions after the shift, or
    /// `None` when it held only the removed track. An insert inside the span
    /// widens it by the new track.
    pub(crate) fn apply_span(self, (start, end): (usize, usize)) -> Option<(usize, usize)> {
        match self {
            TrackShift::Inserted(at) => {
                let start = if start >= at { start + 1 } else { start };
                Some((start, end + usize::from(end >= at)))
            }
            TrackShift::Removed(at) if start == at && end == at => None,
            TrackShift::Removed(at) => {
                let start = if start > at { start - 1 } else { start };
                Some((start, if end >= at { end - 1 } else { end }))
            }
        }
    }
}

/// What a rename's text entry names the track: trimmed, cut to
/// `TRACK_NAME_MAX_CHARS`, and `None` — back to the number — when nothing is
/// left.
pub(crate) fn track_name_from_input(input: &str) -> Option<String> {
    let name: String = input.trim().chars().take(TRACK_NAME_MAX_CHARS).collect();
    let name = name.trim_end();
    (!name.is_empty()).then(|| name.to_owned())
}

/// One arrangement track. See the module docs.
#[derive(Clone, Debug)]
pub(crate) struct Track {
    // --- Identity ---
    /// Stable id, assigned at construction.
    id: Uuid,
    /// The engine slot (`0..MAX_TRACKS`) this track's per-track state lives
    /// in for its whole life — see the module docs. Unique among the live
    /// tracks; assigned by the `Sequencer`.
    slot: usize,
    /// Which of the theme's track colours (`0..TRACK_COLOR_COUNT`) the
    /// track draws in. Picked when the track is created and kept, persisted
    /// (`TrackData::color`) — unlike [`slot`](Self::slot), which a load
    /// resets, and unlike the position, which a remove above it shifts. May
    /// repeat between tracks; nothing but drawing reads it.
    color_slot: usize,
    /// The user's name for the track (⌘R, a double-click on the header's
    /// name row), persisted (`TrackData::name`). `None` until named: the
    /// header then shows the track's number, its position. Never empty —
    /// [`track_name_from_input`] turns a blank entry into `None`.
    name: Option<String>,

    // --- Clip management ---
    /// The track's clips, sorted by start tick, never overlapping.
    clips: Vec<Clip>,
    /// Note-offs staged to be emitted ahead of anything else by
    /// [`tick`](Self::tick) — filled when a sounding clip is muted or removed
    /// mid-playback, or naturally reaches its own region end, so its open
    /// notes are released cleanly.
    pending_note_offs: Vec<Event>,
    /// Note-ons staged to be emitted by [`tick`](Self::tick), drained after
    /// `pending_note_offs` — filled when playback arrives at a clip's start
    /// and a note is already "in flight" there (its `NoteOn` sits just before
    /// the region but its `NoteOff` reaches into it), so the split-off tail
    /// half of a non-destructive edit sounds instead of staying silent.
    pending_note_ons: Vec<Event>,
    /// The arrangement tick at which [`tick`](Self::tick) last ran its
    /// clip-start chase into `pending_note_ons`, so the chase runs exactly
    /// once per arrival at a clip's start. `tick()` is called repeatedly at
    /// one position (once per event it emits) and `current_tick` only moves
    /// on once the clip has nothing more to emit there, so "we are at the
    /// clip's start" alone is true on every one of those calls. Cleared by
    /// [`seek`](Self::seek).
    chased_start_tick: Option<i32>,
    /// Which controllers the track's playback has moved, and the values due
    /// at the next [`tick`](Self::tick) — drained after
    /// `pending_note_offs`, before `pending_note_ons`, so a chased note
    /// sounds at the chased bend. Only the wheels and the knobs' CCs are
    /// chased ([`chase_controllers`](Self::chase_controllers)). See
    /// `models::controllers`.
    controllers: ControllerTracker,

    // --- Playback state ---
    /// Index into [`clips`](Self::clips) of the clip playback is currently in
    /// (or heading toward), or `None` past the last clip. Maintained by
    /// [`seek`](Self::seek).
    current_clip_idx: Option<usize>,
    /// While playback is in the gap *before* the current clip, the
    /// arrangement tick that clip's internal position was parked at — its
    /// start when [`seek`](Self::seek) landed in the gap. That parked walk is
    /// only right if playback reaches exactly that tick: if the clip's start
    /// moves meanwhile (a header-band edge trim or a move under a running
    /// transport), [`tick`](Self::tick) re-seeks instead of playing from the
    /// old start's events. `None` once playback is inside the clip, where the
    /// delta-driven walk is valid whatever happens to the region edges —
    /// unless the start is trimmed *past* the playhead, which `tick` detects
    /// as `None` with `current_tick < start_tick()`: the walk is then frozen
    /// mid-clip with its sounding notes stranded, so it releases them and
    /// re-seeks (re-parking at the new start).
    awaiting_start_tick: Option<i32>,
    /// The track's own playback position, in arrangement ticks.
    current_tick: i32,

    // --- Output routing ---
    /// Where this track's events go — see [`TrackOutput`].
    output: TrackOutput,
    /// The knob panel's eight knobs while this track is selected: the CC
    /// each sends and where it stands — put there by the hand, a load or
    /// playback (`390-knob-panel.md` decision 7) — persisted
    /// (`TrackData::knobs`). The sequencer mirrors the positions into
    /// `TrackMixAtomics` for the view. See `models::knobs`.
    knobs: Knobs,
    /// Where the hand or a load last put each knob — what the unsaved check
    /// compares (`TrackData::knobs`), so a knob that only playback moved
    /// never asks to be saved.
    knobs_by_hand: [Option<u8>; KNOB_COUNT],
    /// Per knob, whether a drag holds it: the hand wins, and playback sends
    /// none of the clip's moves of its CC until release (`390` decision 7).
    held_knobs: [bool; KNOB_COUNT],
    /// Whether playback moved a knob since the sequencer last asked
    /// ([`take_knobs_moved`](Self::take_knobs_moved)), to republish it.
    knobs_moved: bool,
}

impl Track {
    // --- Constructor ---
    /// A track with no clips, positioned at tick 0, routed to `output`, in
    /// engine slot `slot` and colour `color_slot`.
    pub(crate) fn new(output: TrackOutput, slot: usize, color_slot: usize) -> Self {
        Track {
            slot,
            color_slot,
            name: None,
            clips: Vec::new(),
            pending_note_offs: Vec::new(),
            pending_note_ons: Vec::new(),
            chased_start_tick: None,
            controllers: ControllerTracker::default(),
            id: Uuid::new_v4(),
            current_clip_idx: None,
            awaiting_start_tick: None,
            current_tick: 0,
            output,
            knobs: default_knobs(),
            knobs_by_hand: [None; KNOB_COUNT],
            held_knobs: [false; KNOB_COUNT],
            knobs_moved: false,
        }
    }

    // --- Identity ---
    /// The track's stable id.
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    /// The engine slot this track's per-track state lives in — see the
    /// module docs.
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    /// Moves the track to another engine slot — only for a track not in the
    /// arrangement (one being restored), never a live one.
    pub(crate) fn set_slot(&mut self, slot: usize) {
        self.slot = slot;
    }

    /// Which of the theme's track colours the track draws in — see the
    /// field.
    pub(crate) fn color_slot(&self) -> usize {
        self.color_slot
    }

    /// Recolours the track (a project load, a new project).
    pub(crate) fn set_color_slot(&mut self, color_slot: usize) {
        self.color_slot = color_slot;
    }

    /// The user's name for the track, `None` while unnamed — see the field.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Renames the track (`RenameTrackEdit`, a project load); `None` goes
    /// back to the number.
    pub(crate) fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    // --- Output routing ---
    /// Where this track's events go.
    pub(crate) fn output(&self) -> &TrackOutput {
        &self.output
    }

    /// Re-routes the track.
    pub(crate) fn set_output(&mut self, output: TrackOutput) {
        self.output = output;
    }

    /// Mutable access to the routing (e.g. to refresh a plugin `state` blob in
    /// place without rebuilding the [`InstrumentRef`]).
    pub(crate) fn output_mut(&mut self) -> &mut TrackOutput {
        &mut self.output
    }

    /// The track's knobs — see the field.
    pub(crate) fn knobs(&self) -> &Knobs {
        &self.knobs
    }

    /// Replaces all the knobs (a project load).
    pub(crate) fn set_knobs(&mut self, knobs: Knobs) {
        self.knobs = knobs;
        self.knobs_by_hand = knobs.map(|knob| knob.value);
    }

    /// Knob `knob`, if there is one.
    pub(crate) fn knob_mut(&mut self, knob: usize) -> Option<&mut Knob> {
        self.knobs.get_mut(knob)
    }

    /// Where the hand or a load last put each knob — see the field.
    pub(crate) fn knobs_by_hand(&self) -> &[Option<u8>; KNOB_COUNT] {
        &self.knobs_by_hand
    }

    /// The hand put knob `knob` at `value`. Returns whether there is such a
    /// knob.
    pub(crate) fn set_knob_by_hand(&mut self, knob: usize, value: u8) -> bool {
        let Some(slot) = self.knobs.get_mut(knob) else {
            return false;
        };
        slot.value = Some(value);
        self.knobs_by_hand[knob] = Some(value);
        true
    }

    /// A drag takes hold of knob `knob`: until [`release_knobs`](Self::release_knobs),
    /// playback sends none of the clip's moves of its CC.
    pub(crate) fn hold_knob(&mut self, knob: usize) {
        if let Some(held) = self.held_knobs.get_mut(knob) {
            *held = true;
        }
    }

    /// The drag let go: playback takes the knobs over again at their CCs'
    /// next moves.
    pub(crate) fn release_knobs(&mut self) {
        self.held_knobs = [false; KNOB_COUNT];
    }

    /// Whether playback moved a knob since the last call — the sequencer's
    /// cue to republish the positions.
    pub(crate) fn take_knobs_moved(&mut self) -> bool {
        std::mem::take(&mut self.knobs_moved)
    }

    /// Whether `controller` is a knob's CC that a drag holds — playback
    /// drops its moves.
    fn is_held_by_hand(&self, controller: Controller) -> bool {
        let Controller::Cc(cc) = controller else {
            return false;
        };
        self.knobs
            .iter()
            .zip(self.held_knobs)
            .any(|(knob, held)| held && knob.cc == cc)
    }

    /// Playback sent an unmuted move of `controller` to `value`: it is now
    /// where the synth sits, and a knob's CC moves the knob with it.
    fn played_controller(&mut self, controller: Controller, value: u16) {
        self.controllers.observe(controller, value);
        let Controller::Cc(cc) = controller else {
            return;
        };
        let value = Some(value as u8);
        for knob in self.knobs.iter_mut().filter(|knob| knob.cc == cc) {
            if knob.value != value {
                knob.value = value;
                self.knobs_moved = true;
            }
        }
    }

    /// Chases `target` — the values in force where playback lands — on the
    /// controllers this track follows: the two wheels and its knobs' CCs.
    /// Another CC in a clip (pasted, imported) plays as it comes and is never
    /// chased: a sustain pedal chased down by a seek would hold every note
    /// past the stop that releases it.
    fn chase_controllers(&mut self, target: &ControllerValues) {
        let mut followed = [Controller::PitchBend; Controller::WHEELS.len() + KNOB_COUNT];
        followed[..Controller::WHEELS.len()].copy_from_slice(&Controller::WHEELS);
        for (slot, knob) in followed[Controller::WHEELS.len()..]
            .iter_mut()
            .zip(self.knobs)
        {
            *slot = Controller::Cc(knob.cc);
        }
        self.controllers.chase(target, &followed);
    }

    /// Returns the MIDI channel for this track. Instrument tracks play into the
    /// plugin on channel 0.
    pub(crate) fn midi_channel(&self) -> u8 {
        match &self.output {
            TrackOutput::MidiOut { channel } => *channel,
            TrackOutput::Instrument(_) => 0,
        }
    }

    // --- Clip management ---
    /// Whether `clip` could be added without overlapping an existing clip —
    /// the check [`add_clip`](Self::add_clip) gates on, exposed so an edit can
    /// know up front that its add will succeed (a no-op must not enter the
    /// undo record).
    pub(crate) fn fits(&self, clip: &Clip) -> bool {
        !self
            .clips
            .iter()
            .any(|c| clip.start_tick() < c.end_tick() && clip.end_tick() > c.start_tick())
    }

    /// Inserts a clone of `clip` at its sorted position. Returns `false`
    /// (nothing inserted) if it would overlap an existing clip.
    pub(crate) fn add_clip(&mut self, clip: &Clip) -> bool {
        if !self.fits(clip) {
            return false;
        }
        let pos = self
            .clips
            .partition_point(|c| c.start_tick() <= clip.start_tick());
        self.clips.insert(pos, clip.clone());
        // Keep pointing at the same clip, now one slot further on.
        if let Some(current) = self.current_clip_idx.as_mut()
            && pos <= *current
        {
            *current += 1;
        }
        true
    }

    /// Removes and returns the clip with this id, or `None` if absent. Removing
    /// the current clip leaves no current clip — the next [`tick`](Self::tick)
    /// re-seeks — rather than an index that silently names its neighbour
    /// (which a following `release_sounding_notes_for_clip` for that
    /// neighbour would mistake for the playing clip, overwriting the note-offs
    /// already queued).
    pub(crate) fn remove_clip_by_id(&mut self, clip_id: Uuid) -> Option<Clip> {
        let idx = self.clips.iter().position(|c| c.id() == clip_id)?;
        self.current_clip_idx = match self.current_clip_idx {
            Some(current) if current == idx => None,
            Some(current) if current > idx => Some(current - 1),
            current => current,
        };
        Some(self.clips.remove(idx))
    }

    /// Drops every clip and resets playback state to the start.
    pub(crate) fn clear_clips(&mut self) {
        self.clips.clear();
        self.current_clip_idx = None;
        self.current_tick = 0;
    }

    /// Id of the clip whose half-open span `[start, end)` contains `tick`, or
    /// `None` in a gap.
    pub(crate) fn find_clip_id_at(&self, tick: i32) -> Option<Uuid> {
        self.clips
            .iter()
            .find(|c| tick >= c.start_tick() && tick < c.end_tick())
            .map(|c| c.id())
    }

    /// Ids of every clip overlapping `[region_start, region_end)`.
    pub(crate) fn find_clip_ids_in(&self, region_start: i32, region_end: i32) -> Vec<Uuid> {
        self.clips
            .iter()
            .filter(|c| c.start_tick() < region_end && c.end_tick() > region_start)
            .map(|c| c.id())
            .collect()
    }

    /// Ids of every clip lying fully inside `[start, end)`.
    pub(crate) fn find_clip_ids_within(&self, start: i32, end: i32) -> Vec<Uuid> {
        self.clips
            .iter()
            .filter(|c| c.start_tick() >= start && c.end_tick() <= end)
            .map(|c| c.id())
            .collect()
    }

    /// Start tick of the nearest clip that begins strictly after `tick`.
    pub(crate) fn next_clip_start_after(&self, tick: i32) -> Option<i32> {
        self.clips
            .iter()
            .filter(|c| c.start_tick() > tick)
            .map(|c| c.start_tick())
            .min()
    }

    /// End tick of the nearest clip that ends at or before `tick`.
    pub(crate) fn prev_clip_end_before(&self, tick: i32) -> Option<i32> {
        self.clips
            .iter()
            .filter(|c| c.end_tick() <= tick)
            .map(|c| c.end_tick())
            .max()
    }

    /// The track's clips, sorted by start tick.
    pub(crate) fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// The clip with this id, if present.
    pub(crate) fn get_clip_by_id(&self, clip_id: Uuid) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id() == clip_id)
    }

    /// Mutable access to the clip with this id, if present.
    pub(crate) fn get_clip_by_id_mut(&mut self, clip_id: Uuid) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id() == clip_id)
    }

    // --- Playback control ---
    /// Advances the track by one tick, returning at most one event to emit.
    /// Drains staged [`pending_note_offs`](Self::pending_note_offs) then
    /// `pending_note_ons` first, then re-seeks if the current clip went away
    /// or another unmuted clip now covers the position, then plays the
    /// current clip. `None` when nothing sounds this tick.
    pub(crate) fn tick(&mut self) -> Option<Event> {
        if let Some(event) = self.drain_pending() {
            return Some(event);
        }

        if self.clips.is_empty() {
            return None;
        }

        let needs_reseek = match self.current_clip_idx {
            Some(idx) => self.clips.get(idx).is_none_or(|clip| clip.is_muted()),
            None => true,
        } || self.clips.iter().enumerate().any(|(idx, c)| {
            !c.is_muted()
                && self.current_tick >= c.start_tick()
                && self.current_tick < c.end_tick()
                && self.current_clip_idx != Some(idx)
        }) || self
            // The current clip's start moved under us (see
            // `awaiting_start_tick`): while waiting, its parked walk points at
            // the old start; while inside, a start now *ahead* of the playhead
            // has frozen the walk mid-clip and stranded its sounding notes.
            .current_clip_idx
            .and_then(|idx| self.clips.get(idx))
            .is_some_and(|clip| match self.awaiting_start_tick {
                Some(parked_at) => clip.start_tick() != parked_at,
                None => self.current_tick < clip.start_tick(),
            });

        if needs_reseek {
            // Collect pending note-offs from the clip we're leaving before
            // seeking away from it — whether it was just muted or removed,
            // or (the case the second `needs_reseek` term above exists for)
            // it simply reached its own region end exactly where another
            // clip starts, so no clips ever overlap non-overlapping clips
            // guarantee the current one no longer covers `current_tick` once
            // this fires. A directly-adjacent next clip (e.g. the two
            // halves of a split) is exactly this case, and its `NoteOff`
            // otherwise lies outside this piece's event window (a
            // non-destructive edit shares the full event list — see
            // `split.rs`) so `clip.tick()` would never reach it.
            self.release_sounding_notes();

            // The newly-selected clip may start exactly here (the adjacent
            // case above) — the "Play clip" branch below then chases any
            // note already "in flight" there on this same call. A genuine
            // gap instead leaves `current_tick` behind the new clip's own
            // start, so that happens once playback naturally arrives there.
            self.seek(self.current_tick);
        }

        if let Some(clip_idx) = self.current_clip_idx
            && let Some(clip) = self.clips.get(clip_idx)
        {
            if self.current_tick < clip.start_tick() {
                // Wait for clip to start
            } else if self.current_tick < clip.end_tick() {
                // Inside the clip: the walk advances on its own from here, so
                // an edge trim no longer needs a re-seek.
                self.awaiting_start_tick = None;
                if self.current_tick == clip.start_tick()
                    && self.chased_start_tick != Some(self.current_tick)
                {
                    // Just arrived at this clip's own start: a note whose
                    // `NoteOn` sits just before the region but whose
                    // `NoteOff` reaches into it is otherwise silently
                    // dropped — its `NoteOn` event lies outside this piece's
                    // event window (a non-destructive split/carve/resize
                    // shares the full event list; see `split.rs`), so
                    // `clip.tick()` never plays it. Chase it back in — this
                    // is the one site that does, whether playback arrived
                    // here naturally or was seeked onto the start
                    // (`Sequencer::chase_notes` leaves a clip's exact start
                    // to us for that reason). Only ever finds something for
                    // such a piece — an ordinarily-recorded clip has no note
                    // starting before its own region. Exactly once per
                    // arrival: `clip.tick()` returning `Some` below brings
                    // us back here with `current_tick` unchanged.
                    self.pending_note_ons = clip.chased_note_ons_at(self.current_tick);
                    // A wheel held from that hidden material likewise.
                    let held = clip.controllers_at(self.current_tick);
                    self.chase_controllers(&held);
                    self.chased_start_tick = Some(self.current_tick);
                }
                // Anything staged on this very call — the release of the
                // clip we just left, then the chased note-ons — goes out
                // before this clip's own events at the same tick, so a
                // boundary always sounds as release → chase → new content.
                if let Some(event) = self.drain_pending() {
                    return Some(event);
                }
                // Play clip (MIDI events). A move of a knob's CC that a drag
                // holds is dropped: the hand wins (`390` decision 7).
                while let Some(event) = self.clips.get_mut(clip_idx).and_then(Clip::tick) {
                    if !event.is_muted()
                        && let Some((controller, value)) = parse_controller(event.midi_message())
                    {
                        if self.is_held_by_hand(controller) {
                            continue;
                        }
                        self.played_controller(controller, value);
                    }
                    return Some(event);
                }
            } else {
                // Reached the clip's own region end with no other clip
                // taking over the position (a genuine gap ahead, or nothing
                // left — a directly-adjacent next clip is instead caught by
                // the `needs_reseek` check above, since two non-overlapping
                // clips can never both cover the same tick). A note still
                // open here would otherwise hang forever, since
                // `clip.tick()` is never called again for this clip once
                // we've moved on — its `NoteOff` lies outside this piece's
                // event window for the same non-destructive-edit reason as
                // above. Queue it before seeking away.
                self.release_sounding_notes();
                // Move to next clip
                self.seek(self.current_tick);
            }
        }

        self.current_tick += 1;
        None
    }

    /// Pops one staged event: note-offs queued from a clip that was muted,
    /// removed, or reached its own region end mid-playback go first, then
    /// the due controller values (a seek's chase; not a knob's a drag holds),
    /// then the chased note-ons queued when playback arrived at a clip whose
    /// content has a note already "in flight" at its start. `None` once
    /// nothing is staged.
    fn drain_pending(&mut self) -> Option<Event> {
        if let Some(off) = self.pending_note_offs.pop() {
            return Some(off);
        }
        while let Some((controller, value)) = self.controllers.next_due() {
            if self.is_held_by_hand(controller) {
                continue;
            }
            self.played_controller(controller, value);
            return Some(Event::from_midi(&controller.message(value)));
        }
        self.pending_note_ons.pop()
    }

    /// Queues note-offs for every note the current clip has already started but
    /// not yet ended, so the next `tick()` calls emit them as ordinary events.
    /// Used when a track is muted / un-soloed mid-playback to silence it at
    /// once without leaving hanging notes — the same `pending_note_offs`
    /// mechanism clip-mute uses. A no-op with no current clip.
    pub(crate) fn release_sounding_notes(&mut self) {
        if let Some(idx) = self.current_clip_idx
            && let Some(clip) = self.clips.get(idx)
        {
            let offs = clip.pending_note_offs();
            self.queue_note_offs(offs);
        }
    }

    /// Note-offs for everything the track has sounding or already queued to
    /// release, taken out of [`pending_note_offs`](Self::pending_note_offs)
    /// rather than left for [`tick`](Self::tick) — for a track leaving the
    /// arrangement, whose `tick` never runs again to flush them. A queued
    /// note-on is dropped with them.
    pub(crate) fn take_sounding_note_offs(&mut self) -> Vec<Event> {
        self.release_sounding_notes();
        self.pending_note_ons.clear();
        std::mem::take(&mut self.pending_note_offs)
    }

    /// Messages (channel 0) putting every wheel the track's playback moved
    /// back to its rest, for the sequencer to send at once: a transport stop
    /// (no [`tick`](Self::tick) runs to drain a chase) or a track leaving the
    /// arrangement. A knob's CC has no rest and stays where it is. Nothing
    /// stays due.
    pub(crate) fn take_wheel_resets(&mut self) -> Vec<Vec<u8>> {
        self.controllers.take_resets()
    }

    /// Playback is about to jump (a seek, a loop wrap — `reanchor_playback`):
    /// the synth may not have heard the last controller values sent, so the
    /// next [`seek`](Self::seek) resends them — see
    /// [`ControllerTracker::invalidate`].
    pub(crate) fn invalidate_controllers(&mut self) {
        self.controllers.invalidate();
    }

    /// The player moved a controller live on this track (`msg`): a stop or
    /// seek leaves it where the player put it — see
    /// [`ControllerTracker::hand_to_player`].
    pub(crate) fn hand_controller_to_player(&mut self, msg: &[u8]) {
        self.controllers.hand_to_player(msg);
    }

    /// Adds `offs` to [`pending_note_offs`](Self::pending_note_offs) behind
    /// what is already queued — never over it, or a note-off queued earlier
    /// in the same batch of edits (an event edit, then a track mute) is lost
    /// and its note hangs. An off whose note (channel + number) is already
    /// queued is skipped, so releasing the same clip twice before the next
    /// [`tick`](Self::tick) (a carve's two splits) doesn't send it twice.
    fn queue_note_offs(&mut self, offs: impl IntoIterator<Item = Event>) {
        let key = |off: &Event| (off.midi_channel(), off.note_number());
        let queued: Vec<_> = self.pending_note_offs.iter().map(key).collect();
        self.pending_note_offs
            .extend(offs.into_iter().filter(|off| !queued.contains(&key(off))));
    }

    /// Like [`release_sounding_notes`](Self::release_sounding_notes), but scoped
    /// to one clip: queues the pending note-offs only when `clip_id` is the clip
    /// currently playing. Called just before that clip is removed mid-playback so
    /// its open notes get real note-offs — routed through [`tick`](Self::tick)
    /// like any event, so both the MIDI-out `NoteLogger` and the CLAP
    /// `instrument_notes` safety nets clear — instead of hanging. A no-op if the
    /// clip is not the one sounding (only the current clip can have open notes).
    pub(crate) fn release_sounding_notes_for_clip(&mut self, clip_id: Uuid) {
        if self
            .current_clip_idx
            .and_then(|idx| self.clips.get(idx))
            .is_some_and(|clip| clip.id() == clip_id)
        {
            self.release_sounding_notes();
        }
    }

    /// Runs `edit` on the events of `clip_id` (`None` if the track has no
    /// such clip), first noting which of its notes sound at the playhead. A
    /// note the edit stops sounding there — moved away in time or pitch,
    /// deleted, or ended behind the playhead — gets its `NoteOff` queued
    /// into [`pending_note_offs`](Self::pending_note_offs): its old `NoteOff`
    /// is gone from where the walk would meet it, so it would otherwise hang,
    /// or sound a second time if its new start is still ahead. A note still
    /// sounding afterwards (a stretch) is left alone, so it holds to its new
    /// end. Only the clip playback is inside has anything sounding; any other
    /// clip is simply edited.
    pub(crate) fn edit_clip_events<R>(
        &mut self,
        clip_id: Uuid,
        edit: impl FnOnce(&mut Clip) -> R,
    ) -> Option<R> {
        self.edit_clip_in_place(clip_id, false, |clip| (edit(clip), None))
            .map(|(result, _)| result)
    }

    /// Runs `edit` — a retime of the clip's event ticks that keeps its
    /// wall-clock timing, the tempo changing with them — on `clip_id`,
    /// keeping playback in phase: `edit` returns its result and how it moved
    /// the ticks, and a playhead inside the clip moves with them, so the
    /// notes sounding go on sounding and the pass plays on as heard instead
    /// of jumping to wherever the old tick falls in the retimed notes (`220`
    /// § Playback during edits). Returns what `edit` returned and how far the
    /// playhead moved, in ticks, for the sequencer to move `playback_tick`
    /// with it — `0` when playback isn't inside the clip. `None` if the track
    /// has no such clip.
    pub(crate) fn retime_clip_events<R>(
        &mut self,
        clip_id: Uuid,
        edit: impl FnOnce(&mut Clip) -> (R, EventSpaceRetime),
    ) -> Option<((R, EventSpaceRetime), i32)> {
        self.edit_clip_in_place(clip_id, false, |clip| {
            let edited = edit(clip);
            let retime = edited.1;
            (edited, Some(retime))
        })
    }

    /// Moves `clip_id` to `bounds` — an edge edit (`ResizeClipEdit`); `None`
    /// if the track has no such clip. Like
    /// [`edit_clip_events`](Self::edit_clip_events), and a note sounding at
    /// the playhead's new point in the clip that wasn't before is also
    /// chased in, with the wheels: a clip-view start edit maps the same
    /// arrangement tick to another point of the take, and the new window is
    /// heard from the playhead at once. Walking on from the old point until
    /// the next wrap played the rest of the pass out of phase, cut short by
    /// the end that moved in (decided with the user 2026-10-12; `220`
    /// § Playback during edits). The playhead itself never moves.
    pub(crate) fn set_clip_bounds(&mut self, clip_id: Uuid, bounds: ClipBounds) -> Option<()> {
        self.edit_clip_in_place(clip_id, true, |clip| (clip.set_bounds(bounds), None))
            .map(|_| ())
    }

    /// Runs `edit` on `clip_id` and, while playback is inside that clip,
    /// re-seeks it in place: notes the edit stops sounding at the playhead
    /// are released. An `edge_edit` (the clip's bounds moved) differs in two
    /// ways: one that leaves the playhead outside the clip is left alone, to
    /// `tick`'s own release and re-seek, which meet the walk as it was; and
    /// the notes and wheel values in force at the new point are chased in —
    /// not when the playhead sits exactly on the clip's start, which is the
    /// arrival chase's job in [`tick`](Self::tick). When `edit` returns a
    /// retime of the clip's event ticks, the playhead first moves with them
    /// ([`retime_clip_events`](Self::retime_clip_events)). Returns the edit's
    /// result and how far the playhead moved.
    fn edit_clip_in_place<R>(
        &mut self,
        clip_id: Uuid,
        edge_edit: bool,
        edit: impl FnOnce(&mut Clip) -> (R, Option<EventSpaceRetime>),
    ) -> Option<(R, i32)> {
        let clip_idx = self.clips.iter().position(|clip| clip.id() == clip_id)?;
        let playing = self.current_clip_idx == Some(clip_idx) && self.awaiting_start_tick.is_none();
        let clip = &mut self.clips[clip_idx];
        if !playing {
            return Some((edit(clip).0, 0));
        }

        // Only a playhead inside the window moves: the conversion wraps, and
        // would fold one an end left behind back into the clip.
        let event_tick = (clip.start_tick()..clip.end_tick())
            .contains(&self.current_tick)
            .then(|| clip.event_tick_from_arrangement_tick(self.current_tick));
        let sounding = clip.pending_note_offs();
        let (result, retime) = edit(clip);
        let tick = match (retime, event_tick) {
            (Some(retime), Some(event_tick)) => {
                clip.arrangement_tick_from_event_tick(retime.map_tick(event_tick))
            }
            _ => self.current_tick,
        };
        let moved = tick - self.current_tick;
        self.current_tick = tick;
        if edge_edit && (tick < clip.start_tick() || tick >= clip.end_tick()) {
            return Some((result, moved));
        }
        clip.seek(tick);
        let key = |event: &Event| (event.midi_channel(), event.note_number());
        let still_sounding = clip.pending_note_offs();
        let chased = (edge_edit && tick > clip.start_tick()).then(|| {
            let ons: Vec<Event> = clip
                .chased_note_ons_at(tick)
                .into_iter()
                .filter(|on| !sounding.iter().any(|off| key(off) == key(on)))
                .collect();
            (ons, clip.controllers_at(tick))
        });
        self.queue_note_offs(
            sounding
                .into_iter()
                .filter(|off| !still_sounding.iter().any(|still| key(still) == key(off))),
        );
        if let Some((ons, controllers)) = chased {
            self.pending_note_ons.extend(ons);
            self.chase_controllers(&controllers);
        }
        Some((result, moved))
    }

    /// Repositions the track to `playback_tick`: picks the clip containing it,
    /// else the next clip after it, else `None` past the end, and seeks that
    /// clip internally. Muted clips are skipped. The controllers chase the
    /// values in force there — the containing clip's; in a gap the wheels'
    /// rest, and a knob's CC where it is — for the next [`tick`](Self::tick)
    /// to send ([`ControllerTracker::chase`]).
    pub(crate) fn seek(&mut self, playback_tick: i32) {
        // A new arrival: whichever clip start we land on gets chased afresh.
        self.chased_start_tick = None;
        self.awaiting_start_tick = None;

        let mut controllers = ControllerValues::default();

        // 1) Prefer the clip that actually contains the tick (half-open range)
        if let Some(clip_idx) = self.clips.iter().position(|c| {
            !c.is_muted() && playback_tick >= c.start_tick() && playback_tick < c.end_tick()
        }) {
            // Found clip at or before playback position
            self.current_clip_idx = Some(clip_idx);
            if let Some(clip) = self.clips.get_mut(clip_idx) {
                clip.seek(playback_tick);
                controllers = clip.controllers_at(playback_tick);
            }
        // 2) Otherwise, select the next clip after the tick (we're in a gap or before all clips)
        } else if let Some(next_idx) = self
            .clips
            .iter()
            .position(|c| !c.is_muted() && playback_tick < c.start_tick())
        {
            self.current_clip_idx = Some(next_idx);
            if let Some(next_clip) = self.clips.get_mut(next_idx) {
                next_clip.seek(next_clip.start_tick());
                self.awaiting_start_tick = Some(next_clip.start_tick());
            }
        // 3) Past the end
        } else {
            self.current_clip_idx = None;
        }

        self.chase_controllers(&controllers);
        self.current_tick = playback_tick;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{clip::Clip, event::EventType};

    #[test]
    fn a_rename_entry_is_trimmed_and_a_blank_one_goes_back_to_the_number() {
        assert_eq!(track_name_from_input("  Bass  "), Some("Bass".to_owned()));
        assert_eq!(
            track_name_from_input("Lead synth"),
            Some("Lead synth".to_owned())
        );
        assert_eq!(track_name_from_input(""), None);
        assert_eq!(track_name_from_input("   "), None);
    }

    #[test]
    fn a_rename_entry_is_cut_to_the_longest_name() {
        let long = "x".repeat(TRACK_NAME_MAX_CHARS + 10);
        let name = track_name_from_input(&long).unwrap();
        assert_eq!(name.chars().count(), TRACK_NAME_MAX_CHARS);
        // Counted in characters, not bytes, and no space left dangling.
        let cut = format!("{} é", "ø".repeat(TRACK_NAME_MAX_CHARS - 1));
        assert_eq!(
            track_name_from_input(&cut),
            Some("ø".repeat(TRACK_NAME_MAX_CHARS - 1))
        );
    }

    #[test]
    fn track_shift_insert_moves_the_insert_point_and_everything_after_it() {
        let shift = TrackShift::Inserted(2);
        assert_eq!(shift.apply(0), Some(0));
        assert_eq!(shift.apply(1), Some(1));
        assert_eq!(shift.apply(2), Some(3));
        assert_eq!(shift.apply(5), Some(6));
    }

    #[test]
    fn track_shift_remove_drops_the_track_and_pulls_later_ones_up() {
        let shift = TrackShift::Removed(2);
        assert_eq!(shift.apply(1), Some(1));
        assert_eq!(shift.apply(2), None);
        assert_eq!(shift.apply(3), Some(2));
    }

    #[test]
    fn track_shift_spans_follow_their_tracks() {
        // An insert inside a span widens it; before it, moves it; after it,
        // leaves it.
        assert_eq!(TrackShift::Inserted(2).apply_span((1, 3)), Some((1, 4)));
        assert_eq!(TrackShift::Inserted(1).apply_span((1, 3)), Some((2, 4)));
        assert_eq!(TrackShift::Inserted(4).apply_span((1, 3)), Some((1, 3)));
        // A remove inside a span narrows it, before it moves it, and a span
        // of only the removed track goes away.
        assert_eq!(TrackShift::Removed(2).apply_span((1, 3)), Some((1, 2)));
        assert_eq!(TrackShift::Removed(1).apply_span((1, 3)), Some((1, 2)));
        assert_eq!(TrackShift::Removed(0).apply_span((1, 3)), Some((0, 2)));
        assert_eq!(TrackShift::Removed(4).apply_span((1, 3)), Some((1, 3)));
        assert_eq!(TrackShift::Removed(2).apply_span((2, 2)), None);
    }

    fn clip_at(start_tick: i32, length: i32) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut().set_region(Some(0), Some(length));
        clip
    }

    #[test]
    fn midi_channel_is_zero_for_instrument_tracks() {
        let track = Track::new(
            TrackOutput::Instrument(InstrumentRef {
                bundle_path: "/x.clap".into(),
                plugin_id: "com.example.synth".into(),
                display_name: "Synth".into(),
                state: Vec::new(),
            }),
            0,
            0,
        );
        assert_eq!(track.midi_channel(), 0);
    }

    #[test]
    fn add_clip_maintains_sorted_order_by_start_tick() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_at(3000, 960));
        track.add_clip(&clip_at(0, 960));
        track.add_clip(&clip_at(1000, 960));

        let starts: Vec<i32> = track.clips().iter().map(|c| c.start_tick()).collect();
        assert_eq!(starts, vec![0, 1000, 3000]);
    }

    #[test]
    fn find_clip_ids_within_keeps_only_fully_inside_clips() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let straddling = clip_at(0, 960); // [0, 960) straddles 480
        let inside = clip_at(1000, 500); // [1000, 1500)
        let flush = clip_at(1500, 500); // [1500, 2000) ends exactly at `end`
        let beyond = clip_at(2000, 100); // [2000, 2100) starts at `end`
        for clip in [&straddling, &inside, &flush, &beyond] {
            track.add_clip(clip);
        }

        assert_eq!(
            track.find_clip_ids_within(480, 2000),
            vec![inside.id(), flush.id()]
        );
    }

    #[test]
    fn find_clip_id_at_start_tick_is_included() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let clip = clip_at(100, 900); // spans [100, 1000)
        let id = clip.id();
        track.add_clip(&clip);
        assert_eq!(track.find_clip_id_at(100), Some(id));
    }

    #[test]
    fn find_clip_id_at_end_tick_is_excluded() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_at(100, 900)); // spans [100, 1000)
        assert_eq!(track.find_clip_id_at(1000), None);
    }

    #[test]
    fn find_clip_id_at_last_tick_inside_is_included() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let clip = clip_at(100, 900); // spans [100, 1000)
        let id = clip.id();
        track.add_clip(&clip);
        assert_eq!(track.find_clip_id_at(999), Some(id));
    }

    #[test]
    fn find_clip_id_at_before_clip_returns_none() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_at(100, 900));
        assert_eq!(track.find_clip_id_at(50), None);
    }

    #[test]
    fn tick_returns_none_when_track_has_no_clips() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        assert!(track.tick().is_none());
    }

    #[test]
    fn tick_returns_none_when_all_clips_are_muted() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        clip.set_muted(true);
        track.add_clip(&clip);
        track.seek(0);
        assert!(track.tick().is_none());
    }

    #[test]
    fn release_sounding_notes_queues_the_current_clip_pending_note_offs() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        track.add_clip(&clip);
        track.seek(0);

        // First tick emits the note-on; the note-off is still in the future.
        assert_eq!(
            track.tick().and_then(|e| e.event_type()),
            Some(EventType::NoteOn)
        );

        track.release_sounding_notes();

        // The queued note-off comes out ahead of anything else.
        let off = track.tick().expect("a queued note-off");
        assert_eq!(off.event_type(), Some(EventType::NoteOff));
        assert_eq!(off.note_number(), Some(60));
    }

    /// A clip `[start, start + 960)` holding note 60 over `[0, 480)`.
    fn clip_with_note_60(start_tick: i32) -> Clip {
        let mut clip = clip_at(start_tick, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip
    }

    /// Regression: removing the playing clip used to leave its index naming
    /// the next clip, so removing that one too (a `Delete` over both) queued
    /// *its* (empty) note-offs over the playing clip's — the note hung.
    #[test]
    fn removing_the_playing_clip_then_its_neighbour_keeps_the_queued_note_off() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let playing = clip_with_note_60(0);
        let next = clip_at(960, 960);
        track.add_clip(&playing);
        track.add_clip(&next);
        track.seek(0);
        assert_eq!(
            track.tick().and_then(|e| e.event_type()),
            Some(EventType::NoteOn)
        );

        for id in [playing.id(), next.id()] {
            track.release_sounding_notes_for_clip(id);
            track.remove_clip_by_id(id);
        }

        let off = track.tick().expect("the playing clip's note-off");
        assert_eq!(off.event_type(), Some(EventType::NoteOff));
    }

    /// Inserting a clip ahead of the playing one keeps the index on the
    /// playing clip, so its sounding notes are still the ones released.
    #[test]
    fn inserting_a_clip_before_the_playing_one_keeps_it_current() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let playing = clip_with_note_60(960);
        track.add_clip(&playing);
        track.seek(960);
        assert_eq!(
            track.tick().and_then(|e| e.event_type()),
            Some(EventType::NoteOn)
        );

        track.add_clip(&clip_at(0, 960));
        track.release_sounding_notes_for_clip(playing.id());

        let off = track.tick().expect("the playing clip's note-off");
        assert_eq!(off.event_type(), Some(EventType::NoteOff));
    }

    /// Regression: `release_sounding_notes` used to *replace* the queue, so
    /// an event edit's queued note-off (note 60, deleted while sounding)
    /// was lost to a track mute before the next tick — the note hung.
    #[test]
    fn a_release_after_an_event_edit_keeps_the_edits_queued_note_off() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(0, 0, vec![0x90, 62, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        clip.add_event(Event::new(480, 0, vec![0x80, 62, 0]));
        let clip_id = clip.id();
        track.add_clip(&clip);
        track.seek(0);
        for _ in 0..2 {
            assert_eq!(
                track.tick().and_then(|e| e.event_type()),
                Some(EventType::NoteOn)
            );
        }

        // Delete note 60 while it sounds, then mute the track.
        track.edit_clip_events(clip_id, |clip| {
            let kept = clip
                .events()
                .iter()
                .filter(|e| e.note_number() != Some(60))
                .cloned()
                .collect();
            clip.restore_events(kept);
        });
        track.release_sounding_notes();

        let mut released: Vec<u8> = (0..2)
            .map(|_| track.tick().expect("a queued note-off"))
            .inspect(|off| assert_eq!(off.event_type(), Some(EventType::NoteOff)))
            .map(|off| off.note_number().unwrap())
            .collect();
        released.sort();
        assert_eq!(released, vec![60, 62]);
    }

    /// Releasing the same clip twice before a tick queues its note-off once.
    #[test]
    fn releasing_twice_before_a_tick_queues_each_note_off_once() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_note_60(0));
        track.seek(0);
        track.tick();

        track.release_sounding_notes();
        track.release_sounding_notes();

        assert!(
            track
                .tick()
                .is_some_and(|e| e.event_type() == Some(EventType::NoteOff))
        );
        assert!(
            track
                .tick()
                .is_none_or(|e| e.event_type() != Some(EventType::NoteOff))
        );
    }

    #[test]
    fn release_sounding_notes_is_a_no_op_with_no_current_clip() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.release_sounding_notes();
        assert!(track.tick().is_none());
    }

    // --- a waiting clip whose start moves under a running transport ---

    /// Runs `track` up to (not including) `until`, collecting every emitted
    /// `NoteOn` as `(arrangement tick, note)`.
    fn note_ons_until(track: &mut Track, until: i32) -> Vec<(i32, u8)> {
        let mut out = Vec::new();
        while track.current_tick < until {
            let at = track.current_tick;
            if let Some(event) = track.tick()
                && event.event_type() == Some(EventType::NoteOn)
            {
                out.push((at, event.note_number().unwrap()));
            }
        }
        out
    }

    /// A clip at bar 2 with a retained bar-1 pre-roll (event space ==
    /// arrangement space, region `[start, start + len)`), as a running-capture
    /// commit with the cursor a bar in leaves it.
    fn clip_with_pre_roll(start_tick: i32, length: i32) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut()
            .set_region(Some(start_tick), Some(start_tick + length));
        clip.add_event(Event::new(100, 0, vec![0x90, 60, 100])); // pre-roll
        clip.add_event(Event::new(200, 0, vec![0x80, 60, 0]));
        clip.add_event(Event::new(start_tick + 300, 0, vec![0x90, 62, 100]));
        clip.add_event(Event::new(start_tick + 400, 0, vec![0x80, 62, 0]));
        clip
    }

    /// Playback is in the gap before the clip when its left edge is dragged
    /// back behind the playhead (the running-capture "reveal the pre-roll"
    /// gesture). The clip was parked at its old start; without a re-seek it
    /// would play the old start's events from wherever the playhead happens
    /// to be — early by the drag distance — until the next wrap re-seeked it.
    #[test]
    fn tick_reseeks_a_waiting_clip_whose_start_moved_behind_the_playhead() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_pre_roll(1000, 1000));
        track.seek(0);
        assert!(note_ons_until(&mut track, 50).is_empty());

        // Left edge dragged from 1000 to 0: start and region start together.
        let clip = &mut track.clips[0];
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), None);

        assert_eq!(
            note_ons_until(&mut track, 2000),
            vec![(100, 60), (1300, 62)],
            "events must sound at their arrangement ticks, not shifted by the drag"
        );
    }

    /// The other direction: trimming the left edge *forward* while waiting.
    /// The parked walk pointed at the old start's events; arriving at the new
    /// start must play from there, not from the old start.
    #[test]
    fn tick_reseeks_a_waiting_clip_whose_start_moved_ahead() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_pre_roll(1000, 1000));
        track.seek(0);
        assert!(note_ons_until(&mut track, 50).is_empty());

        // Left edge dragged from 1000 to 1450: the note at 1300–1400 is
        // trimmed off entirely (not in flight at the new start, so the
        // arrival chase leaves it alone); the rest keeps its arrangement
        // position.
        let clip = &mut track.clips[0];
        clip.add_event(Event::new(1600, 0, vec![0x90, 64, 100]));
        clip.add_event(Event::new(1700, 0, vec![0x80, 64, 0]));
        clip.set_start_tick(1450);
        clip.region_mut().set_region(Some(1450), None);

        assert_eq!(note_ons_until(&mut track, 2000), vec![(1600, 64)]);
    }

    /// Every emitted note edge as `(arrangement tick, note, is_on)`, running
    /// `track` up to (not including) `until`.
    fn note_edges_until(track: &mut Track, until: i32) -> Vec<(i32, u8, bool)> {
        let mut out = Vec::new();
        while track.current_tick < until {
            let at = track.current_tick;
            if let Some(event) = track.tick()
                && let Some(kind) = event.event_type()
            {
                out.push((at, event.note_number().unwrap(), kind == EventType::NoteOn));
            }
        }
        out
    }

    /// Playback is *inside* the clip when its left edge is trimmed forward
    /// past the playhead (a real drag's first snapped targets can do this
    /// before it heads backwards). The clip stops being ticked, so the note
    /// sounding at that moment must be released at once rather than hang,
    /// and the frozen walk must not resume out of phase when the edge is
    /// dragged back behind the playhead.
    #[test]
    fn tick_releases_and_reseeks_when_the_start_is_trimmed_past_the_playhead() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_with_pre_roll(1000, 1000);
        clip.add_event(Event::new(1600, 0, vec![0x90, 64, 100]));
        clip.add_event(Event::new(1700, 0, vec![0x80, 64, 0]));
        track.add_clip(&clip);
        track.seek(0);
        assert_eq!(note_edges_until(&mut track, 1350), vec![(1300, 62, true)]);

        // Edge trimmed forward past the playhead while pitch 62 is sounding.
        let clip = &mut track.clips[0];
        clip.set_start_tick(1450);
        clip.region_mut().set_region(Some(1450), None);
        // Queued into `pending_note_offs` on the tick that notices, drained
        // on the next one — the same one-tick latency as a mute.
        assert_eq!(
            note_edges_until(&mut track, 1352),
            vec![(1351, 62, false)],
            "the sounding note must be released as the clip leaves the playhead"
        );
        assert!(note_edges_until(&mut track, 1500).is_empty());

        // ...and dragged back behind it: the walk must be in phase again.
        let clip = &mut track.clips[0];
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), None);
        assert_eq!(
            note_edges_until(&mut track, 2000),
            vec![(1600, 64, true), (1700, 64, false)]
        );
    }

    /// Once playback is inside the clip the delta-driven walk is already
    /// right, so an edge trim there must *not* re-seek (a re-seek would
    /// re-emit an event sitting exactly at the playhead).
    #[test]
    fn tick_does_not_reseek_an_edge_trim_while_inside_the_clip() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_pre_roll(1000, 1000));
        track.seek(0);
        // Into the clip, past its first note.
        assert_eq!(note_ons_until(&mut track, 1301), vec![(1300, 62)]);

        let clip = &mut track.clips[0];
        clip.set_start_tick(0);
        clip.region_mut().set_region(Some(0), None);

        assert!(
            note_ons_until(&mut track, 2000).is_empty(),
            "nothing may be re-emitted after the trim"
        );
    }

    /// A track playing one clip whose chord (60 and 64, ticks 0–240) is
    /// sounding at tick 120. Returns the clip's id.
    fn track_mid_chord() -> (Track, Uuid) {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        for (tick, status) in [(0, 0x90), (240, 0x80)] {
            clip.add_event(Event::new(tick, 0, vec![status, 60, 100]));
            clip.add_event(Event::new(tick, 0, vec![status, 64, 100]));
        }
        let clip_id = clip.id();
        track.add_clip(&clip);
        track.seek(0);
        assert_eq!(
            note_edges_until(&mut track, 120),
            vec![(0, 60, true), (0, 64, true)]
        );
        (track, clip_id)
    }

    /// An edit to the playing clip releases a note it takes away from under
    /// the playhead (60 moved later) and leaves one it only stretches (64)
    /// sounding to its new end, with no second `NoteOn` for either.
    #[test]
    fn edit_clip_events_releases_only_the_notes_the_edit_stops_sounding() {
        let (mut track, clip_id) = track_mid_chord();

        track.edit_clip_events(clip_id, |clip| {
            clip.restore_events(vec![
                Event::new(0, 0, vec![0x90, 64, 100]),
                Event::new(480, 0, vec![0x90, 60, 100]),
                Event::new(600, 0, vec![0x80, 60, 0]),
                Event::new(720, 0, vec![0x80, 64, 0]),
            ])
        });

        assert_eq!(
            note_edges_until(&mut track, 960),
            vec![
                (120, 60, false),
                (480, 60, true),
                (600, 60, false),
                (720, 64, false)
            ]
        );
    }

    /// Only the clip playback is inside has notes sounding: editing a clip
    /// the playhead hasn't reached releases nothing.
    #[test]
    fn edit_clip_events_releases_nothing_for_a_clip_not_playing() {
        let (mut track, _) = track_mid_chord();
        let other = clip_at(960, 960);
        let other_id = other.id();
        track.add_clip(&other);

        track.edit_clip_events(other_id, |clip| clip.restore_events(Vec::new()));

        assert_eq!(
            note_edges_until(&mut track, 241),
            vec![(240, 60, false), (240, 64, false)]
        );
    }

    #[test]
    fn release_sounding_notes_for_clip_queues_offs_only_for_the_named_clip() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        track.add_clip(&clip);
        let clip_id = track.clips()[0].id();
        track.seek(0);

        // Sound the note-on; its note-off is still in the future.
        assert_eq!(
            track.tick().and_then(|e| e.event_type()),
            Some(EventType::NoteOn)
        );

        // A different clip id queues nothing — the next tick is idle.
        track.release_sounding_notes_for_clip(Uuid::new_v4());
        assert!(track.tick().is_none());

        // The playing clip's own id queues its open note's off.
        track.release_sounding_notes_for_clip(clip_id);
        let off = track.tick().expect("a queued note-off");
        assert_eq!(off.event_type(), Some(EventType::NoteOff));
        assert_eq!(off.note_number(), Some(60));
    }

    #[test]
    fn tick_releases_a_note_still_open_when_the_clip_reaches_its_own_region_end() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 500); // timeline/region [0, 500)
        // A note whose stored `NoteOff` lies past this clip's own end —
        // exactly what a non-destructive split (`split.rs`) leaves on the
        // left half of a note straddling the cut.
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(700, 0, vec![0x80, 60, 0]));
        track.add_clip(&clip);
        track.seek(0);

        let mut events = Vec::new();
        for _ in 0..600 {
            if let Some(event) = track.tick() {
                events.push(event);
            }
        }

        // The note-on plays; its stored note-off (tick 700) is unreachable
        // once the clip's own region ends at 500 — it must be released
        // there instead of hanging forever.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type(), Some(EventType::NoteOn));
        assert_eq!(events[1].event_type(), Some(EventType::NoteOff));
        assert_eq!(events[1].note_number(), Some(60));
    }

    #[test]
    fn tick_chases_a_note_still_sounding_when_a_directly_adjacent_clip_starts_mid_note() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);

        // Two directly-adjacent clips sharing one note's full, untouched
        // event pair across the boundary — exactly what `SplitClipsEdit`
        // produces: both keep the *same* absolute event ticks (the split
        // clones the event list rather than rebasing it), only their
        // regions/`start_tick` differ. The left half's stored `NoteOff`
        // (tick 700) sits past its own region end (500); the right half's
        // stored `NoteOn` (tick 0) sits before its own region start (500).
        let mut left = clip_at(0, 500); // timeline/region [0, 500)
        left.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        left.add_event(Event::new(700, 0, vec![0x80, 60, 0]));

        let mut right = clip_at(0, 500);
        right.set_start_tick(500);
        right.region_mut().set_region(Some(500), Some(1000));
        right.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        right.add_event(Event::new(700, 0, vec![0x80, 60, 0]));

        track.add_clip(&left);
        track.add_clip(&right);
        track.seek(0);

        let mut events = Vec::new();
        for _ in 0..1000 {
            if let Some(event) = track.tick() {
                events.push(event);
            }
        }

        let types: Vec<_> = events.iter().map(|e| e.event_type()).collect();
        assert_eq!(
            types,
            vec![
                Some(EventType::NoteOn),  // left plays the note
                Some(EventType::NoteOff), // left releases it at its own end (500)
                Some(EventType::NoteOn),  // right chases it back in at its start
                Some(EventType::NoteOff), // right's own stored off fires at its real tick (700)
            ]
        );
    }

    /// The two halves `SplitClipsEdit` leaves when a clip is cut at tick 500
    /// through a sounding note (60: on@0, off@700), with a second note (64)
    /// starting exactly on the cut. Both halves share the full event list.
    fn split_halves_with_a_note_on_the_cut() -> (Clip, Clip) {
        let mut left = clip_at(0, 500); // timeline/region [0, 500)
        left.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        left.add_event(Event::new(500, 0, vec![0x90, 64, 100]));
        left.add_event(Event::new(600, 0, vec![0x80, 64, 0]));
        left.add_event(Event::new(700, 0, vec![0x80, 60, 0]));

        let mut right = clip_at(0, 500);
        right.set_start_tick(500);
        right.region_mut().set_region(Some(500), Some(1000));
        right.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        right.add_event(Event::new(500, 0, vec![0x90, 64, 100]));
        right.add_event(Event::new(600, 0, vec![0x80, 64, 0]));
        right.add_event(Event::new(700, 0, vec![0x80, 60, 0]));
        (left, right)
    }

    /// `(event type, note)` for every event `track.tick()` emits over `ticks`.
    fn tick_types(track: &mut Track, ticks: usize) -> Vec<(Option<EventType>, Option<u8>)> {
        let mut out = Vec::new();
        for _ in 0..ticks {
            if let Some(event) = track.tick() {
                out.push((event.event_type(), event.note_number()));
            }
        }
        out
    }

    #[test]
    fn tick_chases_a_note_exactly_once_when_another_event_sits_on_the_split_tick() {
        // An event exactly at the right half's region start makes `clip.tick()`
        // return `Some` on the first call at that tick, so `tick()` is called
        // again with `current_tick` still equal to `start_tick()` — the chase
        // must not run a second time on that call.
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let (left, right) = split_halves_with_a_note_on_the_cut();
        track.add_clip(&left);
        track.add_clip(&right);
        track.seek(0);

        let events = tick_types(&mut track, 1000);
        assert_eq!(
            events,
            vec![
                (Some(EventType::NoteOn), Some(60)),  // left plays the note
                (Some(EventType::NoteOff), Some(60)), // left releases it at its own end (500)
                (Some(EventType::NoteOn), Some(60)),  // right chases it back in, once
                (Some(EventType::NoteOn), Some(64)),  // right's own note on the cut
                (Some(EventType::NoteOff), Some(64)),
                (Some(EventType::NoteOff), Some(60)),
            ]
        );
    }

    #[test]
    fn tick_chases_a_note_exactly_once_after_an_explicit_seek_onto_a_split_halfs_start() {
        // Playing from the cursor parked on the right half's start seeks the
        // track straight onto `start_tick()` (no reseek path involved).
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let (left, right) = split_halves_with_a_note_on_the_cut();
        track.add_clip(&left);
        track.add_clip(&right);
        track.seek(500);

        let events = tick_types(&mut track, 500);
        assert_eq!(
            events,
            vec![
                (Some(EventType::NoteOn), Some(60)), // chased back in, once
                (Some(EventType::NoteOn), Some(64)),
                (Some(EventType::NoteOff), Some(64)),
                (Some(EventType::NoteOff), Some(60)),
            ]
        );
    }

    #[test]
    fn tick_does_not_chase_a_note_that_genuinely_starts_at_the_clip() {
        // An ordinarily-recorded clip's own note-on, sitting exactly at its
        // region start, must not be mistaken for a hidden "in-flight" note
        // and double-fired.
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut clip = clip_at(0, 960);
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(480, 0, vec![0x80, 60, 0]));
        track.add_clip(&clip);
        track.seek(0);

        let mut note_on_count = 0;
        for _ in 0..960 {
            if let Some(event) = track.tick()
                && event.event_type() == Some(EventType::NoteOn)
            {
                note_on_count += 1;
            }
        }
        assert_eq!(note_on_count, 1);
    }

    #[test]
    fn seek_selects_clip_containing_tick() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let a = clip_at(0, 1000);
        let b = clip_at(2000, 1000);
        let a_id = a.id();
        track.add_clip(&a);
        track.add_clip(&b);

        track.seek(500);

        // Clip at [0, 1000) contains 500; find_clip_id_at(500) should confirm.
        assert_eq!(track.find_clip_id_at(500), Some(a_id));
    }

    #[test]
    fn seek_skips_muted_clip_to_next_unmuted() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let mut muted = clip_at(0, 960);
        muted.set_muted(true);
        let next = clip_at(1000, 960);
        let next_id = next.id();
        track.add_clip(&muted);
        track.add_clip(&next);

        // seek(0): first clip is muted, should fall through to next.
        track.seek(0);

        // Tick enough to advance into the next clip zone and confirm no panic.
        assert!(track.clips().iter().any(|c| c.id() == next_id));
    }

    #[test]
    fn add_clip_rejects_exact_overlap() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        assert!(track.add_clip(&clip_at(0, 960)));
        // Same position and length — regression test for the infinite-loop bug.
        assert!(!track.add_clip(&clip_at(0, 960)));
        assert_eq!(track.clips().len(), 1);
    }

    #[test]
    fn add_clip_rejects_partial_overlap() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        assert!(track.add_clip(&clip_at(0, 960))); // spans [0, 960)
        assert!(!track.add_clip(&clip_at(480, 960))); // starts inside existing clip
        assert_eq!(track.clips().len(), 1);
    }

    #[test]
    fn add_clip_allows_adjacent_clips() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        assert!(track.add_clip(&clip_at(0, 960))); // spans [0, 960)
        assert!(track.add_clip(&clip_at(960, 960))); // spans [960, 1920) — touching but not overlapping
        assert_eq!(track.clips().len(), 2);
    }

    #[test]
    fn fits_mirrors_add_clip_overlap_rule() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_at(1000, 1000)); // spans [1000, 2000)

        assert!(!track.fits(&clip_at(1500, 1000)), "partial overlap");
        assert!(!track.fits(&clip_at(500, 2000)), "engulfing");
        assert!(
            track.fits(&clip_at(2000, 500)),
            "touching the end is adjacent"
        );
        assert!(
            track.fits(&clip_at(0, 1000)),
            "touching the start is adjacent"
        );
        assert!(track.fits(&clip_at(5000, 100)), "clear space");
    }

    #[test]
    fn add_clip_rejects_engulfing_overlap() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        assert!(track.add_clip(&clip_at(100, 200))); // spans [100, 300)
        assert!(!track.add_clip(&clip_at(0, 960))); // would engulf [100, 300)
        assert_eq!(track.clips().len(), 1);
    }

    #[test]
    fn seek_into_gap_selects_next_clip() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let a = clip_at(0, 500);
        let b = clip_at(1000, 500);
        let b_id = b.id();
        track.add_clip(&a);
        track.add_clip(&b);
        // 750 is in the gap between clips; next clip is b
        track.seek(750);
        assert_eq!(track.find_clip_id_at(1000), Some(b_id));
    }

    #[test]
    fn prev_clip_end_before_returns_closest_preceding_clip_end() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let a = clip_at(0, 500); // [0, 500)
        let b = clip_at(1000, 500); // [1000, 1500)
        track.add_clip(&a);
        track.add_clip(&b);

        assert_eq!(track.prev_clip_end_before(1000), Some(500));
    }

    #[test]
    fn prev_clip_end_before_returns_none_when_no_earlier_clip() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_at(1000, 500)); // [1000, 1500)

        assert_eq!(track.prev_clip_end_before(500), None);
    }

    #[test]
    fn find_clip_ids_in_returns_clips_overlapping_region() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        let a = clip_at(0, 500); // [0, 500)
        let b = clip_at(500, 500); // [500, 1000)
        let c = clip_at(1000, 500); // [1000, 1500)
        let a_id = a.id();
        let b_id = b.id();
        track.add_clip(&a);
        track.add_clip(&b);
        track.add_clip(&c);
        // Query [200, 600): overlaps a and b, not c
        let ids = track.find_clip_ids_in(200, 600);
        assert!(ids.contains(&a_id));
        assert!(ids.contains(&b_id));
        assert_eq!(ids.len(), 2);
    }

    const BEND_UP: [u8; 3] = [0xE0, 0x00, 0x60];
    const BEND_CENTRE: [u8; 3] = [0xE0, 0x00, 0x40];

    /// A clip `[start, start + 960)` holding note 60 over `[0, 480)` and a
    /// bend up at 100 that never comes back.
    fn clip_bent_up(start_tick: i32) -> Clip {
        let mut clip = clip_with_note_60(start_tick);
        clip.add_event(Event::new(100, 0, BEND_UP.to_vec()));
        clip.sort_events_by_tick();
        clip
    }

    /// Every emitted non-note event as `(arrangement tick, message)`,
    /// running `track` up to (not including) `until`.
    fn wheel_moves_until(track: &mut Track, until: i32) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        while track.current_tick < until {
            let at = track.current_tick;
            if let Some(event) = track.tick()
                && event.event_type().is_none()
            {
                out.push((at, event.midi_message().to_vec()));
            }
        }
        out
    }

    #[test]
    fn a_clip_that_ends_bent_is_centred_when_playback_leaves_it() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_bent_up(0));
        track.seek(0);

        assert_eq!(
            wheel_moves_until(&mut track, 1200),
            vec![(100, BEND_UP.to_vec()), (961, BEND_CENTRE.to_vec())]
        );
    }

    #[test]
    fn the_next_clip_starts_centred_when_the_one_before_ends_bent() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_bent_up(0));
        track.add_clip(&clip_with_note_60(960));
        track.seek(0);

        let moves = wheel_moves_until(&mut track, 1920);
        assert_eq!(
            moves,
            vec![(100, BEND_UP.to_vec()), (960, BEND_CENTRE.to_vec())]
        );
    }

    #[test]
    fn a_seek_into_a_bent_clip_sends_the_bend_before_anything_else() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_bent_up(0));
        track.seek(300);

        let first = track.tick().expect("the chased bend");
        assert_eq!(first.midi_message(), BEND_UP);
        assert!(wheel_moves_until(&mut track, 900).is_empty());
    }

    #[test]
    fn a_loop_wrap_mid_bend_snaps_back_to_where_the_loop_starts() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_bent_up(0));
        track.seek(0);
        wheel_moves_until(&mut track, 400);

        // The wrap re-seeks to the loop start, before the bend.
        track.seek(0);
        assert_eq!(
            wheel_moves_until(&mut track, 400),
            vec![(0, BEND_CENTRE.to_vec()), (100, BEND_UP.to_vec())]
        );
    }

    #[test]
    fn a_track_whose_clips_never_move_a_wheel_sends_none() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_note_60(0));
        track.add_clip(&clip_with_note_60(1200));
        track.seek(0);
        assert!(wheel_moves_until(&mut track, 700).is_empty());
        track.seek(300);
        assert!(wheel_moves_until(&mut track, 2400).is_empty());
        assert!(track.take_wheel_resets().is_empty());
    }

    /// The right half of a split whose left half bent up: arriving at its
    /// start (from a gap, or from a jump onto it — the seek's chase and the
    /// arrival chase both find the bend) sends the bend held there, exactly
    /// once.
    #[test]
    fn arriving_at_a_split_half_chases_the_bend_held_from_its_hidden_part() {
        let mut right = clip_bent_up(0);
        right.set_start_tick(500);
        right.region_mut().set_region(Some(500), Some(960));

        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&right);
        track.seek(0);
        assert_eq!(
            wheel_moves_until(&mut track, 900),
            vec![(500, BEND_UP.to_vec())]
        );

        // A jump, as `Sequencer::reset_to_tick` makes it.
        track.invalidate_controllers();
        track.seek(500);
        assert_eq!(
            wheel_moves_until(&mut track, 900),
            vec![(500, BEND_UP.to_vec())]
        );
    }

    #[test]
    fn a_muted_bend_moves_nothing() {
        let mut clip = clip_with_note_60(0);
        let mut bend = Event::new(100, 0, BEND_UP.to_vec());
        bend.set_muted(true);
        clip.add_event(bend);
        clip.sort_events_by_tick();
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip);
        track.seek(0);
        wheel_moves_until(&mut track, 400);

        // Neither a seek past it nor a stop sends anything for it.
        track.seek(300);
        assert!(wheel_moves_until(&mut track, 400).is_empty());
        assert!(track.take_wheel_resets().is_empty());
    }

    #[test]
    fn a_stop_centres_the_bend_unless_the_player_moved_it_last() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_bent_up(0));
        track.seek(0);
        wheel_moves_until(&mut track, 400);
        assert_eq!(track.take_wheel_resets(), vec![BEND_CENTRE.to_vec()]);

        track.seek(0);
        wheel_moves_until(&mut track, 400);
        track.hand_controller_to_player(&[0xE5, 0x00, 0x20]);
        assert!(track.take_wheel_resets().is_empty());
    }

    /// A clip `[start, start + 960)` holding note 60 and, at each `(tick,
    /// cc, value)`, a Control Change.
    fn clip_with_ccs(start_tick: i32, ccs: &[(i32, u8, u8)]) -> Clip {
        let mut clip = clip_with_note_60(start_tick);
        for &(tick, cc, value) in ccs {
            clip.add_event(Event::new(tick, 0, vec![0xB0, cc, value]));
        }
        clip.sort_events_by_tick();
        clip
    }

    #[test]
    fn a_knob_follows_playback_of_its_cc_without_counting_as_the_hands() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_ccs(0, &[(100, 14, 90), (200, 30, 5)]));
        track.seek(0);

        wheel_moves_until(&mut track, 150);
        assert_eq!(track.knobs()[0].value, Some(90));
        assert!(track.take_knobs_moved());
        assert!(!track.take_knobs_moved());
        // CC 30 is no knob's: it plays and moves none.
        wheel_moves_until(&mut track, 400);
        assert!(!track.take_knobs_moved());
        assert_eq!(*track.knobs_by_hand(), [None; KNOB_COUNT]);
    }

    /// A knob's CC has no rest: a stop leaves it where playback put it,
    /// while the wheels go back to theirs.
    #[test]
    fn a_stop_leaves_a_knobs_cc_where_it_is_while_the_wheels_reset() {
        let mut clip = clip_with_ccs(0, &[(150, 14, 90)]);
        clip.add_event(Event::new(100, 0, BEND_UP.to_vec()));
        clip.sort_events_by_tick();
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip);
        track.seek(0);
        wheel_moves_until(&mut track, 400);

        assert_eq!(track.take_wheel_resets(), vec![BEND_CENTRE.to_vec()]);
        assert_eq!(track.knobs()[0].value, Some(90));
    }

    /// A seek chases a knob's CC to the clip's value there, and leaves it
    /// alone in a gap; a CC no knob holds (a sustain pedal, imported) is
    /// never chased.
    #[test]
    fn a_seek_chases_a_knobs_cc_but_not_a_cc_no_knob_holds() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_ccs(0, &[(100, 14, 90), (100, 64, 127)]));

        track.seek(300);
        assert_eq!(
            wheel_moves_until(&mut track, 400),
            vec![(300, vec![0xB0, 14, 90])]
        );
        assert_eq!(track.knobs()[0].value, Some(90));

        track.invalidate_controllers();
        track.seek(1000);
        assert!(wheel_moves_until(&mut track, 1100).is_empty());
    }

    /// While a drag holds a knob the hand wins: the clip's moves of its CC
    /// are dropped, and the knob stays; after the release, playback takes
    /// it over at the next move.
    #[test]
    fn a_held_knob_drops_the_clips_moves_of_its_cc_until_release() {
        let mut track = Track::new(TrackOutput::MidiOut { channel: 0 }, 0, 0);
        track.add_clip(&clip_with_ccs(
            0,
            &[(100, 15, 40), (200, 14, 90), (600, 14, 30)],
        ));
        track.seek(0);
        assert!(track.set_knob_by_hand(0, 10));
        track.hold_knob(0);

        assert_eq!(
            wheel_moves_until(&mut track, 400),
            vec![(100, vec![0xB0, 15, 40])]
        );
        assert_eq!(track.knobs()[0].value, Some(10));

        track.release_knobs();
        assert_eq!(
            wheel_moves_until(&mut track, 700),
            vec![(600, vec![0xB0, 14, 30])]
        );
        assert_eq!(track.knobs()[0].value, Some(30));
        assert_eq!(track.knobs_by_hand()[0], Some(10));
    }
}
