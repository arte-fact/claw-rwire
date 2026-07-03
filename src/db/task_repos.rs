//! Pre-task git bases (G0 of `docs/requests.md`): one row per (task, repo) recording the
//! repo's HEAD before the task first ran. `repo` is the workspace-relative rel used across
//! the requests design — `""` for the meta-repo, else `projects/<name>`. Recording is
//! first-write-wins (`INSERT OR IGNORE`), so retries, resumes, and restarts never move a
//! base: it stays the true pre-task state for the life of the task.

use rusqlite::{Connection, params};

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// A repo the task touched and the commit to reset it to on restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoBase {
    pub repo: String,
    pub base_commit: String,
}

/// Record a repo's pre-task base. First write wins — a base already recorded for this
/// (task, repo) is kept, so the earliest observed state stays authoritative.
pub fn record_base(
    conn: &Connection,
    task_id: &str,
    repo: &str,
    base_commit: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO task_repos (task_id, repo, base_commit, created_at)
             VALUES (?1, ?2, ?3, {NOW})"
        ),
        params![task_id, repo, base_commit],
    )?;
    Ok(())
}

/// Every repo base recorded for a task, meta-repo (`""`) first.
pub fn bases_for(conn: &Connection, task_id: &str) -> rusqlite::Result<Vec<RepoBase>> {
    let mut stmt =
        conn.prepare("SELECT repo, base_commit FROM task_repos WHERE task_id = ?1 ORDER BY repo")?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(RepoBase {
            repo: row.get(0)?,
            base_commit: row.get(1)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{endpoints, open_in_memory, tasks, workspaces};

    /// The FK to `tasks(id)` is enforced — bases only exist for real tasks.
    fn seeded(task_ids: &[&str]) -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("endpoint");
        workspaces::create(&conn, "ws", "Project").expect("workspace");
        for id in task_ids {
            tasks::create(
                &conn,
                &tasks::NewTask {
                    id,
                    workspace_id: "ws",
                    agent: None,
                    title: "t",
                    spec: "do",
                    schedule: None,
                    origin_chat: None,
                    max_retries: None,
                },
            )
            .expect("task");
        }
        conn
    }

    #[test]
    fn first_recorded_base_wins() {
        let conn = seeded(&["t1"]);
        record_base(&conn, "t1", "", "aaa").expect("record");
        record_base(&conn, "t1", "", "bbb").expect("re-record is ignored");
        let bases = bases_for(&conn, "t1").expect("bases");
        assert_eq!(bases.len(), 1);
        assert_eq!(bases[0].base_commit, "aaa", "the pre-task base never moves");
    }

    #[test]
    fn bases_list_meta_repo_first_and_scope_by_task() {
        let conn = seeded(&["t1", "t2"]);
        record_base(&conn, "t1", "projects/site", "ccc").expect("record");
        record_base(&conn, "t1", "", "aaa").expect("record");
        record_base(&conn, "t2", "projects/other", "ddd").expect("record");
        let bases = bases_for(&conn, "t1").expect("bases");
        assert_eq!(
            bases,
            vec![
                RepoBase {
                    repo: String::new(),
                    base_commit: "aaa".to_owned()
                },
                RepoBase {
                    repo: "projects/site".to_owned(),
                    base_commit: "ccc".to_owned()
                },
            ]
        );
    }
}
