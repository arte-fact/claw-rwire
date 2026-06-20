//! A minimal MCP client: spawns one stdio JSON-RPC server (the web toolbox —
//! fetch/search/screenshot/interact, backed by headless Chromium) and exposes its
//! tools to the agent's tool loop. Single hardcoded server for now, mirroring
//! rust-claw; generalising to many servers with per-agent config is a later step.

mod conn;

use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;

use conn::Conn;

/// The client name we present to the server, and the default binary to spawn. The
/// command is overridable via `CLAW_WEB_MCP_CMD` so an operator can point at a build
/// elsewhere on disk.
pub const WEB_SERVER_NAME: &str = "claw-web";
pub const DEFAULT_WEB_COMMAND: &str = "mcp-web-search-stdio";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("the mcp server closed the connection")]
    Closed,
    #[error("could not spawn mcp server: {0}")]
    Spawn(String),
    #[error("mcp server exposed no stdio pipes")]
    NoPipes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

/// A live connection to one stdio MCP server. Holds the child process open and
/// serializes calls behind a mutex (the web server drives a single Chromium, so
/// concurrent calls would contend anyway).
pub struct McpClient {
    server_name: String,
    tools: Vec<McpTool>,
    conn: Mutex<Conn<ChildStdin, BufReader<ChildStdout>>>,
    _child: Child,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_name", &self.server_name)
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Spawns `command`, performs the MCP handshake, and caches its tool list. `envs`
    /// are extra environment variables set on the child (e.g. an augmented
    /// `LD_LIBRARY_PATH` so a headless-Chromium server finds its libraries). Any
    /// failure (missing binary, bad handshake) is returned so the caller can run
    /// without the server rather than aborting boot.
    pub fn spawn(
        server_name: &str,
        command: &str,
        args: &[&str],
        envs: &[(String, String)],
    ) -> Result<Self, McpError> {
        let mut process = Command::new(command);
        process
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in envs {
            process.env(key, value);
        }
        let mut child = process
            .spawn()
            .map_err(|err| McpError::Spawn(format!("{command}: {err}")))?;

        let stdin = child.stdin.take().ok_or(McpError::NoPipes)?;
        let stdout = child.stdout.take().ok_or(McpError::NoPipes)?;
        let mut conn = Conn::new(stdin, BufReader::new(stdout));

        conn.initialize(server_name)?;
        let tools = conn.list_tools()?;

        Ok(Self {
            server_name: server_name.to_owned(),
            tools,
            conn: Mutex::new(conn),
            _child: child,
        })
    }

    #[must_use]
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Calls a tool by its bare name and returns the flattened text. A poisoned lock
    /// (a previous call panicked) is reported as a closed connection.
    pub fn call(&self, tool: &str, arguments: Value) -> Result<String, McpError> {
        let mut conn = self.conn.lock().map_err(|_| McpError::Closed)?;
        conn.call_tool(tool, arguments)
    }
}
