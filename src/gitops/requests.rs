//! Request-branch git flow (G1): cut `req/<short-id>-<slug>` from `develop` at a request's
//! first touch of a repo, land its pass commits there, and deliver with a `--no-ff` merge
//! back to `develop`. `main` is release-only and never touched here.

use std::path::Path;

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
/// the branch point when cut (`None` when the repo was already on it). This runs at commit
/// time with the pass's work still in the tree — `checkout -b` carries it over; in the
/// normal flow HEAD sits on `develop` (G0 lands passes there), so the cut is develop's tip.
pub fn ensure_on_request_branch(dir: &Path, branch: &str) -> Result<Option<String>, GitError> {
    if current_branch(dir)? == branch {
        return Ok(None);
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
/// fires once every task landed, so dirt means something is wrong.
pub fn deliver_repo(dir: &Path, branch: &str, message: &str) -> Result<String, GitError> {
    if is_dirty(dir)? {
        return Err(GitError::DirtyTree);
    }
    git(dir, &["checkout", DEVELOP])?;
    git_as(dir, None, &["merge", "--no-ff", "-m", message, branch])?;
    head(dir)
}
