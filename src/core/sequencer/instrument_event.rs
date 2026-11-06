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

/// Who played a note. A plugin that keys its voices by note id can then hold
/// a clip's note and a live key of the same pitch side by side, each ended only
/// by its own off (`vst3::events::NoteIds`; the raw-MIDI CLAP route ignores
/// this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum NoteSource {
    /// Clip playback, with its chases and the note-offs of a stop or a seek,
    /// and anything else no one played (a load's reset, a wheel's rest).
    Clip,
    /// The player's keyboard.
    Live,
    /// A selected note's audition (`Sequencer::preview_notes`). Stays last:
    /// the VST3 note-id table is sized from it.
    Audition,
}

/// What a voice is told: a MIDI message, or a parameter move. `Copy`, so
/// nothing crossing the `rtrb` ring to the audio thread carries a heap
/// payload.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum VoiceMessage {
    /// A MIDI message, and who played it.
    Midi(Midi3, NoteSource),
    /// A plugin parameter's move (`400-parameter-knobs.md`).
    Param(ParamMove),
}

#[cfg(test)]
impl VoiceMessage {
    /// The MIDI message, if it is one.
    pub(crate) fn midi(self) -> Option<Midi3> {
        match self {
            VoiceMessage::Midi(bytes, _) => Some(bytes),
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
