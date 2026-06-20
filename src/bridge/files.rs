//! Files view: directory browsing + file edit/create/rename/delete over an agent's
//! workspace, run on the bridge thread (it owns fs access). Each command resolves the
//! agent's workspace, runs a `workspace::ops` call, then re-projects the listing into the
//! `App.files_*` read-models. Errors surface in `files_error` rather than crashing.

use std::path::PathBuf;

use rwire::{ChangeSet, SharedServerState};

use crate::db::{agents, workspaces};
use crate::state::{App, FileEntry, FileKind, OpenFile};
use crate::workspace::ops::{self, EntryKind};
use crate::workspace::{self, WorkspaceError};

/// Resolve an agent id to its (scaffolded) workspace dir + display name. The workspace is
/// the one the agent is assigned to (shared agents resolve to the same dir); an unmapped
/// agent falls back to its legacy `folder`.
fn resolve(conn: &rusqlite::Connection, agent_id: &str) -> Option<(PathBuf, String)> {
    let agent = agents::get(conn, agent_id).ok().flatten()?;
    let workspace_id = workspaces::for_agent(conn, agent_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| agent.folder.clone());
    let dir = workspace::dir_for(&super::data_dir(), &workspace_id);
    // Ensure the workspace exists (+ a starter AGENT.md) so a never-run agent isn't empty.
    let _ = workspace::scaffold(&dir, &agent.name);
    Some((dir, agent.name))
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

/// `/files` opened: browse the default agent's (web agent, else first) workspace root.
pub fn open_files(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let agent = agents::web_agent(conn)
        .ok()
        .flatten()
        .or_else(|| agents::first(conn).ok().flatten().map(|a| a.id));
    match agent {
        Some(id) => list_dir(conn, shared, &id, ""),
        None => project_dir(
            shared,
            "",
            "",
            Vec::new(),
            Some("No agents configured.".into()),
        ),
    }
}

/// List `path` in `agent`'s workspace and show it (clearing any open file).
pub fn list_dir(conn: &rusqlite::Connection, shared: &SharedServerState, agent: &str, path: &str) {
    let Some((dir, _)) = resolve(conn, agent) else {
        return project_dir(shared, agent, "", Vec::new(), Some("Unknown agent.".into()));
    };
    match ops::list(&dir, path) {
        Ok(entries) => project_dir(shared, agent, path, to_entries(entries), None),
        Err(err) => project_dir(shared, agent, path, Vec::new(), Some(message(&err))),
    }
}

/// Read a file into the editor pane (binary/oversized → a read-only note).
pub fn open_file(conn: &rusqlite::Connection, shared: &SharedServerState, agent: &str, path: &str) {
    let Some((dir, _)) = resolve(conn, agent) else {
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
    agent: &str,
    path: &str,
    content: &str,
) {
    let Some((dir, _)) = resolve(conn, agent) else {
        return;
    };
    if let Err(err) = ops::write_text(&dir, path, content) {
        return surface(shared, message(&err));
    }
    let parent = parent_dir(path).to_owned();
    let entries = ops::list(&dir, &parent).map(to_entries).unwrap_or_default();
    let open = OpenFile {
        path: path.to_owned(),
        content: content.to_owned(),
        editable: true,
        note: None,
    };
    project_dir_keep_open(shared, agent, &parent, entries, Some(open));
}

/// Create a directory at `path`, then re-list its parent.
pub fn new_folder(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent: &str,
    path: &str,
) {
    relist_after(conn, shared, agent, path, |dir| ops::mkdir(dir, path));
}

/// Delete `path` (file or directory), then re-list its parent.
pub fn delete_entry(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent: &str,
    path: &str,
) {
    relist_after(conn, shared, agent, path, |dir| ops::delete(dir, path));
}

/// Rename `from` → `to`, then re-list `from`'s directory.
pub fn rename_entry(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent: &str,
    from: &str,
    to: &str,
) {
    relist_after(conn, shared, agent, from, |dir| ops::rename(dir, from, to));
}

/// Run a mutating op, then re-list the parent of `path` (clearing the open file).
fn relist_after(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent: &str,
    path: &str,
    op: impl FnOnce(&std::path::Path) -> Result<(), WorkspaceError>,
) {
    let Some((dir, _)) = resolve(conn, agent) else {
        return;
    };
    if let Err(err) = op(&dir) {
        return surface(shared, message(&err));
    }
    let parent = parent_dir(path).to_owned();
    match ops::list(&dir, &parent) {
        Ok(entries) => project_dir(shared, agent, &parent, to_entries(entries), None),
        Err(err) => project_dir(shared, agent, &parent, Vec::new(), Some(message(&err))),
    }
}

fn not_editable(path: &str, note: &str) -> OpenFile {
    OpenFile {
        path: path.to_owned(),
        content: String::new(),
        editable: false,
        note: Some(note.to_owned()),
    }
}

/// Project a directory listing + reset the transient row/toolbar state, clearing the open
/// file (navigation always closes the editor). [`project_dir_keep_open`] is the variant
/// that keeps a file open after a save.
fn project_dir(
    shared: &SharedServerState,
    agent: &str,
    path: &str,
    entries: Vec<FileEntry>,
    error: Option<String>,
) {
    let agent = agent.to_owned();
    let path = path.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_FILES_AGENT,
            App::FIELD_FILES_PATH,
            App::FIELD_FILES_ENTRIES,
            App::FIELD_FILES_ERROR,
            App::FIELD_FILES_NEW_NAME,
            App::FIELD_FILES_CREATING,
            App::FIELD_FILES_RENAME,
            App::FIELD_FILES_PENDING_DELETE,
            App::FIELD_FILES_OPEN,
        ]),
        move |app| {
            app.files_agent = (!agent.is_empty()).then_some(agent);
            app.files_path = path;
            app.files_entries = entries;
            app.files_error = error;
            app.files_new_name.clear();
            app.files_creating = None;
            app.files_rename = None;
            app.files_pending_delete = None;
            app.files_open = None;
        },
    );
}

/// Like [`project_dir`] but sets the open file (used after a successful save).
fn project_dir_keep_open(
    shared: &SharedServerState,
    agent: &str,
    path: &str,
    entries: Vec<FileEntry>,
    open: Option<OpenFile>,
) {
    let agent = agent.to_owned();
    let path = path.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_FILES_AGENT,
            App::FIELD_FILES_PATH,
            App::FIELD_FILES_ENTRIES,
            App::FIELD_FILES_ERROR,
            App::FIELD_FILES_NEW_NAME,
            App::FIELD_FILES_CREATING,
            App::FIELD_FILES_RENAME,
            App::FIELD_FILES_PENDING_DELETE,
            App::FIELD_FILES_OPEN,
        ]),
        move |app| {
            app.files_agent = (!agent.is_empty()).then_some(agent);
            app.files_path = path;
            app.files_entries = entries;
            app.files_error = None;
            app.files_new_name.clear();
            app.files_creating = None;
            app.files_rename = None;
            app.files_pending_delete = None;
            app.files_open = open;
        },
    );
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
