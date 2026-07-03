//! Read-only git tools over the workspace's repos (the meta-repo + each `projects/<name>`):
//! `repo_status` / `repo_log` / `repo_diff`. Strictly inspection — the harness owns every
//! write (commits per pass, restart resets, branches later with Requests). The differentiator
//! over bash-git is the **task anchor**: `since:"task-start"` diffs against the base commit
//! recorded in `task_repos` at launch — state the model can't reach any other way.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::db;
use crate::gitops;

use super::{ToolDef, ToolOutcome, Toolbox};

pub const REPO_STATUS: &str = "repo_status";
pub const REPO_LOG: &str = "repo_log";
pub const REPO_DIFF: &str = "repo_diff";

/// Cap on a diff patch fed back to the model — the gist, not a megabyte.
const DIFF_CAP_CHARS: usize = 8_000;
/// Caps on list-shaped output (log lines, per-repo status lines).
const LOG_MAX: usize = 30;
const LOG_DEFAULT: usize = 10;
const STATUS_LINES_PER_REPO: usize = 30;

/// The workspace-scoped read-only git toolbox. Built by the scheduler with the running
/// task's id so `repo_diff` can anchor on the task's recorded pre-task base.
pub struct RepoToolbox {
    workspace: PathBuf,
    anchor: Option<TaskAnchor>,
}

/// Where to look up the task's recorded repo bases (`task_repos`) — the toolbox runs on a
/// worker thread and opens its own connection per call, never the bridge's.
struct TaskAnchor {
    data_dir: PathBuf,
    task_id: String,
}

impl RepoToolbox {
    /// A toolbox anchored to a running task — `since:"task-start"` resolves via `task_repos`.
    #[must_use]
    pub fn for_task(workspace: PathBuf, data_dir: PathBuf, task_id: String) -> Self {
        Self {
            workspace,
            anchor: Some(TaskAnchor { data_dir, task_id }),
        }
    }

    /// A workspace-scoped toolbox with no task anchor (request-level review, G3) — diffs
    /// default to uncommitted-vs-HEAD; explicit commit ids still work.
    #[must_use]
    pub fn for_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace,
            anchor: None,
        }
    }

    /// The diff base a `since` argument names: `None` = the working tree against HEAD.
    fn resolve_base(&self, repo: &str, since: &str) -> Result<Option<String>, String> {
        match since {
            "head" => Ok(None),
            "task-start" => {
                let anchor = self.anchor.as_ref().ok_or(
                    "error: since:\"task-start\" is only available inside a task run — \
                     use \"head\" or a commit id",
                )?;
                let conn = db::open(&anchor.data_dir.join("central.db"))
                    .map_err(|err| format!("error: open registry: {err}"))?;
                let bases = db::task_repos::bases_for(&conn, &anchor.task_id)
                    .map_err(|err| format!("error: read task bases: {err}"))?;
                bases
                    .into_iter()
                    .find(|base| base.repo == repo)
                    .map(|base| Some(base.base_commit))
                    .ok_or_else(|| {
                        "error: this repo has no recorded task base (it appeared mid-task) — \
                         use \"head\" or a commit id"
                            .to_owned()
                    })
            }
            commit if is_commit_hex(commit) => Ok(Some(commit.to_owned())),
            other => Err(format!(
                "error: since must be \"task-start\", \"head\", or a commit id — got {other:?}"
            )),
        }
    }
}

impl Toolbox for RepoToolbox {
    fn defs(&self) -> Vec<ToolDef> {
        let repo_schema = json!({
            "type": "string",
            "description": "Which repo: omit or \"\" for the workspace meta-repo, else \
                            projects/<name> (a bare project name also works).",
        });
        vec![
            ToolDef {
                name: REPO_STATUS.to_owned(),
                description: "Git status of every repo in the workspace (the meta-repo and \
                              each projects/<name>): branch, latest commit, uncommitted \
                              changes. The harness commits your work automatically after \
                              each successful pass — inspect with this, never run git \
                              write commands yourself."
                    .to_owned(),
                parameters: json!({ "type": "object", "properties": {} }),
            },
            ToolDef {
                name: REPO_LOG.to_owned(),
                description: "Recent commits of one workspace repo, newest first (short \
                              hash, date, author, subject)."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "repo": repo_schema.clone(),
                        "count": { "type": "integer", "description": "How many commits (default 10, max 30)." }
                    }
                }),
            },
            ToolDef {
                name: REPO_DIFF.to_owned(),
                description: "What changed in one workspace repo: diff of the working tree \
                              against a base — since:\"task-start\" (default when available) \
                              shows everything this task changed so far; \"head\" shows only \
                              uncommitted changes; a commit id diffs against that commit. \
                              Returns the stat, new files, and a capped patch."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "repo": repo_schema,
                        "since": { "type": "string", "description": "\"task-start\", \"head\", or a commit id." },
                        "path": { "type": "string", "description": "Limit the diff to one file or directory." }
                    }
                }),
            },
        ]
    }

    fn call(&self, name: &str, arguments: &str) -> ToolOutcome {
        let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let result = match name {
            REPO_STATUS => Ok(status_text(&self.workspace)),
            REPO_LOG => self.log(&args),
            REPO_DIFF => self.diff(&args),
            other => Err(format!("error: no tool named {other}")),
        };
        match result {
            Ok(text) => ToolOutcome::text(text),
            Err(err) => ToolOutcome::error(err),
        }
    }
}

impl RepoToolbox {
    fn log(&self, args: &Value) -> Result<String, String> {
        let repo = resolve_repo(&self.workspace, arg(args, "repo"))?;
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .map_or(LOG_DEFAULT, |n| (n as usize).clamp(1, LOG_MAX));
        log_text(&self.workspace, &repo, count)
    }

    fn diff(&self, args: &Value) -> Result<String, String> {
        let repo = resolve_repo(&self.workspace, arg(args, "repo"))?;
        let default_since = if self.anchor.is_some() {
            "task-start"
        } else {
            "head"
        };
        let since = arg(args, "since").unwrap_or(default_since);
        let base = self.resolve_base(&repo, since)?;
        diff_text(&self.workspace, &repo, base.as_deref(), arg(args, "path"))
    }
}

/// A non-empty, trimmed string argument.
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// A model-supplied commit id: hex only, 4–40 chars — anything else is refused so a value
/// can never reach git looking like a flag (`--output=…`) or an arbitrary ref expression.
fn is_commit_hex(value: &str) -> bool {
    (4..=40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve the model's `repo` argument to a known repo rel: `""`/`"."`/`"meta"`/`"workspace"`
/// mean the meta-repo; `projects/<name>` and bare `<name>` resolve to that project.
fn resolve_repo(workspace: &Path, raw: Option<&str>) -> Result<String, String> {
    let known = gitops::repos(workspace);
    let raw = raw.unwrap_or("").trim_matches('/');
    if matches!(raw, "" | "." | "meta" | "workspace") {
        return if known.iter().any(String::is_empty) {
            Ok(String::new())
        } else {
            Err(
                "error: the workspace has no git repos yet — they appear when a task first \
                 runs"
                    .to_owned(),
            )
        };
    }
    let candidates = [raw.to_owned(), format!("projects/{raw}")];
    for candidate in candidates {
        if known.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "error: no repo {raw:?} — available: {}",
        if known.is_empty() {
            "(none yet)".to_owned()
        } else {
            known
                .iter()
                .map(|rel| display_name(rel))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ))
}

fn display_name(rel: &str) -> String {
    if rel.is_empty() {
        "\"\" (workspace meta-repo)".to_owned()
    } else {
        rel.to_owned()
    }
}

/// Chat-side `repo_log`: resolve the repo argument and clamp the count — for callers with a
/// workspace but no task anchor (the concierge).
pub(crate) fn log_for(
    root: &Path,
    repo: Option<&str>,
    count: Option<&Value>,
) -> Result<String, String> {
    let repo = resolve_repo(root, repo)?;
    let count = count
        .and_then(Value::as_u64)
        .map_or(LOG_DEFAULT, |n| (n as usize).clamp(1, LOG_MAX));
    log_text(root, &repo, count)
}

/// Chat-side `repo_diff`: uncommitted changes vs HEAD (no task anchor to diff from).
pub(crate) fn diff_for(root: &Path, repo: Option<&str>) -> Result<String, String> {
    let repo = resolve_repo(root, repo)?;
    diff_text(root, &repo, None, None)
}

/// Every repo's branch, HEAD, and working-tree state — the `repo_status` body. Shared with
/// the concierge's workspace-parameterized variant.
pub fn status_text(workspace: &Path) -> String {
    let repos = gitops::repos(workspace);
    if repos.is_empty() {
        return "(no git repos in this workspace yet — they appear when a task first runs)"
            .to_owned();
    }
    let mut out = Vec::new();
    for rel in repos {
        let Ok(dir) = gitops::repo_dir(workspace, &rel) else {
            continue;
        };
        let branch = gitops::current_branch(&dir).unwrap_or_else(|_| "?".to_owned());
        let head = gitops::head(&dir)
            .map(|commit| commit.chars().take(12).collect::<String>())
            .unwrap_or_else(|_| "(no commits)".to_owned());
        out.push(format!("{} — {branch} @ {head}", display_name(&rel)));
        match gitops::status_porcelain(&dir) {
            Ok(status) if status.is_empty() => out.push("  clean".to_owned()),
            Ok(status) => {
                let lines: Vec<&str> = status.lines().collect();
                for line in lines.iter().take(STATUS_LINES_PER_REPO) {
                    out.push(format!("  {line}"));
                }
                if lines.len() > STATUS_LINES_PER_REPO {
                    out.push(format!(
                        "  … ({} more changed paths)",
                        lines.len() - STATUS_LINES_PER_REPO
                    ));
                }
            }
            Err(err) => out.push(format!("  (status failed: {err})")),
        }
    }
    out.join("\n")
}

/// The `repo_log` body. Shared with the concierge variant.
pub fn log_text(workspace: &Path, repo: &str, count: usize) -> Result<String, String> {
    let dir = gitops::repo_dir(workspace, repo).map_err(|err| format!("error: {err}"))?;
    match gitops::log_oneline(&dir, count) {
        Ok(log) if log.is_empty() => Ok("(no commits yet)".to_owned()),
        Ok(log) => Ok(log),
        Err(err) => Err(format!("error: {err}")),
    }
}

/// The `repo_diff` body: stat, then new (untracked) files, then the patch, capped. Shared
/// with the concierge variant (which never passes a task-start base).
pub fn diff_text(
    workspace: &Path,
    repo: &str,
    base: Option<&str>,
    path: Option<&str>,
) -> Result<String, String> {
    let dir = gitops::repo_dir(workspace, repo).map_err(|err| format!("error: {err}"))?;
    let stat = gitops::diff(&dir, base, path, true).map_err(|err| format!("error: {err}"))?;
    let patch = gitops::diff(&dir, base, path, false).map_err(|err| format!("error: {err}"))?;
    let untracked = gitops::untracked(&dir).unwrap_or_default();
    let mut out = String::new();
    if stat.is_empty() && untracked.is_empty() {
        return Ok("(no changes)".to_owned());
    }
    if !stat.is_empty() {
        out.push_str(&stat);
    }
    if !untracked.is_empty() {
        out.push_str("\n\nnew (untracked) files:\n");
        out.push_str(&untracked);
    }
    if !patch.is_empty() {
        out.push_str("\n\n");
        if patch.chars().count() > DIFF_CAP_CHARS {
            let head: String = patch.chars().take(DIFF_CAP_CHARS).collect();
            out.push_str(&head);
            out.push_str("\n…[patch truncated — narrow with `path` to see more]");
        } else {
            out.push_str(&patch);
        }
    }
    Ok(out.trim_start().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("claw-repo-tool-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    /// A file-backed registry with one task whose bases are snapshotted for `root` — the
    /// anchored-toolbox fixture (`since:"task-start"` opens the db by path).
    fn anchored(root: &Path, data_dir: &Path) -> RepoToolbox {
        let conn = db::open(&data_dir.join("central.db")).expect("db");
        crate::db::endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        crate::db::workspaces::create(&conn, "ws", "W").expect("ws");
        crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "t",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("task");
        let errors = gitops::snapshot_task_bases(&conn, root, "t1");
        assert_eq!(errors, Vec::<String>::new());
        RepoToolbox::for_task(root.to_path_buf(), data_dir.to_path_buf(), "t1".to_owned())
    }

    #[test]
    fn status_covers_every_repo_and_its_dirt() {
        let root = scratch("status");
        let data = scratch("status-db");
        write(&root, "projects/app/src/main.rs", "fn main() {}");
        let tools = anchored(&root, &data);
        write(&root, "docs/new.md", "draft");

        let out = tools.call(REPO_STATUS, "{}");
        assert!(out.ok, "{}", out.result);
        assert!(out.result.contains("workspace meta-repo"), "{}", out.result);
        assert!(out.result.contains("projects/app"), "{}", out.result);
        assert!(out.result.contains("docs/new.md"), "{}", out.result);
        assert!(out.result.contains("clean"), "{}", out.result);
    }

    #[test]
    fn diff_defaults_to_the_task_start_anchor() {
        let root = scratch("diff");
        let data = scratch("diff-db");
        write(&root, "docs/plan.md", "v1\n");
        let tools = anchored(&root, &data);
        // The task edits a tracked file and adds a new one; both must show without args.
        write(&root, "docs/plan.md", "v1\nv2 added by the task\n");
        write(&root, "docs/new.md", "brand new");

        let out = tools.call(REPO_DIFF, "{}");
        assert!(out.ok, "{}", out.result);
        assert!(
            out.result.contains("+v2 added by the task"),
            "{}",
            out.result
        );
        assert!(
            out.result.contains("new (untracked) files:") && out.result.contains("docs/new.md"),
            "{}",
            out.result
        );

        // since:"head" sees the same here (nothing committed since launch)…
        let head = tools.call(REPO_DIFF, r#"{"since":"head"}"#);
        assert!(head.ok && head.result.contains("+v2"), "{}", head.result);
        // …and a bogus `since` is refused, hex-only.
        let evil = tools.call(REPO_DIFF, r#"{"since":"--output=/tmp/x"}"#);
        assert!(
            !evil.ok && evil.result.contains("commit id"),
            "{}",
            evil.result
        );
    }

    #[test]
    fn repo_argument_resolves_bare_names_and_rejects_unknown() {
        let root = scratch("resolve");
        let data = scratch("resolve-db");
        write(&root, "projects/app/main.rs", "x");
        let tools = anchored(&root, &data);

        let log = tools.call(REPO_LOG, r#"{"repo":"app"}"#);
        assert!(log.ok, "bare project name resolves: {}", log.result);
        let missing = tools.call(REPO_LOG, r#"{"repo":"ghost"}"#);
        assert!(
            !missing.ok && missing.result.contains("available"),
            "{}",
            missing.result
        );
    }

    #[test]
    fn log_lists_the_adopt_commit() {
        let root = scratch("log");
        let data = scratch("log-db");
        write(&root, "docs/x.md", "x");
        let tools = anchored(&root, &data);
        let out = tools.call(REPO_LOG, "{}");
        assert!(out.ok, "{}", out.result);
        assert!(
            out.result.contains("adopt pre-task workspace state")
                || out.result.contains("initialize workspace meta-repo"),
            "{}",
            out.result
        );
    }
}
