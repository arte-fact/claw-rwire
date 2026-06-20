//! Filesystem operations for the Files view, scoped to a workspace root. Ported from
//! rust-claw `src/workspace/ops.rs` — every path goes through [`jail`](super::jail), so
//! `..`/absolute/symlink escapes are rejected, and mutating ops additionally refuse to
//! target the workspace root itself.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{FILE_VIEW_MAX_BYTES, WorkspaceError, jail};

/// What a directory entry is. `Symlink` is surfaced (not followed) so the operator sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// One directory entry: name, kind, byte size, and modified time (unix secs, if available).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<u64>,
}

/// List `rel` (a directory relative to `root`), dirs first then case-insensitive by name.
pub fn list(root: &Path, rel: &str) -> Result<Vec<Entry>, WorkspaceError> {
    let dir = jail(root, rel)?;
    if !std::fs::symlink_metadata(&dir)?.is_dir() {
        return Err(WorkspaceError::NotADirectory);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let link = std::fs::symlink_metadata(entry.path())?;
        let kind = if link.is_symlink() {
            EntryKind::Symlink
        } else if link.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let modified = link
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size: link.len(),
            modified,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir)
            .cmp(&(b.kind != EntryKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
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

/// Create a new directory at `rel`. Fails if anything already exists there.
pub fn mkdir(root: &Path, rel: &str) -> Result<(), WorkspaceError> {
    let target = mutable_target(root, rel)?;
    if target.exists() {
        return Err(WorkspaceError::Exists);
    }
    std::fs::create_dir(&target)?;
    Ok(())
}

/// Delete `rel` — a file, or a directory and everything under it.
pub fn delete(root: &Path, rel: &str) -> Result<(), WorkspaceError> {
    let target = mutable_target(root, rel)?;
    let meta = std::fs::symlink_metadata(&target)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&target)?;
    } else {
        std::fs::remove_file(&target)?;
    }
    Ok(())
}

/// Rename/move `from` to `to`. `from` must exist and `to` must not.
pub fn rename(root: &Path, from: &str, to: &str) -> Result<(), WorkspaceError> {
    let src = mutable_target(root, from)?;
    let dst = mutable_target(root, to)?;
    if !src.exists() {
        return Err(WorkspaceError::NotFound);
    }
    if dst.exists() {
        return Err(WorkspaceError::Exists);
    }
    std::fs::rename(&src, &dst)?;
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
    fn list_orders_dirs_first_then_name() {
        let r = root();
        std::fs::write(r.join("b.txt"), "b").unwrap();
        std::fs::write(r.join("a.txt"), "a").unwrap();
        std::fs::create_dir(r.join("zdir")).unwrap();
        let names: Vec<String> = list(&r, "").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["zdir", "a.txt", "b.txt"]);
    }

    #[test]
    fn write_then_read_text_round_trips() {
        let r = root();
        mkdir(&r, "notes").unwrap();
        write_text(&r, "notes/hi.txt", "hello").unwrap();
        assert_eq!(read_text(&r, "notes/hi.txt").unwrap(), "hello");
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
    fn mkdir_then_rename_then_delete() {
        let r = root();
        mkdir(&r, "src").unwrap();
        assert!(matches!(mkdir(&r, "src"), Err(WorkspaceError::Exists)));
        write_text(&r, "src/a.txt", "a").unwrap();
        rename(&r, "src/a.txt", "src/b.txt").unwrap();
        assert!(read_text(&r, "src/a.txt").is_err());
        assert_eq!(read_text(&r, "src/b.txt").unwrap(), "a");
        delete(&r, "src").unwrap();
        assert!(matches!(list(&r, "src"), Err(WorkspaceError::Io(_))));
    }

    #[test]
    fn jail_rejects_escapes_for_every_op() {
        let r = root();
        assert!(matches!(list(&r, "../"), Err(WorkspaceError::Escape)));
        assert!(matches!(
            read_text(&r, "/etc/passwd"),
            Err(WorkspaceError::Escape)
        ));
        assert!(matches!(
            write_text(&r, "../escape.txt", "x"),
            Err(WorkspaceError::Escape)
        ));
        assert!(matches!(delete(&r, "../x"), Err(WorkspaceError::Escape)));
    }

    #[test]
    fn mutating_the_root_is_refused() {
        let r = root();
        assert!(matches!(delete(&r, ""), Err(WorkspaceError::InvalidName)));
        assert!(matches!(
            write_text(&r, "", "x"),
            Err(WorkspaceError::InvalidName)
        ));
    }
}
