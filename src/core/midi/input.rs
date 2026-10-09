//! [`MidiInputForwarder`] — owns the live `midir` input connection and fans
//! each incoming message out to live MIDI-thru and the instrument-plugin tap,
//! its note edges and wheel moves to the sequencer (for capture / recording),
//! and its note edges to the `NoteLogger`.
//!
//! Every message is stamped with two tick coordinates ([`InputTicks`]): a
//! `position` in `clock_tick` space (which a seek repositions) for running
//! capture, and an `elapsed` odometer value (which a seek never moves) for live
//! recording. Both carry a sub-tick interpolation so a note landing between
//! clock firings isn't quantized to the last one — see `precise_input_tick` and
//! `150-clock-position-sync.md`.

use crossbeam_channel::Sender;
use midir::{MidiInput, MidiInputConnection};
use rtrb::Producer;
use std::{
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering},
    },
};

use midir::Ignore;
#[cfg(unix)]
use midir::os::unix::VirtualInput;

#[cfg(unix)]
use crate::core::midi::port::VIRTUAL_PORT_NAME;
use crate::core::{
    midi::{
        input_tick::precise_input_tick,
        message::{Midi3, is_recorded, midi3, parse_note, rewrite_channel},
        out_queue::MidiOutMessage,
        port::pickable_port_names,
    },
    note_logger::NoteLoggerCommand,
};

/// Shared handle to the live-instrument `rtrb` producer — see the field doc on
/// `MidiInputForwarder::instrument_midi_tx` for why it needs `Arc<Mutex<_>>`.
type SharedInstrumentMidiProducer = Arc<Mutex<Producer<(usize, Midi3)>>>;

/// The two tick coordinates a live-input message needs, because its two
/// consumers want different things from a seek.
///
/// `position` is `clock_tick` space — where in the arrangement the note landed,
/// held in region phase with playback and therefore repositioned by
/// [`ClockCommand::AlignToPlayback`](crate::core::clock::ClockCommand). Running
/// capture works in it.
///
/// `elapsed` is odometer space — how much musical time had been *played* when
/// the note landed, never repositioned. Live recording places every note by
/// subtracting the take's start value from it, so a reposition here would move
/// notes already recorded relative to the ones after it.
///
/// Both carry the same sub-tick interpolation; see [`precise_input_tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputTicks {
    /// `clock_tick`-space position — where in the arrangement the note landed.
    /// A seek repositions it.
    pub(crate) position: i32,
    /// Odometer-space value — how much musical time had been played. Never
    /// repositioned; live recording measures from it.
    pub(crate) elapsed: i32,
}

/// Owns the live `midir` input connection and fans each message out to its
/// several consumers. See the module docs.
pub(crate) struct MidiInputForwarder {
    // Communication channels
    /// Raw bytes + [`InputTicks`] to the `"sequencer"` thread — only what a
    /// take records ([`is_recorded`]).
    midi_in_tx: Sender<(Vec<u8>, InputTicks)>,
    /// MIDI-thru to the `"midiout"` thread.
    midi_out_tx: Sender<MidiOutMessage>,
    /// Note-on records to the `NoteLogger`.
    note_logger_command_tx: Sender<NoteLoggerCommand>,
    /// Live-input tap for the hosted instrument plugins (macOS), tagged with
    /// `live_instrument_target`. A realtime-safe `rtrb` SPSC ring producer —
    /// wrapped in `Arc<Mutex<_>>` (unlike a `crossbeam_channel::Sender`, an
    /// `rtrb::Producer` isn't `Clone`) purely so each reconnect in
    /// `forward_messages_to_named` can move a fresh handle into its callback
    /// closure. The lock is only ever taken on this MIDI-input thread — never
    /// on the realtime audio thread that pops the other end. The consumer is
    /// dropped on other platforms, so pushes there just fill the ring once
    /// and fail from then on.
    instrument_midi_tx: SharedInstrumentMidiProducer,

    // State
    /// The free-running musical tick counter, read to stamp `position`.
    clock_tick: Arc<AtomicI32>,
    /// `time::monotonic_nanos` at which `clock_tick` last advanced — read
    /// alongside `clock_tick` to interpolate a fractional tick for a note that
    /// arrives between clock firings. See [`precise_input_tick`] and the
    /// `input_tick` submodule.
    clock_tick_instant_nanos: Arc<AtomicU64>,
    /// The transport odometer, stamped alongside `clock_tick` so live
    /// recording gets a base a seek can't move. See [`InputTicks`].
    elapsed_ticks: Arc<AtomicI32>,
    /// Tempo (µs per quarter), for the ns→tick conversion in the same interpolation.
    tempo: Arc<AtomicI32>,
    /// MIDI channel (0–15) of the armed track.
    arm_channel: Arc<AtomicU8>,
    /// True while the arranger performance lane is armed — suppresses the
    /// MIDI-OUT thru-forward so trigger keys stay silent during performance.
    performance_lane_armed: Arc<AtomicBool>,
    /// The instrument track the live keyboard is routed to (`-1` = none),
    /// written by `Display`'s macOS plugin host. Used to tag `instrument_midi_tx`
    /// sends with the right track.
    live_instrument_target: Arc<AtomicI32>,
}

impl MidiInputForwarder {
    /// Wires the forwarder to its output channels and the shared clock /
    /// tempo / arm atomics.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        midi_in_tx: Sender<(Vec<u8>, InputTicks)>,
        midi_out_tx: Sender<MidiOutMessage>,
        note_logger_command_tx: Sender<NoteLoggerCommand>,
        instrument_midi_tx: Producer<(usize, Midi3)>,
        clock_tick: Arc<AtomicI32>,
        clock_tick_instant_nanos: Arc<AtomicU64>,
        elapsed_ticks: Arc<AtomicI32>,
        tempo: Arc<AtomicI32>,
        arm_channel: Arc<AtomicU8>,
        performance_lane_armed: Arc<AtomicBool>,
        live_instrument_target: Arc<AtomicI32>,
    ) -> Self {
        MidiInputForwarder {
            midi_in_tx,
            midi_out_tx,
            note_logger_command_tx,
            instrument_midi_tx: Arc::new(Mutex::new(instrument_midi_tx)),
            clock_tick,
            clock_tick_instant_nanos,
            elapsed_ticks,
            tempo,
            arm_channel,
            performance_lane_armed,
            live_instrument_target,
        }
    }

    /// Names of the available MIDI input ports, minus our own virtual endpoints,
    /// with the virtual port first on Unix. For the settings modal.
    pub(crate) fn in_port_names() -> Vec<String> {
        let Ok(midi_in) = MidiInput::new("MIDI Input") else {
            return Vec::new();
        };
        pickable_port_names(
            midi_in
                .ports()
                .iter()
                .filter_map(|p| midi_in.port_name(p).ok()),
        )
    }

    /// Opens the input connection on the named port and starts forwarding —
    /// the reconnect path from the settings modal. Returns the live connection
    /// (dropping it closes the port).
    pub(crate) fn forward_messages_to_named(
        &self,
        port_name: &str,
    ) -> Result<MidiInputConnection<()>, Box<dyn Error>> {
        let mut midi_in = MidiInput::new("MIDI Input").unwrap();
        midi_in.ignore(Ignore::None);

        let midi_in_tx = self.midi_in_tx.clone();
        let midi_out_tx = self.midi_out_tx.clone();
        let note_logger_command_tx = self.note_logger_command_tx.clone();
        let instrument_midi_tx = self.instrument_midi_tx.clone();
        let clock_tick = self.clock_tick.clone();
        let clock_tick_instant_nanos = self.clock_tick_instant_nanos.clone();
        let elapsed_ticks = self.elapsed_ticks.clone();
        let tempo = self.tempo.clone();
        let arm_channel = self.arm_channel.clone();
        let performance_lane_armed = self.performance_lane_armed.clone();
        let live_instrument_target = self.live_instrument_target.clone();

        // Channel each currently-held live note was routed out on, indexed by
        // note number. A note-off must be sent on the same channel its note-on
        // used, otherwise switching the armed track mid-hold rewrites the
        // note-off to the new track's channel and strands the note on the old
        // one. Notes played by clips are unaffected — they never pass through
        // here.
        let mut held_note_channels: [Option<u8>; 128] = [None; 128];
        // Same idea for the plugin tap: which instrument track each held
        // note's note-on was tagged for. See `route_held`.
        let mut held_live_targets: [Option<usize>; 128] = [None; 128];
        // Smallest `(callback monotonic time − midir stamp)` seen so far — the
        // running estimate of the fixed offset between midir's per-connection
        // µs epoch and `time::monotonic_nanos`. Converges to the true offset as
        // soon as one message is delivered with near-zero dispatch latency, and
        // then recovers each later message's real arrival time even if a burst
        // is processed late. `i64::MAX` = not yet calibrated.
        let mut min_ts_offset_ns: i64 = i64::MAX;

        let callback = move |midir_ts_us: u64, message: &[u8], _: &mut ()| {
            // Capture timing first, before any of the routing work below.
            let recorded_ticks = precise_input_tick(
                &clock_tick,
                &clock_tick_instant_nanos,
                &elapsed_ticks,
                &tempo,
                midir_ts_us,
                &mut min_ts_offset_ns,
            );
            let arm_channel = arm_channel.load(Ordering::Relaxed);
            let mut msg = message.to_vec();

            let channel = route_channel(&msg, arm_channel, &mut held_note_channels);
            rewrite_channel(&mut msg, channel);

            // Performance-lane triggers drive the transport silently — the
            // physical keyboard isn't playing an instrument right now, so
            // suppress every audible route (MIDI-OUT thru *and* the hosted
            // instrument tap) while armed. `route_held` /
            // `route_channel` are still fed the message so a note held across
            // arming is tracked and its later note-off is routed correctly.
            let armed = performance_lane_armed.load(Ordering::Relaxed);

            // Tap channel-voice messages for the hosted instrument, tagged
            // with whichever track's plugin the live keyboard is currently
            // routed to (a held note's off goes back to the track its on used
            // — see `route_held`). Independent of MIDI-OUT thru, but
            // gated by the performance lane like it.
            let live_target = live_instrument_target.load(Ordering::Relaxed);
            let live_target = (live_target >= 0).then_some(live_target as usize);
            if matches!(msg.first().copied().map(|s| s & 0xF0), Some(0x80..=0xE0))
                && let Some(target) = route_held(&msg, live_target, &mut held_live_targets)
                && !armed
                && let Ok(mut producer) = instrument_midi_tx.lock()
            {
                producer.push((target, midi3(&msg))).ok();
            }

            if !armed {
                midi_out_tx.send(MidiOutMessage::now(msg.clone())).ok();
            }

            // Note edges and the two wheels go on to the sequencer (capture
            // / recording, `is_recorded`); only note edges to the note logger.
            if parse_note(&msg).is_some() {
                note_logger_command_tx
                    .send(NoteLoggerCommand::LogInputEvent(midi3(&msg)))
                    .ok();
            }
            if is_recorded(&msg) {
                midi_in_tx.send((msg, recorded_ticks)).ok();
            }
        };

        #[cfg(unix)]
        if port_name == VIRTUAL_PORT_NAME {
            dprintln!("Creating virtual input port: {}", port_name);
            let conn = midi_in.create_virtual(port_name, callback, ())?;
            dprintln!("Created virtual input port: {}", port_name);
            return Ok(conn);
        }

        let midi_in_ports = midi_in.ports();
        let in_port = midi_in_ports
            .iter()
            .find(|p| midi_in.port_name(p).ok().as_deref() == Some(port_name))
            .ok_or_else(|| format!("error: MIDI input port '{}' not found", port_name))?;

        dprintln!("Using input port: {}", port_name);
        let midi_in_connection = midi_in.connect(in_port, "midi-in", callback, ())?;
        dprintln!("Connected to input port: {}", port_name);
        Ok(midi_in_connection)
    }
}

/// Routes a live-input message by the note it belongs to. A note-on (and any
/// non-note message) goes to `current` — the armed track's channel, or the
/// instrument track the live keyboard is tapped for. A note-off is routed back
/// to whatever its matching note-on went to, so switching the armed track while
/// the note is held cannot strand it; an unmatched note-off gets `None`. `held`
/// tracks the destination per held note number and is updated in place.
fn route_held<T: Copy>(msg: &[u8], current: Option<T>, held: &mut [Option<T>; 128]) -> Option<T> {
    match parse_note(msg) {
        Some((note, false)) => held[note].take(),
        Some((note, true)) => {
            held[note] = current;
            current
        }
        None => current,
    }
}

/// The outbound MIDI channel for a live-input message: [`route_held`] on the
/// armed channel, with an unmatched note-off falling back to `arm_channel`.
fn route_channel(msg: &[u8], arm_channel: u8, held: &mut [Option<u8>; 128]) -> u8 {
    route_held(msg, Some(arm_channel), held).unwrap_or(arm_channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: u8 = 60;

    fn note_on(ch: u8) -> [u8; 3] {
        [0x90 | ch, NOTE, 100]
    }

    fn note_off(ch: u8) -> [u8; 3] {
        [0x80 | ch, NOTE, 0]
    }

    #[test]
    fn note_off_follows_note_on_channel_across_track_switch() {
        let mut held = [None; 128];

        // Note-on while track 1 (channel 0) is armed.
        assert_eq!(route_channel(&note_on(0), 0, &mut held), 0);

        // User switches to track 2 (channel 5) while still holding the note.
        // The note-off must still go out on channel 0.
        assert_eq!(route_channel(&note_off(5), 5, &mut held), 0);

        // Nothing left held.
        assert!(held.iter().all(Option::is_none));
    }

    #[test]
    fn velocity_zero_note_on_is_treated_as_note_off() {
        let mut held = [None; 128];
        route_channel(&note_on(2), 2, &mut held);
        let running_status_off = [0x90 | 7, NOTE, 0];
        assert_eq!(route_channel(&running_status_off, 7, &mut held), 2);
    }

    #[test]
    fn unmatched_note_off_falls_back_to_arm_channel() {
        let mut held = [None; 128];
        assert_eq!(route_channel(&note_off(4), 4, &mut held), 4);
    }

    #[test]
    fn non_note_messages_use_arm_channel() {
        let mut held = [None; 128];
        let cc = [0xB0 | 3, 74, 64];
        assert_eq!(route_channel(&cc, 9, &mut held), 9);
    }

    #[test]
    fn live_target_note_off_follows_note_on_track_across_track_switch() {
        let mut held = [None; 128];

        // Note-on while track 2 (index 1) is armed and hosts a plugin.
        assert_eq!(route_held(&note_on(0), Some(1), &mut held), Some(1));

        // User switches to track 4 (index 3) while still holding the note.
        // The note-off must still go to track 1's plugin, not track 3's.
        assert_eq!(route_held(&note_off(0), Some(3), &mut held), Some(1));

        // Nothing left held.
        assert!(held.iter().all(Option::is_none));
    }

    #[test]
    fn live_target_velocity_zero_note_on_is_treated_as_note_off() {
        let mut held = [None; 128];
        route_held(&note_on(0), Some(2), &mut held);
        let running_status_off = [0x90, NOTE, 0];
        assert_eq!(route_held(&running_status_off, Some(7), &mut held), Some(2));
    }

    #[test]
    fn live_target_unmatched_note_off_is_dropped_not_sent_to_current_target() {
        let mut held = [None; 128];
        assert_eq!(route_held(&note_off(0), Some(4), &mut held), None);
    }

    #[test]
    fn live_target_note_on_while_unarmed_is_not_stranded_by_a_later_arm() {
        let mut held = [None; 128];
        // Held while no instrument track is armed — nothing to send.
        assert_eq!(route_held(&note_on(0), None, &mut held), None);
        // Instrument armed before the key is released — the note-off must
        // still not be sent anywhere, since no plugin ever got the note-on.
        assert_eq!(route_held(&note_off(0), Some(1), &mut held), None);
    }

    #[test]
    fn live_target_non_note_messages_use_current_target() {
        let mut held = [None; 128];
        let cc = [0xB0, 74, 64];
        assert_eq!(route_held(&cc, Some(5), &mut held), Some(5));
        assert_eq!(route_held(&cc, None, &mut held), None);
    }
}
