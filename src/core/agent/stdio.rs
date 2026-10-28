//! The `stev --mcp` process (`360-mcp-server.md` decision 3): no window, no
//! audio, no log. It reads MCP's JSON-RPC from stdin, answers what it can
//! itself (`protocol.rs`), and forwards each tool call to the running app
//! over the socket, one JSON line each way. It never launches the app: with
//! nothing listening, every tool says so.

use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::core::paths::{AGENT_SOCKET_FILE_NAME, config_dir};
use crate::core::settings::load_settings;

use super::protocol::handle_line;
use super::{AgentReply, AgentRequest};

/// The command-line flag that makes the binary the MCP server.
pub(crate) const MCP_FLAG: &str = "--mcp";

/// Every tool's answer while nothing listens on the socket.
const NOT_RUNNING: &str = "Stev isn't running. Open it and try again.";

/// The same, when the settings say the bridge is off (decision 4).
const BRIDGE_OFF: &str = "Stev isn't running, or Allow AI agents is off in its settings \
                          (Settings → MIDI → AGENTS). Open Stev, turn it on and try again.";

/// What a call that got no answer in time says: the sequencer thread was
/// slow, or another client's call was being served (decision 17).
const NO_ANSWER: &str =
    "Stev didn't answer in time. It may be busy, or another agent may be connected; try again.";

/// How long the process waits for the app's answer to one call: past the
/// app's own 5 s wait on the sequencer thread.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The app's socket: `agent.sock` in the config directory
/// (`~/Library/Application Support/stev` on macOS).
pub(crate) fn socket_path() -> PathBuf {
    config_dir().join(AGENT_SOCKET_FILE_NAME)
}

/// Removes the socket, if there is one: on quit, and when the setting is
/// turned off.
pub(crate) fn remove_socket() {
    let _ = fs::remove_file(socket_path());
}

/// Runs the MCP server until stdin closes.
pub(crate) fn run_mcp_server() {
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Some(reply) = handle_line(&line, &mut call) else {
            continue;
        };
        if writeln!(stdout, "{reply}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
}

/// Forwards `request` to the app on a connection of its own, closed once
/// the answer is in, and returns the answer or why there is none. The app
/// serves one connection at a time, so holding one between calls would
/// keep every other client (a second Claude session) waiting for as long
/// as this one stays open.
fn call(request: AgentRequest) -> AgentReply {
    let Ok(line) = serde_json::to_string(&request) else {
        return AgentReply::error("Couldn't encode the call.".to_owned());
    };
    let Some(mut stream) = connect() else {
        let text = if load_settings().agent_bridge {
            NOT_RUNNING
        } else {
            BRIDGE_OFF
        };
        return AgentReply::error(text.to_owned());
    };
    match exchange(&mut stream, &line) {
        Ok(reply) => reply,
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            AgentReply::error(NO_ANSWER.to_owned())
        }
        Err(_) => AgentReply::error(NOT_RUNNING.to_owned()),
    }
}

/// A connection to the app's socket, or `None` with nothing listening.
fn connect() -> Option<UnixStream> {
    let stream = UnixStream::connect(socket_path()).ok()?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
    Some(stream)
}

/// Sends one request line and reads one answer line.
fn exchange(stream: &mut UnixStream, line: &str) -> io::Result<AgentReply> {
    stream.write_all(format!("{line}\n").as_bytes())?;
    let mut reply = String::new();
    if BufReader::new(stream).read_line(&mut reply)? == 0 {
        return Err(ErrorKind::UnexpectedEof.into());
    }
    serde_json::from_str(&reply).map_err(io::Error::other)
}
