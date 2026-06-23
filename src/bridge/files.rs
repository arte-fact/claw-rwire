//! Files view: directory browsing + file edit/create/rename/delete over a workspace, run
//! on the bridge thread (it owns fs access). Each command resolves the workspace dir, runs
//! a `workspace::ops` call, then re-projects the listing into the `App.files_*` read-models.
//! Errors surface in `files_error` rather than crashing. A workspace is shared by every
//! agent assigned to it, so editing here is visible to all of them.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rwire::{ChangeSet, SharedServerState};

use crate::db::workspaces;
use crate::state::{App, FileEntry, FileKind, FileNode, OpenFile};
use crate::workspace::ops::{self, EntryKind};
use crate::workspace::{self, WorkspaceError};

/// Which folders are expanded in the Files tree, per workspace — process-global view state (like
/// the chat/context caches). The tree walk reads it; navigation/toggle update it. Keeping it here
/// (rather than threading an expansion set through every file command) keeps the UI commands thin.
static FILES_EXPANDED: OnceLock<Mutex<HashMap<String, BTreeSet<String>>>> = OnceLock::new();

fn expanded_lock() -> &'static Mutex<HashMap<String, BTreeSet<String>>> {
    FILES_EXPANDED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A snapshot of the workspace's expanded-folder set.
fn expanded_set(workspace: &str) -> BTreeSet<String> {
    expanded_lock()
        .lock()
        .ok()
        .and_then(|map| map.get(workspace).cloned())
        .unwrap_or_default()
}

/// Toggle a folder's expansion in `workspace`.
fn toggle_expanded(workspace: &str, path: &str) {
    if let Ok(mut map) = expanded_lock().lock() {
        let set = map.entry(workspace.to_owned()).or_default();
        if !set.remove(path) {
            set.insert(path.to_owned());
        }
    }
}

/// Ensure `path` and all its ancestors are expanded (so navigating to a deep dir reveals the
/// chain to it).
fn ensure_expanded(workspace: &str, path: &str) {
    if path.is_empty() {
        return;
    }
    if let Ok(mut map) = expanded_lock().lock() {
        let set = map.entry(workspace.to_owned()).or_default();
        let mut prefix = String::new();
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            prefix = join(&prefix, segment);
            set.insert(prefix.clone());
        }
    }
}

/// Resolve a workspace id to its (scaffolded) directory + display name.
fn resolve(conn: &rusqlite::Connection, workspace_id: &str) -> Option<(PathBuf, String)> {
    let workspace = workspaces::get(conn, workspace_id).ok().flatten()?;
    let dir = workspace::dir_for(&super::data_dir(), &workspace.id);
    // Ensure the dir exists (+ a starter AGENT.md) so a never-run workspace isn't empty.
    let _ = workspace::scaffold(&dir, &workspace.name);
    Some((dir, workspace.name))
}

/// Load a workspace's `AGENT.md` (raw) into `App.workspace_brief` for the Overview editor.
pub fn load_brief(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let brief = resolve(conn, workspace)
        .and_then(|(dir, _)| std::fs::read_to_string(dir.join("AGENT.md")).ok())
        .unwrap_or_default();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKSPACE_BRIEF]),
        move |app| app.workspace_brief = brief,
    );
}

/// Save a workspace's `AGENT.md` brief from the Overview editor (a normal file write at the
/// workspace root), then re-project the saved content.
pub fn save_brief(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    content: &str,
) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return;
    };
    if let Err(err) = ops::write_text(&dir, "AGENT.md", content) {
        return surface(shared, message(&err));
    }
    let content = content.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKSPACE_BRIEF]),
        move |app| app.workspace_brief = content,
    );
}

/// The parent directory of a workspace-relative path (`""` for a top-level entry).
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn to_entries(entries: Vec<ops::Entry>) -> Vec<FileEntry> {
    entries
        .into_iter()
        .map(|entry| FileEntry {
            name: entry.name,
            kind: match entry.kind {
                EntryKind::Dir => FileKind::Dir,
                EntryKind::File => FileKind::File,
                EntryKind::Symlink => FileKind::Symlink,
            },
            size: entry.size,
        })
        .collect()
}

/// Select `path` as the current dir and (re)project the tree, revealing the chain to it. Used on
/// the route opening and breadcrumb navigation; keeps any open file.
pub fn list_dir(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
) {
    if resolve(conn, workspace).is_none() {
        return project_tree(
            conn,
            shared,
            workspace,
            "",
            OpenChange::Keep,
            Some("Unknown workspace.".into()),
        );
    }
    ensure_expanded(workspace, path);
    project_tree(conn, shared, workspace, path, OpenChange::Keep, None);
}

/// Toggle a folder's expansion and select it as the current dir (the create target), then
/// re-project the tree. The folder-row click in the browser.
pub fn toggle_dir(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
) {
    toggle_expanded(workspace, path);
    project_tree(conn, shared, workspace, path, OpenChange::Keep, None);
}

/// Read a file into the editor pane (binary/oversized → a read-only note).
pub fn open_file(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return;
    };
    let open = match ops::read_text(&dir, path) {
        Ok(content) => OpenFile {
            path: path.to_owned(),
            content,
            editable: true,
            note: None,
        },
        Err(WorkspaceError::Binary) => not_editable(path, "Binary file — can't edit it here."),
        Err(WorkspaceError::TooLarge) => {
            not_editable(path, "File is larger than 1 MiB — too large to edit here.")
        }
        Err(err) => return surface(shared, message(&err)),
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_FILES_OPEN, App::FIELD_FILES_ERROR]),
        |app| {
            app.files_open = Some(open);
            app.files_error = None;
        },
    );
}

/// Write `content` to `path` (save an edit, or create an empty file), then re-list its
/// directory and keep it open showing the saved content.
pub fn write_file(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
    content: &str,
) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return;
    };
    if let Err(err) = ops::write_text(&dir, path, content) {
        return surface(shared, message(&err));
    }
    let _ = dir;
    let parent = parent_dir(path).to_owned();
    let open = OpenFile {
        path: path.to_owned(),
        content: content.to_owned(),
        editable: true,
        note: None,
    };
    project_tree(
        conn,
        shared,
        workspace,
        &parent,
        OpenChange::Set(open),
        None,
    );
}

/// Create a directory at `path`, then re-list its parent.
pub fn new_folder(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
) {
    relist_after(conn, shared, workspace, path, |dir| ops::mkdir(dir, path));
}

/// Delete `path` (file or directory), then re-list its parent.
pub fn delete_entry(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
) {
    relist_after(conn, shared, workspace, path, |dir| ops::delete(dir, path));
}

/// Rename `from` → `to`, then re-list `from`'s directory.
pub fn rename_entry(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    from: &str,
    to: &str,
) {
    relist_after(conn, shared, workspace, from, |dir| {
        ops::rename(dir, from, to)
    });
}

/// Run a mutating op, then re-project the tree from the parent of `path`, clearing the open file
/// (the affected entry may have been the open one).
fn relist_after(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    path: &str,
    op: impl FnOnce(&std::path::Path) -> Result<(), WorkspaceError>,
) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return;
    };
    if let Err(err) = op(&dir) {
        return surface(shared, message(&err));
    }
    let parent = parent_dir(path).to_owned();
    project_tree(conn, shared, workspace, &parent, OpenChange::Clear, None);
}

fn not_editable(path: &str, note: &str) -> OpenFile {
    OpenFile {
        path: path.to_owned(),
        content: String::new(),
        editable: false,
        note: Some(note.to_owned()),
    }
}

/// Join a workspace-relative dir and an entry name (`""` dir → just the name).
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Walk the workspace into a flattened, depth-tagged node list: the root's entries, and — for each
/// expanded folder — its children inserted right after it (recursively). Lazy: only expanded
/// subtrees are read.
fn walk_tree(dir: &Path, expanded: &BTreeSet<String>) -> Vec<FileNode> {
    let mut out = Vec::new();
    walk(dir, "", 0, expanded, &mut out);
    out
}

fn walk(dir: &Path, rel: &str, depth: usize, expanded: &BTreeSet<String>, out: &mut Vec<FileNode>) {
    let entries = ops::list(dir, rel).unwrap_or_default();
    for entry in entries {
        let path = join(rel, &entry.name);
        let kind = match entry.kind {
            EntryKind::Dir => FileKind::Dir,
            EntryKind::File => FileKind::File,
            EntryKind::Symlink => FileKind::Symlink,
        };
        let is_dir = kind == FileKind::Dir;
        let open = is_dir && expanded.contains(&path);
        out.push(FileNode {
            name: entry.name,
            path: path.clone(),
            kind,
            size: entry.size,
            depth,
            expanded: open,
        });
        if open {
            walk(dir, &path, depth + 1, expanded, out);
        }
    }
}

/// What to do with the open-file pane when re-projecting the tree.
enum OpenChange {
    /// Leave the open file untouched (browsing / toggling folders).
    Keep,
    /// Clear it (an entry was deleted/renamed and may have been the open one).
    Clear,
    /// Set it (after a save).
    Set(OpenFile),
}

/// Re-project the Files tree: `selected_path` becomes the current dir (the create target +
/// breadcrumb), the tree is walked from the workspace's expanded set, and the open-file pane is
/// updated per `open`. Resets the transient row/toolbar state.
fn project_tree(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    selected_path: &str,
    open: OpenChange,
    error: Option<String>,
) {
    let (tree, entries) = match resolve(conn, workspace) {
        Some((dir, _)) => {
            let tree = walk_tree(&dir, &expanded_set(workspace));
            let entries = ops::list(&dir, selected_path)
                .map(to_entries)
                .unwrap_or_default();
            (tree, entries)
        }
        None => (Vec::new(), Vec::new()),
    };
    let mut fields = vec![
        App::FIELD_FILES_WORKSPACE,
        App::FIELD_FILES_PATH,
        App::FIELD_FILES_ENTRIES,
        App::FIELD_FILES_TREE,
        App::FIELD_FILES_ERROR,
        App::FIELD_FILES_NEW_NAME,
        App::FIELD_FILES_CREATING,
        App::FIELD_FILES_RENAME,
        App::FIELD_FILES_PENDING_DELETE,
    ];
    if !matches!(open, OpenChange::Keep) {
        fields.push(App::FIELD_FILES_OPEN);
    }
    let workspace = workspace.to_owned();
    let path = selected_path.to_owned();
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&fields), move |app| {
        app.files_workspace = (!workspace.is_empty()).then_some(workspace);
        app.files_path = path;
        app.files_entries = entries;
        app.files_tree = tree;
        app.files_error = error;
        app.files_new_name.clear();
        app.files_creating = None;
        app.files_rename = None;
        app.files_pending_delete = None;
        match open {
            OpenChange::Keep => {}
            OpenChange::Clear => app.files_open = None,
            OpenChange::Set(file) => app.files_open = Some(file),
        }
    });
}

/// Surface a transient error in the Files banner without touching the listing.
fn surface(shared: &SharedServerState, error: String) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_FILES_ERROR]),
        move |app| app.files_error = Some(error),
    );
}

/// A user-facing message for a workspace error.
fn message(err: &WorkspaceError) -> String {
    match err {
        WorkspaceError::Escape => "That path is outside the workspace.".to_owned(),
        WorkspaceError::Exists => "A file or folder already exists there.".to_owned(),
        WorkspaceError::NotFound | WorkspaceError::Io(_) => "No such file or directory.".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::walk_tree;
    use std::collections::BTreeSet;

    #[test]
    fn walk_tree_flattens_root_and_only_expanded_folders() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path();
        std::fs::create_dir(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "a").expect("write a");
        std::fs::write(root.join("sub/b.txt"), "b").expect("write b");

        // Collapsed: root entries only; `sub`'s child is not walked.
        let nodes = walk_tree(root, &BTreeSet::new());
        assert!(
            nodes
                .iter()
                .any(|n| n.path == "sub" && n.depth == 0 && !n.expanded)
        );
        assert!(nodes.iter().any(|n| n.path == "a.txt" && n.depth == 0));
        assert!(!nodes.iter().any(|n| n.path == "sub/b.txt"));

        // Expanded: `sub`'s child appears at depth 1, immediately after it.
        let expanded: BTreeSet<String> = ["sub".to_owned()].into_iter().collect();
        let nodes = walk_tree(root, &expanded);
        let i = nodes
            .iter()
            .position(|n| n.path == "sub")
            .expect("sub present");
        assert!(nodes[i].expanded);
        assert_eq!(nodes[i + 1].path, "sub/b.txt");
        assert_eq!(nodes[i + 1].depth, 1);
    }
}
