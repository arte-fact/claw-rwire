//! The task board (the `tasks` + `task_blockers` tables): a per-workspace queue of work
//! cards. A workspace runs its tasks one at a time (sequential), workspaces run in parallel,
//! and a task can be blocked by earlier tasks in the same workspace (a chain = a pipeline).
//!
//! Storage + the scheduler scan: `create`/`get`/`list_for_workspace`/`cancel` (the manual
//! queue), `next_ready`/`running_in`/`finish`/`reschedule` (the scheduler), and
//! `add_blocker` (dependencies). Recurrence gates `next_ready` on `next_run`.

use rusqlite::{Connection, OptionalExtension, params};

mod control;
mod queries;
pub use queries::*;

pub mod proposal;
#[cfg(test)]
mod tests;
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
    /// ISO-8601 UTC creation stamp — the agent thread orders its "task opened" turn by it.
    pub created_at: String,
    /// The Request this task belongs to (G1) — `None` for a free-standing board task.
    pub request_id: Option<String>,
}

const SELECT: &str = "SELECT id, workspace_id, agent_group_id, title, spec, kind, schedule, \
                      status, result, priority, origin_chat_id, next_run, step_position, \
                      iteration, run_epoch, auto_retries, max_retries, attempt, is_plan, plan_id, \
                      archived_at, created_at, request_id \
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
        created_at: row.get(21)?,
        request_id: row.get(22)?,
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

/// Attach a task to a Request (G1) — set on the plan at ask time and on each materialised
/// child, so the lane can scope to the active request and delivery can find its task set.
pub fn set_request(conn: &Connection, id: &str, request_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET request_id = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, request_id],
    )?;
    Ok(())
}

/// How many of a request's tasks are **not** `done` yet (any status — pending, running,
/// awaiting, failed, cancelled…). Zero means every task landed: the delivery condition.
pub fn open_for_request(conn: &Connection, request_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE request_id = ?1 AND status != 'done'",
        params![request_id],
        |row| row.get(0),
    )
}

/// A request's tasks (the plan included), oldest first — the request page's timeline.
pub fn list_for_request(conn: &Connection, request_id: &str) -> rusqlite::Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE request_id = ?1 ORDER BY created_at ASC"
    ))?;
    let rows = stmt.query_map(params![request_id], from_row)?;
    rows.collect()
}

/// How many work tasks a request materialised (the plan gate excluded) — the receipt's
/// "N task(s)" and the digest's progress denominator.
pub fn count_for_request(conn: &Connection, request_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE request_id = ?1 AND is_plan = 0",
        params![request_id],
        |row| row.get(0),
    )
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
