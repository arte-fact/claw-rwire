//! The concierge's tools. The user talks only to the concierge (the one chat agent); for
//! work that isn't a quick reply it `ask`s a workspace **owner**, which runs a planning turn
//! and queues task cards. `ask` is fire-and-forget: it posts `Command::Ask` and returns
//! immediately — the board (and a later notify turn) are the feedback channel, so a long
//! planning turn never blocks the chat reply.
//!
//! The two read-only tools came from the concierge's own harness review ("I'm flying a bit
//! blind between delegations"): `board_status` pulls the live board on demand — the same
//! digest the chat turn injects, but mid-turn and whenever it wants — and `read_file`
//! inspects a workspace file without delegating. Neither mutates anything: the concierge
//! stays hands-off by design.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::bridge::{self, Command};
use crate::db;
use crate::workspace;

use super::{ToolDef, ToolOutcome, Toolbox};

pub const ASK: &str = "ask";
pub const BOARD_STATUS: &str = "board_status";
pub const READ_FILE: &str = "read_file";
pub const REPO_STATUS: &str = "repo_status";
pub const REPO_LOG: &str = "repo_log";
pub const REPO_DIFF: &str = "repo_diff";

/// Cap what a read lands in the conversation — the model needs the gist, not a megabyte.
const READ_CAP_CHARS: usize = 8_000;

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
    /// The data dir (central.db + workspaces/) — the read-only tools open their own
    /// connection per call; the toolbox runs on a worker thread, never on the bridge's.
    data_dir: PathBuf,
}

impl ConciergeToolbox {
    #[must_use]
    pub fn new(
        origin_chat: impl Into<String>,
        workspaces: Vec<Delegate>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            origin_chat: origin_chat.into(),
            workspaces,
            data_dir,
        }
    }

    fn board_status(&self) -> ToolOutcome {
        let conn = match db::open(&self.data_dir.join("central.db")) {
            Ok(conn) => conn,
            Err(err) => return ToolOutcome::error(format!("error: open registry: {err}")),
        };
        match bridge::status_digest(&conn) {
            Some(digest) => ToolOutcome::text(digest),
            None => ToolOutcome::text(
                "All lanes are quiet — nothing running, awaiting review, or queued.",
            ),
        }
    }

    /// Resolve the `workspace` argument to its directory, refusing unknown ids — shared by
    /// every workspace-scoped read tool.
    fn workspace_root(&self, args: &Value) -> Result<PathBuf, String> {
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !self.workspaces.iter().any(|w| w.id == workspace) {
            return Err(format!("error: no workspace {workspace:?}"));
        }
        Ok(workspace::dir_for(&self.data_dir, workspace))
    }

    /// The read-only git surface, workspace-parameterized (the chat has no task anchor —
    /// diffs default to uncommitted-vs-HEAD). Bodies shared with the task-side `RepoToolbox`.
    fn repo_tool(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let result = self.workspace_root(&args).and_then(|root| {
            let repo = args
                .get("repo")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match name {
                REPO_STATUS => Ok(super::repo::status_text(&root)),
                REPO_LOG => super::repo::log_for(&root, repo, args.get("count")),
                REPO_DIFF => super::repo::diff_for(&root, repo),
                _ => Err(format!("error: no tool named {name}")),
            }
        });
        match result {
            Ok(text) => ToolOutcome::text(text),
            Err(err) => ToolOutcome::error(err),
        }
    }

    fn read_file(&self, arguments: &str) -> ToolOutcome {
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if path.is_empty() {
            return ToolOutcome::error("error: read_file needs a path");
        }
        if !self.workspaces.iter().any(|w| w.id == workspace) {
            return ToolOutcome::error(format!("error: no workspace {workspace:?}"));
        }
        let root = workspace::dir_for(&self.data_dir, workspace);
        match workspace::ops::read_text(&root, path) {
            Ok(content) => {
                if content.chars().count() > READ_CAP_CHARS {
                    let head: String = content.chars().take(READ_CAP_CHARS).collect();
                    ToolOutcome::text(format!("{head}\n…[truncated — the file continues]"))
                } else {
                    ToolOutcome::text(content)
                }
            }
            Err(err) => ToolOutcome::error(format!("error: {err}")),
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
        let mut defs = vec![ToolDef {
            name: BOARD_STATUS.to_owned(),
            description: "Look up the live board state right now — which task is running \
                          (and whose), what awaits the user's review, and queue depths per \
                          workspace. Present state, not memory: use this instead of recalling \
                          older messages."
                .to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
        }];
        let path_schema = json!({
            "type": "string",
            "description": "Workspace-relative file path, e.g. docs/plan.md",
        });
        defs.push(ToolDef {
            name: READ_FILE.to_owned(),
            description: "Read one file from a workspace (read-only, capped) — inspect a doc \
                          or artifact without delegating a task for it."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace_schema.clone(),
                    "path": path_schema,
                },
                "required": ["workspace", "path"]
            }),
        });
        let repo_schema = json!({
            "type": "string",
            "description": "Which repo in that workspace: omit for the workspace meta-repo \
                            (AGENT.md, docs/, skills/), or projects/<name> (bare <name> works).",
        });
        defs.push(ToolDef {
            name: REPO_STATUS.to_owned(),
            description: "Git status of a workspace's repos (meta-repo + each project): \
                          branch, latest commit, uncommitted changes. Every successful task \
                          pass is committed automatically — use this to see work land."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "workspace": workspace_schema.clone() },
                "required": ["workspace"]
            }),
        });
        defs.push(ToolDef {
            name: REPO_LOG.to_owned(),
            description: "Recent commits of one repo in a workspace, newest first — each \
                          task pass is one commit with Task:/Run: trailers."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace_schema.clone(),
                    "repo": repo_schema.clone(),
                    "count": { "type": "integer", "description": "How many commits (default 10, max 30)." }
                },
                "required": ["workspace"]
            }),
        });
        defs.push(ToolDef {
            name: REPO_DIFF.to_owned(),
            description: "Uncommitted changes in one repo of a workspace (stat + new files \
                          + capped patch) — what the current task has touched but not yet \
                          landed."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace_schema.clone(),
                    "repo": repo_schema,
                },
                "required": ["workspace"]
            }),
        });
        defs.push(ToolDef {
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
        });
        defs
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        match name {
            BOARD_STATUS => return self.board_status(),
            READ_FILE => return self.read_file(arguments),
            REPO_STATUS | REPO_LOG | REPO_DIFF => return self.repo_tool(name, arguments),
            ASK => {}
            other => return ToolOutcome::error(format!("error: no tool named {other}")),
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
        toolbox_at(std::env::temp_dir())
    }

    fn toolbox_at(data_dir: std::path::PathBuf) -> ConciergeToolbox {
        ConciergeToolbox::new(
            "chat-1",
            vec![Delegate {
                id: "site".to_owned(),
                name: "Website".to_owned(),
                brief: "The marketing site.".to_owned(),
            }],
            data_dir,
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
    fn defs_expose_status_read_repo_and_ask() {
        let defs = toolbox().defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                BOARD_STATUS,
                READ_FILE,
                REPO_STATUS,
                REPO_LOG,
                REPO_DIFF,
                ASK
            ]
        );
        let enum_values = &defs[5].parameters["properties"]["workspace"]["enum"];
        assert!(enum_values.to_string().contains("site"));
    }

    #[test]
    fn repo_tools_are_workspace_scoped_and_degrade_without_git() {
        let dir = std::env::temp_dir().join(format!("claw-concierge-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspaces").join("site")).expect("mkdir");
        let tools = toolbox_at(dir);

        // A git-less workspace reads as "not yet", not an error.
        let status = tools.call(REPO_STATUS, r#"{"workspace":"site"}"#);
        assert!(status.ok, "{}", status.result);
        assert!(status.result.contains("no git repos"), "{}", status.result);

        // Unknown workspaces are refused like read_file.
        let ghost = tools.call(REPO_LOG, r#"{"workspace":"ghost"}"#);
        assert!(!ghost.ok && ghost.result.contains("no workspace"));
    }

    #[test]
    fn read_file_is_jailed_to_a_known_workspace_and_caps_output() {
        let dir = std::env::temp_dir().join(format!("claw-concierge-{}", std::process::id()));
        let ws = dir.join("workspaces").join("site");
        std::fs::create_dir_all(&ws).expect("mkdir");
        std::fs::write(ws.join("notes.md"), "hello from the workspace").expect("write");
        let tools = toolbox_at(dir);

        let ok = tools.call(READ_FILE, r#"{"workspace":"site","path":"notes.md"}"#);
        assert!(ok.ok, "{}", ok.result);
        assert_eq!(ok.result, "hello from the workspace");

        // Unknown workspace and path escapes are refused.
        assert!(
            !tools
                .call(READ_FILE, r#"{"workspace":"ghost","path":"x"}"#)
                .ok
        );
        let escape = tools.call(
            READ_FILE,
            r#"{"workspace":"site","path":"../../etc/passwd"}"#,
        );
        assert!(!escape.ok, "{}", escape.result);
    }
}
