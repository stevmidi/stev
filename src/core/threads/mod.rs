//! The thread factory functions — one `start_*` per named thread in
//! `000-architecture.md`'s table. Each spawns its thread, moves the channel
//! ends and `Arc`s it needs into the closure, and runs that thread's loop
//! (usually a `crossbeam_channel::select!`).
//!
//! ## Module split
//!
//! - `mod.rs` — the two factories with no loop of their own:
//!   [`start_clock_thread`] (hands `Clock` its tick sink) and
//!   [`start_audio_engine`] (seeds the `"audio-engine"` stream with the click).
//! - `midi_output.rs` — the `"midiout"` thread and its delay queue
//!   (`160-midi-out-offset.md`).
//! - `midi_watcher.rs` — the `"midiwatcher"` hot-plug / reconnect thread.
//! - `sequencer_pump.rs` — the `"sequencer"` thread, the hub: the tick pump
//!   plus every command dispatch (`150-clock-position-sync.md`).
//! - `controller.rs` — the parked MIDI-CC controller route, `#[allow(dead_code)]`
//!   and never called; kept on purpose (`000-architecture.md`).

mod controller;
mod midi_output;
mod midi_watcher;
mod sequencer_pump;

pub(crate) use midi_output::start_midi_output_thread;
pub(crate) use midi_watcher::start_midi_watcher_thread;
pub(crate) use sequencer_pump::start_sequencer_thread;

use crossbeam_channel::Sender;
use rtrb::Consumer;

use crate::core::audio::{AudioEngine, AudioSource, ClickEvent, EngineHandle, MetronomeSource};
use crate::core::clock::{Clock, ClockTick};
use crate::core::time;

/// Anchors the monotonic origin and starts the `"clock"` thread forwarding each
/// [`ClockTick`] on `tick_tx`.
pub(crate) fn start_clock_thread(clock: Clock, tick_tx: Sender<ClockTick>) {
    // Pin the shared monotonic origin near process start so the timestamps the
    // clock publishes for MIDI-input tick interpolation start near zero.
    time::anchor_monotonic_origin();
    clock.start(move |tick| {
        tick_tx.send(tick).ok();
    });
}

/// Spawns the `"audio-engine"` thread with the built-in metronome click as its
/// first (and, off macOS, only) [`AudioSource`]. Returns the handle so `main`
/// can hand the macOS instrument mixer to the running engine as a second source, or
/// `None` if there is no output device (the app then runs without audio).
pub(crate) fn start_audio_engine(metronome_click_rx: Consumer<ClickEvent>) -> Option<EngineHandle> {
    match AudioEngine::start(move |sample_rate| {
        vec![
            Box::new(MetronomeSource::new(metronome_click_rx, sample_rate)) as Box<dyn AudioSource>,
        ]
    }) {
        Ok(handle) => Some(handle),
        Err(_e) => {
            dprintln!("audio engine disabled: {_e}");
            None
        }
    }
}
