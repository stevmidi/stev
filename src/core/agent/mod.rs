//! The agent bridge: the MCP server that lets an AI client the user already
//! has read the open project (`360-mcp-server.md`).
//!
//! Two processes take part. The client launches `stev --mcp`, which opens no
//! window: it speaks MCP on stdin/stdout (`protocol.rs`, `stdio.rs`) and
//! forwards each tool call, one JSON line each way, to the running app over
//! a user-only Unix socket. In the app, the `"agent-bridge"` thread
//! (`threads/agent_bridge.rs`) takes each call off the socket and hands it
//! to the `"sequencer"` thread as `SequencerCommand::Agent`, whose handler
//! answers it with [`answer`] from the sequencer's own state. Nothing is
//! locked and nothing on the render path changes.
//!
//! - `units.rs` — beats, positions, note names and note rows; no ticks
//!   leave it.
//! - `reads.rs` — the reading tools.
//! - `tools.rs` (Unix) — the tool list and the server instructions.
//! - `protocol.rs` (Unix) — the JSON-RPC methods.
//! - `stdio.rs` (Unix) — the `--mcp` process and the socket's path.
//!
//! The app answers on every platform; the `--mcp` side and the thread are
//! Unix-only, so off Unix nothing ever asks.

// The MCP side runs only as `stev --mcp`, which talks to the app over a
// Unix socket; elsewhere there is none (`360-mcp-server.md` decision 5).
#[cfg(unix)]
mod protocol;
mod reads;
#[cfg(unix)]
mod stdio;
#[cfg(unix)]
mod tools;
mod units;

#[cfg(unix)]
pub(crate) use stdio::{MCP_FLAG, remove_socket, run_mcp_server, socket_path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::sequencer::Sequencer;

/// One tool call, as the `--mcp` process forwards it to the app: a JSON
/// line on the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRequest {
    /// The tool's name, one of `tools::TOOL_NAMES`.
    pub(crate) tool: String,
    /// The call's arguments, an object.
    #[serde(default)]
    pub(crate) arguments: Value,
}

/// A tool's answer, as the app sends it back: text for the model, which
/// reads an error's text too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentReply {
    /// What the model reads.
    pub(crate) text: String,
    /// Whether the call failed; `text` then says why.
    #[serde(default)]
    pub(crate) is_error: bool,
}

impl AgentReply {
    /// A successful answer.
    pub(crate) fn ok(text: String) -> Self {
        AgentReply {
            text,
            is_error: false,
        }
    }

    /// A failed one, `text` saying why.
    pub(crate) fn error(text: String) -> Self {
        AgentReply {
            text,
            is_error: true,
        }
    }
}

/// Answers `request` from `sequencer`, on the `"sequencer"` thread.
/// `project_name` is the open project's, `None` before it is first saved.
pub(crate) fn answer(
    request: &AgentRequest,
    sequencer: &Sequencer,
    project_name: Option<&str>,
) -> AgentReply {
    let args = &request.arguments;
    let result = match request.tool.as_str() {
        "get_context" => Ok(reads::get_context(sequencer)),
        "get_project" => Ok(reads::get_project(sequencer, project_name)),
        "get_timeline" => reads::get_timeline(sequencer, args),
        "get_notes" => reads::get_notes(sequencer, args),
        tool => Err(format!("Stev has no tool {tool}.")),
    };
    match result {
        Ok(text) => AgentReply::ok(text),
        Err(text) => AgentReply::error(text),
    }
}
