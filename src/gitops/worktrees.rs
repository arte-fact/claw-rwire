//! Per-request working trees (G4): a request executes in its own composition of linked
//! worktrees — one per workspace repo, all on the request branch — so several requests run
//! concurrently in one workspace while the canonical tree permanently sits on `develop`.
//! Worktrees share refs and objects with their repo, so read-side surfaces (diffs, `git
//! show`, delivery merges) stay canonical-rooted; only working-tree surfaces move here.

use std::path::{Path, PathBuf};

use super::requests::{DEVELOP, branch_exists, ensure_develop};
use super::sweep::{ensure_workspace_repos, repos};
use super::{GitError, current_branch, git, is_dirty, is_repo, repo_dir};

/// Where a request's tree lives: **outside** the workspace root — no repo scan, file jail,
/// or sibling lane can see it — at `<data>/trees/<workspace-id>/<request-short8>`.
#[must_use]
pub fn tree_root(data_dir: &Path, workspace_id: &str, request_id: &str) -> PathBuf {
    let short: String = request_id.chars().take(8).collect();
    data_dir.join("trees").join(workspace_id).join(short)
}

/// Whether `dir` is a linked worktree (its `.git` is a pointer file) rather than a full
/// repo (`.git` directory) — distinguishes composed trees from repos born inside one.
#[must_use]
pub fn is_linked_worktree(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

/// Compose the request's tree: a linked worktree of every workspace repo — the meta-repo
/// at the tree root, each project at `projects/<name>` — on the request branch (cut from
/// `develop` where it doesn't exist yet; re-attached where it does, so a tree lost to a
/// crash re-composes onto the branch's prior passes). Each canonical repo is settled on
/// `develop` first: free-lane passes and delivery merges share one integration line.
/// Idempotent; per-repo failures are collected, never fatal.
pub fn ensure_request_tree(root: &Path, tree: &Path, branch: &str) -> Vec<String> {
    let mut errors = ensure_workspace_repos(root);
    for repo in repos(root) {
        if let Err(err) = compose_repo(root, tree, branch, &repo) {
            errors.push(format!("{repo}: {err}"));
        }
    }
    errors
}

fn compose_repo(root: &Path, tree: &Path, branch: &str, repo: &str) -> Result<(), GitError> {
    let canonical = repo_dir(root, repo)?;
    settle_on_develop(&canonical)?;
    let dest = tree_repo_dir(tree, repo);
    if is_repo(&dest) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dest_str = dest.to_string_lossy().into_owned();
    if branch_exists(&canonical, branch) {
        git(&canonical, &["worktree", "add", &dest_str, branch])?;
    } else {
        git(
            &canonical,
            &["worktree", "add", "-b", branch, &dest_str, DEVELOP],
        )?;
    }
    Ok(())
}

/// A repo's directory inside a composed tree (`""` — the meta worktree — is the tree root).
fn tree_repo_dir(tree: &Path, repo: &str) -> PathBuf {
    if repo.is_empty() {
        tree.to_path_buf()
    } else {
        tree.join(repo)
    }
}

/// Move the canonical repo onto `develop`: ensure the branch exists and check it out when
/// the tree is clean. A dirty tree (a free-lane pass mid-write, a legacy mid-flight
/// request) is left where it is — the checkout would clobber work.
fn settle_on_develop(dir: &Path) -> Result<(), GitError> {
    ensure_develop(dir)?;
    if current_branch(dir)? != DEVELOP && !is_dirty(dir)? {
        git(dir, &["checkout", DEVELOP])?;
    }
    Ok(())
}

/// Tear a request's tree down: `worktree remove --force` per composed repo (projects first,
/// the meta worktree — the tree root itself — last), prune stale registrations, then delete
/// whatever remains (agent scratch outside any repo). Dirty trees are expected on abandon.
/// Per-repo failures are collected, never fatal.
pub fn remove_request_tree(root: &Path, tree: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    if !tree.exists() {
        return errors;
    }
    for repo in repos(root).iter().rev() {
        let Ok(canonical) = repo_dir(root, repo) else {
            continue;
        };
        let dest = tree_repo_dir(tree, repo);
        if is_linked_worktree(&dest) {
            let dest_str = dest.to_string_lossy().into_owned();
            if let Err(err) = git(&canonical, &["worktree", "remove", "--force", &dest_str]) {
                errors.push(format!(
                    "{}: {err}",
                    if repo.is_empty() { "meta" } else { repo }
                ));
            }
        }
        let _ = git(&canonical, &["worktree", "prune"]);
    }
    if tree.exists()
        && let Err(err) = std::fs::remove_dir_all(tree)
    {
        errors.push(format!("tree: {err}"));
    }
    errors
}

/// Adopt a repo **born inside a request tree** (a full repo there, absent from the
/// canonical workspace) by moving its directory into place — delivery then merges it like
/// any other. A repo already adopted (the retry path) is left alone.
pub fn adopt_tree_repo(tree_dir: &Path, canonical_dir: &Path) -> Result<(), GitError> {
    if is_repo(canonical_dir) {
        return Ok(());
    }
    if is_linked_worktree(tree_dir) {
        return Err(GitError::Io(std::io::Error::other(
            "refusing to adopt a linked worktree — only repos born inside the tree move",
        )));
    }
    if let Some(parent) = canonical_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(tree_dir, canonical_dir)?;
    Ok(())
}

/// Delete a request branch after its tree is gone: `-d` (merged-only — post-delivery
/// housekeeping in untouched repos) or `-D` (abandon: the commits are deliberately dropped).
pub fn drop_branch(dir: &Path, branch: &str, force: bool) -> Result<(), GitError> {
    git(dir, &["branch", if force { "-D" } else { "-d" }, branch])?;
    Ok(())
}
