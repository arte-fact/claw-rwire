pub mod agent_tools;
pub mod agents;
pub mod chat;
pub mod endpoints;
pub mod resolution;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

const SCHEMA: &str = include_str!("schema.sql");

/// How long a connection waits on a locked DB before erroring with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("open central db: {0}")]
    Open(#[source] rusqlite::Error),
    #[error("configure central db: {0}")]
    Configure(#[source] rusqlite::Error),
    #[error("apply schema: {0}")]
    Schema(#[source] rusqlite::Error),
}

/// Opens (creating if absent) the central registry DB and brings it to the current
/// schema. The schema is `CREATE TABLE IF NOT EXISTS`, so opening an existing
/// rust-claw central.db is a no-op — the format is shared.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    let conn = Connection::open(path).map_err(DbError::Open)?;
    configure(&conn)?;
    apply_schema(&conn)?;
    Ok(conn)
}

/// Put the DB in WAL mode with a busy timeout so several connections can read and
/// write concurrently. The bridge currently holds the only connection, but the
/// tool-calling loop will run on worker threads that each open their own connection
/// against the same `central.db`; without WAL those concurrent writers hit
/// `SQLITE_BUSY`. WAL is a property of the database file (set once, persists);
/// `busy_timeout` is per-connection, so every future opener calls this too. WAL is
/// transparent to rust-claw — the file format is unchanged.
fn configure(conn: &Connection) -> Result<(), DbError> {
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(DbError::Configure)?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })
    .map(|_| ())
    .map_err(DbError::Configure)
}

#[cfg(test)]
pub fn open_in_memory() -> Result<Connection, DbError> {
    let conn = Connection::open_in_memory().map_err(DbError::Open)?;
    apply_schema(&conn)?;
    Ok(conn)
}

fn apply_schema(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA).map_err(DbError::Schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("collect")
    }

    #[test]
    fn schema_creates_the_central_entities() {
        let conn = open_in_memory().expect("open");
        let tables = table_names(&conn);
        for expected in [
            "agent_groups",
            "channel_cursors",
            "connectors",
            "endpoints",
            "messaging_group_agents",
            "messaging_groups",
            "sessions",
            "web_messages",
        ] {
            assert!(tables.contains(&expected.to_owned()), "missing {expected}");
        }
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let conn = open_in_memory().expect("open");
        apply_schema(&conn).expect("second apply is a no-op");
    }

    #[test]
    fn open_creates_a_file_backed_db_with_the_schema() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("central.db");
        {
            let conn = open(&path).expect("open file db");
            assert!(table_names(&conn).contains(&"endpoints".to_owned()));
        }
        // Reopen from disk: the schema must have been persisted, not just cached.
        let reopened = Connection::open(&path).expect("reopen");
        assert!(table_names(&reopened).contains(&"connectors".to_owned()));
    }

    #[test]
    fn open_enables_wal() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("central.db");
        let conn = open(&path).expect("open file db");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal_mode");
        assert_eq!(mode, "wal");
    }
}
