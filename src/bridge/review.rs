//! The review inbox (Phase F-3): the cross-workspace surface for tasks parked at a human gate.
//! Projects the inbox list (the `/review` page) and the nav count badge; a verdict from a row
//! drives the loop via `scheduler::human_verdict`.

use rwire::{ChangeSet, SharedServerState};

use crate::db::{tasks, workspaces};
use crate::state::{App, ReviewItem};

/// Project just the count of awaiting tasks — the nav badge. Cheap; refreshed on each tick and
/// whenever a task parks or resolves.
pub fn project_count(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let count = tasks::count_awaiting(conn).unwrap_or(0).max(0) as usize;
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_REVIEW_COUNT]),
        move |app| app.review_count = count,
    );
}

/// Project the full inbox — every task parked at a human gate, with the work to review. Loaded
/// when the `/review` page opens and after a verdict.
pub fn load_inbox(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let items: Vec<ReviewItem> = tasks::list_awaiting(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|task| {
            let workspace = workspaces::get(conn, &task.workspace_id)
                .ok()
                .flatten()
                .map(|ws| ws.name)
                .unwrap_or_else(|| task.workspace_id.clone());
            ReviewItem {
                output: super::scheduler::last_producer_output(conn, &task.id),
                task_id: task.id,
                title: task.title,
                workspace_id: task.workspace_id,
                workspace,
                spec: task.spec,
            }
        })
        .collect();
    let count = items.len();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_REVIEW_INBOX, App::FIELD_REVIEW_COUNT]),
        move |app| {
            app.review_inbox = items;
            app.review_count = count;
        },
    );
}
