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

pub use sweep::{commit_pass, ensure_meta_repo, repos};
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
/// fails fast instead of hanging a worker), and runs hermetically:
///
/// - `core.hooksPath=/dev/null` on every invocation — `.git/hooks` sits *inside* the
///   workspace jail, so an agent (or a hostile cloned repo) can write a hook; without this,
///   the harness's own commit sweep would execute that code on the bridge thread.
/// - The host user's global/system git config is cut off (aliases, credential helpers, a
///   redirected hooksPath) — identity is passed per-commit, so nothing ambient is needed.
fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// The checked-out branch name (`HEAD` when detached).
pub fn current_branch(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// `status --porcelain` verbatim — one line per changed/untracked path, empty when clean.
/// `-uall` expands untracked directories to their files (`?? docs/` hides what's inside).
pub fn status_porcelain(dir: &Path) -> Result<String, GitError> {
    git(dir, &["status", "--porcelain", "-uall"])
}

/// Recent commits, one line each (`<short> <date> <author> <subject>`), newest first.
pub fn log_oneline(dir: &Path, count: usize) -> Result<String, GitError> {
    git(
        dir,
        &[
            "--no-pager",
            "log",
            "--date=short",
            "--format=%h %ad %an %s",
            "-n",
            &count.to_string(),
        ],
    )
}

/// `git diff` of the working tree against `base` (against HEAD when `None`), optionally
/// scoped to one `path` — always passed after `--`, so a path can never be read as a flag.
/// `stat` selects the summary form. Callers validate `base` (hex only) before passing it.
pub fn diff(
    dir: &Path,
    base: Option<&str>,
    path: Option<&str>,
    stat: bool,
) -> Result<String, GitError> {
    let mut args: Vec<&str> = vec!["--no-pager", "diff"];
    if stat {
        args.push("--stat");
    }
    if let Some(base) = base {
        args.push(base);
    }
    if let Some(path) = path {
        args.push("--");
        args.push(path);
    }
    git(dir, &args)
}

/// Untracked (not ignored) paths — `git diff` can't show brand-new files, so surfaces that
/// present a diff list these alongside it.
pub fn untracked(dir: &Path) -> Result<String, GitError> {
    git(dir, &["ls-files", "--others", "--exclude-standard"])
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
