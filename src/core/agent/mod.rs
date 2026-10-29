//! The agent bridge: the MCP server that lets an AI client the user already
//! has read and edit the open project (`360-mcp-server.md`).
//!
//! Two processes take part. The client launches `stev --mcp`, which opens no
//! window: it speaks MCP on stdin/stdout (`protocol.rs`, `stdio.rs`) and
//! forwards each tool call, one JSON line each way, to the running app over
//! a user-only Unix socket. In the app, the `"agent-bridge"` thread
//! (`threads/agent_bridge.rs`) takes each call off the socket and hands it
//! to the `"sequencer"` thread as `SequencerCommand::Agent`, whose handler
//! answers it with [`answer`] from the sequencer's own state, recording a
//! write's edit as one undo step. Nothing is locked and nothing on the
//! render path changes.
//!
//! - `units.rs` — beats, positions, note names and note rows; no ticks
//!   leave it.
//! - `reads.rs` — the reading tools.
//! - `checks.rs` — `check_notes`, a dry run of a write.
//! - `writes.rs` — the writing tools and the cleanup of what they write.
//! - `tools.rs` (Unix) — the tool list and the server instructions.
//! - `protocol.rs` (Unix) — the JSON-RPC methods.
//! - `stdio.rs` (Unix) — the `--mcp` process and the socket's path.
//!
//! The app answers on every platform; the `--mcp` side and the thread are
//! Unix-only, so off Unix nothing ever asks.

mod checks;
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
mod writes;

#[cfg(unix)]
pub(crate) use stdio::{MCP_FLAG, remove_socket, run_mcp_server, socket_path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::sequencer::{Sequencer, SequencerEdit};

/// One tool call, as the `--mcp` process forwards it to the app: a JSON
/// line on the socket.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentRequest {
    /// The tool's name, one of `tools::TOOL_NAMES`.
    pub(crate) tool: String,
    /// The call's arguments, an object.
    #[serde(default)]
    pub(crate) arguments: Value,
}

/// A tool's answer, as the app sends it back: text for the model, which
/// reads an error's text too.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
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

/// A write's edit, built but not yet applied: the `"sequencer"` thread's
/// handler records it as one undo step, shows `footer` and answers with
/// `reply` (`360-mcp-server.md` decision 11).
pub(crate) struct AgentWrite {
    /// The edit.
    pub(crate) edit: SequencerEdit,
    /// What the model reads once it is recorded.
    pub(crate) reply: String,
    /// The footer's line, `Agent: 2 new clips, bars 17–20`.
    pub(crate) footer: String,
}

/// What a call comes to: an answer, or a write to record first.
pub(crate) enum AgentAnswer {
    /// An answer; nothing changes.
    Reply(AgentReply),
    /// An edit to record, then its reply. Boxed: an edit is large.
    Write(Box<AgentWrite>),
}

impl From<AgentWrite> for AgentAnswer {
    fn from(write: AgentWrite) -> Self {
        AgentAnswer::Write(Box::new(write))
    }
}

/// Answers `request` from `sequencer`, on the `"sequencer"` thread.
/// `project_name` is the open project's, `None` before it is first saved.
/// Changes nothing itself: a write comes back as its edit.
pub(crate) fn answer(
    request: &AgentRequest,
    sequencer: &Sequencer,
    project_name: Option<&str>,
) -> AgentAnswer {
    let args = &request.arguments;
    let read = |text: String| AgentAnswer::Reply(AgentReply::ok(text));
    match request.tool.as_str() {
        "get_context" => Ok(read(reads::get_context(sequencer))),
        "get_project" => Ok(read(reads::get_project(sequencer, project_name))),
        "get_timeline" => reads::get_timeline(sequencer, args).map(read),
        "get_notes" => reads::get_notes(sequencer, args).map(read),
        "get_capture" => reads::get_capture(sequencer, args).map(read),
        "check_notes" => checks::check_notes(sequencer, args).map(read),
        tool @ ("create_clips" | "replace_clip_notes" | "set_tempo") => {
            writes::write(tool, sequencer, args)
        }
        tool => Err(format!("Stev has no tool {tool}.")),
    }
    .unwrap_or_else(|text| AgentAnswer::Reply(AgentReply::error(text)))
}
