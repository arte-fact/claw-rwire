//! The `preview` tool (P2): a producer starts (or restarts) its project's preview server and gets
//! the URI back — the debug loop as a tool call, not an improvised `bash cargo run &` that dies
//! with the turn. One lane's supervisor entry, driven by the same [`preview`](crate::bridge::preview)
//! start/stop the harness uses at request boundaries.

use std::path::PathBuf;

use serde_json::json;

use super::exec::ExecStrategy;
use super::{ToolDef, ToolOutcome, Toolbox};

/// Bound to one lane: the workspace id (for the kind + preview-override lookup), the lane root, the
/// exec strategy (host or the lane sandbox), and the registry dir (to open a fresh connection).
pub struct PreviewToolbox {
    workspace_id: String,
    root: PathBuf,
    exec: ExecStrategy,
    data_dir: PathBuf,
}

impl PreviewToolbox {
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

impl Toolbox for PreviewToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "preview".to_owned(),
            description: "Start (or restart) a live preview server for this branch and return its \
                          URL. Runs the project's own start command (an rwire app: `cargo run`; a \
                          Node app: `npm start`; or a per-project override) from your working tree, \
                          reusing the build the checks already produced. Use it to see the running \
                          branch — load the URL, reproduce a failure, fix, and re-run `preview` to \
                          restart. Safe to call repeatedly; it replaces the previous instance in \
                          place. Pass `project` to pick a specific project when the workspace has \
                          more than one; omit it to preview the first previewable project."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Which project to preview, e.g. `projects/site` or `site`. \
                                        Optional — defaults to the first previewable project."
                    }
                }
            }),
        }]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        if name != "preview" {
            return ToolOutcome::error(format!("error: no tool named {name}"));
        }
        let Ok(conn) = crate::db::open(&self.data_dir.join("central.db")) else {
            return ToolOutcome::error("error: preview could not open the registry");
        };
        let target = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("project").and_then(|p| p.as_str()).map(str::to_owned));
        match crate::bridge::preview::start_for_lane(
            &conn,
            &self.workspace_id,
            &self.root,
            &self.exec,
            target.as_deref(),
        ) {
            // Whether up or still building, the process is spawned — report and let the agent
            // proceed; only a resolve/spawn failure is a tool error.
            Ok(detail) => ToolOutcome::text(detail),
            Err(err) => ToolOutcome::error(format!("error: {err}")),
        }
    }
}
