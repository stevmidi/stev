//! Every channel the threads talk over, created up front in one place.
//!
//! The core threads use `crossbeam_channel` (`Sender`/`Receiver`); the three
//! feeds that cross into the realtime audio callback use `rtrb` SPSC rings
//! (`Producer`/`Consumer`) instead, because a `crossbeam_channel`'s internal
//! segment allocate/free is not safe to run on the audio thread. Each `rtrb`
//! ring is strict single-producer, so a feed with a distinct producer thread
//! gets its own ring. The `"sequencer"` thread's `select!` loop is the hub that
//! most of these converge on — see the thread table in `000-architecture.md`.
//!
//! Every field is one half of a pair; the sending (`_tx`) field carries the
//! description of what flows over it.

use crossbeam_channel::{Receiver, Sender};
use rtrb::{Consumer, Producer};

use crate::{
    core::{
        audio::ClickEvent,
        clock::{ClockCommand, ClockTick},
        input_event::InputEvent,
        midi::{
            input::InputTicks, message::Midi3, out_queue::MidiOutMessage,
            output::MidiOutputConnection,
        },
        note_logger::NoteLoggerCommand,
        sequencer::{ClipInstrumentEvent, SequencerCommand},
        transport::{TransportCommand, TransportEvent},
    },
    view::display::UiEvent,
};

/// The channel bundle. Built once in `setup_shared_channels`; each thread
/// factory takes the ends it needs. The `rtrb` halves are `Producer`/`Consumer`
/// and so can only be moved, not cloned.
pub(crate) struct SharedChannels {
    /// `"clock"` thread → `"sequencer"` thread: one [`ClockTick`] per BPM
    /// firing, driving the transport tick pump and `Metronome`.
    pub(crate) tick_tx: Sender<ClockTick>,
    /// Receiving half of the pair documented above.
    pub(crate) tick_rx: Receiver<ClockTick>,
    /// Anything → `"sequencer"` thread: the main command stream (handlers,
    /// MIDI).
    pub(crate) sequencer_command_tx: Sender<SequencerCommand>,
    /// Receiving half of the pair documented above.
    pub(crate) sequencer_command_rx: Receiver<SequencerCommand>,
    /// Handlers → `"sequencer"` thread: transport-only commands, handled in the
    /// same `select!` loop against the thread-local `Transport`.
    pub(crate) transport_command_tx: Sender<TransportCommand>,
    /// Receiving half of the pair documented above.
    pub(crate) transport_command_rx: Receiver<TransportCommand>,
    /// `"sequencer"` thread → `"midiout"` thread's `NoteLogger`: log an input
    /// note, or release everything the logger is holding.
    pub(crate) note_logger_command_tx: Sender<NoteLoggerCommand>,
    /// Receiving half of the pair documented above.
    pub(crate) note_logger_command_rx: Receiver<NoteLoggerCommand>,
    /// `Transport` → `"sequencer"` tick pump: playback discontinuities that
    /// need a re-anchor (seek, chase). A loop wrap does *not* come this way —
    /// see [`TickOutcome::Wrapped`](crate::core::transport::TickOutcome).
    pub(crate) transport_event_tx: Sender<TransportEvent>,
    /// Receiving half of the pair documented above.
    pub(crate) transport_event_rx: Receiver<TransportEvent>,
    /// `Transport` (on the `"sequencer"` thread) → `"clock"` thread: realign
    /// the free-running counter onto playback's phase. See
    /// `150-clock-position-sync.md`.
    pub(crate) clock_command_tx: Sender<ClockCommand>,
    /// Receiving half of the pair documented above.
    pub(crate) clock_command_rx: Receiver<ClockCommand>,
    /// MIDI-input callback → `"sequencer"` thread: raw bytes plus the
    /// sub-tick-corrected arrival time ([`InputTicks`]).
    pub(crate) midi_in_tx: Sender<(Vec<u8>, InputTicks)>,
    /// Receiving half of the pair documented above.
    pub(crate) midi_in_rx: Receiver<(Vec<u8>, InputTicks)>,
    /// `"sequencer"` thread → `"midiout"` thread: outbound MIDI, clip messages
    /// tagged with their tick [`Instant`](std::time::Instant) for the delay
    /// queue. See `160-midi-out-offset.md`.
    pub(crate) midi_out_tx: Sender<MidiOutMessage>,
    /// Receiving half of the pair documented above.
    pub(crate) midi_out_rx: Receiver<MidiOutMessage>,
    /// [`ClipInstrumentEvent`]s for `TrackOutput::Instrument` tracks' clip
    /// playback (`Sequencer::tick` / `chase_notes` / `release_instrument_notes`),
    /// demuxed to each track's hosted plugin by the audio engine's
    /// `InstrumentMixer` (macOS only) and scheduled to the sample from each
    /// event's `when`. A
    /// dedicated `rtrb` SPSC ring, not a `crossbeam_channel`: the consumer is
    /// drained on the realtime audio callback thread, where a channel's
    /// internal segment allocate/free isn't safe. Elsewhere the consumer is
    /// dropped so pushes fail fast (fill the ring, then every push fails).
    /// See `130-plugin-host.md`.
    pub(crate) clip_instrument_midi_tx: Producer<ClipInstrumentEvent>,
    /// Receiving half of the pair documented above.
    pub(crate) clip_instrument_midi_rx: Consumer<ClipInstrumentEvent>,
    /// Same, for live keyboard input (`MidiInputForwarder`, tagged via
    /// `SharedAtomics.live_instrument_target`). A second, independent ring —
    /// `rtrb` is strict single-producer, and this and the clip feed above
    /// have different producer threads, so they can't share one.
    pub(crate) live_instrument_midi_tx: Producer<(usize, Midi3)>,
    /// Receiving half of the pair documented above.
    pub(crate) live_instrument_midi_rx: Consumer<(usize, Midi3)>,
    /// Scheduled metronome clicks from `Metronome` (sequencer thread) to the
    /// `MetronomeSource` in the audio engine. `rtrb` SPSC ring — drained on the
    /// realtime audio callback.
    pub(crate) metronome_click_tx: Producer<ClickEvent>,
    /// Receiving half of the pair documented above.
    pub(crate) metronome_click_rx: Consumer<ClickEvent>,
    /// `InputPoller` (render thread) → `"main"` dispatch loop: keyboard/mouse
    /// [`InputEvent`]s, drained each frame and handed to `EventHandlers`.
    pub(crate) input_event_tx: Sender<InputEvent>,
    /// Receiving half of the pair documented above.
    pub(crate) input_event_rx: Receiver<InputEvent>,
    /// `"sequencer"` thread → `Display` (render thread): [`UiEvent`]s telling
    /// the UI what changed (clip added, project loaded, ports refreshed, …).
    pub(crate) ui_event_tx: Sender<UiEvent>,
    /// Receiving half of the pair documented above.
    pub(crate) ui_event_rx: Receiver<UiEvent>,
    /// `Display` (MIDI settings modal) → `"midiwatcher"` thread: the input port
    /// name the user confirmed; becomes the wanted port and fires an immediate
    /// reconnect rather than waiting for the next poll.
    pub(crate) midi_in_reconnect_tx: Sender<String>,
    /// Receiving half of the pair documented above.
    pub(crate) midi_in_reconnect_rx: Receiver<String>,
    /// Same, for the output port.
    pub(crate) midi_out_reconnect_tx: Sender<String>,
    /// Receiving half of the pair documented above.
    pub(crate) midi_out_reconnect_rx: Receiver<String>,
    /// `"midiwatcher"` → `"midiout"` thread: an output connection the watcher
    /// opened (a pick, or a hot-plug of the wanted port) — or a closed one
    /// when the open failed. The `"midiout"` thread swaps it in and owns it.
    pub(crate) midi_out_connection_tx: Sender<MidiOutputConnection>,
    /// Receiving half of the pair documented above.
    pub(crate) midi_out_connection_rx: Receiver<MidiOutputConnection>,
}
