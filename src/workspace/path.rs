//! Path jailing: resolve a model-supplied path inside a workspace root, rejecting any
//! escape. Ported from rust-claw `src/workspace/path.rs` — two layers: lexical containment
//! (an absolute, `..`-folded target must sit under the workspace root) then symlink
//! canonicalization (the resolved real path of the deepest existing ancestor must too).
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

/// Require `joined` to live under `root`, in two layers:
///
/// 1. **Lexical containment**: the absolute, `..`-folded target must start with the absolute,
///    `..`-folded root. This is computed *without* touching the filesystem, so a workspace whose
///    directory hasn't been created yet (a fresh scaffold, or a transient during setup) still
///    validates — instead of being misreported as an escape because the root couldn't be
///    canonicalized. `write` creates the tree on demand, so a not-yet-existing target is normal.
/// 2. **Symlink defence**: when the root *does* exist, the deepest existing ancestor of the target
///    must also canonicalize (symlinks resolved) inside the canonical root — so a symlink planted
///    inside the tree can't point out. Skipped when the root isn't on disk (nothing to resolve;
///    layer 1 already bounds the path).
fn verify_within(root: &Path, joined: &Path) -> Result<(), WorkspaceError> {
    let root_abs = absolutize(root)?;
    if !absolutize(joined)?.starts_with(&root_abs) {
        return Err(WorkspaceError::Escape);
    }
    // Layer 2 only applies once the root is materialized. A canonicalize failure here means the
    // root isn't on disk yet — not an escape — so we accept on the strength of layer 1.
    let Ok(root_canon) = root.canonicalize() else {
        return Ok(());
    };
    let mut ancestor = joined;
    loop {
        match ancestor.canonicalize() {
            Ok(real) => {
                return if real.starts_with(&root_canon) {
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

/// `path` as an absolute, lexically-normalized path — `.`/`..` folded, no filesystem access (so it
/// works for paths that don't exist yet). A `..` at the root is dropped (can't climb above `/`).
/// Fails only when the path can't be made absolute at all (an unreadable cwd) — reported distinctly
/// as [`WorkspaceError::RootUnavailable`], never as an escape.
fn absolutize(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let base = std::path::absolute(path).map_err(|_| WorkspaceError::RootUnavailable)?;
    let mut out = PathBuf::new();
    for component in base.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Ok(out)
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
    fn absolute_candidate_through_a_noncanonical_root_is_accepted() {
        // The live symptom: production passes a *non-canonical* root (a relative `./claw-data`, or
        // a symlink) and the agent writes with the workspace's own *absolute* path. That must
        // resolve inside, not be rejected as an escape. Modelled here with a symlink root + the
        // absolute path that goes through it.
        let real = root();
        let link = std::env::temp_dir().join(format!("claw-jail-abs-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let abs = link.join("docs/report.md");
        let raw = abs.to_str().expect("utf-8");
        assert_eq!(jail(&link, raw).expect("inside"), abs);
    }

    #[test]
    fn a_freshly_scaffolded_root_that_isnt_on_disk_yet_is_not_an_escape() {
        // Regression for the misreport: when the workspace dir hasn't been created yet, a contained
        // path must still validate (write() creates the tree) — a root-canonicalization failure is
        // NOT an escape. Both a relative candidate and the workspace's own absolute path resolve.
        let missing =
            std::env::temp_dir().join(format!("claw-jail-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(!missing.exists());
        assert_eq!(
            jail(&missing, "docs/report.md").expect("relative into a not-yet-created root"),
            missing.join("docs/report.md")
        );
        let abs = missing.join("docs/report.md");
        assert_eq!(
            jail(&missing, abs.to_str().expect("utf-8"))
                .expect("absolute into a not-yet-created root"),
            abs
        );
    }

    #[test]
    fn an_escape_is_still_rejected_when_the_root_isnt_on_disk() {
        // The lexical fallback must not weaken the boundary: an out-of-root target is rejected even
        // when the root dir doesn't exist yet.
        let missing = std::env::temp_dir().join(format!("claw-jail-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(matches!(
            jail(&missing, "/etc/passwd"),
            Err(WorkspaceError::Escape)
        ));
        assert!(matches!(
            jail(&missing, "../../etc/passwd"),
            Err(WorkspaceError::Escape)
        ));
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
