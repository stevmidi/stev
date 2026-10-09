//! Wiring entrypoint: `setup_*` each subsystem, spawn every named thread (see
//! `000-architecture.md`), hand `eframe` the [`Display`](crate::view::display)
//! app. `native_options()` owns the window config.

/// Debug-only `println!` — compiles to `()` in `--release`. Use this, never
/// `println!` (`080-conventions.md`).
#[cfg(debug_assertions)]
macro_rules! dprintln {
    ($($t:tt)*) => { println!($($t)*) };
}
/// Release build of [`dprintln!`] — expands to nothing.
#[cfg(not(debug_assertions))]
macro_rules! dprintln {
    ($($t:tt)*) => {
        ()
    };
}

use std::sync::{Arc, OnceLock, atomic::Ordering};

use eframe::NativeOptions;
use egui::{Context, ViewportBuilder};

use crate::core::{
    settings::{clamp_midi_out_offset_ms, load_settings},
    setup::{
        setup_clock, setup_display, setup_event_handlers, setup_metronome,
        setup_midi_input_forwarder, setup_sequencer, setup_shared_atomics, setup_shared_channels,
        setup_transport,
    },
    threads::{
        start_audio_engine, start_clock_thread, start_midi_output_thread,
        start_midi_watcher_thread, start_sequencer_thread,
    },
};

#[cfg(target_os = "macos")]
use crate::core::midi::port::anchor_coremidi_to_main_thread;
#[cfg(target_os = "macos")]
use crate::core::plugin_host::{
    install_key_guard, run_if_scan_child, start_plugin_catalog_scan, start_plugin_host,
};

#[cfg(target_os = "macos")]
use crate::view::appkit::route_quit_through_window_close;
use crate::view::theme;

/// Pure data — `Clip`, `Event`, `Track`, `Region`, `PerformanceLane`,
/// `Selection`, `WheelTracker`. No I/O.
mod models {
    pub mod clip;
    pub mod event;
    pub mod performance_lane;
    pub mod region;
    pub mod selection;
    pub mod track;
    pub mod wheels;
}
/// Render-side clip snapshots, built by the engine and shipped in `UiEvent`s.
mod metadata {
    pub mod clip_metadata;
    pub mod clip_view;
}
/// The engine: threading, sequencer, transport, clock, audio, MIDI,
/// persistence, event routing.
mod core {
    pub mod audio;
    pub mod clock;
    pub mod config;
    pub mod event_handlers;
    pub mod input_event;
    pub mod metronome;
    pub mod midi;
    pub mod note_logger;
    pub mod paths;
    #[cfg(target_os = "macos")]
    pub mod plugin_host;
    pub mod project;
    pub mod sequencer;
    pub mod settings;
    pub mod setup;
    pub mod shared_atomics;
    pub mod shared_channels;
    pub mod threads;
    pub mod time;
    pub mod timer;
    pub mod transport;
    pub mod view_state;
}
/// The UI: the `Display` eframe app, input polling, the colour theme, and
/// (macOS) the AppKit glue for the main window.
mod view {
    #[cfg(target_os = "macos")]
    pub mod appkit;
    pub mod display;
    pub mod input_poller;
    pub mod theme;
}
/// Rendering primitives — the reconciled clip / event shape models.
mod shapes {
    pub mod clip_shape;
    pub mod event_shape;
}

/// Resizable desktop window, `2560×1440` default inner size, vsync-paced render
/// loop (`glow_options.vsync` — eframe's default, left as is). No fullscreen /
/// platform lock — see `030-ui-design.md`.
fn native_options() -> NativeOptions {
    NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([2560.0, 1440.0]) // sensible desktop default
            .with_title("Stev"),
        ..Default::default()
    }
}

fn main() {
    // Must come first. Stev re-invokes its own binary to scan VST3 plugin
    // bundles out of process (some plugins' `bundleEntry` demands a main thread
    // of its own — see `180-vst3-host.md`), and this is where such a child
    // recognises itself, does its one job and exits. It returns immediately,
    // having done nothing, on an ordinary launch; putting anything before it
    // would have every scan child open an audio device, MIDI ports and a
    // window of its own.
    #[cfg(target_os = "macos")]
    run_if_scan_child();

    // Before any MIDI thread starts, or hot-plugged devices never show up —
    // see `anchor_coremidi_to_main_thread`. Held until `main` returns.
    #[cfg(target_os = "macos")]
    let _coremidi_anchor = anchor_coremidi_to_main_thread();

    let shared_atomics = setup_shared_atomics();
    let shared_channels = setup_shared_channels();
    let settings = load_settings();
    theme::set_active_theme(settings.theme_index);

    let midi_input_forwarder = setup_midi_input_forwarder(
        shared_channels.midi_in_tx,
        shared_channels.midi_out_tx.clone(),
        shared_channels.note_logger_command_tx.clone(),
        shared_channels.live_instrument_midi_tx,
        &shared_atomics,
    );
    // Publish the persisted MIDI-output offset before the `"midiout"` thread
    // starts reading it. See `160-midi-out-offset.md`.
    let midi_out_offset_ms = clamp_midi_out_offset_ms(settings.midi_out_offset_ms);
    shared_atomics
        .midi_out_offset_ms
        .store(midi_out_offset_ms, Ordering::Relaxed);

    let clock = setup_clock(shared_channels.clock_command_rx, &shared_atomics);

    let transport = setup_transport(
        shared_channels.transport_event_tx,
        shared_channels.clock_command_tx.clone(),
        &shared_atomics,
    );

    let metronome = setup_metronome(shared_channels.metronome_click_tx, &shared_atomics);

    let sequencer = setup_sequencer(
        shared_channels.midi_out_tx.clone(),
        shared_channels.clip_instrument_midi_tx,
        &shared_atomics,
    );

    // Filled in once `eframe::run_native` hands over the real context below —
    // lets background threads wake the reactive UI (see `EventHandlers::request_repaint`).
    let repaint_ctx: Arc<OnceLock<Context>> = Arc::new(OnceLock::new());

    let event_handlers = Arc::new(setup_event_handlers(
        shared_channels.ui_event_tx.clone(),
        shared_channels.transport_command_tx.clone(),
        shared_channels.sequencer_command_tx.clone(),
        shared_channels.note_logger_command_tx.clone(),
        &shared_atomics,
        repaint_ctx.clone(),
    ));

    let mut display = setup_display(
        event_handlers.clone(),
        shared_channels.input_event_rx,
        shared_channels.input_event_tx.clone(),
        shared_channels.ui_event_rx,
        &shared_atomics,
        shared_channels.midi_in_reconnect_tx.clone(),
        shared_channels.midi_out_reconnect_tx.clone(),
        settings.midi_in_port.clone(),
        settings.midi_out_port.clone(),
        settings.last_project_folder.clone(),
        midi_out_offset_ms,
    );

    // The unified audio engine: one cpal output stream, seeded with the
    // built-in metronome click. On macOS the plugin host mixer is added below as a
    // second source.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut audio_engine = start_audio_engine(shared_channels.metronome_click_rx);
    if let Some(engine) = audio_engine.as_ref() {
        display.attach_audio_load(engine.load.clone());
    }

    // macOS instrument plugin host: hand the mixer to the running engine. Plugins
    // are loaded per-track later, from the browser panel (see
    // `Display::load_slot_instrument`). Any failure disables the whole feature
    // and drops the MIDI receivers so sends fail fast.
    #[cfg(target_os = "macos")]
    let mut plugin_host_started = false;
    #[cfg(target_os = "macos")]
    if let Some(engine) = audio_engine.as_mut() {
        match start_plugin_host(
            engine,
            shared_channels.clip_instrument_midi_rx,
            shared_channels.live_instrument_midi_rx,
            &shared_atomics,
            repaint_ctx.clone(),
        ) {
            Ok(handle) => {
                display
                    .attach_instrument_audio(handle, shared_atomics.live_instrument_target.clone());
                // The catalog scan is deliberately *not* started here — see
                // the eframe creation closure below.
                plugin_host_started = true;
            }
            Err(_e) => dprintln!("plugin host disabled: {_e}"),
        }
    }
    // No plugin host off macOS — drop the consumers so the taps fail fast
    // (fill the ring, then every push fails) instead of running forever
    // unread.
    #[cfg(not(target_os = "macos"))]
    {
        drop(shared_channels.clip_instrument_midi_rx);
        drop(shared_channels.live_instrument_midi_rx);
    }

    start_clock_thread(clock, shared_channels.tick_tx);

    start_midi_output_thread(
        shared_channels.midi_out_rx,
        shared_channels.midi_out_tx,
        shared_channels.note_logger_command_rx,
        shared_channels.midi_out_connection_rx,
        shared_atomics.midi_out_offset_ms.clone(),
    );

    start_sequencer_thread(
        sequencer,
        transport,
        metronome,
        event_handlers.clone(),
        shared_channels.tick_rx,
        shared_channels.midi_in_rx,
        shared_channels.transport_event_rx,
        shared_channels.sequencer_command_rx,
        shared_channels.transport_command_rx,
    );

    start_midi_watcher_thread(
        settings.midi_in_port,
        settings.midi_out_port,
        midi_input_forwarder,
        shared_channels.midi_in_reconnect_rx,
        shared_channels.midi_out_reconnect_rx,
        shared_channels.midi_out_connection_tx,
        shared_channels.ui_event_tx,
        repaint_ctx.clone(),
    );

    eframe::run_native(
        "Stev",
        native_options(),
        Box::new(move |cc| {
            let mut display = display;
            repaint_ctx.set(cc.egui_ctx.clone()).ok();
            display.attach_drag_pointer(cc);
            // Must run after AppKit/NSApplication is up, which this creation
            // closure guarantees — installing it any earlier is unsafe. See
            // `core::plugin_host::key_guard`.
            #[cfg(target_os = "macos")]
            install_key_guard(shared_channels.input_event_tx, repaint_ctx.clone());
            // ⌘Q closes the window rather than terminating, so quitting goes
            // through the unsaved-changes check (`view::appkit`).
            #[cfg(target_os = "macos")]
            route_quit_through_window_close(cc);
            // The plugin catalog scan starts here, for the same reason, and it
            // is not optional: a VST3 `bundleEntry` starts the plugin's real
            // runtime, and plenty of those touch AppKit (`NSApplication`,
            // class registration, `NSBundle`) from whatever thread called
            // them. Spawning the scan before `run_native` therefore races
            // winit's own `NSApplication` creation on the main thread, and the
            // app hangs on launch with no window and no crash. A CLAP `init()`
            // is a light entry-point registration and never provoked this,
            // which is why the scan used to start earlier. See
            // `180-vst3-host.md`.
            #[cfg(target_os = "macos")]
            if plugin_host_started {
                display.attach_plugin_catalog_rx(start_plugin_catalog_scan());
            }
            Ok(Box::new(display))
        }),
    )
    .unwrap();
    // A window close — the close button, or ⌘Q (`view::appkit`) — returns
    // here, where AppKit's `terminate:` would have called `exit()` straight
    // after `App::on_exit`. Exit the same way: dropping `main`'s locals would
    // drop the audio engine and with it the plugin voices the shutdown
    // deliberately leaves to leak (`plugin_host::shutdown`).
    #[cfg(target_os = "macos")]
    std::process::exit(0);
}
