//! The [`Sequencer`] struct itself — construction and the bulk of the plain
//! accessors / mutators. The per-tick playback pump and the note-release paths
//! are in `playback.rs`.
//!
//! `Sequencer` owns the tracks (up to `MAX_TRACKS`), the running capture and
//! live-record clips, the three selections, the performance
//! lane and the clip clipboard. It holds `Arc<AtomicX>` handles into
//! `SharedAtomics` for the values the rest of the app reads, but deliberately
//! *not* `clock_tick` — every position it needs arrives stamped on the input
//! message, and everything it measures itself is a duration (`elapsed_ticks`).
//! The concern-specific methods live in the sibling files; see the parent
//! module docs.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, Ordering},
};

use crossbeam_channel::Sender;
use rtrb::Producer;
use uuid::Uuid;

use super::clipboard::ClipClipboard;
use super::instrument_event::ClipInstrumentEvent;
use super::instrument_notes::InstrumentNotes;
use crate::core::midi::out_queue::MidiOutMessage;
use crate::{
    core::{
        config,
        shared_atomics::{LiveRecState, TrackMixAtomics},
        time::Meter,
    },
    metadata::clip_metadata::ClipMetadata,
    models::selection::Selection,
    models::{
        clip::{Clip, CopiedNote},
        performance_lane::PerformanceLane,
        track::{Track, TrackOutput},
    },
};

/// Book-keeping for one live-recording take. See `live_recording.rs`.
#[derive(Clone, Copy)]
pub(crate) struct LiveRecSession {
    /// Track being recorded onto.
    pub track_idx: usize,
    /// Playback tick the take started at — the clip's arrangement position.
    pub rec_start_tick: i32,
    /// The odometer value the take started at — see
    /// [`SharedAtomics::elapsed_ticks`](crate::core::shared_atomics::SharedAtomics::elapsed_ticks).
    /// Only ever used as the base of a duration, never as a position.
    pub elapsed_start_tick: i32,
}

/// Outcome of ending a live-recording take.
pub(crate) enum LiveRecResult {
    /// Take was empty or couldn't be placed; the recording clip `rec_clip_id`
    /// on `track_idx` is gone.
    Canceled {
        /// Track the abandoned take was on.
        track_idx: usize,
        /// Id of the discarded recording clip.
        rec_clip_id: Uuid,
    },
    /// Take committed; `clip` is the new clip's metadata.
    Completed {
        /// The committed clip.
        clip: ClipMetadata,
    },
}

/// The model the `"sequencer"` thread owns. See the module docs.
pub(crate) struct Sequencer {
    // --- Communication ---
    /// Outbound MIDI to the `"midiout"` thread — clip playback for
    /// `MidiOut` tracks, chased notes, releases.
    pub(super) midi_out_tx: Sender<MidiOutMessage>,
    /// `ClipInstrumentEvent`s (`track`, a `Midi3`, and `when`) for
    /// `TrackOutput::Instrument` tracks' clip events, demuxed to the per-track
    /// plugin by the audio-engine callback (macOS only). Live-thru is a
    /// separate `rtrb` ring, tagged by `MidiInputForwarder` instead — `rtrb` is
    /// strict single-producer, unlike a `crossbeam_channel::Sender`, so the two
    /// feeds can't share one.
    /// Elsewhere the consumer is dropped, so pushes here fill the ring once
    /// and fail from then on. See `130-plugin-host.md`.
    pub(super) instrument_midi_tx: Producer<ClipInstrumentEvent>,
    /// Which notes each instrument track's plugin currently has sounding, so
    /// [`release_instrument_notes`](Self::release_instrument_notes) can release
    /// exactly those. Fed from `tick` / `chase_notes`.
    pub(super) instrument_notes: InstrumentNotes,

    // --- Selection state ---
    /// The selected track (a `Selection` for its moving-lead semantics, though
    /// track selection is single).
    pub(super) track_selection: Selection,
    /// The selected clip(s) on the selected track.
    pub(super) clip_selection: Selection,
    /// MIDI channel (0–15) of the armed track.
    pub(super) arm_channel: Arc<AtomicU8>,

    // --- Tracks and clips ---
    /// The arrangement tracks, `1..=MAX_TRACKS` of them, each in its own
    /// engine slot ([`Track::slot`]).
    pub(super) tracks: Vec<Track>,
    /// Per-track volume / stereo balance, shared lock-free with the CLAP mixer
    /// (audio thread) and the arranger track header. Written by
    /// [`set_track_volume`](Self::set_track_volume) /
    /// [`set_track_pan`](Self::set_track_pan) (see `sequencer/mix.rs`); the
    /// atomics are the source of truth, not mirrored onto `Track`.
    pub(super) track_mix: Arc<TrackMixAtomics>,

    /// Session-only clip copy/paste buffer (see `clipboard.rs`). Not persisted,
    /// not part of the undo record. Cleared on new/load project.
    pub(super) clip_clipboard: Option<ClipClipboard>,
    /// Session-only note copy/paste buffer for the clip view (see
    /// `clipboard.rs`), separate from `clip_clipboard`. Empty when nothing
    /// is copied. Not persisted, not part of the undo record. Cleared on
    /// new/load project.
    pub(super) note_clipboard: Vec<CopiedNote>,

    // --- Region and playback state ---
    /// Playback position, shared with the transport / UI.
    pub(super) playback_tick: Arc<AtomicI32>,
    /// Edit cursor, shared with the transport / UI.
    pub(super) cursor_tick: Arc<AtomicI32>,
    /// Loop-region start, shared.
    pub(super) region_start: Arc<AtomicI32>,
    /// Loop-region end, shared.
    pub(super) region_end: Arc<AtomicI32>,
    /// Mirror of `SharedAtomics.loop_enabled` — the sequencer reads it when
    /// sizing a running-capture commit (a looping take is cropped to the loop
    /// region, a non-looping one to the played content).
    pub(super) loop_enabled: Arc<AtomicBool>,
    /// Tempo in µs per quarter note, shared.
    pub(super) tempo: Arc<AtomicI32>,
    /// The project's time signature, shared (packed by [`Meter::to_bits`]).
    pub(super) meter: Arc<AtomicU16>,
    /// Whether the transport is running, shared.
    pub(super) running: Arc<AtomicBool>,

    // --- Recording state ---
    /// The rolling buffer every incoming note is fed into, ready to be
    /// committed (`capture.rs`).
    pub(super) capture_clip: Clip,
    /// `clock_tick − playback_tick` when the latest note-on was captured
    /// while running — how far the capture clock's numbering sits from the
    /// cursor's. The clock only ever matches playback's *phase*, so this is
    /// whole loop lengths (plus a few ticks of input latency); a commit
    /// shifts its anchor by it (`capture_clock_shift`). `None` until a note
    /// arrives while running, and cleared with the buffer.
    pub(super) capture_clock_offset: Option<i32>,
    /// The clip a dedicated live-record take grows into (`live_recording.rs`).
    pub(super) live_rec_clip: Clip,
    /// The active live-record session, if any.
    pub(super) live_rec_session: Option<LiveRecSession>,
    /// Shared last-note / thumbnail surface the UI draws the take from.
    pub(super) live_rec_state: LiveRecState,
    /// The transport odometer. The sequencer deliberately does **not** hold
    /// `clock_tick`: every position it needs arrives stamped on the input
    /// message, and everything it measures here is a duration.
    pub(super) elapsed_ticks: Arc<AtomicI32>,
    /// Set when a take should stop itself at a known tick (loop-length reached);
    /// consumed by [`end_live_recording`](Self::end_live_recording).
    pub(super) auto_end_tick: Option<i32>,

    // --- Arranger performance lane ---
    /// Tracks the physical keyboard's currently-held trigger while the
    /// lane is armed — live-input only, nothing is recorded or persisted.
    pub(super) performance_lane: PerformanceLane,
    /// True while the performance lane (not a regular track) is selected,
    /// which arms the physical MIDI keyboard for bar-jump triggering
    /// instead of normal note capture. Mutually exclusive with track
    /// selection. Shared with `MidiInputForwarder` (a different thread)
    /// so it can suppress the MIDI-OUT thru-forward while armed.
    pub(super) performance_lane_armed: Arc<AtomicBool>,
}

impl Sequencer {
    // --- Constructor ---
    /// Builds the sequencer, wiring it to the shared atomics / channels and
    /// creating `DEFAULT_TRACK_COUNT` tracks each pre-routed to its own MIDI channel.
    /// Parameter order follows the field grouping (channels, then the atomics)
    /// — see `080-conventions.md`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        midi_out_tx: Sender<MidiOutMessage>,
        instrument_midi_tx: Producer<ClipInstrumentEvent>,
        playback_tick: Arc<AtomicI32>,
        elapsed_ticks: Arc<AtomicI32>,
        cursor_tick: Arc<AtomicI32>,
        region_start: Arc<AtomicI32>,
        region_end: Arc<AtomicI32>,
        loop_enabled: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
        tempo: Arc<AtomicI32>,
        meter: Arc<AtomicU16>,
        arm_channel: Arc<AtomicU8>,
        performance_lane_armed: Arc<AtomicBool>,
        track_mix: Arc<TrackMixAtomics>,
        live_rec_state: LiveRecState,
    ) -> Self {
        Sequencer {
            midi_out_tx,
            instrument_midi_tx,
            instrument_notes: InstrumentNotes::new(),
            track_selection: Selection::new(),
            clip_selection: Selection::new(),
            arm_channel,
            tracks: (0..config::DEFAULT_TRACK_COUNT)
                .map(default_track)
                .collect(),
            track_mix,
            clip_clipboard: None,
            note_clipboard: Vec::new(),
            playback_tick,
            cursor_tick,
            region_start,
            region_end,
            loop_enabled,
            tempo,
            meter,
            running,
            capture_clip: Clip::new(),
            capture_clock_offset: None,
            live_rec_clip: Clip::new(),
            live_rec_session: None,
            elapsed_ticks,
            auto_end_tick: None,
            live_rec_state,
            performance_lane: PerformanceLane::new(),
            performance_lane_armed,
        }
    }

    // --- Core state accessors/mutators ---
    /// Whether the transport is playing.
    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Whether a dedicated live-record take is in progress.
    pub(crate) fn is_recording(&self) -> bool {
        self.live_rec_session.is_some()
    }

    /// Current cursor position, in ticks.
    pub(crate) fn cursor_tick(&self) -> i32 {
        self.cursor_tick.load(Ordering::Relaxed)
    }

    /// Places the cursor (clamped to `>= 0`) — the same shared atomic and the
    /// same clamp as `Transport::set_cursor_tick`, for the sequencer-side
    /// workflows that position the cursor themselves (the band-press
    /// `select_clip_span_workflow`; the capture paths store
    /// to it directly).
    pub(crate) fn set_cursor_tick(&mut self, tick: i32) {
        self.cursor_tick.store(tick.max(0), Ordering::Relaxed);
    }

    /// Current playback position, in ticks.
    pub(crate) fn playback_tick(&self) -> i32 {
        self.playback_tick.load(Ordering::Relaxed)
    }

    /// The transport odometer — musical time played, in ticks. A duration base,
    /// never a position (`150-clock-position-sync.md`).
    pub(crate) fn elapsed_tick(&self) -> i32 {
        self.elapsed_ticks.load(Ordering::Relaxed)
    }

    /// Total clip count across all tracks.
    pub(super) fn number_of_clips(&self) -> usize {
        self.tracks.iter().map(|t| t.clips().len()).sum()
    }

    /// Sets the project tempo, in µs per quarter note.
    pub(crate) fn set_tempo(&self, value: i32) {
        self.tempo.store(value, Ordering::Relaxed);
    }

    /// Sets the project's time signature. Moves nothing: notes, clips and
    /// the loop region keep their ticks (`archive/270-time-signature.md`).
    pub(crate) fn set_meter(&self, meter: Meter) {
        self.meter.store(meter.to_bits(), Ordering::Relaxed);
    }

    /// Sets both loop-region bounds directly (no normalization / clock realign
    /// — the transport owns that path).
    pub(crate) fn set_global_region(&mut self, start: i32, end: i32) {
        self.region_start.store(start, Ordering::Relaxed);
        self.region_end.store(end, Ordering::Relaxed);
    }

    // --- Clip and track management ---
    /// Empties the running capture buffer.
    pub(crate) fn reset_capture(&mut self) {
        self.capture_clip = Clip::new();
        self.capture_clock_offset = None;
    }

    /// The engine slot of the instrument track the live keyboard is currently
    /// routed to (the armed track, if it hosts a plugin) — used to protect
    /// live-held notes from
    /// [`release_instrument_notes`](Self::release_instrument_notes) (in
    /// `playback.rs`).
    pub(super) fn live_instrument_target(&self) -> Option<usize> {
        let track = self.tracks.get(self.selected_track_index()?)?;
        matches!(track.output(), TrackOutput::Instrument(_)).then_some(track.slot())
    }

    // --- Project accessors ---
    /// Tempo in µs per quarter note.
    pub(crate) fn tempo_us(&self) -> i32 {
        self.tempo.load(Ordering::Relaxed)
    }

    /// The project's time signature.
    pub(crate) fn meter(&self) -> Meter {
        Meter::from_bits(self.meter.load(Ordering::Relaxed))
    }

    /// Loop-region start tick.
    pub(crate) fn region_start(&self) -> i32 {
        self.region_start.load(Ordering::Relaxed)
    }

    /// Loop-region end tick.
    pub(crate) fn region_end(&self) -> i32 {
        self.region_end.load(Ordering::Relaxed)
    }

    /// Whether playback loops at the region end.
    pub(crate) fn is_loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Relaxed)
    }

    /// Whether playback is actually wrapping — the loop is on *and*
    /// `playback_tick` is inside the region, the same condition
    /// `Transport::tick` wraps on. A start after the region end runs linearly
    /// with the loop flag still set; running capture sizes and windows such a
    /// take as linear (`100-running-capture.md`).
    pub(crate) fn playback_is_looping(&self) -> bool {
        self.is_loop_enabled()
            && (self.region_start()..self.region_end()).contains(&self.playback_tick())
    }

    /// The loop region's length, floored to [`config::REGION_LENGTH_DEFAULT`].
    /// The default phrase-window size: the stopped `/`'s detection window.
    pub(crate) fn loop_reference_length(&self) -> i32 {
        (self.region_end() - self.region_start()).max(config::REGION_LENGTH_DEFAULT)
    }

    /// Shifts the loop region by `delta` ticks (both edges). Used by
    /// `DuplicateTimeEdit` so a loop that sat within or after the duplicated
    /// span slides along with the inserted time and keeps its relative position.
    /// The two atomics are stored separately and can momentarily tear; readers
    /// (`Display::region_bounds_snapshot`) already retry, and this runs on an
    /// edit action, not a hot path.
    pub(crate) fn shift_region(&self, delta: i32) {
        self.region_start.fetch_add(delta, Ordering::Relaxed);
        self.region_end.fetch_add(delta, Ordering::Relaxed);
    }

    /// The arrangement tracks.
    pub(crate) fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Mutable access to the arrangement tracks.
    pub(crate) fn tracks_mut(&mut self) -> &mut [Track] {
        &mut self.tracks
    }

    /// The engine slot of the track at `track_idx`, `None` out of range.
    pub(super) fn slot_of(&self, track_idx: usize) -> Option<usize> {
        self.tracks.get(track_idx).map(Track::slot)
    }

    /// The lowest engine slot no track holds — always `Some` below
    /// `MAX_TRACKS` tracks.
    pub(super) fn free_slot(&self) -> Option<usize> {
        (0..config::MAX_TRACKS).find(|&slot| self.tracks.iter().all(|t| t.slot() != slot))
    }

    // --- Project mutators ---
    /// Sets how many tracks the project has, clamped to `1..=MAX_TRACKS`:
    /// drops tracks off the end, or appends empty MIDI-Out tracks each on
    /// the channel matching its index. For a project load, after
    /// [`new_project`](Self::new_project) (which puts every track back in the
    /// slot matching its position, so this one's appends get theirs too) — it
    /// doesn't release a dropped track's sounding notes.
    pub(crate) fn set_track_count(&mut self, count: usize) {
        let count = count.clamp(1, config::MAX_TRACKS);
        self.tracks.truncate(count);
        while self.tracks.len() < count {
            let idx = self.tracks.len();
            let mut track = default_track(idx);
            track.set_slot(self.free_slot().unwrap_or(idx));
            self.tracks.push(track);
        }
    }

    /// Wipes the session back to an empty project: clears every track, the
    /// capture buffer, selections, the clipboard and the
    /// performance lane; resets position, region, tempo, meter and the mixer to
    /// defaults. See `060-persistence.md`.
    pub(crate) fn new_project(&mut self) {
        // Back to slot = position: the loaded project's plugins are all
        // reloaded and its mix values all written afresh, so no engine state
        // has to stay where it was. Colours go back to position and names
        // to the number too; a load then sets each track's saved ones.
        for (idx, track) in self.tracks.iter_mut().enumerate() {
            track.clear_clips();
            track.set_slot(idx);
            track.set_color_slot(idx);
            track.set_name(None);
        }
        self.instrument_notes.clear();
        self.reset_capture();
        self.track_selection = Selection::new();
        self.clip_selection = Selection::new();
        self.playback_tick.store(0, Ordering::Relaxed);
        self.cursor_tick.store(0, Ordering::Relaxed);
        self.region_start.store(0, Ordering::Relaxed);
        self.region_end
            .store(config::REGION_LENGTH_DEFAULT, Ordering::Relaxed);
        self.tempo
            .store(config::TEMPO_US_DEFAULT, Ordering::Relaxed);
        self.set_meter(Meter::FOUR_FOUR);
        self.performance_lane.clear();
        self.performance_lane_armed.store(false, Ordering::Relaxed);
        self.clip_clipboard = None;
        self.note_clipboard.clear();
        self.reset_track_mix();
    }
}

/// A fresh empty track for position `idx`, in the slot and colour of the
/// same number: MIDI out on the channel matching its index (track 1 on
/// channel 1).
fn default_track(idx: usize) -> Track {
    Track::new(TrackOutput::MidiOut { channel: idx as u8 }, idx, idx)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::core::config;
    use crate::core::sequencer::test_support::sequencer_at_tempo;
    use crate::core::time::Meter;

    #[test]
    fn new_project_resets_tempo_to_default() {
        let mut sequencer = sequencer_at_tempo(config::TEMPO_US_DEFAULT / 2).0;

        sequencer.new_project();

        assert_eq!(sequencer.tempo_us(), config::TEMPO_US_DEFAULT);
    }

    #[test]
    fn a_fresh_sequencer_has_the_default_track_count() {
        let sequencer = sequencer_at_tempo(config::TEMPO_US_DEFAULT).0;

        assert_eq!(sequencer.tracks().len(), config::DEFAULT_TRACK_COUNT);
    }

    #[test]
    fn set_track_count_grows_with_midi_tracks_on_their_own_channels() {
        let mut sequencer = sequencer_at_tempo(config::TEMPO_US_DEFAULT).0;

        sequencer.set_track_count(6);

        let channels: Vec<u8> = sequencer
            .tracks()
            .iter()
            .map(|t| t.midi_channel())
            .collect();
        assert_eq!(channels, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn set_track_count_shrinks_and_clamps_to_one_through_max() {
        let mut sequencer = sequencer_at_tempo(config::TEMPO_US_DEFAULT).0;

        sequencer.set_track_count(2);
        assert_eq!(sequencer.tracks().len(), 2);

        sequencer.set_track_count(0);
        assert_eq!(sequencer.tracks().len(), 1);

        sequencer.set_track_count(config::MAX_TRACKS + 5);
        assert_eq!(sequencer.tracks().len(), config::MAX_TRACKS);
    }

    #[test]
    fn loop_reference_length_tracks_the_loop_region_with_a_default_floor() {
        let mut seq = sequencer_at_tempo(config::TEMPO_US_DEFAULT).0;
        let bar = Meter::FOUR_FOUR.bar_ticks();

        // Fresh sequencer: region is the default window.
        assert_eq!(seq.loop_reference_length(), config::REGION_LENGTH_DEFAULT);

        // A wider loop is reflected directly.
        seq.set_global_region(bar, bar * 5);
        assert_eq!(seq.loop_reference_length(), bar * 4);

        // A degenerate zero-width region floors to the default.
        seq.set_global_region(bar * 2, bar * 2);
        assert_eq!(seq.loop_reference_length(), config::REGION_LENGTH_DEFAULT);
    }

    /// Looping means the loop flag *and* playback inside the region — the
    /// transport's own wrap condition. Playback parked outside the region
    /// (a start after its end) runs linearly with the flag still set.
    #[test]
    fn playback_is_looping_requires_the_flag_and_playback_inside_the_region() {
        let mut seq = sequencer_at_tempo(config::TEMPO_US_DEFAULT).0;
        let bar = Meter::FOUR_FOUR.bar_ticks();
        seq.set_global_region(bar * 2, bar * 4);
        seq.loop_enabled.store(true, Ordering::Relaxed);

        seq.playback_tick.store(bar * 2, Ordering::Relaxed);
        assert!(seq.playback_is_looping(), "region start is inside");
        seq.playback_tick.store(bar * 4 - 1, Ordering::Relaxed);
        assert!(
            seq.playback_is_looping(),
            "last tick before the end is inside"
        );

        seq.playback_tick.store(bar * 4, Ordering::Relaxed);
        assert!(!seq.playback_is_looping(), "region end is half-open");
        seq.playback_tick.store(bar * 2 - 1, Ordering::Relaxed);
        assert!(!seq.playback_is_looping(), "before the region never wraps");

        seq.playback_tick.store(bar * 3, Ordering::Relaxed);
        seq.loop_enabled.store(false, Ordering::Relaxed);
        assert!(!seq.playback_is_looping(), "flag off never wraps");
    }
}
