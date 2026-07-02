//! The read side of the task store: lane scans, per-agent views, and the aggregate counts
//! the projections draw. Split from `tasks/mod.rs` (types + mutations); re-exported flat, so
//! callers keep addressing `tasks::next_ready` etc.

use rusqlite::{Connection, OptionalExtension, params};

use super::{SELECT, Task, from_row};

/// A workspace's lane, ordered for display: the running task first, then pending (highest
/// priority, then oldest), then the finished/cancelled tail.
pub fn list_for_workspace(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 AND archived_at IS NULL
          ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                   priority DESC, created_at ASC"
    ))?;
    let rows = stmt.query_map(params![workspace_id], from_row)?;
    rows.collect()
}

/// Every task this agent is the effective assignee of, oldest first — the agent-thread
/// timeline. That's tasks explicitly assigned to it, plus unassigned tasks in a workspace it
/// owns (an unassigned task defaults to the owner at run time). Includes archived tasks: the
/// agent thread is a full working history, unlike the board (which hides them).
pub fn list_for_agent(conn: &Connection, agent_id: &str) -> rusqlite::Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE agent_group_id = ?1
              OR (agent_group_id IS NULL
                  AND workspace_id IN (SELECT id FROM workspaces WHERE owner_agent_id = ?1))
          ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = stmt.query_map(params![agent_id], from_row)?;
    rows.collect()
}

/// The most recent terminally-finished task in `workspace_id` that `agent_id` produced for —
/// the "last task kept warm" recap source at producer launch (R3). Matched by the run chain
/// (an ok producer/stage run by this agent), not the assignee column, so owner-defaulted work
/// counts. Excludes `exclude` (the task being launched) and plan gate tasks.
pub fn last_finished_for_agent(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
    exclude: &str,
) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE workspace_id = ?2 AND id <> ?3 AND is_plan = 0
               AND status IN ('done', 'failed', 'rejected')
               AND EXISTS (SELECT 1 FROM task_runs r
                            WHERE r.task_id = tasks.id AND r.agent_id = ?1 AND r.ok = 1
                              AND r.role IN ('producer', 'stage'))
             ORDER BY updated_at DESC, id DESC LIMIT 1"
        ),
        params![agent_id, workspace_id, exclude],
        from_row,
    )
    .optional()
}

/// Whether the agent has open work (pending / running / awaiting) in this workspace — the
/// steer decision: open work means a queued message rides its next pass; idle means the
/// message materialises as a task. Effective-assignee semantics (owner-defaulted counts).
pub fn has_open_for_agent(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM tasks
             WHERE workspace_id = ?2 AND is_plan = 0
               AND status IN ('pending', 'running', 'awaiting')
               AND (agent_group_id = ?1
                    OR (agent_group_id IS NULL
                        AND ?2 IN (SELECT id FROM workspaces WHERE owner_agent_id = ?1))))",
        params![agent_id, workspace_id],
        |row| row.get(0),
    )
}

/// The workspace's currently running task, if any — the lane's busy check (a workspace
/// runs one task at a time).
pub fn running_in(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("{SELECT} WHERE workspace_id = ?1 AND status = 'running' LIMIT 1"),
        params![workspace_id],
        from_row,
    )
    .optional()
}

/// The workspace's currently running task **other than** `exclude_id`, if any. Used to hold the
/// one-running-task-per-workspace invariant when a task is resumed from a human gate: while it sat
/// parked the scheduler may have started another task, so the resume path must check the lane
/// before launching a second worker into the shared workspace tree.
pub fn other_running_in(
    conn: &Connection,
    workspace_id: &str,
    exclude_id: &str,
) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("{SELECT} WHERE workspace_id = ?1 AND status = 'running' AND id != ?2 LIMIT 1"),
        params![workspace_id, exclude_id],
        from_row,
    )
    .optional()
}

/// The next pending task ready to run in a workspace: highest priority, then oldest, and
/// with no still-unfinished blocker. `None` when the lane is empty or everything pending
/// is still waiting on a blocker.
pub fn next_ready(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE workspace_id = ?1 AND status = 'pending' AND is_plan = 0
               AND (next_run IS NULL OR next_run <= strftime('%Y-%m-%dT%H:%M:%fZ','now'))
               AND NOT EXISTS (
                 SELECT 1 FROM task_blockers b JOIN tasks bt ON bt.id = b.blocked_by
                  WHERE b.task_id = tasks.id AND bt.status != 'done')
             ORDER BY priority DESC, created_at ASC LIMIT 1"
        ),
        params![workspace_id],
        from_row,
    )
    .optional()
}

/// How many tasks are parked at a human gate (`awaiting`) in each workspace that has any —
/// the per-workspace "needs you" count for the sidebar badge. Workspaces with none are omitted.
pub fn count_awaiting_by_workspace(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT workspace_id, COUNT(*) FROM tasks WHERE status = 'awaiting' \
         GROUP BY workspace_id",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Per effective-assignee counts of in-flight tasks — `(agent_id, running, awaiting)` — for the
/// nav's agent status dot + approval badge. The effective assignee is the explicit
/// `agent_group_id`, or the workspace owner when the task is unassigned. Rows whose assignee
/// resolves to nothing (unassigned in an unowned workspace) are dropped.
pub fn count_inflight_by_agent(conn: &Connection) -> rusqlite::Result<Vec<(String, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(t.agent_group_id, w.owner_agent_id) AS agent,
                SUM(CASE WHEN t.status = 'running' THEN 1 ELSE 0 END),
                SUM(CASE WHEN t.status = 'awaiting' THEN 1 ELSE 0 END)
           FROM tasks t JOIN workspaces w ON w.id = t.workspace_id
          WHERE t.status IN ('running', 'awaiting')
          GROUP BY agent
          HAVING agent IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect()
}

/// Whether a workspace has a task **mid-pipeline** (`running` or `awaiting`) — the pipeline
/// editor is locked only while these exist, so a task actively flowing through the steps never
/// sees them change under it. A `pending` task is just queued: it picks up the pipeline fresh
/// when it starts, so it does *not* lock editing.
pub fn has_inflight(conn: &Connection, workspace_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE workspace_id = ?1
            AND status IN ('running','awaiting'))",
        params![workspace_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
}
