//! One hosted CLAP instrument as seen by the audio thread: [`ClapVoice`] wraps
//! a single plugin's `Send` audio processor plus the reusable CLAP event and
//! audio-port buffers, and implements the host's format-agnostic
//! [`InstrumentVoice`] trait.
//!
//! The `!Send` plugin *instance* stays on the UI thread ([`ClapEditor`](super::ClapEditor));
//! only the processor comes here.
//! [`InstrumentMixer`](crate::core::plugin_host::mixer) owns a bank of voices
//! and is the sole caller of [`queue_midi`](InstrumentVoice::queue_midi) /
//! [`render_block`](InstrumentVoice::render_block) — once per engine sub-block
//! on the `"audio-engine"` callback thread. See `130-plugin-host.md`.

use clack_host::events::event_types::{MidiEvent, TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};

use crate::core::midi::message::Midi3;
use crate::core::plugin_host::buffers::{AudioIoLayout, PortBuffers, channel_total};
use crate::core::plugin_host::transport::BlockTransport;
use crate::core::plugin_host::voice::{EVENT_CAPACITY, InstrumentVoice, VoiceMix};
use crate::core::time::ticks_to_beats_f64;

use super::host::StevClapHost;

/// One hosted instrument: its audio processor plus the reusable CLAP event /
/// audio-port buffers. MIDI is pushed in by the mixer before each block.
pub(crate) struct ClapVoice {
    /// The plugin's audio processor.
    processor: PluginAudioProcessor<StevClapHost>,
    /// Reused per-block input event buffer. The mixer pushes events in with
    /// `queue_midi` and clears it with `clear_events` at the end of each block.
    in_events: EventBuffer,
    /// Reused CLAP input audio-port descriptors — one entry per input port the
    /// plugin declares, empty for a plain instrument.
    in_ports: AudioPorts,
    /// Reused CLAP output audio-port descriptors — one entry per output port.
    out_ports: AudioPorts,
    /// The silent input feed and the output audio. See [`PortBuffers`].
    bufs: PortBuffers,
    /// Whether the plugin's last `process()` call returned
    /// `ProcessStatus::Sleep` — CLAP's own "no more processing is required
    /// until the next event or variation in audio input" — plus the mixer's
    /// per-block render flag and gain ramp. See [`VoiceMix`].
    mix: VoiceMix,
}

impl ClapVoice {
    /// Wraps a stopped processor into a ready voice with pre-reserved buffers
    /// matching the plugin's declared audio-port layout.
    pub(super) fn new(
        processor: StoppedPluginAudioProcessor<StevClapHost>,
        max_frames: usize,
        io: &AudioIoLayout,
    ) -> Self {
        Self {
            processor: processor.into(),
            in_events: EventBuffer::with_capacity(EVENT_CAPACITY),
            in_ports: AudioPorts::with_capacity(channel_total(&io.inputs), io.inputs.len()),
            out_ports: AudioPorts::with_capacity(channel_total(&io.outputs), io.outputs.len()),
            bufs: PortBuffers::new(io, max_frames),
            mix: VoiceMix::default(),
        }
    }
}

impl InstrumentVoice for ClapVoice {
    /// Queues a MIDI message at sample `time` within the next processed block.
    /// Wakes the voice if it was sleeping — see the `sleeping` field doc.
    fn queue_midi(&mut self, bytes: Midi3, time: u32) {
        if let Some(event) = midi_bytes_to_clap_event(bytes, time) {
            self.in_events.push(&event);
            self.mix.sleeping = false;
        }
    }

    /// Runs one CLAP process block of `frames` samples, leaving each output
    /// port's audio in `bufs.outputs[port][channel][..frames]` (the mixer sums only
    /// port 0). Returns `false` if the plugin failed to
    /// process (the caller then treats this voice as silent for the block).
    /// A successful call updates `sleeping` from the plugin's returned
    /// `ProcessStatus` — the block just rendered is still valid output either
    /// way, `sleeping` only affects whether the *next* callback calls in.
    fn render_block(&mut self, frames: usize, steady: u64, transport: &BlockTransport) -> bool {
        let transport = transport_event(transport);
        self.bufs.prepare_outputs(frames);

        let started = match self.processor.ensure_processing_started() {
            Ok(started) => started,
            Err(_) => return false,
        };

        let input_events = InputEvents::from_buffer(&self.in_events);
        let mut output_events = OutputEvents::void();

        // The silent input channels stay at their full `max_frames` length; the
        // process call clamps to the shorter (output) frame count.
        let input_audio = self
            .in_ports
            .with_input_buffers(self.bufs.inputs.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(port.iter_mut().map(|channel| {
                    InputChannel {
                        buffer: channel.as_mut_slice(),
                        is_constant: true,
                    }
                })),
            }));

        let mut output_audio =
            self.out_ports
                .with_output_buffers(self.bufs.outputs.iter_mut().map(|port| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        port.iter_mut().map(|channel| channel.as_mut_slice()),
                    ),
                }));

        let status = started.process(
            &input_audio,
            &mut output_audio,
            &input_events,
            &mut output_events,
            Some(steady),
            Some(&transport),
        );

        match status {
            Ok(status) => {
                self.mix.sleeping = status == ProcessStatus::Sleep;
                true
            }
            Err(_) => false,
        }
    }

    /// Clears the per-block input event buffer once the mixer has consumed the
    /// events it queued. Called at the end of every `render_into`.
    fn clear_events(&mut self) {
        self.in_events.clear();
    }

    /// Reads the main output port's `channel` at `frame` — the audio the last
    /// `render_block` left for this voice. Only valid when
    /// [`VoiceMix::rendered`] is set. A mono main port feeds both mix
    /// channels; a plugin that somehow reported an empty main port is silent.
    fn sample(&self, channel: usize, frame: usize) -> f32 {
        self.bufs.main_sample(channel, frame)
    }

    /// Wakes the voice when the plugin called `request_process` — e.g. to
    /// apply a knob turned in its own editor while it was asleep.
    fn wake_on_request(&mut self, _frames: usize) {
        if self
            .processor
            .access_shared_handler(|shared| shared.take_process_request())
        {
            self.mix.sleeping = false;
        }
    }

    /// The mixer-owned render flags and gain ramp for this voice.
    fn mix(&self) -> &VoiceMix {
        &self.mix
    }

    /// Mutable [`mix`](Self::mix), for the mixer's two passes.
    fn mix_mut(&mut self) -> &mut VoiceMix {
        &mut self.mix
    }
}

/// Builds the CLAP transport info for a block — tempo, playhead (tick-granular:
/// `playback_tick` only moves once per sequencer tick), loop bounds and the
/// project's meter.
fn transport_event(t: &BlockTransport) -> TransportEvent {
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    if t.running {
        flags |= TransportFlags::IS_PLAYING;
    }
    if t.looping {
        flags |= TransportFlags::IS_LOOP_ACTIVE;
    }

    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(ticks_to_beats_f64(t.playback_tick)),
        song_pos_seconds: SecondsTime::default(),
        tempo: t.bpm(),
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_float(ticks_to_beats_f64(t.region_start)),
        loop_end_beats: BeatTime::from_float(ticks_to_beats_f64(t.region_end)),
        loop_start_seconds: SecondsTime::default(),
        loop_end_seconds: SecondsTime::default(),
        bar_start: BeatTime::from_float(t.bar_start_beats()),
        bar_number: t.bar_number(),
        time_signature_numerator: t.meter.numerator().into(),
        time_signature_denominator: t.meter.denominator().into(),
    }
}

/// Translates a MIDI 1.0 message into a CLAP `MidiEvent` at sample `time`
/// within the block.
///
/// Only channel-voice messages (`0x80..=0xEF`) are forwarded; System Common and
/// System Realtime bytes (clock, active sensing, SysEx, …) are dropped. The
/// payload is already the 3-byte CLAP form (shorter messages zero-padded by
/// [`midi3`](crate::core::midi::message::midi3)).
fn midi_bytes_to_clap_event(bytes: Midi3, time: u32) -> Option<MidiEvent> {
    if !(0x80..=0xEF).contains(&bytes[0]) {
        return None;
    }
    Some(MidiEvent::new(time, 0, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plugin_host::voice::note_reset_messages;
    use crate::core::time::{Meter, PPQN};

    fn transport(running: bool, looping: bool, tick: i32) -> BlockTransport {
        BlockTransport {
            running,
            looping,
            tempo_us: 500_000,
            meter: Meter::FOUR_FOUR,
            playback_tick: tick,
            region_start: PPQN * 4,
            region_end: PPQN * 8,
        }
    }

    #[test]
    fn transport_event_carries_tempo_position_and_loop_bounds() {
        // Two bars in at 4/4, 120 BPM.
        let ev = transport_event(&transport(true, true, PPQN * 8));
        assert!((ev.tempo - 120.0).abs() < 1e-9);
        assert_eq!(ev.bar_number, 2);
        assert!((ev.song_pos_beats.to_float() - 8.0).abs() < 1e-9);
        assert!((ev.bar_start.to_float() - 8.0).abs() < 1e-9);
        assert!((ev.loop_start_beats.to_float() - 4.0).abs() < 1e-9);
        assert!((ev.loop_end_beats.to_float() - 8.0).abs() < 1e-9);
        assert_eq!(ev.time_signature_numerator, 4);
        assert_eq!(ev.time_signature_denominator, 4);
    }

    #[test]
    fn transport_event_carries_the_project_meter() {
        // Two bars of 6/8 are six quarters.
        let t = BlockTransport {
            meter: Meter::new(6, 8).unwrap(),
            ..transport(true, false, PPQN * 6 + 1)
        };
        let ev = transport_event(&t);
        assert_eq!(ev.time_signature_numerator, 6);
        assert_eq!(ev.time_signature_denominator, 8);
        assert_eq!(ev.bar_number, 2);
        assert!((ev.bar_start.to_float() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn transport_event_flags_follow_running_and_looping() {
        let rolling = transport_event(&transport(true, true, 0));
        assert!(rolling.flags.contains(TransportFlags::IS_PLAYING));
        assert!(rolling.flags.contains(TransportFlags::IS_LOOP_ACTIVE));

        let stopped = transport_event(&transport(false, false, 0));
        assert!(!stopped.flags.contains(TransportFlags::IS_PLAYING));
        assert!(!stopped.flags.contains(TransportFlags::IS_LOOP_ACTIVE));
        // The always-on capability flags are independent of transport state.
        assert!(stopped.flags.contains(TransportFlags::HAS_TEMPO));
        assert!(stopped.flags.contains(TransportFlags::HAS_BEATS_TIMELINE));
        assert!(stopped.flags.contains(TransportFlags::HAS_TIME_SIGNATURE));
    }

    #[test]
    fn translates_note_on_and_note_off() {
        assert_eq!(
            midi_bytes_to_clap_event([0x90, 60, 100], 0).unwrap().data(),
            [0x90, 60, 100]
        );
        assert_eq!(
            midi_bytes_to_clap_event([0x80, 60, 0], 0).unwrap().data(),
            [0x80, 60, 0]
        );
    }

    #[test]
    fn carries_the_sample_offset_into_the_event_time() {
        let ev = midi_bytes_to_clap_event([0x90, 60, 100], 128).unwrap();
        assert_eq!(ev.time(), 128);
    }

    #[test]
    fn keeps_control_change_and_pitch_bend() {
        assert!(midi_bytes_to_clap_event([0xB0, 74, 64], 0).is_some());
        assert!(midi_bytes_to_clap_event([0xE5, 0, 64], 0).is_some());
    }

    #[test]
    fn accepts_the_zero_padded_short_message() {
        // Channel pressure is 2 bytes; `midi3` padded it to `[0xD0, 90, 0]`.
        assert_eq!(
            midi_bytes_to_clap_event([0xD0, 90, 0], 0).unwrap().data(),
            [0xD0, 90, 0]
        );
    }

    #[test]
    fn the_note_reset_burst_survives_translation_to_clap_events() {
        assert!(note_reset_messages().all(|m| midi_bytes_to_clap_event(m, 0).is_some()));
    }

    #[test]
    fn drops_system_and_realtime_messages() {
        assert!(midi_bytes_to_clap_event([0xF8, 0, 0], 0).is_none()); // timing clock
        assert!(midi_bytes_to_clap_event([0xFE, 0, 0], 0).is_none()); // active sensing
        assert!(midi_bytes_to_clap_event([0xF0, 0x7E, 0x00], 0).is_none()); // sysex
    }
}
