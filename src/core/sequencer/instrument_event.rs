//! The clip event `Sequencer` hands to the macOS CLAP host, and its scheduling
//! stamp. Kept here (cross-platform) rather than in `core::clap_host` because
//! the producer side — `Sequencer::tick` / `chase_notes` /
//! `release_instrument_notes` — is cross-platform and the `rtrb` ring is built
//! unconditionally in `setup.rs`; only the consumer is macOS-only.

use std::time::Instant;

use crate::core::midi::message::Midi3;
use crate::models::event::ParamMove;

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

/// What a voice is told: a MIDI message, or a parameter move. `Copy`, so
/// nothing crossing the `rtrb` ring to the audio thread carries a heap
/// payload.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum VoiceMessage {
    /// A MIDI message.
    Midi(Midi3),
    /// A plugin parameter's move (`400-parameter-knobs.md`).
    Param(ParamMove),
}

#[cfg(test)]
impl VoiceMessage {
    /// The MIDI message, if it is one.
    pub(crate) fn midi(self) -> Option<Midi3> {
        match self {
            VoiceMessage::Midi(bytes) => Some(bytes),
            VoiceMessage::Param(_) => None,
        }
    }
}

/// A `TrackOutput::Instrument` track's clip event on its way to the hosted
/// plugin: what it says, the target track, and when it should sound.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ClipInstrumentEvent {
    /// Target instrument track's engine slot ([`Track::slot`]) — the
    /// mixer's voice index, which a track keeps however tracks above it are
    /// added or removed.
    ///
    /// [`Track::slot`]: crate::models::track::Track::slot
    pub(crate) track: usize,
    /// What it says.
    pub(crate) message: VoiceMessage,
    /// When the plugin should play it.
    pub(crate) when: EventTime,
}
