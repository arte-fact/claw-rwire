//! Path jailing: resolve a model-supplied relative path inside a workspace root,
//! rejecting any escape. Ported from rust-claw `src/workspace/path.rs` — two layers:
//! lexical normalization (reject `..` past root, absolute paths) then symlink
//! canonicalization (the resolved real path must still start with the root).
//!
//! Unlike rust-claw — whose file tools deliberately *don't* call this — claw-rwire's
//! `read`/`write`/`edit` tools apply it, so the workspace is an enforced boundary.
//!
//! An **absolute** path is accepted when it points *inside* the workspace and rejected
//! otherwise, rather than rejected outright: weaker models routinely echo the absolute
//! path they saw from `pwd`/`ls` (e.g. `<data>/workspaces/foo/docs/x.md`), and failing
//! every such call — the dominant real-world failure — taught them nothing. The `..`
//! escape check applies to absolute paths too, so a climb out is still blocked.

use std::path::{Component, Path, PathBuf};

use super::WorkspaceError;

/// Resolve `rel` (a model-supplied path) against `root`, returning the absolute target
/// only if it stays within `root`. Folds `.`/`..` lexically (any climb above the root —
/// relative *or* absolute — is an escape), then canonicalizes the deepest existing
/// ancestor to defeat symlinks pointing out. A leading `/` is taken as an absolute path
/// that must itself resolve inside the workspace.
pub fn jail(root: &Path, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let candidate = Path::new(rel);
    let mut normalized = PathBuf::new();
    let mut absolute = false;
    for component in candidate.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping the root itself (`/..`, or `a/../..`) climbs out — reject.
                if !normalized.pop() {
                    return Err(WorkspaceError::Escape);
                }
            }
            // A leading `/`: an absolute path. Keep it (so `verify_within` compares real
            // absolutes) rather than rejecting — but it still has to land inside `root`.
            Component::RootDir => {
                absolute = true;
                normalized.push(std::path::MAIN_SEPARATOR_STR);
            }
            // A Windows drive prefix (`C:`) can't be inside a unix workspace root — reject.
            Component::Prefix(_) => return Err(WorkspaceError::Escape),
        }
    }
    let joined = if absolute {
        normalized
    } else {
        root.join(&normalized)
    };
    verify_within(root, &joined)?;
    Ok(joined)
}

/// Walk up from `joined` to the deepest ancestor that exists, canonicalize it (resolving
/// symlinks), and require the real path to still live under `root`.
fn verify_within(root: &Path, joined: &Path) -> Result<(), WorkspaceError> {
    // Canonicalize the root too, so the `starts_with` compares like with like. Without
    // this, a relative root (the default `CLAW_DATA_DIR=./claw-data`) or a symlinked root
    // never prefixes the absolute, canonicalized target — and *every* path is rejected.
    // The root is always scaffolded before use, so it exists.
    let root = root.canonicalize().map_err(|_| WorkspaceError::Escape)?;
    let mut ancestor = joined;
    loop {
        match ancestor.canonicalize() {
            Ok(real) => {
                return if real.starts_with(&root) {
                    Ok(())
                } else {
                    Err(WorkspaceError::Escape)
                };
            }
            Err(_) => {
                ancestor = ancestor.parent().ok_or(WorkspaceError::Escape)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `root` must be canonicalized for `starts_with` to match (temp dirs may be symlinks,
    /// e.g. `/tmp` → `/private/tmp`).
    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claw-jail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.canonicalize().expect("canon")
    }

    #[test]
    fn empty_resolves_to_the_root() {
        let r = root();
        assert_eq!(jail(&r, "").expect("ok"), r);
    }

    #[test]
    fn interior_parent_that_stays_inside_is_allowed() {
        let r = root();
        assert_eq!(jail(&r, "a/b/../c").expect("ok"), r.join("a/c"));
    }

    #[test]
    fn lexical_escape_is_rejected() {
        let r = root();
        assert!(matches!(jail(&r, "../escape"), Err(WorkspaceError::Escape)));
        assert!(matches!(
            jail(&r, "a/../../escape"),
            Err(WorkspaceError::Escape)
        ));
    }

    #[test]
    fn absolute_paths_outside_the_root_are_rejected() {
        let r = root();
        assert!(matches!(
            jail(&r, "/etc/passwd"),
            Err(WorkspaceError::Escape)
        ));
    }

    #[test]
    fn absolute_paths_inside_the_root_are_accepted() {
        // Regression: a model that echoes the workspace's own absolute path (from `pwd`/`ls`)
        // must resolve to that exact file, not be rejected as an escape.
        let r = root();
        let inside = r.join("projects/rust-color/src/lib.rs");
        let raw = inside.to_str().expect("utf-8");
        assert_eq!(jail(&r, raw).expect("inside"), inside);
        // The root itself, given absolutely, resolves to the root.
        assert_eq!(jail(&r, r.to_str().expect("utf-8")).expect("root"), r);
    }

    #[test]
    fn absolute_path_climbing_out_with_dotdot_is_rejected() {
        // An absolute path that starts inside but `..`s past the root must still be blocked.
        let r = root();
        let sneaky = format!("{}/../../etc/passwd", r.to_str().expect("utf-8"));
        assert!(matches!(jail(&r, &sneaky), Err(WorkspaceError::Escape)));
        // A bare `/..` can't climb above filesystem root either.
        assert!(matches!(jail(&r, "/.."), Err(WorkspaceError::Escape)));
    }

    #[test]
    fn accepts_interior_paths_when_the_root_is_not_canonical() {
        // Regression: the root may be a non-canonical path (a relative `./claw-data`, or a
        // symlink). `verify_within` must canonicalize it before comparing, or every path
        // is wrongly rejected as an escape.
        let real = root();
        let link = std::env::temp_dir().join(format!("claw-jail-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        // Jail through the *symlink* root — an interior path must be allowed…
        assert_eq!(jail(&link, "a/b").expect("ok"), link.join("a/b"));
        // …and an escape still rejected.
        assert!(matches!(jail(&link, "../x"), Err(WorkspaceError::Escape)));
    }

    #[test]
    fn symlink_pointing_outside_is_rejected() {
        let r = root();
        let link = r.join("out");
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", &link).expect("symlink");
        // The symlink target (`/etc`) canonicalizes outside the root → rejected.
        assert!(matches!(
            jail(&r, "out/passwd"),
            Err(WorkspaceError::Escape)
        ));
    }
}
