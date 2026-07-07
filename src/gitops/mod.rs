//! Jailed `git` shell helpers (G0 of `docs/requests.md`). The harness — never the agents —
//! versions the workspace: the meta-repo owns the workspace's own material and ignores
//! `projects/` wholesale; each `projects/<name>` is a sovereign repo. Every invocation is
//! workspace-jailed via [`repo_dir`], runs synchronously (bridge/worker threads only — never
//! the async runtime), and commits are authored `claw <agent-id@claw>` so history shows
//! which agent made each pass.

mod inspect;
mod merge;
mod requests;
mod sweep;
mod task_tree;
#[cfg(test)]
mod tests;
mod worktrees;

pub use inspect::{local_branches, ref_tree};
pub use merge::{MAIN, MergeOutcome, divergence, would_conflict};
pub use requests::{
    DEVELOP, RequestRef, abandon_branch, branch_exists, deliver_repo, has_remote, push_branch,
    push_refs, release_repo, restore_file, revert_merge,
};
pub use sweep::{commit_pass, ensure_meta_repo, ensure_project_repo, repos};
pub use task_tree::{reset_task_tree, snapshot_task_bases};
pub use worktrees::{
    adopt_tree_repo, drop_branch, ensure_request_tree, remove_request_tree, tree_root,
};

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
    #[error("the working tree is dirty — refusing to merge")]
    DirtyTree,
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
    let run = git_run(dir, &[], args)?;
    if !run.ok {
        return Err(GitError::Command {
            args: args.join(" "),
            stderr: run.stderr,
        });
    }
    Ok(run.stdout)
}

/// A git invocation's result when the non-zero exit is itself meaningful (e.g. `merge-tree`'s
/// conflict signal) — [`git`] turns that into an error, this keeps it as data.
struct GitRun {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// The shared spawn: hermetic env + optional leading `-c` config, returning the outcome without
/// treating a non-zero exit as an error. `cfg` slots between the always-on `core.hooksPath` and
/// the command args (how identity rides `commit-tree`).
fn git_run(dir: &Path, cfg: &[&str], args: &[&str]) -> Result<GitRun, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(cfg)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(GitError::Spawn)?;
    Ok(GitRun {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
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

/// Totals of `git diff --numstat <range>`: (files, insertions, deletions) — the delivery
/// receipt's "+A/−D across F files". Binary files count as a file with 0/0 lines.
pub fn diff_numstat(dir: &Path, range: &str) -> Result<(u64, u64, u64), GitError> {
    let out = git(dir, &["--no-pager", "diff", "--numstat", range])?;
    Ok(numstat_totals(&out))
}

/// `git diff` of a commit range (`base..tip`), as stat or patch — the request page's
/// global-diff source.
pub fn diff_range(dir: &Path, range: &str, stat: bool) -> Result<String, GitError> {
    let mut args: Vec<&str> = vec!["--no-pager", "diff"];
    if stat {
        args.push("--stat");
    }
    args.push(range);
    git(dir, &args)
}

/// One pass commit's (files, insertions, deletions) vs its parent(s) — per-run diff stats.
pub fn show_numstat(dir: &Path, commit: &str) -> Result<(u64, u64, u64), GitError> {
    let out = git(
        dir,
        &["--no-pager", "show", "--numstat", "--format=", commit],
    )?;
    Ok(numstat_totals(&out))
}

/// The paths one pass commit touched — the restore affordance's file list (G3).
pub fn show_files(dir: &Path, commit: &str) -> Result<Vec<String>, GitError> {
    let out = git(
        dir,
        &["--no-pager", "show", "--numstat", "--format=", commit],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .map(str::to_owned)
        .collect())
}

/// One pass commit as stat + patch (the Peek's Diff disclosure) — capped by the caller.
pub fn show_patch(dir: &Path, commit: &str) -> Result<String, GitError> {
    git(
        dir,
        &[
            "--no-pager",
            "show",
            "--stat",
            "--patch",
            "--format=%h %s",
            commit,
        ],
    )
}

fn numstat_totals(out: &str) -> (u64, u64, u64) {
    let (mut files, mut adds, mut dels) = (0, 0, 0);
    for line in out.lines().filter(|line| !line.trim().is_empty()) {
        let mut cols = line.split('\t');
        let added = cols.next().unwrap_or("0");
        let deleted = cols.next().unwrap_or("0");
        files += 1;
        adds += added.parse::<u64>().unwrap_or(0);
        dels += deleted.parse::<u64>().unwrap_or(0);
    }
    (files, adds, dels)
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

/// Commit what's staged, authored `claw <agent@claw>`.
pub fn commit(dir: &Path, agent: Option<&str>, message: &str) -> Result<String, GitError> {
    git_as(dir, agent, &["commit", "--allow-empty", "-m", message])?;
    head(dir)
}

/// Run git with the harness commit identity (`claw <agent@claw>`, signing off) — for
/// anything that creates commits (commit, delivery merge). Identity rides per-invocation
/// `-c` flags so no repo or global config is ever touched.
fn git_as(dir: &Path, agent: Option<&str>, args: &[&str]) -> Result<String, GitError> {
    let email_cfg = format!("user.email={}@claw", email_local_part(agent));
    let mut full: Vec<&str> = vec![
        "-c",
        "user.name=claw",
        "-c",
        &email_cfg,
        "-c",
        "commit.gpgsign=false",
    ];
    full.extend_from_slice(args);
    git(dir, &full)
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
