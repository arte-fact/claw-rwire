//! Workspace repo topology: the meta-repo at the root (ignoring `projects/` wholesale) plus
//! one sovereign repo per `projects/<name>`. `commit_pass` is the harness commit step — after
//! a successful producer/stage pass it commits every dirty repo with `Task:`/`Run:` trailers.

use std::path::Path;

use super::requests::{RequestRef, ensure_on_request_branch};
use super::{GitError, commit, commit_all, git, head, is_dirty, is_repo, repo_dir};

/// The repo rel of the workspace meta-repo (the workspace root itself).
pub const META_REPO: &str = "";

/// The meta-repo must never track `projects/` — a project with its own `.git` would appear
/// as a gitlink (a pinned SHA, not files) and silently stop being captured.
const META_IGNORE_LINE: &str = "projects/";

/// Seeded into a project repo at birth: the usual build/dependency caches, plus `.env` so a
/// secrets file never rides a harness commit.
const PROJECT_IGNORE: &str = "target/\nnode_modules/\ndist/\nbuild/\n__pycache__/\n.venv/\n.env\n";

/// Ensure the workspace root is the meta-repo: `git init` + initial commit capturing the
/// existing workspace material (lazy-init for workspaces that predate G0). Always re-asserts
/// the `projects/` ignore line — without it a sweep would record project gitlinks.
pub fn ensure_meta_repo(root: &Path) -> Result<(), GitError> {
    std::fs::create_dir_all(root)?;
    ensure_ignore_line(root)?;
    if is_repo(root) {
        return Ok(());
    }
    git(root, &["init", "--initial-branch", "main"])?;
    commit_all(root, None, "claw: initialize workspace meta-repo")?;
    Ok(())
}

/// Append the `projects/` line to the meta `.gitignore` when it's missing (creating the file
/// if need be) — idempotent, and it survives an agent rewriting the file.
fn ensure_ignore_line(root: &Path) -> Result<(), GitError> {
    let path = root.join(".gitignore");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.lines().any(|line| line.trim() == META_IGNORE_LINE) {
        return Ok(());
    }
    let newline = if current.is_empty() || current.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(&path, format!("{current}{newline}{META_IGNORE_LINE}\n"))?;
    Ok(())
}

/// Birth a sovereign project repo: `git init` on `main`, seed `.gitignore`, commit **only**
/// that seed (so a repo born mid-task has a base that predates the pass's work), then cut
/// and check out `develop` — the git flow integration branch pass commits land on.
pub fn ensure_project_repo(dir: &Path) -> Result<(), GitError> {
    if is_repo(dir) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "--initial-branch", "main"])?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, PROJECT_IGNORE)?;
    }
    git(dir, &["add", ".gitignore"])?;
    commit(dir, None, "claw: initialize project repo")?;
    git(dir, &["checkout", "-b", "develop"])?;
    Ok(())
}

/// Every repo under the workspace, as rels: the meta-repo (when initialized) first, then
/// each `projects/<name>` that is one, sorted by name.
#[must_use]
pub fn repos(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if is_repo(root) {
        out.push(META_REPO.to_owned());
    }
    let Ok(entries) = std::fs::read_dir(root.join("projects")) else {
        return out;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir() && is_repo(&entry.path()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    out.extend(names.into_iter().map(|name| format!("projects/{name}")));
    out
}

/// Git-ify what exists: the meta-repo plus a sovereign repo for every `projects/<name>` dir
/// that lacks one. Per-repo failures are collected, not fatal — one broken repo must not
/// block the others.
pub fn ensure_workspace_repos(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(err) = ensure_meta_repo(root) {
        errors.push(format!("meta-repo: {err}"));
    }
    let Ok(entries) = std::fs::read_dir(root.join("projects")) else {
        return errors;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir()
            && !is_repo(&path)
            && let Err(err) = ensure_project_repo(&path)
        {
            errors.push(format!("{}: {err}", path.display()));
        }
    }
    errors
}

/// One harness commit made by [`commit_pass`].
pub struct PassCommit {
    pub repo: String,
    pub commit: String,
    /// HEAD before this commit — the repo's pre-pass state, recorded as the task's base when
    /// the repo first appears mid-task. `None` only for a repo with no prior commit at all
    /// (an agent-made bare `git init`), where there is no pre-pass state to reset to.
    pub base_before: Option<String>,
    /// The branch point when this pass cut the request's branch in this repo (its first
    /// touch, G1) — the caller records it as the `request_repos` base. `None` otherwise.
    pub request_base: Option<String>,
}

/// The harness commit step: after a successful producer/stage pass, commit every dirty repo
/// (meta + projects) with the structured message + `Request:`/`Task:`/`Run:` trailers,
/// birthing repos for project dirs that appeared during the pass. A request-bound pass lands
/// on the request's branch, cut from `develop` at the repo's first touch — a branch failure
/// is collected but never loses work (the commit falls back to wherever HEAD is). Failed
/// passes never reach this — the revise loop's context must match the (deliberately dirty)
/// disk. Returns the commits made and any per-repo errors.
pub fn commit_pass(
    root: &Path,
    agent: Option<&str>,
    subject: &str,
    task_id: &str,
    run_id: &str,
    request: Option<RequestRef<'_>>,
) -> (Vec<PassCommit>, Vec<String>) {
    let mut errors = ensure_workspace_repos(root);
    let mut commits = Vec::new();
    let request_trailer = request
        .map(|req| format!("Request: {}\n", req.id))
        .unwrap_or_default();
    let message = format!("{subject}\n\n{request_trailer}Task: {task_id}\nRun: {run_id}");
    for repo in repos(root) {
        let dir = match repo_dir(root, &repo) {
            Ok(dir) => dir,
            Err(err) => {
                errors.push(format!("{repo}: {err}"));
                continue;
            }
        };
        match is_dirty(&dir) {
            Ok(false) => continue,
            Ok(true) => {
                let request_base = match request {
                    Some(req) => match ensure_on_request_branch(&dir, req.branch) {
                        Ok(base) => base,
                        Err(err) => {
                            errors.push(format!("{repo}: request branch: {err}"));
                            None
                        }
                    },
                    None => None,
                };
                let base_before = head(&dir).ok();
                match commit_all(&dir, agent, &message) {
                    Ok(commit) => commits.push(PassCommit {
                        repo,
                        commit,
                        base_before,
                        request_base,
                    }),
                    Err(err) => errors.push(format!("{repo}: {err}")),
                }
            }
            Err(err) => errors.push(format!("{repo}: {err}")),
        }
    }
    (commits, errors)
}
