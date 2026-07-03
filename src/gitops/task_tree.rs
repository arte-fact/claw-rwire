//! The task ↔ repo-base binding: snapshot every repo's HEAD at the task's first launch, and
//! reset them all on restart — the fix for the dirty-tree inheritance flaw (a restarted
//! task's fresh attempt used to inherit whatever the abandoned attempt left on disk).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use super::sweep::{ensure_workspace_repos, repos};
use super::{commit_all, head, is_dirty, is_repo, repo_dir, reset_hard};
use crate::db::task_repos;

/// Record the pre-task state of the workspace's repos for `task_id`, git-ifying what exists
/// first (lazy meta-repo init + project births). On the task's **first** launch a dirty tree
/// is adopted into a housekeeping commit before its HEAD is recorded — so the base is exactly
/// the pre-task disk state (a Files-view edit between tasks isn't lost to a later restart).
/// Later launches (retries, post-restart re-claims) only record repos not yet seen, without
/// adopting — a failed pass's deliberate dirt must not be committed. Errors are collected,
/// never fatal: a git-less environment degrades to the pre-G0 behaviour.
pub fn snapshot_task_bases(conn: &Connection, root: &Path, task_id: &str) -> Vec<String> {
    let mut errors = ensure_workspace_repos(root);
    let known: HashSet<String> = task_repos::bases_for(conn, task_id)
        .unwrap_or_default()
        .into_iter()
        .map(|base| base.repo)
        .collect();
    let first_launch = known.is_empty();
    for repo in repos(root) {
        if known.contains(&repo) {
            continue;
        }
        let dir = match repo_dir(root, &repo) {
            Ok(dir) => dir,
            Err(err) => {
                errors.push(format!("{repo}: {err}"));
                continue;
            }
        };
        if first_launch
            && is_dirty(&dir).unwrap_or(false)
            && let Err(err) = commit_all(&dir, None, "claw: adopt pre-task workspace state")
        {
            errors.push(format!("{repo}: adopt: {err}"));
        }
        match head(&dir) {
            Ok(base) => {
                if let Err(err) = task_repos::record_base(conn, task_id, &repo, &base) {
                    errors.push(format!("{repo}: record base: {err}"));
                }
            }
            Err(err) => errors.push(format!("{repo}: {err}")),
        }
    }
    errors
}

/// Reset every repo the task recorded a base for to that base (`reset --hard` + clean) — the
/// restart path's "fresh attempt gets fresh files". A repo that vanished since is skipped;
/// per-repo failures are collected so one broken repo doesn't stop the others.
pub fn reset_task_tree(conn: &Connection, root: &Path, task_id: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for base in task_repos::bases_for(conn, task_id).unwrap_or_default() {
        let dir = match repo_dir(root, &base.repo) {
            Ok(dir) => dir,
            Err(err) => {
                errors.push(format!("{}: {err}", base.repo));
                continue;
            }
        };
        if !is_repo(&dir) {
            continue;
        }
        if let Err(err) = reset_hard(&dir, &base.base_commit) {
            errors.push(format!("{}: {err}", base.repo));
        }
    }
    errors
}
