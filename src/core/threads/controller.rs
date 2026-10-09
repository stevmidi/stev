//! The parked MIDI-CC hardware-controller thread.
//!
//! [`start_controller_thread`] is `#[allow(dead_code)]` and never spawned — the
//! physical keypad is a plain HID keyboard now. It is kept deliberately as the
//! reference implementation of the CC → `egui::Key` mapping in
//! `archive/010-keypad.md`,
//! and as the only producer of [`InputEvent`]s besides `Display`.

use crossbeam_channel::Sender;
use egui::Key;
use midir::{Ignore, MidiInput};

use std::thread::{self, JoinHandle};

use crate::core::config;
use crate::core::input_event::{InputEvent, KeyModifiers};

/// Reference implementation of a MIDI-CC hardware controller thread, kept
/// on purpose but currently unused — the physical keypad is now a plain HID
/// keyboard. Mirrors the key mapping in `archive/010-keypad.md`.
#[allow(dead_code)]
pub(crate) fn start_controller_thread(input_event_tx: Sender<InputEvent>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("controller".to_string())
        .spawn(move || {
            let mut midi_in = match MidiInput::new("Controller Input") {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Controller: failed to create MIDI input: {}", e);
                    return;
                }
            };
            midi_in.ignore(Ignore::Sysex);

            let ports = midi_in.ports();
            let port = ports.iter().find(|p| {
                midi_in
                    .port_name(p)
                    .ok()
                    .map(|n| n.contains(config::CONTROLLER_DEVICE_NAME))
                    .unwrap_or(false)
            });

            let port = match port {
                Some(p) => p,
                None => {
                    eprintln!(
                        "Controller: '{}' not found. Available ports: {}",
                        config::CONTROLLER_DEVICE_NAME,
                        ports
                            .iter()
                            .filter_map(|p| midi_in.port_name(p).ok())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    return;
                }
            };

            let mut shift_held = false;

            let _conn = midi_in
                .connect(
                    port,
                    "controller-in",
                    move |_, msg, _| {
                        match msg {
                            [0xB0, 90, 127] => {
                                shift_held = true;
                            }
                            [0xB0, 90, 0] => {
                                shift_held = false;
                            }
                            [0xB0, cc, 127] => {
                                let key: Option<Key> = match cc {
                                    22 => Some(Key::F1), // Softkey 1
                                    23 => Some(Key::F2), // Softkey 2
                                    24 => Some(Key::F3), // Softkey 3
                                    25 => Some(Key::F4), // Softkey 4

                                    26 => Some(Key::Delete), // Clear
                                    27 => Some(Key::D),      // Duplicate
                                    28 => Some(Key::Q),      // Quantize
                                    29 => Some(Key::S),      // Mute

                                    30 => Some(Key::M), // Metronome
                                    31 => Some(Key::T), // Tempo
                                    86 => Some(Key::Z), // Undo

                                    87 => Some(Key::Period), // Stop
                                    88 => Some(Key::Num0),   // Play
                                    89 => Some(Key::R),      // Record
                                    97 => Some(Key::C),      // Commit

                                    91 => Some(Key::ArrowUp),   // Up arrow
                                    92 => Some(Key::ArrowDown), // Down arrow
                                    93 => Some(Key::Minus),     // -
                                    94 => Some(Key::Plus),      // +

                                    95 => Some(Key::ArrowLeft), // Left arrow
                                    96 => Some(Key::ArrowRight), // Right arrow
                                    98 => Some(Key::Enter),     // Enter arrow
                                    _ => None,
                                };
                                if let Some(key) = key {
                                    // Undo is now gated on the cross-platform
                                    // command modifier (Cmd/Ctrl+Z), so the
                                    // softkey must assert it even though this
                                    // controller route has no physical
                                    // command key of its own.
                                    let command = matches!(key, Key::Z);
                                    input_event_tx
                                        .send(InputEvent::KeyPressed {
                                            key,
                                            modifiers: KeyModifiers {
                                                shift: shift_held,
                                                command,
                                                alt: false,
                                                ctrl: false,
                                            },
                                        })
                                        .ok();
                                }
                            }
                            _ => {}
                        }
                    },
                    (),
                )
                .expect("Controller: failed to connect MIDI input");

            thread::park();
        })
        .expect("Failed to spawn controller thread")
}
