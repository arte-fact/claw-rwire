//! Request-branch git flow (G1): cut `req/<short-id>-<slug>` from `develop` at a request's
//! first touch of a repo, land its pass commits there, and deliver with a `--no-ff` merge
//! back to `develop`. `develop` is the integration line; `main` is the release line, advanced
//! only by [`release_repo`] (P5a) via a working-tree-less merge — never by the request flow.

use std::path::Path;

use super::merge::{MAIN, MergeOutcome, ensure_main, merge_treeless};
use super::{GitError, current_branch, git, git_as, head, is_dirty};

/// The git flow integration branch requests cut from and merge back to.
pub const DEVELOP: &str = "develop";

/// A request's identity for the commit sweep: the trailer id + the branch passes land on.
#[derive(Debug, Clone, Copy)]
pub struct RequestRef<'a> {
    pub id: &'a str,
    pub branch: &'a str,
}

/// Whether a local branch exists.
#[must_use]
pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    git(
        dir,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok()
}

/// Ensure the integration branch exists — created at HEAD for repos that predate it (the
/// clone edge: `develop` starts at the clone's HEAD, so a request's diff against it is
/// exactly what the request did to pristine upstream).
pub fn ensure_develop(dir: &Path) -> Result<(), GitError> {
    if branch_exists(dir, DEVELOP) {
        return Ok(());
    }
    git(dir, &["branch", DEVELOP])?;
    Ok(())
}

/// Put the repo on the request's branch, cutting it on the request's first touch. Returns
/// the branch point — the base a delivery diff anchors on; the caller's `record_repo` is
/// first-write-wins, so later passes re-reporting it is harmless. This runs at commit time
/// with the pass's work still in the tree — `checkout -b` carries it over; in the legacy
/// flow HEAD sits on `develop`, so the cut is develop's tip. A repo **already on** the
/// branch (a composed G4 worktree — the branch was cut at composition, before any commit
/// sweep ran) reports the fork point from `develop` instead.
pub fn ensure_on_request_branch(dir: &Path, branch: &str) -> Result<Option<String>, GitError> {
    if current_branch(dir)? == branch {
        ensure_develop(dir)?;
        return Ok(Some(git(dir, &["merge-base", DEVELOP, "HEAD"])?));
    }
    ensure_develop(dir)?;
    if branch_exists(dir, branch) {
        // Something moved HEAD off the request branch mid-request — go back to it. (May
        // refuse over a conflicting dirty tree; the caller collects the error and commits
        // where HEAD is, so the pass's work is never lost.)
        git(dir, &["checkout", branch])?;
        return Ok(None);
    }
    let base = head(dir)?;
    git(dir, &["checkout", "-b", branch])?;
    Ok(Some(base))
}

/// Deliver one repo: `--no-ff` merge of the request branch into `develop`, leaving the repo
/// on `develop` (the next request cuts fresh from it). Refuses a dirty tree — delivery only
/// fires once every task landed, so dirt means something is wrong (or a free-lane pass is
/// mid-write — the caller's retry ping covers it). A failed merge — parallel branches can
/// conflict (G4) — is aborted so `develop` is never left mid-merge.
pub fn deliver_repo(dir: &Path, branch: &str, message: &str) -> Result<String, GitError> {
    if is_dirty(dir)? {
        return Err(GitError::DirtyTree);
    }
    git(dir, &["checkout", DEVELOP])?;
    if let Err(err) = git_as(dir, None, &["merge", "--no-ff", "-m", message, branch]) {
        let _ = git(dir, &["merge", "--abort"]);
        return Err(err);
    }
    head(dir)
}

/// Release one repo (P5a): merge `develop` into `main` **without a checkout** (the keystone), so
/// the canonical develop tree and any running preview are untouched. Returns whether `main`
/// advanced, was already up to date, or would conflict — the caller reports it and, on a real
/// advance, pushes `main` iff an origin exists.
pub fn release_repo(dir: &Path, message: &str) -> Result<MergeOutcome, GitError> {
    ensure_main(dir)?;
    merge_treeless(dir, MAIN, DEVELOP, message)
}

/// Whether the repo has any remote configured — the Push affordance's visibility. Reflects the
/// per-project origin too, since [`set_origin`] writes the configured URL onto the `origin` remote.
#[must_use]
pub fn has_remote(dir: &Path) -> bool {
    git(dir, &["remote"])
        .map(|out| !out.is_empty())
        .unwrap_or(false)
}

/// Point the repo's `origin` remote at `url` (P5b) — `set-url` if it exists, else `add`; a blank
/// `url` removes it. Writing the configured deploy URL onto `origin` (rather than pushing a bare
/// URL) is what lets pushes **and** `origin/*` tracking (the sync badges) work through one name.
pub fn set_origin(dir: &Path, url: &str) -> Result<(), GitError> {
    if url.trim().is_empty() {
        let _ = git(dir, &["remote", "remove", "origin"]);
        return Ok(());
    }
    if git(dir, &["remote", "get-url", "origin"]).is_ok() {
        git(dir, &["remote", "set-url", "origin", url])?;
    } else {
        git(dir, &["remote", "add", "origin", url])?;
    }
    Ok(())
}

/// Push `refs` to `remote` (G3/P5a), optionally under a chosen SSH deploy key (P5b): the explicit,
/// user-gated act — the harness never touches a remote on its own. `key_path` sets
/// `GIT_SSH_COMMAND -i <key>` in batch mode (never prompts; first-seen host keys are accepted so a
/// fresh deploy target doesn't hang). Hermetic like all harness git; https credential helpers stay
/// cut off. The request Push passes `develop` + the req branch; a Release passes `main`.
pub fn push_signed(
    dir: &Path,
    remote: &str,
    refs: &[&str],
    key_path: Option<&Path>,
) -> Result<String, GitError> {
    let ssh = key_path.map(|path| {
        format!(
            "ssh -i {} -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
            path.display()
        )
    });
    let envs: Vec<(&str, &str)> = ssh
        .as_deref()
        .map(|command| vec![("GIT_SSH_COMMAND", command)])
        .unwrap_or_default();
    let mut args = vec!["push", remote];
    args.extend_from_slice(refs);
    let run = super::git_run_env(dir, &[], &envs, &args)?;
    if !run.ok {
        return Err(GitError::Command {
            args: args.join(" "),
            stderr: run.stderr,
        });
    }
    Ok(run.stdout)
}

/// Post-delivery rollback (G3): revert the delivery merge on `develop` — a new commit,
/// honest history; the branch and its passes stay browsable.
pub fn revert_merge(dir: &Path, merge_commit: &str) -> Result<String, GitError> {
    git(dir, &["checkout", DEVELOP])?;
    git_as(dir, None, &["revert", "-m", "1", "--no-edit", merge_commit])?;
    head(dir)
}

/// Pre-merge abandon (G3): return the tree to `develop` and drop the request branch — the
/// work's commits become unreachable (deliberate: `main`/`develop` never saw them).
pub fn abandon_branch(dir: &Path, branch: &str) -> Result<(), GitError> {
    git(dir, &["checkout", DEVELOP])?;
    git(dir, &["branch", "-D", branch])?;
    Ok(())
}

/// File-level restore (G3): put one path back to its state **before** `commit` (the pass
/// that changed it). Leaves the tree dirty — the next pass commit records the restore.
/// Fails when the file didn't exist before that pass (nothing to restore to).
pub fn restore_file(dir: &Path, commit: &str, path: &str) -> Result<(), GitError> {
    git(dir, &["checkout", &format!("{commit}^"), "--", path])?;
    Ok(())
}
