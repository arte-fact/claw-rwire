//! The tool seam. A [`Toolbox`] is what the agent loop advertises to the model (the
//! `tools` array) and dispatches calls through. Built-in tools, MCP tools, and (later)
//! SMS tools all become implementors; today's sources are the web MCP server and the
//! workspace coder tools (`coder`).

mod coder;
mod concierge;
pub(crate) mod exec;
mod git_screen;
mod planning;
mod repo;
mod validator;

pub use coder::CoderToolbox;
pub use concierge::{ConciergeToolbox, Delegate};
pub use exec::ExecStrategy;
pub use planning::PlanningToolbox;
pub use repo::RepoToolbox;
pub use validator::{VERDICT, ValidatorToolbox, parse_verdict, parse_verdict_prose};

/// The workspace tool names, in display order — the static half of the selectable catalog.
/// They operate on the agent's workspace (`coder` module), so they're only offered when the
/// agent has one.
pub const WORKSPACE_TOOLS: [&str; 6] = [
    coder::BASH,
    coder::READ,
    coder::LIST,
    coder::FIND,
    coder::WRITE,
    coder::EDIT,
];

use std::sync::Arc;

use serde_json::{Value, json};

use crate::mcp::McpClient;

/// One tool definition advertised to the model (becomes an OpenAI `function` tool).
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

/// The result of running a tool. Errors are folded into `result` text rather than
/// raised — the model sees the message and can recover, and a flaky tool never aborts
/// the turn (matches rust-claw). `ok` records whether it succeeded, for the UI card.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub result: String,
    pub ok: bool,
}

impl ToolOutcome {
    pub fn text(result: impl Into<String>) -> Self {
        Self {
            result: result.into(),
            ok: true,
        }
    }

    pub fn error(result: impl Into<String>) -> Self {
        Self {
            result: result.into(),
            ok: false,
        }
    }
}

/// A source of tools: what to advertise, and how to run a call by name.
pub trait Toolbox: Send + Sync {
    fn defs(&self) -> Vec<ToolDef>;
    fn call(&self, name: &str, arguments: &str) -> ToolOutcome;
}

/// Merge several toolboxes into one: the model sees the union of their tools, and a call
/// dispatches to the first toolbox that advertises that name. Lets a coder run expose the
/// workspace tools *and* the MCP web tools at once.
pub struct CompositeToolbox {
    boxes: Vec<Box<dyn Toolbox>>,
}

impl CompositeToolbox {
    #[must_use]
    pub fn new(boxes: Vec<Box<dyn Toolbox>>) -> Self {
        Self { boxes }
    }
}

impl Toolbox for CompositeToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        self.boxes.iter().flat_map(|tb| tb.defs()).collect()
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        for tb in &self.boxes {
            if tb.defs().iter().any(|def| def.name == name) {
                return tb.call(name, arguments);
            }
        }
        ToolOutcome::error(format!("error: no tool named {name}"))
    }
}

/// Wraps a toolbox, exposing only the tools whose names are in `allowed`. Lets a run
/// honour a per-agent tool selection without each source knowing about it.
pub struct FilteredToolbox {
    inner: Box<dyn Toolbox>,
    allowed: Vec<String>,
}

impl FilteredToolbox {
    #[must_use]
    pub fn new(inner: Box<dyn Toolbox>, allowed: Vec<String>) -> Self {
        Self { inner, allowed }
    }

    fn permits(&self, name: &str) -> bool {
        self.allowed.iter().any(|allowed| allowed == name)
    }
}

impl Toolbox for FilteredToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        self.inner
            .defs()
            .into_iter()
            .filter(|def| self.permits(&def.name))
            .collect()
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        if self.permits(name) {
            self.inner.call(name, arguments)
        } else {
            ToolOutcome::error(format!("error: tool {name} is not enabled for this agent"))
        }
    }
}

/// A toolbox with no tools — used when no MCP server is connected, so a run degrades
/// to a plain single-shot completion.
pub struct EmptyToolbox;

impl Toolbox for EmptyToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call(&self, name: &str, _arguments: &str) -> ToolOutcome {
        ToolOutcome::error(format!("error: no tool named {name}"))
    }
}

/// Wraps the web MCP client as a `Toolbox`. Tool names are bare (one server, no
/// collisions yet); qualification by server arrives with multiple sources.
pub struct McpToolbox {
    client: Arc<McpClient>,
}

impl McpToolbox {
    #[must_use]
    pub fn new(client: Arc<McpClient>) -> Self {
        Self { client }
    }
}

impl Toolbox for McpToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        self.client
            .tools()
            .iter()
            .map(|tool| ToolDef {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: if tool.input_schema.is_null() {
                    json!({ "type": "object" })
                } else {
                    tool.input_schema.clone()
                },
            })
            .collect()
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        match self.client.call(name, args) {
            // A tool-level failure (`isError`) is a failed call even though the RPC succeeded —
            // otherwise the agent treats a backend crash as usable data (the "browser launch
            // failed" search that still read `ok:true`).
            Ok(result) if result.is_error => {
                ToolOutcome::error(format!("error: {name} failed: {}", result.text))
            }
            Ok(result) => ToolOutcome::text(result.text),
            Err(err) => ToolOutcome::error(format!("error: {name} failed: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub toolbox advertising two fixed tools, echoing the call name back.
    struct Stub;

    impl Toolbox for Stub {
        fn defs(&self) -> Vec<ToolDef> {
            ["alpha", "beta"]
                .into_iter()
                .map(|name| ToolDef {
                    name: name.to_owned(),
                    description: String::new(),
                    parameters: json!({}),
                })
                .collect()
        }

        fn call(&self, name: &str, _arguments: &str) -> ToolOutcome {
            ToolOutcome::text(format!("called {name}"))
        }
    }

    #[test]
    fn filtered_toolbox_hides_and_blocks_excluded_tools() {
        let filtered = FilteredToolbox::new(Box::new(Stub), vec!["alpha".to_owned()]);
        let names: Vec<String> = filtered.defs().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["alpha".to_owned()]);

        let allowed = filtered.call("alpha", "{}");
        assert!(allowed.ok && allowed.result == "called alpha");

        let blocked = filtered.call("beta", "{}");
        assert!(!blocked.ok, "excluded tool must be refused");
        assert!(blocked.result.contains("not enabled"));
    }

    #[test]
    fn composite_routes_by_membership() {
        let a = FilteredToolbox::new(Box::new(Stub), vec!["alpha".to_owned()]);
        let b = FilteredToolbox::new(Box::new(Stub), vec!["beta".to_owned()]);
        let composite = CompositeToolbox::new(vec![Box::new(a), Box::new(b)]);
        assert_eq!(composite.defs().len(), 2);
        assert_eq!(composite.call("beta", "{}").result, "called beta");
        assert!(!composite.call("gamma", "{}").ok);
    }
}
