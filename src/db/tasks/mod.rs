//! The task board (the `tasks` + `task_blockers` tables): a per-workspace queue of work
//! cards. A workspace runs its tasks one at a time (sequential), workspaces run in parallel,
//! and a task can be blocked by earlier tasks in the same workspace (a chain = a pipeline).
//!
//! Storage + the scheduler scan: `create`/`get`/`list_for_workspace`/`cancel` (the manual
//! queue), `next_ready`/`running_in`/`finish`/`reschedule` (the scheduler), and
//! `add_blocker` (dependencies). Recurrence gates `next_ready` on `next_run`.

use rusqlite::{Connection, OptionalExtension, params};

mod control;
pub mod proposal;
pub use control::*;

/// A task's lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    /// Parked at a human gate (or a loop's on-exhaust=human) — frees the lane, awaits the user.
    Awaiting,
    /// Paused by the user — idle (frees the lane), invisible to the ready-scan, and editable.
    /// The resting state from which Start / Restart / edit / ask-owner happen. Non-terminal.
    Stopped,
    Done,
    Failed,
    /// A reviewer (agent or human) rejected the work — terminal.
    Rejected,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Awaiting => "awaiting",
            TaskStatus::Stopped => "stopped",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "running" => TaskStatus::Running,
            "awaiting" => TaskStatus::Awaiting,
            "stopped" => TaskStatus::Stopped,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            "rejected" => TaskStatus::Rejected,
            "cancelled" => TaskStatus::Cancelled,
            _ => TaskStatus::Pending,
        }
    }
}

/// Whether a task runs once or on a recurring schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    OneShot,
    Recurrent,
}

impl TaskKind {
    fn as_str(self) -> &'static str {
        match self {
            TaskKind::OneShot => "oneshot",
            TaskKind::Recurrent => "recurrent",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "recurrent" => TaskKind::Recurrent,
            _ => TaskKind::OneShot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub workspace_id: String,
    /// The assigned agent (a workspace member), or `None` to default to the owner.
    pub agent: Option<String>,
    pub title: String,
    pub spec: String,
    pub kind: TaskKind,
    pub schedule: Option<String>,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub priority: i64,
    /// The chat the delegation came from — where the concierge notifies on completion.
    pub origin_chat: Option<String>,
    /// The next scheduled fire (recurrent tasks), ISO-8601 UTC, or `None`.
    pub next_run: Option<String>,
    /// The current pipeline step the task is on (Phase F).
    pub step_position: i64,
    /// The iteration within the current loop step (Phase F).
    pub iteration: i64,
    /// The dispatch guard: a run's result is dropped unless its captured epoch still matches.
    /// Bumped on every launch and on every stop / cancel / restart.
    pub run_epoch: i64,
    /// Auto-retries consumed since the last manual start/restart (capped by the retry budget).
    pub auto_retries: i64,
    /// Per-task auto-retry budget, or `None` to use the global default.
    pub max_retries: Option<i64>,
    /// The manual-restart generation — runs are stamped with it, so a restart's context excludes
    /// earlier attempts.
    pub attempt: i64,
    /// A reviewable "Plan" gate task (R3): it carries no pipeline work; its held children await a
    /// human verdict on the workspace board (the Peek modal's Review tab).
    pub is_plan: bool,
    /// For a task materialised from a plan (R3): the id of the plan it came from.
    pub plan_id: Option<String>,
    /// When set, the task is **archived** — hidden from the board to declutter it.
    pub archived_at: Option<String>,
}

const SELECT: &str = "SELECT id, workspace_id, agent_group_id, title, spec, kind, schedule, \
                      status, result, priority, origin_chat_id, next_run, step_position, \
                      iteration, run_epoch, auto_retries, max_retries, attempt, is_plan, plan_id, \
                      archived_at \
                      FROM tasks";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        agent: row.get(2)?,
        title: row.get(3)?,
        spec: row.get(4)?,
        kind: TaskKind::from_str(&row.get::<_, String>(5)?),
        schedule: row.get(6)?,
        status: TaskStatus::from_str(&row.get::<_, String>(7)?),
        result: row.get(8)?,
        priority: row.get(9)?,
        origin_chat: row.get(10)?,
        next_run: row.get(11)?,
        step_position: row.get(12)?,
        iteration: row.get(13)?,
        run_epoch: row.get(14)?,
        auto_retries: row.get(15)?,
        max_retries: row.get(16)?,
        attempt: row.get(17)?,
        is_plan: row.get::<_, i64>(18)? != 0,
        plan_id: row.get(19)?,
        archived_at: row.get(20)?,
    })
}

/// A new task to queue. `schedule` (a cron string) makes it recurrent.
pub struct NewTask<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub agent: Option<&'a str>,
    pub title: &'a str,
    pub spec: &'a str,
    pub schedule: Option<&'a str>,
    pub origin_chat: Option<&'a str>,
    /// Per-task auto-retry budget override, or `None` to use the global default.
    pub max_retries: Option<i64>,
}

pub fn create(conn: &Connection, task: &NewTask) -> rusqlite::Result<()> {
    let kind = if task.schedule.is_some() {
        TaskKind::Recurrent
    } else {
        TaskKind::OneShot
    };
    conn.execute(
        "INSERT INTO tasks
           (id, workspace_id, agent_group_id, title, spec, kind, schedule, status,
            origin_chat_id, max_retries, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            task.id,
            task.workspace_id,
            task.agent,
            task.title,
            task.spec,
            kind.as_str(),
            task.schedule,
            task.origin_chat,
            task.max_retries,
        ],
    )?;
    Ok(())
}

/// Create a task **materialised from an approved plan** (R3, Option A): identical to [`create`]
/// (queued `pending`, picked up by the scheduler) but tagged with `plan_id` for traceability.
/// Before approval a plan holds only its JSON [`proposal`] — no child rows exist.
pub fn create_planned(conn: &Connection, task: &NewTask, plan_id: &str) -> rusqlite::Result<()> {
    let kind = if task.schedule.is_some() {
        TaskKind::Recurrent
    } else {
        TaskKind::OneShot
    };
    conn.execute(
        "INSERT INTO tasks
           (id, workspace_id, agent_group_id, title, spec, kind, schedule, status,
            origin_chat_id, max_retries, plan_id, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            task.id,
            task.workspace_id,
            task.agent,
            task.title,
            task.spec,
            kind.as_str(),
            task.schedule,
            task.origin_chat,
            task.max_retries,
            plan_id,
        ],
    )?;
    Ok(())
}

/// Create a reviewable **plan gate task** (R3): `is_plan`, parked `awaiting` for the user. It does
/// no pipeline work — it just holds its proposed children until approved.
pub fn create_plan(
    conn: &Connection,
    id: &str,
    workspace_id: &str,
    title: &str,
    spec: &str,
    origin_chat: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tasks
           (id, workspace_id, title, spec, kind, status, origin_chat_id, is_plan, priority,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'oneshot', 'awaiting', ?5, 1, 0,
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![id, workspace_id, title, spec, origin_chat],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), params![id], from_row)
        .optional()
}

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

pub fn set_status(conn: &Connection, id: &str, status: TaskStatus) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET status = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, status.as_str()],
    )?;
    Ok(())
}

/// Set a task's `result` note without changing its status — used to record *why* a task parked at
/// a human gate (e.g. an on-exhaust escalation reason), so the board/detail can show it.
pub fn set_result(conn: &Connection, id: &str, result: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET result = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, result],
    )?;
    Ok(())
}

/// Cancel a task (a terminal status; it stays on the board as a record). Bumps `run_epoch` so a
/// still-running worker's late result is dropped rather than overwriting the cancellation.
pub fn cancel(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET status = 'cancelled', run_epoch = run_epoch + 1,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Move a task to a pipeline step (Phase F); it stays `running` as it flows through the chain.
/// Resets `iteration` to 0 — a fresh step starts a fresh loop budget.
pub fn set_step(conn: &Connection, id: &str, position: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET step_position = ?2, iteration = 0,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, position],
    )?;
    Ok(())
}

/// Set a task's loop iteration counter (within its current loop step).
pub fn set_iteration(conn: &Connection, id: &str, iteration: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET iteration = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, iteration],
    )?;
    Ok(())
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

/// Cancel the tasks still waiting on `blocker_id` because it ended **without succeeding** (failed /
/// rejected / cancelled). They can never become ready — the ready-scan requires every blocker
/// `done` — so leaving them `pending` strands them silently forever. Cascades transitively (a
/// cancelled dependent strands *its* own dependents) and returns every id it cancelled, for
/// notification / re-projection. Only touches not-yet-started tasks (`pending` / `stopped`); a
/// running / awaiting / terminal dependent is left alone.
pub fn cancel_dependents(
    conn: &Connection,
    blocker_id: &str,
    reason: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut cancelled: Vec<String> = Vec::new();
    let mut frontier = vec![blocker_id.to_owned()];
    while let Some(blocker) = frontier.pop() {
        for dependent in waiting_dependents_of(conn, &blocker)? {
            conn.execute(
                "UPDATE tasks SET status = 'cancelled', result = ?2, run_epoch = run_epoch + 1,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                  WHERE id = ?1",
                params![dependent, reason],
            )?;
            cancelled.push(dependent.clone());
            frontier.push(dependent);
        }
    }
    Ok(cancelled)
}

/// The not-yet-started tasks (`pending` / `stopped`) that list `blocker_id` among their blockers.
fn waiting_dependents_of(conn: &Connection, blocker_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.id FROM tasks t JOIN task_blockers b ON b.task_id = t.id
          WHERE b.blocked_by = ?1 AND t.status IN ('pending', 'stopped')",
    )?;
    let rows = stmt.query_map(params![blocker_id], |row| row.get(0))?;
    rows.collect()
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

/// Reschedule a recurrent task after a run: store its result + `last_run`, set the computed
/// `next_run`, and return it to `pending` so the lane re-runs it once that slot arrives. Resets
/// `auto_retries` so **each** cron cycle gets a fresh retry budget — otherwise the budget spent in
/// the first cycle would leave every later occurrence with zero transient-failure tolerance.
pub fn reschedule(
    conn: &Connection,
    id: &str,
    next_run: &str,
    result: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks
            SET status = 'pending', next_run = ?2, result = ?3, step_position = 0,
                auto_retries = 0,
                last_run = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, next_run, result],
    )?;
    Ok(())
}

/// Record that `task_id` is blocked by `blocked_by` (both must be in the same workspace —
/// the caller enforces that). Idempotent on the composite primary key.
pub fn add_blocker(conn: &Connection, task_id: &str, blocked_by: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO task_blockers (task_id, blocked_by) VALUES (?1, ?2)",
        params![task_id, blocked_by],
    )?;
    Ok(())
}

/// The ids of the tasks blocking `task_id` (its dependencies).
pub fn blockers_of(conn: &Connection, task_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT blocked_by FROM task_blockers WHERE task_id = ?1")?;
    let rows = stmt.query_map(params![task_id], |row| row.get(0))?;
    rows.collect()
}

/// Replace a task's blocker set with `blockers` (clears the old set first). The caller
/// validates each blocker shares the workspace.
pub fn set_blockers(conn: &Connection, task_id: &str, blockers: &[String]) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM task_blockers WHERE task_id = ?1",
        params![task_id],
    )?;
    for blocked_by in blockers {
        add_blocker(conn, task_id, blocked_by)?;
    }
    Ok(())
}

/// Update a task's editable fields (title / spec / assignee / schedule / retry budget); `kind` is
/// recomputed from the new schedule. Leaves status / result / next_run untouched.
pub fn update(
    conn: &Connection,
    id: &str,
    title: &str,
    spec: &str,
    agent: Option<&str>,
    schedule: Option<&str>,
    max_retries: Option<i64>,
) -> rusqlite::Result<()> {
    let kind = if schedule.is_some() {
        TaskKind::Recurrent
    } else {
        TaskKind::OneShot
    };
    conn.execute(
        "UPDATE tasks
            SET title = ?2, spec = ?3, agent_group_id = ?4, schedule = ?5, kind = ?6,
                max_retries = ?7, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, title, spec, agent, schedule, kind.as_str(), max_retries],
    )?;
    Ok(())
}

/// Mark a task finished with a terminal status and its result text, stamping `last_run`.
pub fn finish(
    conn: &Connection,
    id: &str,
    status: TaskStatus,
    result: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks
            SET status = ?2, result = ?3,
                last_run = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, status.as_str(), result],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, endpoints, open_in_memory, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-a", "Coder", Some("e"), Some("m")).expect("a");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        conn
    }

    fn task(id: &str) -> NewTask<'_> {
        NewTask {
            id,
            workspace_id: "ws",
            agent: None,
            title: id,
            spec: "do the thing",
            schedule: None,
            origin_chat: None,
            max_retries: None,
        }
    }

    #[test]
    fn create_get_defaults_to_pending_oneshot() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("create");
        let t = get(&conn, "t1").expect("get").expect("some");
        assert_eq!(t.status, TaskStatus::Pending);
        assert_eq!(t.kind, TaskKind::OneShot);
        assert_eq!(t.workspace_id, "ws");
        assert_eq!(
            t.max_retries, None,
            "no override → the global default applies"
        );
    }

    #[test]
    fn create_persists_a_max_retries_override() {
        let conn = seeded();
        create(
            &conn,
            &NewTask {
                max_retries: Some(5),
                ..task("t1")
            },
        )
        .expect("create");
        assert_eq!(get(&conn, "t1").expect("get").unwrap().max_retries, Some(5));
    }

    #[test]
    fn schedule_makes_it_recurrent() {
        let conn = seeded();
        create(
            &conn,
            &NewTask {
                schedule: Some("0 8 * * *"),
                ..task("t1")
            },
        )
        .expect("create");
        let t = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(t.kind, TaskKind::Recurrent);
        assert_eq!(t.schedule.as_deref(), Some("0 8 * * *"));
    }

    #[test]
    fn cancel_is_terminal() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("create");
        cancel(&conn, "t1").expect("cancel");
        assert_eq!(
            get(&conn, "t1").expect("get").unwrap().status,
            TaskStatus::Cancelled
        );
    }

    #[test]
    fn running_in_reports_the_running_task_only() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1");
        assert!(running_in(&conn, "ws").expect("none").is_none());
        set_status(&conn, "t1", TaskStatus::Running).expect("run");
        assert_eq!(running_in(&conn, "ws").expect("some").unwrap().id, "t1");
    }

    #[test]
    fn next_ready_skips_running_and_terminal() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1");
        set_status(&conn, "t1", TaskStatus::Running).expect("run");
        // Only task is running → nothing ready.
        assert!(next_ready(&conn, "ws").expect("scan").is_none());
        create(&conn, &task("t2")).expect("t2");
        assert_eq!(next_ready(&conn, "ws").expect("scan").unwrap().id, "t2");
    }

    #[test]
    fn next_ready_skips_a_task_with_an_unfinished_blocker() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1"); // older, but blocked
        create(&conn, &task("t2")).expect("t2"); // the blocker
        conn.execute(
            "INSERT INTO task_blockers (task_id, blocked_by) VALUES ('t1', 't2')",
            [],
        )
        .expect("block");
        // t1 is older (would sort first) but is blocked by the still-pending t2 → t2 runs.
        assert_eq!(next_ready(&conn, "ws").expect("scan").unwrap().id, "t2");
        finish(&conn, "t2", TaskStatus::Done, Some("ok")).expect("finish");
        // Blocker done → t1 becomes ready.
        assert_eq!(next_ready(&conn, "ws").expect("scan").unwrap().id, "t1");
    }

    #[test]
    fn next_ready_gates_a_recurrent_task_on_its_next_run() {
        let conn = seeded();
        create(
            &conn,
            &NewTask {
                schedule: Some("0 8 * * *"),
                ..task("t1")
            },
        )
        .expect("create");
        // A future next_run hides it from the ready scan…
        reschedule(&conn, "t1", "2999-01-01T00:00:00.000Z", Some("ran")).expect("future");
        assert!(next_ready(&conn, "ws").expect("scan").is_none());
        // …a past next_run makes it ready again (its slot has arrived).
        reschedule(&conn, "t1", "2000-01-01T00:00:00.000Z", Some("ran")).expect("past");
        let ready = next_ready(&conn, "ws").expect("scan").expect("ready");
        assert_eq!(ready.id, "t1");
        assert_eq!(
            ready.status,
            TaskStatus::Pending,
            "rescheduled back to pending"
        );
    }

    #[test]
    fn update_rewrites_fields_and_recomputes_kind() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("create");
        update(
            &conn,
            "t1",
            "New title",
            "new spec",
            None,
            Some("0 9 * * *"),
            Some(5),
        )
        .expect("update");
        let t = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(t.title, "New title");
        assert_eq!(t.spec, "new spec");
        assert_eq!(t.kind, TaskKind::Recurrent, "a schedule makes it recurrent");
        assert_eq!(t.max_retries, Some(5), "the retry budget round-trips");
        // Clearing the schedule flips it back to one-shot; clearing max_retries restores default.
        update(&conn, "t1", "New title", "new spec", None, None, None).expect("update2");
        let t = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(t.kind, TaskKind::OneShot);
        assert_eq!(t.max_retries, None);
    }

    #[test]
    fn list_for_agent_covers_assigned_and_owned_unassigned_tasks() {
        let conn = seeded();
        agents::create(&conn, "ag-b", "Writer", Some("e"), Some("m")).expect("b");
        workspaces::create(&conn, "ws2", "Other").expect("ws2");
        workspaces::set_owner(&conn, "ws", "ag-a").expect("own ws");
        // Assigned directly to ag-a (in a workspace it doesn't own).
        create(
            &conn,
            &NewTask {
                id: "assigned",
                workspace_id: "ws2",
                agent: Some("ag-a"),
                ..task("assigned")
            },
        )
        .expect("assigned");
        // Unassigned in ag-a's owned workspace → defaults to the owner, so it's ag-a's.
        create(&conn, &task("owned")).expect("owned");
        // Assigned to someone else → not ag-a's.
        create(
            &conn,
            &NewTask {
                id: "other",
                workspace_id: "ws2",
                agent: Some("ag-b"),
                ..task("other")
            },
        )
        .expect("other");

        let ids: Vec<String> = list_for_agent(&conn, "ag-a")
            .expect("list")
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert!(ids.contains(&"assigned".to_owned()));
        assert!(ids.contains(&"owned".to_owned()));
        assert!(
            !ids.contains(&"other".to_owned()),
            "another agent's task is not this agent's"
        );
        // The non-owner ag-b only sees its explicit assignment, not ws's unassigned task.
        let b_ids: Vec<String> = list_for_agent(&conn, "ag-b")
            .expect("list b")
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(b_ids, vec!["other".to_owned()]);
    }

    #[test]
    fn set_blockers_replaces_the_blocker_set() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1");
        create(&conn, &task("t2")).expect("t2");
        create(&conn, &task("t3")).expect("t3");
        set_blockers(&conn, "t1", &["t2".to_owned(), "t3".to_owned()]).expect("set");
        let mut blockers = blockers_of(&conn, "t1").expect("of");
        blockers.sort();
        assert_eq!(blockers, vec!["t2".to_owned(), "t3".to_owned()]);
        // Replacing collapses to just t3.
        set_blockers(&conn, "t1", &["t3".to_owned()]).expect("set2");
        assert_eq!(
            blockers_of(&conn, "t1").expect("of2"),
            vec!["t3".to_owned()]
        );
    }

    #[test]
    fn other_running_in_excludes_self_and_finds_peers() {
        let conn = seeded();
        create(&conn, &task("a")).expect("a");
        create(&conn, &task("b")).expect("b");
        set_status(&conn, "a", TaskStatus::Running).expect("run a");
        // From a's own perspective no *other* task is running…
        assert!(other_running_in(&conn, "ws", "a").expect("scan").is_none());
        // …but from b's perspective, a holds the lane.
        assert_eq!(
            other_running_in(&conn, "ws", "b")
                .expect("scan")
                .unwrap()
                .id,
            "a"
        );
    }

    #[test]
    fn reschedule_resets_the_retry_budget_each_cycle() {
        // Regression (B4): a recurrent task that spent its retry budget in one cycle must get a
        // fresh budget on the next, not stay maxed out forever.
        let conn = seeded();
        create(
            &conn,
            &NewTask {
                schedule: Some("0 8 * * *"),
                ..task("t1")
            },
        )
        .expect("create");
        note_retry(&conn, "t1", "2999-01-01T00:00:00.000Z").expect("retry"); // auto_retries → 1
        assert_eq!(get(&conn, "t1").expect("get").unwrap().auto_retries, 1);
        reschedule(&conn, "t1", "2000-01-01T00:00:00.000Z", Some("ran")).expect("reschedule");
        assert_eq!(
            get(&conn, "t1").expect("get").unwrap().auto_retries,
            0,
            "each cron cycle starts with a fresh retry budget"
        );
    }

    #[test]
    fn cancel_dependents_cascades_through_the_chain() {
        // Regression (B2): when a blocker fails, its waiting followers (and theirs) must be
        // cancelled rather than left pending forever.
        let conn = seeded();
        create(&conn, &task("a")).expect("a"); // the blocker that fails
        create(&conn, &task("b")).expect("b"); // blocked by a
        create(&conn, &task("c")).expect("c"); // blocked by b
        add_blocker(&conn, "b", "a").expect("b<-a");
        add_blocker(&conn, "c", "b").expect("c<-b");
        let cancelled = cancel_dependents(&conn, "a", "blocker did not succeed").expect("cascade");
        assert_eq!(cancelled.len(), 2, "both b and c are cancelled");
        for id in ["b", "c"] {
            let t = get(&conn, id).expect("get").unwrap();
            assert_eq!(t.status, TaskStatus::Cancelled);
            assert_eq!(t.result.as_deref(), Some("blocker did not succeed"));
        }
    }

    #[test]
    fn cancel_dependents_leaves_started_followers_untouched() {
        let conn = seeded();
        create(&conn, &task("a")).expect("a");
        create(&conn, &task("b")).expect("b");
        add_blocker(&conn, "b", "a").expect("b<-a");
        set_status(&conn, "b", TaskStatus::Running).expect("run b");
        // A running dependent already cleared its blockers; never cancel it out from under a worker.
        assert!(
            cancel_dependents(&conn, "a", "x")
                .expect("cascade")
                .is_empty()
        );
        assert_eq!(
            get(&conn, "b").expect("get").unwrap().status,
            TaskStatus::Running
        );
    }

    #[test]
    fn finish_sets_terminal_status_and_result() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1");
        finish(&conn, "t1", TaskStatus::Done, Some("the report")).expect("finish");
        let t = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(t.status, TaskStatus::Done);
        assert_eq!(t.result.as_deref(), Some("the report"));
    }

    #[test]
    fn list_orders_running_then_pending_then_terminal() {
        let conn = seeded();
        create(&conn, &task("t1")).expect("t1");
        create(&conn, &task("t2")).expect("t2");
        create(&conn, &task("t3")).expect("t3");
        set_status(&conn, "t1", TaskStatus::Done).expect("done");
        set_status(&conn, "t2", TaskStatus::Running).expect("run");
        let ids: Vec<String> = list_for_workspace(&conn, "ws")
            .expect("list")
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec!["t2".to_owned(), "t3".to_owned(), "t1".to_owned()]);
    }

    #[test]
    fn count_awaiting_by_workspace_groups_only_parked_tasks() {
        let conn = seeded();
        workspaces::create(&conn, "ws2", "Other").expect("ws2");
        // ws: two parked + one still pending; ws2: one parked; a third workspace has none.
        for id in ["a1", "a2", "pending"] {
            create(&conn, &task(id)).expect("create");
        }
        create(
            &conn,
            &NewTask {
                workspace_id: "ws2",
                ..task("b1")
            },
        )
        .expect("create");
        for id in ["a1", "a2", "b1"] {
            set_status(&conn, id, TaskStatus::Awaiting).expect("park");
        }

        let mut counts = count_awaiting_by_workspace(&conn).expect("counts");
        counts.sort();
        // Only `awaiting` tasks count; the pending one and the empty workspace are absent.
        assert_eq!(counts, vec![("ws".to_owned(), 2), ("ws2".to_owned(), 1)]);
    }
}
