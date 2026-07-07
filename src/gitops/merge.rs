//! Working-tree-less merges + branch divergence (P5a keystone). `merge-tree --write-tree`
//! computes the merged tree without a checkout; `commit-tree` + `update-ref` land it on the
//! target branch. Nothing here touches a working tree — refs advance in place, so a merge to
//! `develop`/`main` never disturbs the canonical checkout or a running preview.

use std::path::Path;

use super::{GitError, branch_exists, email_local_part, git, git_run};

/// The release branch every project advances to on a Release (git-flow `main`).
pub const MAIN: &str = "main";

/// The outcome of merging `from` into `into`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// A two-parent merge commit was created and `into` was advanced to it (the new commit id).
    Merged(String),
    /// `from` is already an ancestor of `into` — nothing to do, `into` untouched.
    UpToDate,
    /// The merge conflicts; `into` untouched. Carries the conflicting paths.
    Conflict(Vec<String>),
}

/// Ensure the release branch exists. Repos are born on `main`, so this only matters for one
/// created before the two-line lineage; it forks `main` from the current tip.
pub fn ensure_main(dir: &Path) -> Result<(), GitError> {
    if branch_exists(dir, MAIN) {
        return Ok(());
    }
    git(dir, &["branch", MAIN])?;
    Ok(())
}

/// Merge `from` into `into` without a checkout. Computes the merged tree with
/// `merge-tree --write-tree`, commits it as a two-parent merge, and compare-and-swaps the
/// branch ref from its old tip. The working tree, index, and `HEAD` are never touched.
pub fn merge_treeless(
    dir: &Path,
    into: &str,
    from: &str,
    message: &str,
) -> Result<MergeOutcome, GitError> {
    let into_oid = git(dir, &["rev-parse", into])?;
    let from_oid = git(dir, &["rev-parse", from])?;

    // `from` already contained in `into` → nothing to merge.
    if git_run(
        dir,
        &[],
        &["merge-base", "--is-ancestor", &from_oid, &into_oid],
    )?
    .ok
    {
        return Ok(MergeOutcome::UpToDate);
    }

    // `merge-tree --write-tree`: line 1 is the merged tree oid; a non-zero exit means conflicts,
    // whose paths follow (one per line up to a blank line) thanks to `--name-only`.
    let merged = git_run(
        dir,
        &[],
        &["merge-tree", "--write-tree", "--name-only", into, from],
    )?;
    if !merged.ok {
        let paths = merged
            .stdout
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        return Ok(MergeOutcome::Conflict(paths));
    }
    let tree = merged.stdout.lines().next().unwrap_or_default().to_owned();

    // `commit-tree` reads identity from the leading `-c user.*` config, git_as-style; a merge is
    // harness housekeeping, so it's authored plain `claw`.
    let email = format!("user.email={}@claw", email_local_part(None));
    let cfg = [
        "-c",
        "user.name=claw",
        "-c",
        &email,
        "-c",
        "commit.gpgsign=false",
    ];
    let committed = git_run(
        dir,
        &cfg,
        &[
            "commit-tree",
            &tree,
            "-p",
            &into_oid,
            "-p",
            &from_oid,
            "-m",
            message,
        ],
    )?;
    if !committed.ok {
        return Err(GitError::Command {
            args: "commit-tree".to_owned(),
            stderr: committed.stderr,
        });
    }
    let new_oid = committed.stdout;

    // Compare-and-swap: advance `into` only if it still points where we merged from, so a
    // concurrent advance fails loudly instead of being silently clobbered.
    git(
        dir,
        &[
            "update-ref",
            &format!("refs/heads/{into}"),
            &new_oid,
            &into_oid,
        ],
    )?;
    Ok(MergeOutcome::Merged(new_oid))
}

/// `(ahead, behind)` — commits reachable from `a` but not `b`, and from `b` but not `a`
/// (`git rev-list --left-right --count a...b`). `divergence(dir, develop, main)` → how far
/// develop leads main, and how far main leads develop.
pub fn divergence(dir: &Path, a: &str, b: &str) -> Result<(usize, usize), GitError> {
    let out = git(
        dir,
        &["rev-list", "--left-right", "--count", &format!("{a}...{b}")],
    )?;
    let mut counts = out.split_whitespace();
    let ahead = counts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let behind = counts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    Ok((ahead, behind))
}

/// Whether merging `from` into `into` would conflict — a dry `merge-tree` that computes the
/// merge but commits nothing. `false` when `from` is already an ancestor of `into`.
pub fn would_conflict(dir: &Path, into: &str, from: &str) -> Result<bool, GitError> {
    let into_oid = git(dir, &["rev-parse", into])?;
    let from_oid = git(dir, &["rev-parse", from])?;
    if git_run(
        dir,
        &[],
        &["merge-base", "--is-ancestor", &from_oid, &into_oid],
    )?
    .ok
    {
        return Ok(false);
    }
    Ok(!git_run(
        dir,
        &[],
        &["merge-tree", "--write-tree", "--name-only", into, from],
    )?
    .ok)
}
