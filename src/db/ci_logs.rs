//! Full CI check logs (P4c): the whole output of each check, persisted per run so the logs drawer
//! can open it on demand — without bloating the run's `checks` JSON, which keeps only a capped tail
//! for the inline glance and the reviewer prompt. Keyed by (run, repo, check).
//!
//! Rows reference `task_runs(id)` with `ON DELETE CASCADE`, so a run's logs go when the run does.

use rusqlite::{Connection, params};

/// Store one check's full log for a run. `INSERT OR REPLACE`, so a restart re-running the same
/// (run, repo, check) overwrites the stale log rather than erroring on the primary key. Called
/// after the run is recorded, so the `run_id` foreign key resolves.
pub fn insert(
    conn: &Connection,
    run_id: &str,
    repo: &str,
    check: &str,
    log: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO ci_logs (run_id, repo, check_name, log, created_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![run_id, repo, check, log],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_stores_and_replaces_the_log_for_a_run_repo_check() {
        let conn = crate::db::open_in_memory().expect("open");
        // Unit-test the store in isolation: production inserts only after `task_runs::append`, so the
        // `run_id` FK holds — here we drop it to avoid standing up a whole task/run chain.
        conn.execute_batch("PRAGMA foreign_keys = OFF")
            .expect("fk off");

        let read = |run: &str, repo: &str, check: &str| -> Option<String> {
            conn.query_row(
                "SELECT log FROM ci_logs WHERE run_id=?1 AND repo=?2 AND check_name=?3",
                params![run, repo, check],
                |row| row.get::<_, String>(0),
            )
            .ok()
        };

        insert(&conn, "run-1", "projects/app", "clippy", "warning: unused\n…").expect("insert");
        assert_eq!(
            read("run-1", "projects/app", "clippy").as_deref(),
            Some("warning: unused\n…"),
        );
        // A different check / run is absent.
        assert!(read("run-1", "projects/app", "test").is_none());
        assert!(read("run-2", "projects/app", "clippy").is_none());
        // A restart re-running the same check overwrites the stale log.
        insert(&conn, "run-1", "projects/app", "clippy", "clean").expect("reinsert");
        assert_eq!(read("run-1", "projects/app", "clippy").as_deref(), Some("clean"));
    }
}
