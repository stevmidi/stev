//! Fixtures shared by the sequencer's unit tests.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam_channel::{Receiver, unbounded};
use rtrb::{Consumer, RingBuffer};

use crate::core::{
    config,
    input_event::TimeSelectionRect,
    midi::out_queue::MidiOutMessage,
    shared_atomics::{LiveRecState, TrackMixAtomics},
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
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicBool::new(loop_enabled)),
        Arc::new(AtomicBool::new(false)), // running
        Arc::new(AtomicI32::new(config::TEMPO_US_DEFAULT)),
        Arc::new(AtomicU8::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(TrackMixAtomics::new()),
        LiveRecState {
            last_note_on: Arc::new(AtomicU8::new(0)),
            last_note_velocity: Arc::new(AtomicU8::new(0)),
            elapsed_start_tick: Arc::new(AtomicI32::new(0)),
            thumbnail_snapshot: Arc::new(Mutex::new(Vec::new())),
        },
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
