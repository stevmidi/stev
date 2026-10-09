//! [`MidiOutputConnection`] — a thin owner of the live `midir` output port,
//! driven by the `"midiout"` thread. Handles picking / reconnecting a real
//! port and creating the user-facing Unix virtual port.

use std::error::Error;

#[cfg(unix)]
use midir::os::unix::VirtualOutput;
use midir::{MidiOutput, MidiOutputConnection as MidirOutputConnection};

#[cfg(unix)]
use crate::core::midi::port::VIRTUAL_PORT_NAME;
use crate::core::midi::port::pickable_port_names;

/// Owns the current `midir` output connection (or none).
pub(crate) struct MidiOutputConnection {
    /// The live connection, or `None` when disconnected.
    midi_output_connection: Option<MidirOutputConnection>,
}

impl MidiOutputConnection {
    /// A disconnected instance.
    pub(crate) fn new() -> Self {
        MidiOutputConnection {
            midi_output_connection: None,
        }
    }

    /// Names of the available MIDI output ports, minus our own endpoints, with
    /// the virtual port first on Unix. For the settings modal.
    pub(crate) fn out_port_names() -> Vec<String> {
        let Ok(midi_out) = MidiOutput::new("MIDI Output") else {
            return Vec::new();
        };
        pickable_port_names(
            midi_out
                .ports()
                .iter()
                .filter_map(|p| midi_out.port_name(p).ok()),
        )
    }

    /// Reconnects to the named port, dropping any existing connection. Creates
    /// the user virtual port on Unix if `port_name` is `VIRTUAL_PORT_NAME`.
    pub(crate) fn connect_to_named(&mut self, port_name: &str) -> Result<(), Box<dyn Error>> {
        #[cfg(unix)]
        if port_name == VIRTUAL_PORT_NAME {
            return self.connect_virtual(port_name);
        }

        // Drop existing connection first
        self.midi_output_connection = None;
        let midi_out = MidiOutput::new("MIDI Output")?;

        let out_ports = midi_out.ports();
        let out_port = out_ports
            .iter()
            .find(|p| midi_out.port_name(p).ok().as_deref() == Some(port_name))
            .ok_or_else(|| format!("error: MIDI output port '{}' not found", port_name))?;
        dprintln!("Connecting to output port: {}", port_name);
        self.midi_output_connection = Some(midi_out.connect(out_port, "midi_output")?);
        Ok(())
    }

    /// Registers a virtual output port under `name` (Unix/CoreMIDI/ALSA only)
    /// and connects to it: the user-facing virtual port.
    #[cfg(unix)]
    fn connect_virtual(&mut self, name: &str) -> Result<(), Box<dyn Error>> {
        self.midi_output_connection = None;
        let midi_out = MidiOutput::new("MIDI Output")?;
        dprintln!("Creating virtual output port: {}", name);
        self.midi_output_connection = Some(midi_out.create_virtual(name)?);
        Ok(())
    }

    /// Sends raw MIDI bytes if connected; a no-op otherwise.
    pub(crate) fn send(&mut self, message: &[u8]) -> Result<(), Box<dyn Error>> {
        if let Some(conn) = &mut self.midi_output_connection {
            conn.send(message)?;
        }
        Ok(())
    }
}
