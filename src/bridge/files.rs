//! Workspace file access on the bridge thread (it owns fs access). Two concerns live here:
//!
//! - The **Overview** editors (`AGENT.md` brief, `STANDARDS.md` memo, the machine-check command):
//!   plain writes at the workspace root, re-projected into `App.workspace_*`.
//! - The **Files** tab's [`FsSnapshot`](rwire_components::FsSnapshot): scanned per workspace and
//!   cached here (the snapshot can't live on `App` — no `Default`), then handed to the
//!   `rwire_editor::FileEditor` kit, which owns browsing + jailed edit/create/rename/delete itself.
//!
//! A workspace is shared by every agent assigned to it, so editing here is visible to all of them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use rwire::{ChangeSet, SharedServerState};
use rwire_components::FsSnapshot;
use rwire_editor::FileEditorState;

use crate::db::workspaces;
use crate::state::App;
use crate::workspace::ops;
use crate::workspace::{self, WorkspaceError};

/// How deep the Files tree scans, and the build-output dirs it never walks (matching
/// `workspace::ops::list_files`; the dot-prefixed noise — `.git`, `.cache`, `.venv`, `.isola` — is
/// already skipped by `FsSnapshot`).
const FILES_SCAN_DEPTH: usize = 8;
const FILES_IGNORE: &[&str] = &["target", "node_modules", "dist"];

/// The scanned tree per workspace — process-global (the `FsSnapshot` can't be an `App` field). The
/// `FileEditor` renderer and its event handler both read it via [`snapshot_for`]; [`open_files`]
/// refreshes it on entry. `Arc` so callers get an owned handle without holding the lock.
static FILES_SNAP: OnceLock<Mutex<HashMap<String, Arc<FsSnapshot>>>> = OnceLock::new();

fn snapshot_lock() -> &'static Mutex<HashMap<String, Arc<FsSnapshot>>> {
    FILES_SNAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cached snapshot for `workspace`, if it's been opened. The `FileEditor` renderer/handler
/// resolve every path through it, so it is the Files tab's jail root.
pub fn snapshot_for(workspace: &str) -> Option<Arc<FsSnapshot>> {
    snapshot_lock()
        .lock()
        .ok()
        .and_then(|map| map.get(workspace).cloned())
}

/// Resolve a workspace id to its (scaffolded) directory + display name.
fn resolve(conn: &rusqlite::Connection, workspace_id: &str) -> Option<(PathBuf, String)> {
    let workspace = workspaces::get(conn, workspace_id).ok().flatten()?;
    let dir = workspace::dir_for(&super::data_dir(), &workspace.id);
    // Ensure the dir exists (+ a starter AGENT.md) so a never-run workspace isn't empty.
    let _ = workspace::scaffold(&dir, &workspace.name);
    Some((dir, workspace.name))
}

/// Open the Files tab for `workspace`: (re)scan its dir into the snapshot cache, then point the
/// editor at it — resetting the editor only when the workspace actually changed, so tab-switching
/// within one workspace keeps the open file. Runs on the bridge thread (needs the DB + scaffold), so
/// the dir exists before `scan`'s `canonicalize`.
pub fn open_files(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return super::tasks::surface(shared, "Unknown workspace.".to_owned());
    };
    let snap = match FsSnapshot::scan_ignoring(&dir, FILES_SCAN_DEPTH, FILES_IGNORE) {
        Ok(snap) => snap,
        Err(err) => {
            return super::tasks::surface(shared, format!("Couldn't open the files: {err}"));
        }
    };
    if let Ok(mut map) = snapshot_lock().lock() {
        map.insert(workspace.to_owned(), Arc::new(snap));
    }
    let workspace = workspace.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_FILES_WORKSPACE, App::FIELD_EDITOR]),
        move |app| {
            let switched = app.files_workspace.as_deref() != Some(workspace.as_str());
            app.files_workspace = Some(workspace);
            if switched {
                app.editor = FileEditorState::manual();
            }
        },
    );
}

/// Load a workspace's `AGENT.md` (raw) into `App.workspace_brief` for the Overview editor.
pub fn load_brief(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let dir = resolve(conn, workspace).map(|(dir, _)| dir);
    let read = |name: &str| {
        dir.as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(name)).ok())
            .unwrap_or_default()
    };
    let brief = read("AGENT.md");
    let standards = read("STANDARDS.md");
    let checks = crate::db::settings::get_or(conn, &checks_key(workspace), "");
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_WORKSPACE_BRIEF,
            App::FIELD_WORKSPACE_STANDARDS,
            App::FIELD_WORKSPACE_CHECKS,
        ]),
        move |app| {
            app.workspace_brief = brief;
            app.workspace_standards = standards;
            app.workspace_checks = checks;
        },
    );
}

/// The settings key for a workspace's machine-check command (L1 of docs/levers.md).
pub(crate) fn checks_key(workspace: &str) -> String {
    format!("workspace.{workspace}.checks")
}

/// Save the workspace's machine-check command — run by the harness after each successful
/// producer pass, its capped output injected into the review prompt.
pub fn save_checks(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    command: &str,
) {
    let _ = crate::db::settings::set(conn, &checks_key(workspace), command.trim());
    let command = command.trim().to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKSPACE_CHECKS]),
        move |app| app.workspace_checks = command,
    );
}

/// Save a workspace's `STANDARDS.md` — the validator standards memo (R5) — then re-project it.
pub fn save_standards(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    content: &str,
) {
    let Some((dir, _)) = resolve(conn, workspace) else {
        return;
    };
    if let Err(err) = ops::write_text(&dir, "STANDARDS.md", content) {
        return super::tasks::surface(shared, message(&err));
    }
    let content = content.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKSPACE_STANDARDS]),
        move |app| app.workspace_standards = content,
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
        return super::tasks::surface(shared, message(&err));
    }
    let content = content.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_WORKSPACE_BRIEF]),
        move |app| app.workspace_brief = content,
    );
}

/// A user-facing message for a workspace error.
fn message(err: &WorkspaceError) -> String {
    match err {
        WorkspaceError::Escape => "That path is outside the workspace.".to_owned(),
        WorkspaceError::NotFound | WorkspaceError::Io(_) => "No such file or directory.".to_owned(),
        other => other.to_string(),
    }
}
