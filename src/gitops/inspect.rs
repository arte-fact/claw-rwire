//! Read-only "inspect a ref" helpers (P5c): a repo's local branches, and a ref's file tree —
//! the project page's branch switcher + read-only file browser. Both go through the hermetic
//! [`git`](super::git) runner; neither touches the working tree.

use std::path::Path;

use super::{GitError, git};

/// One node of a ref's tree — files and the intermediate directories, in git's pre-order (a
/// directory immediately followed by its whole subtree). That is exactly the shape rwire's
/// `FileTree` consumes, so the output drops straight in with no re-sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Path relative to the repo root (`src/main.rs`).
    pub rel: String,
    /// The final path segment (`main.rs`).
    pub name: String,
    pub is_dir: bool,
    /// Slashes in `rel` — the tree depth (`src` → 0, `src/main.rs` → 1).
    pub depth: usize,
}

/// A repo's local branch names, newest-commit first — plumbing, never `git branch` (which marks
/// the current branch with `* ` and honours pager/column config). A repo with no commits lists
/// nothing (`Ok(vec![])`).
pub fn local_branches(dir: &Path) -> Result<Vec<String>, GitError> {
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=-committerdate",
            "refs/heads/",
        ],
    )?;
    Ok(out.lines().map(str::to_owned).collect())
}

/// The whole tree of `reference` as pre-order entries (directories and files). An unknown or
/// unborn ref is an `Err` the caller renders as an empty tree.
pub fn ref_tree(dir: &Path, reference: &str) -> Result<Vec<TreeEntry>, GitError> {
    let out = git(dir, &["ls-tree", "-rt", reference])?;
    Ok(out.lines().filter_map(parse_line).collect())
}

/// Parse one `git ls-tree -rt` line: `"<mode> <type> <object>\t<path>"`. A `tree` is a
/// directory; `blob`/`commit` (a submodule gitlink) is a leaf.
fn parse_line(line: &str) -> Option<TreeEntry> {
    let (meta, path) = line.split_once('\t')?;
    let kind = meta.split_whitespace().nth(1)?;
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    Some(TreeEntry {
        depth: path.matches('/').count(),
        is_dir: kind == "tree",
        name,
        rel: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_reads_type_path_depth_and_name() {
        let dir = parse_line("040000 tree abc123\tsrc").expect("dir line");
        assert_eq!(
            dir,
            TreeEntry {
                rel: "src".to_owned(),
                name: "src".to_owned(),
                is_dir: true,
                depth: 0,
            }
        );
        let file = parse_line("100644 blob def456\tsrc/main.rs").expect("file line");
        assert_eq!(
            file,
            TreeEntry {
                rel: "src/main.rs".to_owned(),
                name: "main.rs".to_owned(),
                is_dir: false,
                depth: 1,
            }
        );
        // A submodule gitlink (commit) is a leaf, not a directory.
        assert!(
            !parse_line("160000 commit f00\tvendor/lib")
                .expect("gitlink")
                .is_dir
        );
        // A malformed line (no tab) is dropped.
        assert!(parse_line("garbage").is_none());
    }
}
