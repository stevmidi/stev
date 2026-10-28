//! The MCP surface the `--mcp` process speaks on stdin/stdout: JSON-RPC 2.0,
//! one message per line (MCP's stdio transport), and only the methods tools
//! need — `initialize`, `ping`, `tools/list`, `tools/call`, and the
//! notifications, which get no answer. Hand-written over `serde_json`
//! rather than an SDK (`360-mcp-server.md` decision 6). Pure: a tool call
//! goes to the `call` the caller passes, which forwards it to the app.

use serde_json::{Value, json};

use super::tools::{INSTRUCTIONS, TOOL_NAMES, tool_list};
use super::{AgentReply, AgentRequest};

/// The protocol versions the server knows, oldest first. It answers a
/// client asking for one of them with that one, and any other with the
/// newest.
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// JSON-RPC's code for a line that isn't JSON.
const PARSE_ERROR: i64 = -32700;

/// JSON-RPC's code for JSON that isn't a request.
const INVALID_REQUEST: i64 = -32600;

/// JSON-RPC's code for a method the server doesn't have.
const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC's code for a request whose params are wrong.
const INVALID_PARAMS: i64 = -32602;

/// Answers one line from the client, or `None` for a notification (or a
/// blank line). `call` answers a `tools/call` for a known tool.
pub(crate) fn handle_line(
    line: &str,
    call: &mut impl FnMut(AgentRequest) -> AgentReply,
) -> Option<String> {
    if line.trim().is_empty() {
        return None;
    }
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Some(error(&Value::Null, PARSE_ERROR, &e.to_string())),
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response from the client to a request this server never sends,
        // or something that isn't JSON-RPC at all.
        let id = message.get("id")?;
        return Some(error(id, INVALID_REQUEST, "expected a request"));
    };
    // A notification (`notifications/initialized`, a cancellation) asks
    // for nothing back.
    let id = message.get("id")?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let answer = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => tools_call(&params, call),
        _ => Err((METHOD_NOT_FOUND, format!("no method {method}"))),
    };
    Some(match answer {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        Err((code, text)) => error(id, code, &text),
    })
}

/// The `initialize` result: the protocol version, a tools capability, who
/// the server is and its instructions.
fn initialize_result(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|asked| PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "stev", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// A `tools/call`: the tool's answer as one text block, flagged as an error
/// when it is one, so the model reads why. An unknown tool is an error the
/// model reads too, not a protocol error.
fn tools_call(
    params: &Value,
    call: &mut impl FnMut(AgentRequest) -> AgentReply,
) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((INVALID_PARAMS, "tools/call needs a name".to_owned()));
    };
    let reply = if TOOL_NAMES.contains(&name) {
        call(AgentRequest {
            tool: name.to_owned(),
            arguments: params.get("arguments").cloned().unwrap_or(json!({})),
        })
    } else {
        AgentReply::error(format!("Stev has no tool {name}."))
    };
    Ok(json!({
        "content": [{ "type": "text", "text": reply.text }],
        "isError": reply.is_error,
    }))
}

/// A JSON-RPC error response.
fn error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `handle_line` with a `call` that echoes the tool and its arguments,
    /// parsed back.
    fn answer(line: &str) -> Option<Value> {
        let mut call = |request: AgentRequest| {
            AgentReply::ok(format!("{} {}", request.tool, request.arguments))
        };
        handle_line(line, &mut call).map(|text| serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn initialize_names_the_server_and_agrees_a_version() {
        let reply = answer(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        )
        .unwrap();
        assert_eq!(reply["id"], 1);
        let result = &reply["result"];
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["serverInfo"]["name"], "stev");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(
            result["instructions"]
                .as_str()
                .unwrap()
                .contains("get_context")
        );

        let newer = answer(
            r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(newer["id"], "a");
        assert_eq!(newer["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn notifications_and_blank_lines_get_no_answer() {
        assert_eq!(
            answer(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(answer("   "), None);
    }

    #[test]
    fn a_tool_call_is_forwarded_and_wrapped_as_text() {
        let reply = answer(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_notes","arguments":{"from":"9"}}}"#,
        )
        .unwrap();
        let result = &reply["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], r#"get_notes {"from":"9"}"#);
    }

    #[test]
    fn an_unknown_tool_is_an_error_the_model_reads() {
        let reply = answer(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delete_everything"}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
    }

    #[test]
    fn protocol_errors_carry_their_codes() {
        let bad = answer("{not json").unwrap();
        assert_eq!(bad["error"]["code"], PARSE_ERROR);
        assert_eq!(bad["id"], Value::Null);

        let unknown = answer(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#).unwrap();
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);

        let nameless = answer(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#);
        assert_eq!(nameless.unwrap()["error"]["code"], INVALID_PARAMS);

        let ping = answer(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#).unwrap();
        assert_eq!(ping["result"], json!({}));
    }

    #[test]
    fn tools_list_lists_the_tools() {
        let reply = answer(r#"{"jsonrpc":"2.0","id":6,"method":"tools/list"}"#).unwrap();
        assert_eq!(
            reply["result"]["tools"].as_array().unwrap().len(),
            TOOL_NAMES.len()
        );
    }
}
