//! The task board (the `tasks` + `task_blockers` tables): a per-workspace queue of work
//! cards. A workspace runs its tasks one at a time (sequential), workspaces run in parallel,
//! and a task can be blocked by earlier tasks in the same workspace (a chain = a pipeline).
//!
//! Storage + the scheduler scan: `create`/`get`/`list_for_workspace`/`cancel` (the manual
//! queue), `next_ready`/`running_in`/`finish`/`reschedule` (the scheduler), and
//! `add_blocker` (dependencies). Recurrence gates `next_ready` on `next_run`.

use rusqlite::{Connection, OptionalExtension, params};

/// A task's lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    /// Parked at a human gate (or a loop's on-exhaust=human) — frees the lane, awaits the user.
    Awaiting,
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
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "running" => TaskStatus::Running,
            "awaiting" => TaskStatus::Awaiting,
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
}

const SELECT: &str = "SELECT id, workspace_id, agent_group_id, title, spec, kind, schedule, \
                      status, result, priority, origin_chat_id, next_run, step_position, \
                      iteration FROM tasks";

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
            origin_chat_id, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, 0,
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
        ],
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
        "{SELECT} WHERE workspace_id = ?1
          ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                   priority DESC, created_at ASC"
    ))?;
    let rows = stmt.query_map(params![workspace_id], from_row)?;
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

/// Cancel a task (a terminal status; it stays on the board as a record).
pub fn cancel(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    set_status(conn, id, TaskStatus::Cancelled)
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

/// The next pending task ready to run in a workspace: highest priority, then oldest, and
/// with no still-unfinished blocker. `None` when the lane is empty or everything pending
/// is still waiting on a blocker.
pub fn next_ready(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!(
            "{SELECT} WHERE workspace_id = ?1 AND status = 'pending'
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

/// Reschedule a recurrent task after a run: store its result + `last_run`, set the computed
/// `next_run`, and return it to `pending` so the lane re-runs it once that slot arrives.
pub fn reschedule(
    conn: &Connection,
    id: &str,
    next_run: &str,
    result: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks
            SET status = 'pending', next_run = ?2, result = ?3, step_position = 0,
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

/// Update a task's editable fields (title / spec / assignee / schedule); `kind` is recomputed
/// from the new schedule. Leaves status / result / next_run untouched.
pub fn update(
    conn: &Connection,
    id: &str,
    title: &str,
    spec: &str,
    agent: Option<&str>,
    schedule: Option<&str>,
) -> rusqlite::Result<()> {
    let kind = if schedule.is_some() {
        TaskKind::Recurrent
    } else {
        TaskKind::OneShot
    };
    conn.execute(
        "UPDATE tasks
            SET title = ?2, spec = ?3, agent_group_id = ?4, schedule = ?5, kind = ?6,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![id, title, spec, agent, schedule, kind.as_str()],
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
        )
        .expect("update");
        let t = get(&conn, "t1").expect("get").unwrap();
        assert_eq!(t.title, "New title");
        assert_eq!(t.spec, "new spec");
        assert_eq!(t.kind, TaskKind::Recurrent, "a schedule makes it recurrent");
        // Clearing the schedule flips it back to one-shot.
        update(&conn, "t1", "New title", "new spec", None, None).expect("update2");
        assert_eq!(
            get(&conn, "t1").expect("get").unwrap().kind,
            TaskKind::OneShot
        );
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
}
