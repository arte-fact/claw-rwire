//! The task board (the `tasks` + `task_blockers` tables): a per-workspace queue of work
//! cards. A workspace runs its tasks one at a time (sequential), workspaces run in parallel,
//! and a task can be blocked by earlier tasks in the same workspace (a chain = a pipeline).
//!
//! This increment is the storage + the manual queue (create / list / cancel); the scheduler
//! scan (ready/running/finish) and blockers land with the scheduler.

use rusqlite::{Connection, OptionalExtension, params};

/// A task's lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "running" => TaskStatus::Running,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
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
}

const SELECT: &str = "SELECT id, workspace_id, agent_group_id, title, spec, kind, schedule, \
                      status, result, priority FROM tasks";

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
