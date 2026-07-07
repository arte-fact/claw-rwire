//! Filesystem operations for the Files view, scoped to a workspace root. Ported from
//! rust-claw `src/workspace/ops.rs` — every path goes through [`jail`](super::jail), so
//! `..`/absolute/symlink escapes are rejected, and mutating ops additionally refuse to
//! target the workspace root itself.

use std::path::{Path, PathBuf};

use super::{FILE_VIEW_MAX_BYTES, WorkspaceError, jail};

/// Skipped directories when walking a workspace tree — VCS metadata and build output, which
/// would bury the actual source under thousands of paths.
const WALK_SKIP: [&str; 7] = [
    ".git",
    "target",
    "node_modules",
    "dist",
    ".cache",
    ".venv",
    ".isola",
];

/// Every file under `root` as a workspace-relative, forward-slashed path — sorted, deduped, and
/// capped at `limit` (so a huge tree can't blow up a prompt). Skips VCS/build noise. Used to give
/// a reviewer the real file layout, and to resolve a guessed `read` path by its suffix.
#[must_use]
pub fn list_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let name = entry.file_name();
                if !WALK_SKIP.contains(&name.to_string_lossy().as_ref()) {
                    stack.push(entry.path());
                }
            } else if kind.is_file()
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(limit);
    out
}

/// Read `rel` as UTF-8 text for the editor. Rejects non-files, files over
/// [`FILE_VIEW_MAX_BYTES`], and binary/non-UTF-8 content.
pub fn read_text(root: &Path, rel: &str) -> Result<String, WorkspaceError> {
    let target = jail(root, rel)?;
    let meta = std::fs::symlink_metadata(&target)?;
    if !meta.is_file() {
        return Err(WorkspaceError::NotAFile);
    }
    if meta.len() > FILE_VIEW_MAX_BYTES {
        return Err(WorkspaceError::TooLarge);
    }
    let bytes = std::fs::read(&target)?;
    if bytes.contains(&0) {
        return Err(WorkspaceError::Binary);
    }
    String::from_utf8(bytes).map_err(|_| WorkspaceError::Binary)
}

/// Write `content` to `rel` (create or overwrite). The parent directory must already exist.
pub fn write_text(root: &Path, rel: &str, content: &str) -> Result<(), WorkspaceError> {
    let target = mutable_target(root, rel)?;
    match target.parent() {
        Some(parent) if parent.is_dir() => {}
        _ => return Err(WorkspaceError::NotFound),
    }
    std::fs::write(&target, content.as_bytes())?;
    Ok(())
}

/// Jail `rel`, additionally refusing the workspace root — a mutating op must name a child.
fn mutable_target(root: &Path, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let target = jail(root, rel)?;
    if target == root {
        return Err(WorkspaceError::InvalidName);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("claw-ops-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.canonicalize().expect("canon")
    }

    #[test]
    fn write_then_read_text_round_trips() {
        let r = root();
        std::fs::create_dir(r.join("notes")).unwrap();
        write_text(&r, "notes/hi.txt", "hello").unwrap();
        assert_eq!(read_text(&r, "notes/hi.txt").unwrap(), "hello");
    }

    #[test]
    fn list_files_walks_recursively_skips_noise_and_caps() {
        let r = root();
        std::fs::create_dir_all(r.join("projects/rogue/src")).unwrap();
        std::fs::write(r.join("projects/rogue/src/render.rs"), "x").unwrap();
        std::fs::write(r.join("AGENT.md"), "x").unwrap();
        std::fs::create_dir_all(r.join("target/debug")).unwrap();
        std::fs::write(r.join("target/debug/junk.o"), "x").unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".git/config"), "x").unwrap();

        let files = list_files(&r, 100);
        assert!(files.contains(&"projects/rogue/src/render.rs".to_owned()));
        assert!(files.contains(&"AGENT.md".to_owned()));
        assert!(
            !files.iter().any(|f| f.starts_with("target/")),
            "build output skipped"
        );
        assert!(
            !files.iter().any(|f| f.starts_with(".git/")),
            "VCS metadata skipped"
        );
        // The cap bounds the output.
        assert_eq!(list_files(&r, 1).len(), 1);
    }

    #[test]
    fn write_requires_existing_parent() {
        let r = root();
        assert!(matches!(
            write_text(&r, "missing/x.txt", "x"),
            Err(WorkspaceError::NotFound)
        ));
    }

    #[test]
    fn read_text_rejects_binary() {
        let r = root();
        std::fs::write(r.join("bin"), [0u8, 1, 2]).unwrap();
        assert!(matches!(read_text(&r, "bin"), Err(WorkspaceError::Binary)));
    }

    #[test]
    fn jail_rejects_escapes_for_every_op() {
        let r = root();
        assert!(matches!(
            read_text(&r, "/etc/passwd"),
            Err(WorkspaceError::Escape)
        ));
        assert!(matches!(
            write_text(&r, "../escape.txt", "x"),
            Err(WorkspaceError::Escape)
        ));
    }

    #[test]
    fn mutating_the_root_is_refused() {
        let r = root();
        assert!(matches!(
            write_text(&r, "", "x"),
            Err(WorkspaceError::InvalidName)
        ));
    }
}
