//! One JSON-RPC 2.0 conversation over MCP's newline-delimited stdio transport.
//! Blocking (`std::io`), generic over the IO so a child process drives it in
//! production and an in-memory buffer drives it in tests. Ported from rust-claw's
//! async `mcp::conn`, kept synchronous — the bridge is thread-based, not tokio.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{McpError, McpTool, ToolResult};

/// MCP protocol revision we advertise in `initialize`. Servers echo the version
/// they'll actually use; most proceed even on a minor mismatch.
const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Serialize)]
struct Notification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ReplyError>,
}

#[derive(Deserialize)]
struct ReplyError {
    code: i64,
    message: String,
}

pub struct Conn<W, R> {
    writer: W,
    reader: R,
    next_id: u64,
}

impl<W: Write, R: BufRead> Conn<W, R> {
    pub fn new(writer: W, reader: R) -> Self {
        Self {
            writer,
            reader,
            next_id: 1,
        }
    }

    fn write_message(&mut self, message: &impl Serialize) -> Result<(), McpError> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Sends a request and reads replies until the one matching our id arrives,
    /// skipping interleaved notifications or log lines.
    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;

        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(McpError::Closed);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let reply: Reply = serde_json::from_str(trimmed)?;
            if reply.id != Some(id) {
                continue;
            }
            if let Some(error) = reply.error {
                return Err(McpError::Rpc {
                    code: error.code,
                    message: error.message,
                });
            }
            return Ok(reply.result.unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        self.write_message(&Notification {
            jsonrpc: "2.0",
            method,
            params: None,
        })
    }

    /// The MCP startup dance: `initialize` request, then the `initialized` notice.
    pub fn initialize(&mut self, client_name: &str) -> Result<(), McpError> {
        self.request(
            "initialize",
            Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": client_name, "version": env!("CARGO_PKG_VERSION") },
            })),
        )?;
        self.notify("notifications/initialized")
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", Some(json!({})))?;
        let tools = result
            .get("tools")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        Ok(serde_json::from_value(tools)?)
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolResult, McpError> {
        let result = self.request(
            "tools/call",
            Some(json!({ "name": name, "arguments": arguments })),
        )?;
        // MCP marks a *tool-level* failure (as opposed to a protocol error) with `isError:true`
        // and puts the message in `content`. Honour it: a headless-browser crash the web server
        // reports this way must reach the agent as a failed call, not as an ok result whose text
        // happens to read "browser launch failed" — the agent would take that as data and march on.
        Ok(ToolResult {
            text: flatten_content(&result),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Reduces an MCP `tools/call` result to the text an LLM can consume: text blocks
/// joined by newlines, non-text blocks (images, …) noted but not inlined.
fn flatten_content(result: &Value) -> String {
    let Some(items) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut parts = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some(other) => parts.push(format!("[{other} content omitted]")),
            None => {}
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Canned server replies (ids 1..3), consumed in order as the client issues
    /// initialize → tools/list → tools/call.
    fn canned() -> Cursor<Vec<u8>> {
        let lines = [
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"search","description":"web search","inputSchema":{"type":"object"}},{"name":"fetch"}]}}"#,
            r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"ran with rust"}]}}"#,
        ];
        Cursor::new(lines.join("\n").into_bytes())
    }

    #[test]
    fn initialize_list_and_call_round_trip() {
        let mut conn = Conn::new(Vec::new(), canned());
        conn.initialize("claw-test").expect("initialize");

        let tools = conn.list_tools().expect("list");
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["search", "fetch"]
        );
        assert_eq!(tools[0].description, "web search");

        let out = conn
            .call_tool("search", json!({ "q": "rust" }))
            .expect("call");
        assert_eq!(out.text, "ran with rust");
        assert!(!out.is_error, "a plain result is not an error");
    }

    #[test]
    fn call_tool_flags_is_error_results() {
        // A tool-level failure the server marks with `isError:true` (e.g. the web server's
        // headless browser dying) must surface as an error, not a normal result.
        let reply = r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"browser launch failed"}]}}"#;
        let mut conn = Conn::new(Vec::new(), Cursor::new(reply.as_bytes().to_vec()));
        let out = conn.call_tool("search", json!({})).expect("call");
        assert!(out.is_error, "isError:true must be reported");
        assert_eq!(out.text, "browser launch failed");
    }

    #[test]
    fn request_surfaces_rpc_errors() {
        let reply = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such tool"}}"#;
        let mut conn = Conn::new(Vec::new(), Cursor::new(reply.as_bytes().to_vec()));
        assert!(matches!(
            conn.list_tools(),
            Err(McpError::Rpc { code: -32601, .. })
        ));
    }

    #[test]
    fn flatten_joins_text_and_notes_other_blocks() {
        let result = json!({"content": [
            {"type": "text", "text": "line one"},
            {"type": "image", "data": "..."},
            {"type": "text", "text": "line two"},
        ]});
        assert_eq!(
            flatten_content(&result),
            "line one\n[image content omitted]\nline two"
        );
    }
}
