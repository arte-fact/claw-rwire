//! Jailed `git` shell helpers (G0 of `docs/requests.md`). The harness — never the agents —
//! versions the workspace: the meta-repo owns the workspace's own material and ignores
//! `projects/` wholesale; each `projects/<name>` is a sovereign repo. Every invocation is
//! workspace-jailed via [`repo_dir`], runs synchronously (bridge/worker threads only — never
//! the async runtime), and commits are authored `claw <agent-id@claw>` so history shows
//! which agent made each pass.

mod sweep;
mod task_tree;
#[cfg(test)]
mod tests;

pub use sweep::{commit_pass, ensure_meta_repo};
pub use task_tree::{reset_task_tree, snapshot_task_bases};

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workspace;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git could not be run: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git {args}: {stderr}")]
    Command { args: String, stderr: String },
    #[error("repo path escapes the workspace")]
    Jail,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve a repo rel to its directory under the workspace root — `""` (the meta-repo) is the
/// root itself, anything else (`projects/<name>`) must jail-resolve inside the root.
pub fn repo_dir(root: &Path, repo: &str) -> Result<PathBuf, GitError> {
    workspace::jail(root, repo).map_err(|_| GitError::Jail)
}

/// Run `git -C dir <args>`, returning trimmed stdout. Never prompts (a missing credential
/// fails fast instead of hanging a worker).
fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::Command {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `dir` is the top level of a git repo (its own `.git`, not an ancestor's).
#[must_use]
pub fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// The current HEAD commit id. Errs on a repo with no commits yet.
pub fn head(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "HEAD"])
}

/// The checked-out branch name (`HEAD` when detached). Test-only until the request entity
/// (G1) starts cutting branches.
#[cfg(test)]
pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Whether the working tree differs from HEAD (`status --porcelain` non-empty; untracked
/// files count, ignored ones don't).
pub fn is_dirty(dir: &Path) -> Result<bool, GitError> {
    Ok(!git(dir, &["status", "--porcelain"])?.is_empty())
}

/// Stage everything and commit as `agent` (see [`commit`]). The caller guards dirtiness —
/// the commit allows empty so a fresh repo's initial commit always lands.
pub fn commit_all(dir: &Path, agent: Option<&str>, message: &str) -> Result<String, GitError> {
    git(dir, &["add", "-A"])?;
    commit(dir, agent, message)
}

/// Commit what's staged, authored `claw <agent@claw>` (identity passed per-invocation so no
/// global config is touched, and signing is off so a host gpg setup can't fail the harness).
pub fn commit(dir: &Path, agent: Option<&str>, message: &str) -> Result<String, GitError> {
    let email = format!("{}@claw", email_local_part(agent));
    git(
        dir,
        &[
            "-c",
            "user.name=claw",
            "-c",
            &format!("user.email={email}"),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
    )?;
    head(dir)
}

/// Reset the working tree to `commit` and drop untracked files (`clean -fd` respects
/// `.gitignore`, so build caches and — in the meta-repo — `projects/` survive).
pub fn reset_hard(dir: &Path, commit: &str) -> Result<(), GitError> {
    git(dir, &["reset", "--hard", commit])?;
    git(dir, &["clean", "-fd"])?;
    Ok(())
}

/// An agent id as an email local part: alphanumerics, `-`, `_`, `.` kept, everything else
/// dropped; empty (or no agent — harness housekeeping commits) falls back to `claw`.
fn email_local_part(agent: Option<&str>) -> String {
    let cleaned: String = agent
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if cleaned.is_empty() {
        "claw".to_owned()
    } else {
        cleaned
    }
}
