//! The thread factory functions — one `start_*` per named thread in
//! `000-architecture.md`'s table. Each spawns its thread, moves the channel
//! ends and `Arc`s it needs into the closure, and runs that thread's loop
//! (usually a `crossbeam_channel::select!`).
//!
//! ## Module split
//!
//! - `mod.rs` — the two factories with no loop of their own:
//!   [`start_clock_thread`] (hands `Clock` its tick sink) and
//!   [`start_audio_engine`] (seeds the `"audio-engine"` mixer with the click).
//! - `midi_output.rs` — the `"midiout"` thread and its delay queue
//!   (`160-midi-out-offset.md`).
//! - `midi_watcher.rs` — the `"midiwatcher"` hot-plug / reconnect thread.
//! - `computer_keys.rs` — the `"computerkeys"` thread, routing the computer
//!   keyboard's notes as live input.
//! - `sequencer_pump.rs` — the `"sequencer"` thread, the hub: the tick pump
//!   plus every command dispatch (`150-clock-position-sync.md`).
//! - `storage.rs` — the `"storage"` thread, the project's one writer: saves,
//!   templates and the crash recovery copy (`420-large-projects.md`,
//!   `410-crash-recovery.md`).
//! - `controller.rs` — the parked MIDI-CC controller route, `#[allow(dead_code)]`
//!   and never called; kept on purpose (`000-architecture.md`).
//! - `agent_bridge.rs` (Unix) — the `"agent-bridge"` thread, the app's end of
//!   the MCP server (`360-mcp-server.md`).

#[cfg(unix)]
mod agent_bridge;
mod computer_keys;
mod controller;
mod midi_output;
mod midi_watcher;
mod sequencer_pump;
mod storage;

#[cfg(unix)]
pub(crate) use agent_bridge::start_agent_bridge_thread;
pub(crate) use computer_keys::start_computer_keys_thread;
pub(crate) use midi_output::start_midi_output_thread;
#[cfg(test)]
pub(crate) use midi_output::{MidiSink, run_midi_output_loop};
pub(crate) use midi_watcher::start_midi_watcher_thread;
pub(crate) use sequencer_pump::start_sequencer_thread;
pub(crate) use storage::start_storage_thread;

use std::sync::{Arc, OnceLock};

use crossbeam_channel::Sender;
use egui::Context;
use rtrb::Consumer;

use crate::core::audio::{
    AudioEngine, AudioSource, ClickEvent, EngineHandle, FALLBACK_SAMPLE_RATE, MetronomeSource,
};
use crate::core::clock::{Clock, ClockTick};
use crate::core::time;
use crate::view::display::UiEvent;

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
/// can hand the macOS instrument mixer to the running engine as a second
/// source. With no output device the app starts silent and the engine opens
/// one when it appears. The engine's lines about an outside change (a device
/// gone, a new rate) go to the footer through `ui_event_tx`.
pub(crate) fn start_audio_engine(
    metronome_click_rx: Consumer<ClickEvent>,
    ui_event_tx: Sender<UiEvent>,
    repaint_ctx: Arc<OnceLock<Context>>,
) -> EngineHandle {
    // The click is built at the fallback rate; the engine's first open tells
    // it the device's.
    let click = MetronomeSource::new(metronome_click_rx, f64::from(FALLBACK_SAMPLE_RATE));
    AudioEngine::start(
        vec![Box::new(click) as Box<dyn AudioSource>],
        move |message| {
            ui_event_tx.send(UiEvent::Status { message }).ok();
            // A device change is no window input event, so an idle window would
            // not redraw the footer on its own.
            if let Some(ctx) = repaint_ctx.get() {
                ctx.request_repaint();
            }
        },
    )
}
