//! The clip event `Sequencer` hands to the macOS CLAP host, and its scheduling
//! stamp. Kept here (cross-platform) rather than in `core::clap_host` because
//! the producer side — `Sequencer::tick` / `chase_notes` /
//! `release_instrument_notes` — is cross-platform and the `rtrb` ring is built
//! unconditionally in `setup.rs`; only the consumer is macOS-only.

use std::time::Instant;

use crate::core::midi::message::Midi3;

/// When a clip event should sound, relative to when the CLAP host receives it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EventTime {
    /// Play as soon as possible *without overtaking this track's queued
    /// events*: the head of the next rendered block, or after the track's
    /// latest pending event (`immediate_target_frame` in the mixer). For seeks
    /// and stop-time note-offs, which must follow a clip note-on already held
    /// a buffer ahead. Not for "sound now" (an audition): that is
    /// `At(Instant::now())` — `Immediate` would queue it behind the previous
    /// audition's pending note-off.
    Immediate,
    /// Play at the sample matching this [`Instant`] — a scheduled clip event,
    /// timestamped with the tick's intended time back in `Clock`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    At(Instant),
}

/// A `TrackOutput::Instrument` track's clip event on its way to the hosted
/// CLAP plugin: the MIDI bytes, the target track, and when it should sound.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ClipInstrumentEvent {
    /// Target instrument track's engine slot ([`Track::slot`]) — the
    /// mixer's voice index, which a track keeps however tracks above it are
    /// added or removed.
    ///
    /// [`Track::slot`]: crate::models::track::Track::slot
    pub(crate) track: usize,
    /// The MIDI message, as a fixed `[u8; 3]` so nothing crossing the `rtrb`
    /// ring to the audio thread carries a heap payload.
    pub(crate) message: Midi3,
    /// When the plugin should play it.
    pub(crate) when: EventTime,
}
