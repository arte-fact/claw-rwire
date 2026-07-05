//! The `checks` tool (P1b): run the project's own machine checks on demand — the *same*
//! per-repo, kind-bound runner the harness runs after every producer pass. One source of truth
//! for machine judgement: a producer can pre-empt the review by running it first (the cheapest
//! review round), and a validator can confirm "builds and tests clean" with the harness's exact
//! recipe instead of guessing an ad-hoc `bash cargo test`.

use std::path::PathBuf;

use serde_json::json;

use super::exec::ExecStrategy;
use super::{ToolDef, ToolOutcome, Toolbox};

/// Runs the lane's per-repo CI. Bound to one lane: the workspace id (for the kind + override
/// lookup), the lane root, the exec strategy (host or the lane sandbox), and the registry dir.
pub struct ChecksToolbox {
    workspace_id: String,
    root: PathBuf,
    exec: ExecStrategy,
    data_dir: PathBuf,
}

impl ChecksToolbox {
    #[must_use]
    pub fn new(workspace_id: String, root: PathBuf, exec: ExecStrategy, data_dir: PathBuf) -> Self {
        Self {
            workspace_id,
            root,
            exec,
            data_dir,
        }
    }
}

impl Toolbox for ChecksToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "checks".to_owned(),
            description: "Run this project's machine checks — the harness's own per-repo CI \
                          (cargo fmt --check, clippy -D warnings, and cargo test for Rust \
                          projects) across every repo in the workspace. Returns each check's \
                          pass/fail with the failure output. These are the exact checks that \
                          judge your pass before review — run them to catch problems first."
                .to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
        }]
    }

    fn call(&self, name: &str, _arguments: &str) -> ToolOutcome {
        if name != "checks" {
            return ToolOutcome::error(format!("error: no tool named {name}"));
        }
        let Ok(conn) = crate::db::open(&self.data_dir.join("central.db")) else {
            return ToolOutcome::error("error: checks could not open the registry");
        };
        let repos = crate::gitops::repos(&self.root);
        let outcomes =
            crate::ci::run_for_repos(&conn, &self.workspace_id, &self.root, &self.exec, &repos);
        if outcomes.is_empty() {
            return ToolOutcome::text(
                "No machine checks are configured for this project (no cargo project and no \
                 `checks` override).",
            );
        }
        let mut body = String::new();
        for outcome in &outcomes {
            let mark = if outcome.ok { "✓" } else { "✗" };
            let where_ = if outcome.repo.is_empty() {
                outcome.check.clone()
            } else {
                format!("{} ({})", outcome.check, outcome.repo)
            };
            body.push_str(&format!("{mark} {where_}\n"));
            if !outcome.ok && !outcome.tail.is_empty() {
                body.push_str(&format!("{}\n", outcome.tail));
            }
        }
        // A red result is a failed call, so the agent reads it as "not clean" rather than data.
        if crate::ci::all_green(&outcomes) {
            ToolOutcome::text(body)
        } else {
            ToolOutcome::error(body)
        }
    }
}
