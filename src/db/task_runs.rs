//! A task's run history as it flows through its pipeline (the `task_runs` table, Phase F): one
//! row per agent run. It's both the **chained context** (later steps see earlier outputs) and
//! the detail-view **audit trail** (the produce ⇄ validate thread of a loop).

use rusqlite::{Connection, params};

/// One recorded run of a task at a pipeline step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRun {
    pub id: String,
    pub position: i64,
    /// `stage` | `producer` | `validator`.
    pub role: String,
    pub agent: Option<String>,
    pub output: Option<String>,
    pub verdict: Option<String>,
    pub feedback: Option<String>,
    pub iteration: i64,
    pub ok: bool,
}

/// What a run produced, recorded after it finishes.
pub struct NewRun<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub position: i64,
    pub role: &'a str,
    pub agent: Option<&'a str>,
    pub output: Option<&'a str>,
    pub verdict: Option<&'a str>,
    pub feedback: Option<&'a str>,
    pub iteration: i64,
    pub ok: bool,
}

pub fn append(conn: &Connection, run: &NewRun) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO task_runs
           (id, task_id, position, role, agent_id, output, verdict, feedback, iteration, ok,
            created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            run.id,
            run.task_id,
            run.position,
            run.role,
            run.agent,
            run.output,
            run.verdict,
            run.feedback,
            run.iteration,
            i64::from(run.ok),
        ],
    )?;
    Ok(())
}

/// A task's runs in order (oldest first) — the chain.
pub fn list_for_task(conn: &Connection, task_id: &str) -> rusqlite::Result<Vec<TaskRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, position, role, agent_id, output, verdict, feedback, iteration, ok
           FROM task_runs WHERE task_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(TaskRun {
            id: row.get(0)?,
            position: row.get(1)?,
            role: row.get(2)?,
            agent: row.get(3)?,
            output: row.get(4)?,
            verdict: row.get(5)?,
            feedback: row.get(6)?,
            iteration: row.get(7)?,
            ok: row.get::<_, i64>(8)? != 0,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{endpoints, open_in_memory, tasks, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        workspaces::create(&conn, "ws", "WS").expect("ws");
        tasks::create(
            &conn,
            &tasks::NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "t",
                spec: "s",
                schedule: None,
                origin_chat: None,
            },
        )
        .expect("task");
        conn
    }

    #[test]
    fn append_and_list_in_order() {
        let conn = seeded();
        append(
            &conn,
            &NewRun {
                id: "r1",
                task_id: "t1",
                position: 0,
                role: "stage",
                agent: Some("ag"),
                output: Some("did the thing"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
            },
        )
        .expect("r1");
        append(
            &conn,
            &NewRun {
                id: "r2",
                task_id: "t1",
                position: 1,
                role: "stage",
                agent: Some("ag"),
                output: Some("refined it"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
            },
        )
        .expect("r2");
        let runs = list_for_task(&conn, "t1").expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].output.as_deref(), Some("did the thing"));
        assert_eq!(runs[1].position, 1);
    }
}
