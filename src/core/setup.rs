//! Construction helpers called from `main.rs` — one `setup_*` per subsystem,
//! each building its piece from the shared atomics / channels and leaving
//! `main` to spawn the threads. Keeps the wiring in one readable place.

use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, AtomicU64},
};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use egui::Context;
use rtrb::{Producer, RingBuffer};

use crate::core::config::{
    INSTRUMENT_MIDI_RING_CAPACITY, METRONOME_CLICK_RING_CAPACITY, MIDI_OUT_OFFSET_DEFAULT_MS,
    REGION_LENGTH_DEFAULT, TEMPO_US_DEFAULT,
};
use crate::core::{
    shared_atomics::{LiveRecState, TrackMixAtomics},
    time::Meter,
    timer::Timer,
};
use crate::{
    core::{
        audio::ClickEvent,
        clock::{Clock, ClockCommand},
        event_handlers::EventHandlers,
        input_event::InputEvent,
        metronome::Metronome,
        midi::{
            input::{InputTicks, MidiInputForwarder},
            message::Midi3,
            out_queue::MidiOutMessage,
            output::MidiOutputConnection,
        },
        note_logger::NoteLoggerCommand,
        sequencer::{ClipInstrumentEvent, Sequencer, SequencerCommand},
        shared_atomics::SharedAtomics,
        shared_channels::SharedChannels,
        transport::{Transport, TransportCommand, TransportEvent},
    },
    view::display::{Display, UiEvent},
};

/// Builds the [`SharedAtomics`] bundle with every value at its startup default.
pub(crate) fn setup_shared_atomics() -> SharedAtomics {
    SharedAtomics {
        running: Arc::new(AtomicBool::new(false)),
        tempo: Arc::new(AtomicI32::new(TEMPO_US_DEFAULT)),
        meter: Arc::new(AtomicU16::new(Meter::FOUR_FOUR.to_bits())),
        clock_tick: Arc::new(AtomicI32::new(0)),
        clock_tick_instant_nanos: Arc::new(AtomicU64::new(0)),
        elapsed_ticks: Arc::new(AtomicI32::new(0)),
        playback_tick: Arc::new(AtomicI32::new(0)),
        cursor_tick: Arc::new(AtomicI32::new(0)),
        region_start: Arc::new(AtomicI32::new(0)),
        region_end: Arc::new(AtomicI32::new(REGION_LENGTH_DEFAULT)),
        loop_enabled: Arc::new(AtomicBool::new(true)),
        metronome_mute: Arc::new(AtomicBool::new(true)),
        view_state: Arc::new(AtomicU8::new(0)),
        has_event_selection: Arc::new(AtomicBool::new(false)),
        arm_channel: Arc::new(AtomicU8::new(0)),
        performance_lane_armed: Arc::new(AtomicBool::new(false)),
        live_instrument_target: Arc::new(AtomicI32::new(-1)),
        midi_out_offset_ms: Arc::new(AtomicI32::new(MIDI_OUT_OFFSET_DEFAULT_MS)),
        track_mix: Arc::new(TrackMixAtomics::new()),
        live_rec_state: LiveRecState {
            last_note_on: Arc::new(AtomicU8::new(0)),
            last_note_velocity: Arc::new(AtomicU8::new(0)),
            elapsed_start_tick: Arc::new(AtomicI32::new(0)),
            thumbnail_snapshot: Arc::new(Mutex::new(Vec::new())),
        },
    }
}

/// Builds every channel / `rtrb` ring in one place — see [`SharedChannels`].
pub(crate) fn setup_shared_channels() -> SharedChannels {
    let (tick_tx, tick_rx) = unbounded();
    let (sequencer_command_tx, sequencer_command_rx) = unbounded::<SequencerCommand>();
    let (transport_command_tx, transport_command_rx) = unbounded::<TransportCommand>();
    let (note_logger_command_tx, note_logger_command_rx) = unbounded::<NoteLoggerCommand>();
    let (transport_event_tx, transport_event_rx) = unbounded::<TransportEvent>();
    let (clock_command_tx, clock_command_rx) = unbounded::<ClockCommand>();
    let (midi_in_tx, midi_in_rx) = unbounded::<(Vec<u8>, InputTicks)>();
    let (midi_out_tx, midi_out_rx) = unbounded::<MidiOutMessage>();
    let (clip_instrument_midi_tx, clip_instrument_midi_rx) =
        RingBuffer::new(INSTRUMENT_MIDI_RING_CAPACITY);
    let (live_instrument_midi_tx, live_instrument_midi_rx) =
        RingBuffer::new(INSTRUMENT_MIDI_RING_CAPACITY);
    let (metronome_click_tx, metronome_click_rx) = RingBuffer::new(METRONOME_CLICK_RING_CAPACITY);
    let (input_event_tx, input_event_rx) = unbounded::<InputEvent>();
    let (ui_event_tx, ui_event_rx) = unbounded::<UiEvent>();
    let (midi_in_reconnect_tx, midi_in_reconnect_rx) = bounded::<String>(1);
    let (midi_out_reconnect_tx, midi_out_reconnect_rx) = bounded::<String>(1);
    let (midi_out_connection_tx, midi_out_connection_rx) = unbounded::<MidiOutputConnection>();

    SharedChannels {
        tick_tx,
        tick_rx,
        sequencer_command_tx,
        sequencer_command_rx,
        transport_command_tx,
        transport_command_rx,
        note_logger_command_tx,
        note_logger_command_rx,
        transport_event_tx,
        transport_event_rx,
        clock_command_tx,
        clock_command_rx,
        midi_in_tx,
        midi_in_rx,
        midi_out_tx,
        midi_out_rx,
        clip_instrument_midi_tx,
        clip_instrument_midi_rx,
        live_instrument_midi_tx,
        live_instrument_midi_rx,
        metronome_click_tx,
        metronome_click_rx,
        input_event_tx,
        input_event_rx,
        ui_event_tx,
        ui_event_rx,
        midi_in_reconnect_tx,
        midi_in_reconnect_rx,
        midi_out_reconnect_tx,
        midi_out_reconnect_rx,
        midi_out_connection_tx,
        midi_out_connection_rx,
    }
}

/// Builds the [`Clock`] and its [`Timer`] (platform-specific), wired to the shared atomics.
pub(crate) fn setup_clock(
    clock_command_rx: Receiver<ClockCommand>,
    shared_atomics: &SharedAtomics,
) -> Clock {
    Clock::new(
        Timer::new(1),
        clock_command_rx,
        shared_atomics.tempo.clone(),
        shared_atomics.clock_tick.clone(),
        shared_atomics.clock_tick_instant_nanos.clone(),
        shared_atomics.elapsed_ticks.clone(),
        shared_atomics.running.clone(),
    )
}

/// Builds the thread-local [`Transport`] from the shared atomics and channels.
pub(crate) fn setup_transport(
    transport_event_tx: Sender<TransportEvent>,
    clock_command_tx: Sender<ClockCommand>,
    shared_atomics: &SharedAtomics,
) -> Transport {
    Transport::new(
        transport_event_tx,
        clock_command_tx,
        shared_atomics.playback_tick.clone(),
        shared_atomics.cursor_tick.clone(),
        shared_atomics.running.clone(),
        shared_atomics.region_start.clone(),
        shared_atomics.region_end.clone(),
        shared_atomics.loop_enabled.clone(),
    )
}

/// Builds the [`Metronome`] wired to the mute atomic and the click ring.
pub(crate) fn setup_metronome(
    metronome_click_tx: Producer<ClickEvent>,
    shared_atomics: &SharedAtomics,
) -> Metronome {
    Metronome::new(shared_atomics.metronome_mute.clone(), metronome_click_tx)
}

/// Builds the [`MidiInputForwarder`] wired to its output channels and the shared clock/tempo/arm atomics.
pub(crate) fn setup_midi_input_forwarder(
    midi_in_tx: Sender<(Vec<u8>, InputTicks)>,
    midi_out_tx: Sender<MidiOutMessage>,
    note_logger_command_tx: Sender<NoteLoggerCommand>,
    instrument_midi_tx: Producer<(usize, Midi3)>,
    shared_atomics: &SharedAtomics,
) -> MidiInputForwarder {
    MidiInputForwarder::new(
        midi_in_tx,
        midi_out_tx,
        note_logger_command_tx,
        instrument_midi_tx,
        shared_atomics.clock_tick.clone(),
        shared_atomics.clock_tick_instant_nanos.clone(),
        shared_atomics.elapsed_ticks.clone(),
        shared_atomics.tempo.clone(),
        shared_atomics.arm_channel.clone(),
        shared_atomics.performance_lane_armed.clone(),
        shared_atomics.live_instrument_target.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
/// Builds the [`Display`] (eframe app) with its channel ends and shared-atomic readers.
pub(crate) fn setup_display(
    event_handlers: Arc<EventHandlers>,
    input_event_rx: Receiver<InputEvent>,
    input_event_tx: Sender<InputEvent>,
    ui_event_rx: Receiver<UiEvent>,
    shared_atomics: &SharedAtomics,
    midi_in_reconnect_tx: Sender<String>,
    midi_out_reconnect_tx: Sender<String>,
    midi_in_current_port: Option<String>,
    midi_out_current_port: Option<String>,
    last_project_folder: Option<String>,
    midi_out_offset_ms: i32,
) -> Display {
    Display::new(
        event_handlers,
        input_event_rx,
        input_event_tx,
        ui_event_rx,
        midi_in_reconnect_tx,
        midi_out_reconnect_tx,
        midi_in_current_port,
        midi_out_current_port,
        last_project_folder,
        midi_out_offset_ms,
        shared_atomics.tempo.clone(),
        shared_atomics.meter.clone(),
        shared_atomics.running.clone(),
        shared_atomics.elapsed_ticks.clone(),
        shared_atomics.playback_tick.clone(),
        shared_atomics.cursor_tick.clone(),
        shared_atomics.region_start.clone(),
        shared_atomics.region_end.clone(),
        shared_atomics.loop_enabled.clone(),
        shared_atomics.view_state.clone(),
        shared_atomics.has_event_selection.clone(),
        shared_atomics.track_mix.clone(),
        shared_atomics.live_rec_state.clone(),
    )
}

/// Builds the [`Sequencer`] wired to its output channels and the shared atomics.
pub(crate) fn setup_sequencer(
    midi_out_tx: Sender<MidiOutMessage>,
    instrument_midi_tx: Producer<ClipInstrumentEvent>,
    shared_atomics: &SharedAtomics,
) -> Sequencer {
    Sequencer::new(
        midi_out_tx,
        instrument_midi_tx,
        shared_atomics.playback_tick.clone(),
        shared_atomics.elapsed_ticks.clone(),
        shared_atomics.cursor_tick.clone(),
        shared_atomics.region_start.clone(),
        shared_atomics.region_end.clone(),
        shared_atomics.loop_enabled.clone(),
        shared_atomics.running.clone(),
        shared_atomics.tempo.clone(),
        shared_atomics.meter.clone(),
        shared_atomics.arm_channel.clone(),
        shared_atomics.performance_lane_armed.clone(),
        shared_atomics.track_mix.clone(),
        shared_atomics.live_rec_state.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
/// Builds the [`EventHandlers`] and the `OnceLock` the repaint context is later filled into.
pub(crate) fn setup_event_handlers(
    ui_event_tx: Sender<UiEvent>,
    transport_command_tx: Sender<TransportCommand>,
    sequencer_command_tx: Sender<SequencerCommand>,
    note_logger_command_tx: Sender<NoteLoggerCommand>,
    shared_atomics: &SharedAtomics,
    repaint_ctx: Arc<OnceLock<Context>>,
) -> EventHandlers {
    EventHandlers::new(
        ui_event_tx,
        transport_command_tx,
        sequencer_command_tx,
        note_logger_command_tx,
        shared_atomics.view_state.clone(),
        shared_atomics.has_event_selection.clone(),
        shared_atomics.midi_out_offset_ms.clone(),
        repaint_ctx,
    )
}
