//! The `"agent-bridge"` thread (Unix): the app's end of the agent bridge
//! (`360-mcp-server.md`, `core::agent`). While *Allow AI agents* is on it
//! listens on the user-only socket, serves one connection at a time, and
//! hands each tool call to the `"sequencer"` thread as
//! [`SequencerCommand::Agent`], waiting up to 5 s for the answer. Off, it
//! holds no socket and sleeps on its control channel.

use std::fs::{self, Permissions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded};
use log::{error, warn};

use crate::core::agent::{AgentReply, AgentRequest, remove_socket, socket_path};
use crate::core::sequencer::SequencerCommand;

/// How often the thread looks at its control channel while it waits on a
/// connection or a call: how soon turning the setting off takes effect.
const CONTROL_POLL: Duration = Duration::from_millis(250);

/// How long one call waits on the `"sequencer"` thread (decision 7).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts the `"agent-bridge"` thread, listening from the start when
/// `enabled`. Each `bool` on `control_rx` turns it on or off (the settings
/// row); the thread ends when the channel closes.
pub(crate) fn start_agent_bridge_thread(
    enabled: bool,
    control_rx: Receiver<bool>,
    sequencer_command_tx: Sender<SequencerCommand>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("agent-bridge".to_string())
        .spawn(move || {
            // A socket left by a crash goes, whatever the setting.
            remove_socket();
            let mut enabled = enabled;
            loop {
                if !enabled {
                    match control_rx.recv() {
                        Ok(on) => enabled = on,
                        Err(_) => return,
                    }
                    continue;
                }
                match listen() {
                    Some(listener) => {
                        enabled = serve(&listener, &control_rx, &sequencer_command_tx);
                        drop(listener);
                        remove_socket();
                    }
                    // Can't listen: wait for the setting to change.
                    None => enabled = false,
                }
            }
        })
        .expect("Failed to spawn agent-bridge thread")
}

/// Binds the socket, readable and writable by the user alone, in
/// non-blocking mode so the control channel is read between connections.
fn listen() -> Option<UnixListener> {
    let path = socket_path();
    remove_socket();
    let bound = UnixListener::bind(&path).and_then(|listener| {
        fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(listener)
    });
    match bound {
        Ok(listener) => Some(listener),
        Err(e) => {
            error!("agent bridge: can't listen on {}: {e}", path.display());
            None
        }
    }
}

/// Takes connections one at a time until the setting goes off (returns
/// `false`) or the control channel closes (also `false`).
fn serve(
    listener: &UnixListener,
    control_rx: &Receiver<bool>,
    sequencer_command_tx: &Sender<SequencerCommand>,
) -> bool {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if !serve_connection(stream, control_rx, sequencer_command_tx) {
                    return false;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if let Some(false) | None = control(control_rx, CONTROL_POLL) {
                    return false;
                }
            }
            Err(e) => {
                warn!("agent bridge: accept failed: {e}");
                if let Some(false) | None = control(control_rx, CONTROL_POLL) {
                    return false;
                }
            }
        }
    }
}

/// What the control channel says within `wait`: `Some(true)` to stay on
/// (nothing sent counts as that), `Some(false)` to turn off, `None` when it
/// has closed. Only the latest of several values counts.
fn control(control_rx: &Receiver<bool>, wait: Duration) -> Option<bool> {
    let mut on = match control_rx.recv_timeout(wait) {
        Ok(on) => on,
        Err(e) if e.is_timeout() => return Some(true),
        Err(_) => return None,
    };
    while let Ok(next) = control_rx.try_recv() {
        on = next;
    }
    Some(on)
}

/// Answers one connection's calls, a JSON line each way, until it closes
/// (returns `true`) or the setting goes off (`false`).
fn serve_connection(
    stream: UnixStream,
    control_rx: &Receiver<bool>,
    sequencer_command_tx: &Sender<SequencerCommand>,
) -> bool {
    // An accepted stream inherits the listener's non-blocking mode on BSD.
    let set_up = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(CONTROL_POLL)))
        .and_then(|()| stream.try_clone());
    let mut writer = match set_up {
        Ok(writer) => writer,
        Err(e) => {
            warn!("agent bridge: can't use a connection: {e}");
            return true;
        }
    };
    let mut reader = BufReader::new(stream);
    // Kept across read timeouts: a line that arrives in pieces is read on.
    let mut line = String::new();
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => return true,
            Ok(_) => {
                let reply = forward(&line, sequencer_command_tx);
                line.clear();
                let Ok(text) = serde_json::to_string(&reply) else {
                    continue;
                };
                if writer.write_all(format!("{text}\n").as_bytes()).is_err() {
                    return true;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                match control(control_rx, Duration::ZERO) {
                    Some(true) => {}
                    Some(false) | None => return false,
                }
            }
            Err(_) => return true,
        }
    }
}

/// Hands one request line to the `"sequencer"` thread and waits for its
/// answer.
fn forward(line: &str, sequencer_command_tx: &Sender<SequencerCommand>) -> AgentReply {
    let request: AgentRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => return AgentReply::error(format!("Stev couldn't read the call: {e}")),
    };
    let (reply_tx, reply_rx) = bounded(1);
    let sent = sequencer_command_tx.send(SequencerCommand::Agent {
        request,
        reply: reply_tx,
    });
    if sent.is_err() {
        return AgentReply::error("Stev is shutting down.".to_owned());
    }
    reply_rx.recv_timeout(ANSWER_TIMEOUT).unwrap_or_else(|_| {
        AgentReply::error("Stev is busy and didn't answer in time; try again.".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;

    use crossbeam_channel::unbounded;

    use super::*;

    #[test]
    fn a_connection_gets_the_sequencers_answer_and_ends_when_switched_off() {
        let (client, server) = UnixStream::pair().unwrap();
        let (command_tx, command_rx) = unbounded();
        let (control_tx, control_rx) = unbounded();
        // The `"sequencer"` thread, answering with the tool's name.
        thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                if let SequencerCommand::Agent { request, reply } = command {
                    reply.send(AgentReply::ok(request.tool)).ok();
                }
            }
        });
        let served = thread::spawn(move || serve_connection(server, &control_rx, &command_tx));

        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        writer
            .write_all(b"{\"tool\":\"get_context\",\"arguments\":{}}\nnot json\n")
            .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let reply: AgentReply = serde_json::from_str(&line).unwrap();
        assert_eq!(reply, AgentReply::ok("get_context".to_owned()));
        line.clear();
        reader.read_line(&mut line).unwrap();
        let reply: AgentReply = serde_json::from_str(&line).unwrap();
        assert!(reply.is_error, "{line}");

        control_tx.send(false).unwrap();
        assert!(!served.join().unwrap());
    }
}
