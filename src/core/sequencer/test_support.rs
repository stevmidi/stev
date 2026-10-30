//! Fixtures shared by the sequencer's unit tests.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, Ordering},
};
use std::time::Instant;

use crossbeam_channel::{Receiver, unbounded};
use rtrb::{Consumer, RingBuffer};

use crate::core::{
    config,
    input_event::TimeSelectionRect,
    midi::out_queue::MidiOutMessage,
    shared_atomics::{AudioExportAtomics, TrackMixAtomics},
    time::Meter,
};
use crate::models::{
    clip::Clip,
    event::Event,
    track::{InstrumentRef, Track, TrackOutput},
};

use super::{ClipInstrumentEvent, Sequencer};

/// A stopped sequencer with its default tracks and the default tempo (region
/// `[0, 0)`), plus both output ends so a test can read what playback sent: the
/// MIDI-out receiver and the consumer end of the plugin MIDI ring.
pub(super) fn sequencer_with_outputs(
    loop_enabled: bool,
) -> (
    Sequencer,
    Receiver<MidiOutMessage>,
    Consumer<ClipInstrumentEvent>,
) {
    let (midi_out_tx, midi_out_rx) = unbounded();
    let (plugin_midi_tx, plugin_midi_rx) = RingBuffer::new(1024);

    let sequencer = Sequencer::new(
        midi_out_tx,
        plugin_midi_tx,
        RingBuffer::new(8).0,
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicBool::new(loop_enabled)),
        Arc::new(AtomicBool::new(false)), // running
        Arc::new(AtomicI32::new(config::TEMPO_US_DEFAULT)),
        Arc::new(AtomicU16::new(Meter::FOUR_FOUR.to_bits())),
        Arc::new(AtomicU8::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(TrackMixAtomics::new()),
        Arc::new(AudioExportAtomics::default()),
    );
    (sequencer, midi_out_rx, plugin_midi_rx)
}

/// [`sequencer_with_outputs`] with the loop on over the default-length region
/// `[0, REGION_LENGTH_DEFAULT)` and the tempo at `tempo_us`.
pub(super) fn sequencer_at_tempo(
    tempo_us: i32,
) -> (
    Sequencer,
    Receiver<MidiOutMessage>,
    Consumer<ClipInstrumentEvent>,
) {
    let (sequencer, midi_out_rx, plugin_midi_rx) = sequencer_with_outputs(true);
    sequencer.set_tempo(tempo_us);
    sequencer
        .region_end
        .store(config::REGION_LENGTH_DEFAULT, Ordering::Relaxed);
    (sequencer, midi_out_rx, plugin_midi_rx)
}

/// [`sequencer_with_outputs`] without the MIDI-out receiver.
pub(super) fn sequencer_with(loop_enabled: bool) -> (Sequencer, Consumer<ClipInstrumentEvent>) {
    let (sequencer, _midi_out_rx, plugin_midi_rx) = sequencer_with_outputs(loop_enabled);
    (sequencer, plugin_midi_rx)
}

/// Every MIDI message playback has pushed to the plugin ring so far — the
/// consumer end [`sequencer_with`] hands back.
pub(super) fn drain(rx: &mut Consumer<ClipInstrumentEvent>) -> Vec<[u8; 3]> {
    std::iter::from_fn(|| rx.pop().ok())
        .map(|e| e.message)
        .collect()
}

/// [`sequencer_with`] with the loop enabled, without the ring.
pub(crate) fn test_sequencer() -> Sequencer {
    sequencer_with(true).0
}

/// The tracks' colour slots, in order.
pub(crate) fn track_colors(sequencer: &Sequencer) -> Vec<usize> {
    sequencer.tracks().iter().map(Track::color_slot).collect()
}

/// An empty clip spanning `[start_tick, start_tick + length)`.
pub(crate) fn clip_at(start_tick: i32, length: i32) -> Clip {
    let mut clip = Clip::new();
    clip.set_start_tick(start_tick);
    clip.region_mut().set_region(Some(0), Some(length));
    clip
}

/// An arranger marquee over ticks `[start, end)` × tracks
/// `track_start..=track_end`.
pub(crate) fn rect(
    start: i32,
    end: i32,
    track_start: usize,
    track_end: usize,
) -> TimeSelectionRect {
    TimeSelectionRect {
        start,
        end,
        track_start,
        track_end,
    }
}

/// A stub plugin output with `state` as its state blob.
pub(crate) fn stub_instrument(state: Vec<u8>) -> TrackOutput {
    TrackOutput::Instrument(InstrumentRef {
        bundle_path: "/x.clap".into(),
        plugin_id: "id".into(),
        display_name: "Synth".into(),
        state,
    })
}

/// Routes track 0 to a stub instrument, so its clip events go to the plugin
/// ring rather than MIDI out.
pub(super) fn instrument_track_0(sequencer: &mut Sequencer) {
    sequencer.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
}

/// A middle-C `NoteOn` (velocity 100) at `tick`, length unpaired.
pub(crate) fn note_on(tick: i32) -> Event {
    Event::new(tick, 0, vec![0x90, 60, 100])
}

/// The middle-C `NoteOff` matching [`note_on`], at `tick`.
pub(crate) fn note_off(tick: i32) -> Event {
    Event::new(tick, 0, vec![0x80, 60, 0])
}

/// A pitch-bend move at `tick`, `msb` its upper seven bits.
pub(crate) fn bend_at(tick: i32, msb: u8) -> Event {
    Event::new(tick, 0, vec![0xE0, 0x00, msb])
}

/// A mod-wheel move to `value` at `tick`.
pub(crate) fn mod_at(tick: i32, value: u8) -> Event {
    Event::new(tick, 0, vec![0xB0, 0x01, value])
}

/// The wheel moves among `events` as `(tick, message)`.
pub(crate) fn wheel_moves(events: &[Event]) -> Vec<(i32, Vec<u8>)> {
    events
        .iter()
        .filter(|event| !event.is_note_edge())
        .map(|event| (event.tick(), event.midi_message().to_vec()))
        .collect()
}

/// `ticks` sequencer ticks from wherever playback is, collecting the
/// note numbers of the NoteOns that reached the instrument.
pub(super) fn tick_and_collect(
    seq: &mut Sequencer,
    rx: &mut Consumer<ClipInstrumentEvent>,
    ticks: i32,
) -> Vec<u8> {
    tick_and_collect_messages(seq, rx, ticks)
        .into_iter()
        .filter_map(|(status, note)| (status == 0x90).then_some(note))
        .collect()
}

/// Every note message `(status high nibble, note)` the next `ticks` ticks
/// send, a velocity-0 `NoteOn` counted as a `NoteOff`.
pub(super) fn tick_and_collect_messages(
    seq: &mut Sequencer,
    rx: &mut Consumer<ClipInstrumentEvent>,
    ticks: i32,
) -> Vec<(u8, u8)> {
    let at = Instant::now();
    let mut messages = Vec::new();
    for _ in 0..ticks {
        seq.tick(at);
        while let Ok(ev) = rx.pop() {
            let status = match ev.message[0] & 0xf0 {
                0x90 if ev.message[2] == 0 => 0x80,
                status @ (0x80 | 0x90) => status,
                _ => continue,
            };
            messages.push((status, ev.message[1]));
        }
    }
    messages
}
