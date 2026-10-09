//! The `"midiwatcher"` thread: the one owner of *which* MIDI ports are wanted,
//! and the one place ports are opened. It opens both at startup (its first
//! poll runs immediately), reconnects on the settings modal's pick, and
//! hot-plugs a wanted port that isn't open on a 2 s poll. Owns the live input
//! connection; an opened output connection is handed to the `"midiout"`
//! thread, which owns it from then on. Emits `UiEvent::MidiPortsRefreshed`
//! (with a repaint, so an idle window redraws) whenever the port lists change,
//! so the modal's lists stay current.

use crossbeam_channel::{Receiver, Sender, after, select};
use egui::Context;
use midir::MidiInputConnection;

use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::midi::input::MidiInputForwarder;
use crate::core::midi::output::MidiOutputConnection;
use crate::core::midi::port::matching_port_index;
use crate::view::display::UiEvent;

/// How often the port lists are polled for hot-plug.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// One side's wanted port and its connection, if open. The output side's
/// connection lives on the `"midiout"` thread, so there `C` is `()` — only
/// whether it opened.
#[derive(Debug)]
struct WantedPort<C> {
    /// The saved / last-picked port name; `None` = no port.
    wanted: Option<String>,
    /// The open connection to `wanted`, `None` while closed.
    conn: Option<C>,
}

impl<C> WantedPort<C> {
    /// A side wanting `wanted`, with nothing open yet.
    fn new(wanted: Option<String>) -> Self {
        WantedPort { wanted, conn: None }
    }

    /// The port in `ports` to open now, if any: the wanted one (matched
    /// ignoring the ALSA suffix), while nothing is open.
    fn hot_plug<'a>(&self, ports: &'a [String]) -> Option<&'a str> {
        if self.conn.is_some() {
            return None;
        }
        matching_port_index(ports, self.wanted.as_deref()).map(|i| ports[i].as_str())
    }
}

/// Starts the `"midiwatcher"` thread (see the module docs). `midi_in_port` /
/// `midi_out_port` are the saved port names; with none, that side stays closed
/// until the user picks a port. Never exits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_midi_watcher_thread(
    midi_in_port: Option<String>,
    midi_out_port: Option<String>,
    midi_input_forwarder: MidiInputForwarder,
    midi_in_reconnect_rx: Receiver<String>,
    midi_out_reconnect_rx: Receiver<String>,
    midi_out_connection_tx: Sender<MidiOutputConnection>,
    ui_event_tx: Sender<UiEvent>,
    repaint_ctx: Arc<OnceLock<Context>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("midiwatcher".to_string())
        .spawn(move || {
            let mut input: WantedPort<MidiInputConnection<()>> = WantedPort::new(midi_in_port);
            let mut output: WantedPort<()> = WantedPort::new(midi_out_port);
            // The lists last sent to the UI. Opening the modal enumerates the
            // ports itself, so only a change needs announcing.
            let mut last_ports: Option<(Vec<String>, Vec<String>)> = None;
            // The first poll opens the saved ports right away.
            let mut next_poll = Duration::ZERO;
            loop {
                let tick = after(next_poll);
                select! {
                    recv(midi_in_reconnect_rx) -> port_name => {
                        if let Ok(name) = port_name {
                            // Close the old port first, so a failed pick
                            // leaves nothing open rather than the old port
                            // looking like the new one.
                            input.conn = None;
                            input.conn = connect_input(&midi_input_forwarder, &name);
                            input.wanted = Some(name);
                        }
                    }
                    recv(midi_out_reconnect_rx) -> port_name => {
                        if let Ok(name) = port_name {
                            output.conn = connect_output(&name, &midi_out_connection_tx);
                            output.wanted = Some(name);
                        }
                    }
                    recv(tick) -> _ => {
                        next_poll = POLL_INTERVAL;
                        let in_ports = MidiInputForwarder::in_port_names();
                        let out_ports = MidiOutputConnection::out_port_names();

                        if let Some(name) = input.hot_plug(&in_ports) {
                            input.conn = connect_input(&midi_input_forwarder, name);
                        }
                        if let Some(name) = output.hot_plug(&out_ports) {
                            output.conn = connect_output(name, &midi_out_connection_tx);
                        }

                        let ports = (in_ports, out_ports);
                        if last_ports.as_ref() != Some(&ports) {
                            ui_event_tx
                                .send(UiEvent::MidiPortsRefreshed {
                                    in_ports: ports.0.clone(),
                                    out_ports: ports.1.clone(),
                                })
                                .ok();
                            // A hot-plug is no window input event, so an idle
                            // window would not redraw the modal on its own.
                            if let Some(ctx) = repaint_ctx.get() {
                                ctx.request_repaint();
                            }
                            last_ports = Some(ports);
                        }
                    }
                }
            }
        })
        .expect("Failed to spawn midiwatcher thread")
}

/// Opens the input connection on `port_name`, logging a failure.
fn connect_input(
    forwarder: &MidiInputForwarder,
    port_name: &str,
) -> Option<MidiInputConnection<()>> {
    forwarder
        .forward_messages_to_named(port_name)
        .inspect_err(|e| eprintln!("Failed to connect MIDI input '{}': {}", port_name, e))
        .ok()
}

/// Opens `port_name` as the output and hands the connection to the
/// `"midiout"` thread. A failed open hands over a closed connection, so the
/// old port closes either way — as on the input side. `Some` if it opened.
fn connect_output(port_name: &str, tx: &Sender<MidiOutputConnection>) -> Option<()> {
    let mut conn = MidiOutputConnection::new();
    let opened = conn
        .connect_to_named(port_name)
        .inspect_err(|e| eprintln!("Failed to connect MIDI output '{}': {}", port_name, e))
        .ok();
    tx.send(conn).ok();
    opened
}

#[cfg(test)]
mod tests {
    use super::WantedPort;

    fn ports(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn hot_plug_opens_the_wanted_port_while_closed() {
        let mut port: WantedPort<()> = WantedPort::new(Some("Synth".into()));
        let list = ports(&["Other", "Synth"]);
        assert_eq!(port.hot_plug(&list), Some("Synth"));
        port.conn = Some(());
        assert_eq!(port.hot_plug(&list), None);
    }

    #[test]
    fn a_failed_open_is_retried() {
        // Regression: the early `midi_out_connected` store claimed a port
        // that never opened, so hot-plug never retried it.
        let port: WantedPort<()> = WantedPort::new(Some("Synth".into()));
        assert_eq!(port.hot_plug(&ports(&["Synth"])), Some("Synth"));
    }

    #[test]
    fn hot_plug_chases_the_picked_port_not_the_saved_one() {
        // Regression: the watcher kept the startup output name, so after a
        // pick failed, hot-plug reopened the old port.
        let mut port: WantedPort<()> = WantedPort::new(Some("Old".into()));
        port.wanted = Some("New".into());
        assert_eq!(port.hot_plug(&ports(&["Old", "New"])), Some("New"));
    }

    #[test]
    fn nothing_wanted_opens_nothing() {
        let port: WantedPort<()> = WantedPort::new(None);
        assert_eq!(port.hot_plug(&ports(&["Synth"])), None);
    }
}
