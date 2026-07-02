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
    /// The run's full tool transcript (JSON array of `{name, arguments, result, ok}`), or `None`.
    pub transcript: Option<String>,
    /// The task attempt (manual-restart generation) this run belongs to.
    pub attempt: i64,
    /// ISO-8601 UTC creation stamp — the thread view shows it as the entry's `HH:MM`.
    pub created_at: String,
    /// The memory layer the run was launched with (R3.5 M4 provenance, capped at write time),
    /// or `None` — answers "what did the agent remember when it did this?".
    pub memory: Option<String>,
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
    pub transcript: Option<&'a str>,
    pub attempt: i64,
    /// The memory layer the run launched with (producer/stage runs; validators run memory-free).
    pub memory: Option<&'a str>,
}

pub fn append(conn: &Connection, run: &NewRun) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO task_runs
           (id, task_id, position, role, agent_id, output, verdict, feedback, iteration, ok,
            transcript, attempt, memory_snapshot, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
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
            run.transcript,
            run.attempt,
            run.memory,
        ],
    )?;
    Ok(())
}

/// A task's runs in order (oldest first) — the chain. Ordered by `created_at`, not `id`: the id
/// is a random token, so ordering by it scrambles the chain *and* makes "the latest producer
/// output" (the task's result) pick a wrong, often empty, run.
pub fn list_for_task(conn: &Connection, task_id: &str) -> rusqlite::Result<Vec<TaskRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, position, role, agent_id, output, verdict, feedback, iteration, ok, \
         transcript, attempt, created_at, memory_snapshot
           FROM task_runs WHERE task_id = ?1 ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![task_id], from_row)?;
    rows.collect()
}

/// A task's runs for one **attempt** only, oldest first — the chained context the scheduler
/// feeds the next step, so a restart (a fresh attempt) never inherits an earlier attempt's work.
pub fn list_for_task_attempt(
    conn: &Connection,
    task_id: &str,
    attempt: i64,
) -> rusqlite::Result<Vec<TaskRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, position, role, agent_id, output, verdict, feedback, iteration, ok, \
         transcript, attempt, created_at, memory_snapshot
           FROM task_runs WHERE task_id = ?1 AND attempt = ?2 ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![task_id, attempt], from_row)?;
    rows.collect()
}

/// One of an agent's runs with its task's context — for the agent thread, where a run on
/// *someone else's* task (a review) still needs the task's title + workspace for its link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub run: TaskRun,
    pub task_id: String,
    pub workspace_id: String,
    pub task_title: String,
}

/// The most recent `limit` runs executed *by* this agent, newest first, each joined with its
/// task's context. The agent thread's source for work the agent did outside its own lane
/// (reviews of other agents' tasks); capped in SQL so a long history never floods the projection.
pub fn recent_by_agent(
    conn: &Connection,
    agent_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<AgentRun>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.position, r.role, r.agent_id, r.output, r.verdict, r.feedback, \
         r.iteration, r.ok, r.transcript, r.attempt, r.created_at, r.memory_snapshot, \
         r.task_id, t.workspace_id, t.title
           FROM task_runs r JOIN tasks t ON t.id = r.task_id
          WHERE r.agent_id = ?1
          ORDER BY r.created_at DESC, r.id DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![agent_id, limit as i64], |row| {
        Ok(AgentRun {
            run: from_row(row)?,
            task_id: row.get(13)?,
            workspace_id: row.get(14)?,
            task_title: row.get(15)?,
        })
    })?;
    rows.collect()
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<TaskRun> {
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
        transcript: row.get(9)?,
        attempt: row.get(10)?,
        created_at: row.get(11)?,
        memory: row.get(12)?,
    })
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
                max_retries: None,
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
                transcript: Some(r#"[{"name":"write","arguments":"{}","result":"ok","ok":true}]"#),
                attempt: 0,
                memory: None,
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
                transcript: None,
                attempt: 0,
                memory: None,
            },
        )
        .expect("r2");
        let runs = list_for_task(&conn, "t1").expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].output.as_deref(), Some("did the thing"));
        assert_eq!(runs[1].position, 1);
        assert!(runs[0].transcript.as_deref().unwrap().contains("write"));
        assert_eq!(runs[1].transcript, None);
    }

    #[test]
    fn memory_snapshot_round_trips() {
        let conn = seeded();
        append(
            &conn,
            &NewRun {
                id: "r1",
                task_id: "t1",
                position: 0,
                role: "producer",
                agent: Some("ag"),
                output: Some("done"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt: 0,
                memory: Some("Last task — \"Build\" (done)\nknows the layout"),
            },
        )
        .expect("r1");
        let runs = list_for_task(&conn, "t1").expect("list");
        assert!(
            runs[0]
                .memory
                .as_deref()
                .unwrap()
                .contains("knows the layout")
        );
    }

    #[test]
    fn list_for_task_attempt_scopes_to_one_attempt() {
        let conn = seeded();
        fn run<'a>(id: &'a str, attempt: i64) -> NewRun<'a> {
            NewRun {
                id,
                task_id: "t1",
                position: 0,
                role: "stage",
                agent: None,
                output: Some(id),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt,
                memory: None,
            }
        }
        append(&conn, &run("a0", 0)).expect("a0");
        append(&conn, &run("a1", 1)).expect("a1");
        // The full chain has both; the attempt-scoped read has only the matching attempt.
        assert_eq!(list_for_task(&conn, "t1").expect("all").len(), 2);
        let scoped = list_for_task_attempt(&conn, "t1", 1).expect("scoped");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].output.as_deref(), Some("a1"));
    }

    #[test]
    fn recent_by_agent_joins_task_context_and_caps() {
        let conn = seeded();
        for (id, agent) in [("r1", "critic"), ("r2", "critic"), ("r3", "forge")] {
            append(
                &conn,
                &NewRun {
                    id,
                    task_id: "t1",
                    position: 0,
                    role: "validator",
                    agent: Some(agent),
                    output: Some(id),
                    verdict: Some("revise"),
                    feedback: None,
                    iteration: 0,
                    ok: true,
                    transcript: None,
                    attempt: 0,
                    memory: None,
                },
            )
            .expect(id);
        }

        let runs = recent_by_agent(&conn, "critic", 10).expect("by agent");
        assert_eq!(runs.len(), 2, "only critic's runs");
        assert_eq!(runs[0].task_title, "t", "joined with the task's title");
        assert_eq!(runs[0].workspace_id, "ws");
        assert_eq!(runs[0].task_id, "t1");
        // Newest first, and the SQL cap holds.
        assert_eq!(runs[0].run.id, "r2");
        assert_eq!(
            recent_by_agent(&conn, "critic", 1).expect("capped").len(),
            1
        );
    }
}
