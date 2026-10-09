//! Compile-time constants: musical defaults, capture-window sizing, quantize
//! tuning, the fixed track count, and the realtime ring capacities.
//!
//! Where a value encodes a judgement call rather than a fact, its doc says what
//! the value trades off — follow `core/audio/mix.rs` for that style. Timing
//! constants that are a position vs. an amount follow `080-conventions.md`'s
//! `tick`/`ticks` rule.

use crate::core::time::{bars_to_ticks, bpm_to_tempo_us, sixteenth_straight_ticks};

/// Tempo a brand-new project starts at, in BPM. `TEMPO_*` are the same value
/// expressed as microseconds per quarter note, which is what the clock and the
/// persisted DTO actually carry.
pub const BPM_DEFAULT: i32 = 90;
/// [`BPM_DEFAULT`] as microseconds per quarter note — the unit the `"clock"`
/// thread and `ProjectData` use.
pub const TEMPO_US_DEFAULT: i32 = bpm_to_tempo_us(BPM_DEFAULT);
/// Loop-region length a fresh project / clip starts with, in ticks (an amount).
/// Two bars.
pub const REGION_LENGTH_DEFAULT: i32 = bars_to_ticks(2);
/// First-clip tempo detection octave-corrects an implausible result: a tempo
/// detected at or slower than this (µs-per-quarter, ~50 BPM) is reinterpreted
/// as an octave up — tempo halved-in-µs (BPM doubled) and the clip's bar count
/// doubled. `_FAST` is the mirror (~140 BPM): octave down. This is the
/// automatic form of `⌥=` / `⌥-` in the clip view; the clip keeps its wall-clock
/// timing, only the metric reading changes. See `040-phrase-detection.md`.
pub const FIRST_CLIP_TEMPO_US_SLOW: i32 = bpm_to_tempo_us(50);
/// Fast-side octave-correction threshold — see [`FIRST_CLIP_TEMPO_US_SLOW`].
/// A first-clip tempo at or faster than this (~140 BPM) is reinterpreted an
/// octave down.
pub const FIRST_CLIP_TEMPO_US_FAST: i32 = bpm_to_tempo_us(140);
/// Quantize catch radius, in ticks (an amount): an event within this distance
/// of a grid point is pulled toward it, one further away is left alone. See
/// `070-quantization.md`.
pub const QUANTIZE_TOLERANCE_TICKS: i32 = 30;
/// How far a quantized event moves toward its grid point: `1.0` snaps exactly,
/// `0.0` does nothing. Below 1.0 so quantize tightens timing without erasing
/// the performance's feel.
pub const QUANTIZE_LERP_FACTOR: f32 = 0.8;
/// When cropping a fresh capture: a note landing within this many ticks of the
/// region *end* was played slightly ahead of the downbeat, so it is relocated
/// to the region start rather than lost to the crop. One straight 16th. An
/// amount. See `relocate_late_notes_to_region_start` and `100-running-capture.md`.
pub const LATE_NOTE_TOLERANCE_TICKS: i32 = sixteenth_straight_ticks();
/// The arranger's *default* zoom, in bars across the content width: the scale
/// latched on the first arranger frame, before the user zooms. After that the
/// scale is a fixed pixels-per-beat (a wider window shows more bars rather
/// than stretching them). See `030-ui-design.md`, `archive/190-arranger-zoom.md`.
pub const BARS_IN_VIEWPORT: i32 = 32;
/// Hard floor on the arranger scale, in pixels per beat — ≈ 160 bars across a
/// 1300px content width. The zoom-out limit is normally content-relative
/// (whole arrangement + [`ARRANGER_ZOOM_OUT_HEADROOM`], never less than the
/// default `BARS_IN_VIEWPORT`); this only binds for very long arrangements,
/// and keeps the grid from ever facing absurd scales.
pub const ARRANGER_MIN_PX_PER_BEAT: f32 = 2.0;
/// How far past the arrangement's end a full zoom-out reaches, as a factor of
/// its length: 1.25 leaves a quarter of empty timeline after the last
/// material — room to see where the song ends without drowning it in nothing.
pub const ARRANGER_ZOOM_OUT_HEADROOM: f32 = 1.25;
/// Most zoomed-in horizontal scale, in pixels per beat, in the arranger and
/// the clip views alike: four pixels per tick (`PPQN` = 960). Set by `Z`: a
/// one-beat selection must still fill the width (less `ZOOM_FIT_MARGIN` a
/// side) on large displays — this covers content widths up to ≈ 4200pt.
/// History: 160 (≈ 2 bars across 1300px) capped `Z` for anything under ~2
/// bars; 960 (1 px/tick) still capped 1–2 beat selections on a ≈ 2100pt+
/// content width, so their margins grew as the selection shrank. Not much
/// deeper: `scroll_x` is `f32`, and at 4 px/tick even a 200-bar arrangement
/// keeps sub-pixel scroll precision.
pub const MAX_PX_PER_BEAT: f32 = 3840.0;
/// Screen margin `Z` (zoom to fit, arranger and clip views) leaves on each
/// side of what it frames, as a fraction of the content width — so every
/// selection, whatever its length, lands on the same span of screen (here
/// the middle 92%), with the range's edges clear of the content edges. A
/// fixed *time* margin can't do that: one beat a side was ~17% of the width
/// around a 1-bar selection and <1% around a 32-bar one. 4% a side.
pub const ZOOM_FIT_MARGIN: f32 = 0.04;
/// Multiplicative zoom step per `+`/`-` press, in the arranger and the clip
/// views. 1.25 takes ~3 presses to double, fine enough to frame precisely
/// without feeling sluggish.
pub const ZOOM_KEY_STEP: f32 = 1.25;
/// Maximum amount of live capture history a stopped `/` frames from. Older
/// material is discarded from the detection source so long recording
/// sessions cannot influence phrase-token detection.
pub const CAPTURE_BUFFER_BARS: i32 = 8;
/// How many bars at the end of the capture phrase-token detection analyses.
pub const PHRASE_DETECTION_WINDOW_BARS: i32 = 4;
/// Minimum span (in bars) the last detected phrase token must cover before a
/// refinement pass is allowed to split it once more using an internal
/// prominent-gap heuristic.
pub const PHRASE_LAST_TOKEN_REFINE_MIN_BARS: i32 = 2;
/// How long an audible note preview sounds when a selected note's pitch is
/// nudged in `Clip`, in milliseconds — the NoteOff follows after this delay
/// (`Sequencer::preview_notes`).
pub const PITCH_PREVIEW_MS: u64 = 120;
/// Most notes one event-marquee update auditions: the notes that just joined
/// the selection, earliest first, one per pitch (`Clip::audition_note_ons`).
/// Keeps a sweep over a dense passage from sounding as one loud cluster.
pub const MARQUEE_AUDITION_MAX_NOTES: usize = 4;
/// Most arrangement tracks a project can hold. The live count varies
/// (`Sequencer::tracks`), but every per-track array (`SharedAtomics`, the
/// instrument mixer, `InstrumentNotes`, the DSP load meter) is sized to this
/// capacity so the audio thread never allocates or changes shape. 16 = one per
/// MIDI channel, so every track can default to a channel of its own.
pub const MAX_TRACKS: usize = 16;
/// Tracks a new project starts with (`Sequencer::new`, `ProjectData::default`).
pub const DEFAULT_TRACK_COUNT: usize = 4;
/// Track accent colours each theme palette holds (`Track::color_slot`
/// indexes them). Equal to `MAX_TRACKS`, so a full project still gives every
/// track a colour of its own; a fork raising `MAX_TRACKS` past it gets
/// repeats rather than new palette entries to fill.
pub const TRACK_COLOR_COUNT: usize = 16;
/// Longest track name, in characters (`track_name_from_input`, the rename
/// field). Far past what the header column shows — the header cuts a long
/// name with an ellipsis — but keeps a pasted paragraph out of the project.
pub const TRACK_NAME_MAX_CHARS: usize = 40;
/// Longest project name, in characters (`project_name_from_input`, the Save
/// As field) — the file name the project is saved under.
pub const PROJECT_NAME_MAX_CHARS: usize = 64;
/// MIDI note number that maps to bar 1 on the arranger performance lane
/// (C3, matching this app's Cubase-style octave convention in
/// `piano_keys.rs`, where MIDI note 60 = C3). Note N maps to bar
/// `(N - PERFORMANCE_LANE_BASE_NOTE + 1)`; notes below this are ignored.
pub const PERFORMANCE_LANE_BASE_NOTE: u8 = 60;
/// MIDI port name the (currently unused) `start_controller_thread` would bind
/// to — the physical keypad's own port. Kept for the reference controller
/// implementation described in `archive/010-keypad.md`.
#[allow(dead_code)]
pub const CONTROLLER_DEVICE_NAME: &str = "Pico MIDI Controller";
/// Capacity of each `rtrb` SPSC ring feeding the macOS instrument mixer with
/// per-track [`Midi3`](crate::core::midi::message::Midi3) events (one ring for clip playback, one for
/// live keyboard input — see `130-plugin-host.md`). Sized with generous headroom
/// over a realistic worst-case burst (an all-tracks `chase_notes` on
/// transport start/seek); a full ring silently drops the event rather than
/// blocking the realtime audio thread.
pub const INSTRUMENT_MIDI_RING_CAPACITY: usize = 512;

/// Default delay applied to clip MIDI leaving the output port, in
/// milliseconds. The audio path already schedules the metronome click and every
/// instrument plugin one whole buffer into the future
/// (`SCHEDULE_DELAY_FRAMES`, ~5.3 ms at 48 kHz / 256), so an undelayed port
/// write reaches external gear that much *early*. This is the nearest whole
/// millisecond to that buffer. The audio device's own output latency and
/// whatever the external synth adds are both unknown to us, so this is only a
/// starting point — the user dials the rest in by ear against the click. See
/// `160-midi-out-offset.md`.
pub const MIDI_OUT_OFFSET_DEFAULT_MS: i32 = 5;
/// Upper bound for the MIDI-output offset. Nothing physical needs more, and a
/// runaway value would park notes in the output queue for an audible age.
pub const MIDI_OUT_OFFSET_MAX_MS: i32 = 200;
/// Step applied by one arrow-key press in the MIDI settings modal.
pub const MIDI_OUT_OFFSET_STEP_MS: i32 = 1;

/// Capacity of the `rtrb` SPSC ring carrying scheduled metronome clicks from
/// `Metronome` (sequencer thread) to the audio engine. Clicks are one per beat
/// and consumed every audio callback, so a handful of slots is ample; a full
/// ring drops the click rather than blocking.
pub const METRONOME_CLICK_RING_CAPACITY: usize = 16;
