//! The concierge's delegation tool. The user talks only to the concierge (the one chat
//! agent); for work that isn't a quick reply it `ask`s a workspace **owner**, which runs a
//! planning turn and queues task cards. `ask` is fire-and-forget: it posts `Command::Ask`
//! and returns immediately — the board (and a later notify turn) are the feedback channel,
//! so a long planning turn never blocks the chat reply.

use serde_json::{Value, json};

use crate::bridge::{self, Command};

use super::{ToolDef, ToolOutcome, Toolbox};

pub const ASK: &str = "ask";

/// One workspace the concierge can delegate to.
#[derive(Debug, Clone)]
pub struct Delegate {
    pub id: String,
    pub name: String,
    pub brief: String,
}

/// The concierge toolbox: a single `ask` tool, bound to the chat it speaks in (`origin_chat`)
/// and the set of workspaces it can delegate to.
pub struct ConciergeToolbox {
    origin_chat: String,
    workspaces: Vec<Delegate>,
}

impl ConciergeToolbox {
    #[must_use]
    pub fn new(origin_chat: impl Into<String>, workspaces: Vec<Delegate>) -> Self {
        Self {
            origin_chat: origin_chat.into(),
            workspaces,
        }
    }
}

impl Toolbox for ConciergeToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        let ids: Vec<String> = self.workspaces.iter().map(|w| w.id.clone()).collect();
        let roster: Vec<String> = self
            .workspaces
            .iter()
            .map(|w| {
                let brief = w.brief.lines().next().unwrap_or("").trim();
                if brief.is_empty() {
                    format!("- {} ({})", w.id, w.name)
                } else {
                    format!("- {} ({}) — {brief}", w.id, w.name)
                }
            })
            .collect();
        let mut workspace_schema = json!({
            "type": "string",
            "description": "Which workspace's owner should handle this.",
        });
        if !ids.is_empty() {
            workspace_schema["enum"] = json!(ids);
        }
        vec![ToolDef {
            name: ASK.to_owned(),
            description: format!(
                "Delegate work to a workspace owner, who will plan it into tasks. Use this for \
                 anything multi-step, recurring, or tied to a project rather than a quick \
                 answer.\nWorkspaces:\n{}",
                roster.join("\n")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace_schema,
                    "request": {
                        "type": "string",
                        "description": "What you want done, in your own words."
                    }
                },
                "required": ["workspace", "request"]
            }),
        }]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        if name != ASK {
            return ToolOutcome::error(format!("error: no tool named {name}"));
        }
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let request = args
            .get("request")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if request.is_empty() {
            return ToolOutcome::error("error: ask needs a request");
        }
        let Some(target) = self.workspaces.iter().find(|w| w.id == workspace) else {
            return ToolOutcome::error(format!("error: no workspace {workspace:?} to delegate to"));
        };
        bridge::send(Command::Ask {
            workspace: target.id.clone(),
            request: request.to_owned(),
            origin_chat: self.origin_chat.clone(),
        });
        ToolOutcome::text(format!(
            "Delegated to the {} workspace — its owner will plan this into tasks; \
             I'll report back here when they finish.",
            target.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolbox() -> ConciergeToolbox {
        ConciergeToolbox::new(
            "chat-1",
            vec![Delegate {
                id: "site".to_owned(),
                name: "Website".to_owned(),
                brief: "The marketing site.".to_owned(),
            }],
        )
    }

    #[test]
    fn ask_accepts_a_known_workspace() {
        let out = toolbox().call(ASK, r#"{"workspace":"site","request":"refresh the copy"}"#);
        assert!(out.ok, "{}", out.result);
        assert!(out.result.contains("Website"));
    }

    #[test]
    fn ask_rejects_an_unknown_workspace() {
        let out = toolbox().call(ASK, r#"{"workspace":"ghost","request":"x"}"#);
        assert!(!out.ok);
        assert!(out.result.contains("no workspace"));
    }

    #[test]
    fn ask_requires_a_request() {
        let out = toolbox().call(ASK, r#"{"workspace":"site","request":"  "}"#);
        assert!(!out.ok);
    }

    #[test]
    fn defs_expose_a_workspace_enum() {
        let defs = toolbox().defs();
        assert_eq!(defs.len(), 1);
        let enum_values = &defs[0].parameters["properties"]["workspace"]["enum"];
        assert!(enum_values.to_string().contains("site"));
    }
}
