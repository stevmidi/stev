//! Every piece of state shared between threads that isn't a channel message.
//!
//! The rule (`000-architecture.md`, `080-conventions.md`): UI-visible state the
//! render thread reads on the hot path is an `Arc<AtomicX>` here, never a lock
//! and never a channel round-trip. All of these are read and written with
//! [`Ordering::Relaxed`] — each value is
//! independent, nothing here publishes a buffer that another field points into,
//! so there is no ordering to establish between them.
//!
//! Each field's doc names the thread that writes it. The bundle is built once
//! in `setup_shared_atomics` and cloned into every thread that needs a handle.
//! The [`crate::models::region::Region`] precedent applies throughout: where an
//! atomic pair backs a model type, the atomics *are* the source of truth, not a
//! mirror of some owned field.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

use crate::core::config::MAX_TRACKS;

/// Per-track mixer state — volume, stereo balance, mute and solo. `volume_db`
/// and `pan` are stored as `f32` bit patterns
/// ([`f32::to_bits`]/[`f32::from_bits`]) so each is a single lock-free atomic;
/// `mute` and `solo` are plain [`AtomicBool`]. Written by the sequencer thread
/// (`Sequencer::set_track_*` / `toggle_track_*`), read once per audio block by
/// the instrument mixer (`volume_db`/`pan` only — mute/solo gate MIDI, not audio) and
/// once per frame by the arranger's track header. Follows the
/// [`crate::models::region::Region`] precedent: the atomics *are* the source of
/// truth, not a mirror of a model field. `volume_db` is clamped to
/// `[mix::MIN_DB, mix::MAX_DB]`, `pan` to `[-1.0, 1.0]`; the all-zero / all-false
/// default (`0.0` dB, centre, unmuted, unsoloed) is the neutral value, so a
/// fresh atomic, a new project and an old project with no stored value all
/// agree. Every array is indexed by engine slot (`Track::slot`), not track
/// position — a slot no track holds is kept at neutral. See
/// `130-plugin-host.md`.
pub(crate) struct TrackMixAtomics {
    /// Per-track volume in dB, as an `f32` bit pattern. Clamped
    /// `[mix::MIN_DB, mix::MAX_DB]`.
    pub(crate) volume_db: [AtomicU32; MAX_TRACKS],
    /// Per-track stereo balance in `[-1.0, 1.0]` (`0.0` = centre), as an `f32`
    /// bit pattern.
    pub(crate) pan: [AtomicU32; MAX_TRACKS],
    /// Per-track mute. Gates MIDI, not audio.
    pub(crate) mute: [AtomicBool; MAX_TRACKS],
    /// Per-track solo. Gates MIDI, not audio.
    pub(crate) solo: [AtomicBool; MAX_TRACKS],
}

impl TrackMixAtomics {
    /// All slots at the neutral default — `0.0` dB (unity), centre pan,
    /// unmuted, unsoloed — which is the all-zero / all-false bit pattern, so a
    /// fresh instance needs no per-track init.
    pub(crate) fn new() -> Self {
        TrackMixAtomics {
            volume_db: std::array::from_fn(|_| AtomicU32::new(0)),
            pan: std::array::from_fn(|_| AtomicU32::new(0)),
            mute: std::array::from_fn(|_| AtomicBool::new(false)),
            solo: std::array::from_fn(|_| AtomicBool::new(false)),
        }
    }

    /// `track_idx`'s volume in dB (`0.0` — unity — for an out-of-range index).
    pub(crate) fn volume_db(&self, track_idx: usize) -> f32 {
        self.volume_db
            .get(track_idx)
            .map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
    }

    /// `track_idx`'s stereo balance (`0.0` — centre — for an out-of-range
    /// index).
    pub(crate) fn pan(&self, track_idx: usize) -> f32 {
        self.pan
            .get(track_idx)
            .map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
    }

    /// Whether `track_idx`'s own mute flag is set (ignores solo).
    pub(crate) fn muted(&self, track_idx: usize) -> bool {
        self.mute
            .get(track_idx)
            .is_some_and(|s| s.load(Ordering::Relaxed))
    }

    /// Whether `track_idx`'s solo flag is set.
    pub(crate) fn soloed(&self, track_idx: usize) -> bool {
        self.solo
            .get(track_idx)
            .is_some_and(|s| s.load(Ordering::Relaxed))
    }
}

/// The live-recording surface the render thread reads to draw the take as it
/// grows. Written by the `"sequencer"` thread as notes arrive; see
/// `090-live-recording.md`. The `Mutex` is the one lock in this module — it is
/// never touched on the audio path, only sequencer-write / render-read of a
/// small `Vec`, so a `parking_lot`-style spin isn't warranted.
pub(crate) struct LiveRecState {
    /// Note number of the most recent NoteOn — drives the "currently playing"
    /// key highlight while recording.
    pub(crate) last_note_on: Arc<AtomicU8>,
    /// Velocity of the most recent NoteOn, paired with
    /// [`last_note_on`](Self::last_note_on).
    pub(crate) last_note_velocity: Arc<AtomicU8>,
    /// [`SharedAtomics::elapsed_ticks`] as it read when the take started. Every
    /// live-rec tick is `elapsed_ticks - elapsed_start_tick`, so this is only
    /// ever used as the base of a duration.
    pub(crate) elapsed_start_tick: Arc<AtomicI32>,
    /// `(start_tick, end_tick, pitch_frac)` for every note captured so far —
    /// the thumbnail the arranger draws over the recording clip each frame. A
    /// held note's `end_tick` is [`Self::HELD_NOTE_SENTINEL`].
    pub(crate) thumbnail_snapshot: Arc<Mutex<Vec<(i32, i32, f32)>>>,
}

impl LiveRecState {
    /// The `end_tick` of a [`thumbnail_snapshot`](Self::thumbnail_snapshot)
    /// entry whose note is still held: the renderer draws it to the current
    /// live-rec tick instead, so it grows every frame without a rebuild.
    pub(crate) const HELD_NOTE_SENTINEL: i32 = i32::MAX;

    /// Clones every `Arc` handle — the shared state itself is not duplicated.
    /// Named rather than `#[derive(Clone)]` because the fields are `Arc`s and a
    /// derive would read as a deep copy.
    pub(crate) fn clone(&self) -> Self {
        LiveRecState {
            last_note_on: self.last_note_on.clone(),
            last_note_velocity: self.last_note_velocity.clone(),
            elapsed_start_tick: self.elapsed_start_tick.clone(),
            thumbnail_snapshot: self.thumbnail_snapshot.clone(),
        }
    }
}

/// The whole shared-state bundle. Built in `setup_shared_atomics`, cloned into
/// each thread. `track_mix` and `live_rec_state` group related sub-bundles;
/// everything else is a single value. Unless a field says otherwise it is
/// written from the `"sequencer"` thread (through `Sequencer` or its
/// thread-local `Transport`) and read by the render thread each frame.
pub(crate) struct SharedAtomics {
    /// True while the transport is playing. Written by `Transport::start`/`stop`.
    pub(crate) running: Arc<AtomicBool>,
    /// Current tempo, microseconds per quarter note. Read by the `"clock"`
    /// thread every firing to size the next tick.
    pub(crate) tempo: Arc<AtomicI32>,
    /// Free-running musical tick counter. Written by the `"clock"` thread; held
    /// to `clock_tick ≡ playback_tick (mod region_length)` — *not* absolute
    /// equality — via [`ClockCommand::AlignToPlayback`](crate::core::clock::ClockCommand).
    /// See `150-clock-position-sync.md`.
    pub(crate) clock_tick: Arc<AtomicI32>,
    /// `time::monotonic_nanos` value at which `clock_tick` last advanced.
    /// Written by the `"clock"` thread, read by the MIDI-input callback to
    /// interpolate a fractional tick for a note arriving between clock firings
    /// (the counter itself only steps on the ~1 ms firing). See
    /// `MidiInputForwarder` and `interpolate_input_tick`.
    pub(crate) clock_tick_instant_nanos: Arc<AtomicU64>,
    /// The transport odometer: musical time *played*, in ticks. Credited by the
    /// `"clock"` thread on the same firings as `clock_tick`, but only while
    /// `running`, and — unlike `clock_tick` — **never** repositioned by
    /// [`ClockCommand::AlignToPlayback`](crate::core::clock::ClockCommand).
    ///
    /// That is the whole point of it. `clock_tick` is a *position*, so a seek
    /// must move it; live recording measures a *duration* by subtraction, so a
    /// seek must not. Sharing one counter for both corrupted a take from the
    /// seek onward. Read only by live recording. See
    /// `150-clock-position-sync.md`.
    pub(crate) elapsed_ticks: Arc<AtomicI32>,
    /// Playback position, in ticks. Advanced by the `"sequencer"` thread's tick
    /// pump and repositioned on a seek / loop wrap.
    pub(crate) playback_tick: Arc<AtomicI32>,
    /// The edit cursor / play-from point, in ticks. Moved by navigation keys
    /// and clicks.
    pub(crate) cursor_tick: Arc<AtomicI32>,
    /// Loop-region bounds, in ticks. This pair *is* the source of truth for
    /// [`Region::from_shared`](crate::models::region::Region::from_shared) — the
    /// `Transport`'s `Region` reads and writes straight through them.
    pub(crate) region_start: Arc<AtomicI32>,
    /// See [`region_start`](Self::region_start).
    pub(crate) region_end: Arc<AtomicI32>,
    /// Whether playback wraps at [`region_end`](Self::region_end). Toggled from
    /// the UI; also read by the tick pump.
    pub(crate) loop_enabled: Arc<AtomicBool>,
    /// Silences the built-in metronome click. Flipped by the UI toggle, read
    /// by [`Metronome`](crate::core::metronome::Metronome).
    pub(crate) metronome_mute: Arc<AtomicBool>,
    /// The active screen, as a [`ViewState`](crate::core::view_state::ViewState)
    /// discriminant. Written by `EventHandlers::set_view_state`.
    pub(crate) view_state: Arc<AtomicU8>,
    /// Whether the open clip has any selected events — a mirror of the
    /// `Sequencer`'s `Clip::event_selection`, which lives on the
    /// `"sequencer"` thread, for the UI thread's key routing
    /// (`input_handler.rs`'s `ClipContext`, `Display::forward_input_event`).
    /// Written by `EventHandlers::publish_event_selection` wherever the
    /// selection changes. Replaced the old `ViewState::ClipEdit`, which
    /// carried the same bit as a fake view.
    pub(crate) has_event_selection: Arc<AtomicBool>,
    /// MIDI channel (0–15) of the armed track.
    pub(crate) arm_channel: Arc<AtomicU8>,
    /// True while the arranger performance lane is armed (selected). Shared
    /// with `MidiInputForwarder` (a different thread than `Sequencer`) so
    /// it can suppress the MIDI-OUT thru-forward for trigger keys — the
    /// physical keyboard drives the transport silently during performance,
    /// it doesn't also play a musical note.
    pub(crate) performance_lane_armed: Arc<AtomicBool>,
    /// The instrument track the live keyboard is currently routed to, as its
    /// engine slot (`Track::slot`; `-1` = none). Written by `Display` (macOS plugin host,
    /// `set_live_instrument_target`) whenever the selected/plugin-loaded track
    /// changes; read by `MidiInputForwarder` to tag live-thru MIDI with the
    /// right track before sending it on `instrument_midi_tx`. See
    /// `130-plugin-host.md`.
    pub(crate) live_instrument_target: Arc<AtomicI32>,
    /// Delay applied to clip MIDI leaving the output port, in milliseconds.
    /// Written by the sequencer thread when the MIDI settings modal is
    /// confirmed, read by the `"midiout"` thread as each clip message is
    /// queued — so a change takes effect on the next note without restarting
    /// anything. See `160-midi-out-offset.md`.
    pub(crate) midi_out_offset_ms: Arc<AtomicI32>,
    /// Per-track volume / stereo balance / mute / solo — see [`TrackMixAtomics`].
    pub(crate) track_mix: Arc<TrackMixAtomics>,
    /// Live-recording thumbnail + last-note state — see [`LiveRecState`].
    pub(crate) live_rec_state: LiveRecState,
}
