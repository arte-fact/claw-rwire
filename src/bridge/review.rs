//! Review projection: tasks parked at a human gate are reviewed *in the workspace* (the board's
//! "Needs you" filter + the Peek modal's Review tab), so there's no cross-workspace inbox. All
//! that's projected here is the per-workspace "needs you" count for the sidebar badges; a verdict
//! from the board/Peek drives the loop via `scheduler::human_verdict`.

use rwire::{ChangeSet, SharedServerState};

use crate::db::tasks;
use crate::state::{App, WorkspaceAwaiting};

/// Project the per-workspace count of awaiting tasks — the sidebar rows' "needs you" badges.
/// Cheap (one grouped query); refreshed on each tick and whenever a task parks or resolves.
pub fn project_awaiting(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let awaiting: Vec<WorkspaceAwaiting> = tasks::count_awaiting_by_workspace(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, count)| WorkspaceAwaiting {
            id,
            count: count.max(0) as usize,
        })
        .collect();
    shared
        .update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_AWAITING]), move |app| {
            app.awaiting = awaiting
        });
}
