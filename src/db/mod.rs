pub mod agent_memory;
pub mod agent_prompts;
pub mod agent_tools;
pub mod agents;
pub mod chat;
pub mod endpoints;
pub mod pipeline;
pub mod resolution;
pub mod settings;
pub mod task_runs;
pub mod tasks;
pub mod workspaces;

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
    migrate(&conn)?;
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
    migrate(&conn)?;
    Ok(conn)
}

fn apply_schema(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA).map_err(DbError::Schema)
}

/// In-place migrations for claw-rwire-only tables whose shape changed after a DB was first
/// created (`CREATE TABLE IF NOT EXISTS` can't alter an existing table). Each step is
/// idempotent, so this runs safely on every open. rust-claw never reads these tables.
fn migrate(conn: &Connection) -> Result<(), DbError> {
    add_workspace_owner_column(conn).map_err(DbError::Schema)?;
    widen_agent_workspaces_to_m_n(conn).map_err(DbError::Schema)?;
    add_agent_memory_paused_column(conn).map_err(DbError::Schema)?;
    add_task_pipeline_columns(conn).map_err(DbError::Schema)?;
    Ok(())
}

/// Add the Phase F pipeline columns to a `tasks` table that predates them. A fresh DB already
/// has them from the schema, so each ALTER fails "duplicate column" — treated as success.
fn add_task_pipeline_columns(conn: &Connection) -> rusqlite::Result<()> {
    for column in [
        "ALTER TABLE tasks ADD COLUMN step_position INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE tasks ADD COLUMN iteration INTEGER NOT NULL DEFAULT 0",
    ] {
        match conn.execute(column, []) {
            Ok(_) => {}
            Err(err) if is_duplicate_column(&err) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Add `agent_memory.paused` to a DB whose `agent_memory` predates it. A fresh DB already has
/// it from the schema, so the ALTER fails "duplicate column" — treated as success.
fn add_agent_memory_paused_column(conn: &Connection) -> rusqlite::Result<()> {
    match conn.execute(
        "ALTER TABLE agent_memory ADD COLUMN paused INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(err) if is_duplicate_column(&err) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Add `workspaces.owner_agent_id` to a DB created before it existed. A fresh DB already has
/// it from the schema, so the ALTER fails with "duplicate column" — which we treat as success.
fn add_workspace_owner_column(conn: &Connection) -> rusqlite::Result<()> {
    match conn.execute("ALTER TABLE workspaces ADD COLUMN owner_agent_id TEXT", []) {
        Ok(_) => conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_workspaces_owner
               ON workspaces(owner_agent_id) WHERE owner_agent_id IS NOT NULL",
        ),
        Err(err) if is_duplicate_column(&err) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Rebuild `agent_workspaces` from its old single-column PK (`agent_group_id`, one workspace
/// per agent) to the composite PK (M:N membership). Detected by whether `workspace_id` is
/// part of the primary key; a fresh DB already has the composite PK, so this is a no-op.
fn widen_agent_workspaces_to_m_n(conn: &Connection) -> rusqlite::Result<()> {
    let workspace_in_pk: bool = conn
        .prepare(
            "SELECT pk FROM pragma_table_info('agent_workspaces') WHERE name = 'workspace_id'",
        )?
        .query_row([], |row| row.get::<_, i64>(0))
        .map(|pk| pk > 0)
        .unwrap_or(false);
    if workspace_in_pk {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE agent_workspaces_new (
           agent_group_id TEXT NOT NULL REFERENCES agent_groups(id),
           workspace_id   TEXT NOT NULL REFERENCES workspaces(id),
           PRIMARY KEY (agent_group_id, workspace_id)
         );
         INSERT OR IGNORE INTO agent_workspaces_new (agent_group_id, workspace_id)
           SELECT agent_group_id, workspace_id FROM agent_workspaces;
         DROP TABLE agent_workspaces;
         ALTER TABLE agent_workspaces_new RENAME TO agent_workspaces;",
    )
}

fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    err.to_string().contains("duplicate column name")
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
            "agent_prompts",
            "agent_workspaces",
            "channel_cursors",
            "connectors",
            "endpoints",
            "messaging_group_agents",
            "messaging_groups",
            "sessions",
            "web_messages",
            "workspaces",
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

    /// `pk` position of a column in `table`'s primary key (0 = not part of the PK).
    fn pk_of(conn: &Connection, table: &str, column: &str) -> i64 {
        conn.query_row(
            &format!("SELECT pk FROM pragma_table_info('{table}') WHERE name = ?1"),
            [column],
            |row| row.get(0),
        )
        .expect("table_info")
    }

    #[test]
    fn migrate_upgrades_an_old_shape_db_in_place() {
        // Start from the full current schema (so agent_groups etc. exist), then *downgrade*
        // the two tables to their pre-Phase-A shape — workspaces without an owner column and
        // agent_workspaces with the old single-column PK — to simulate an existing dev DB.
        let conn = Connection::open_in_memory().expect("db");
        apply_schema(&conn).expect("schema");
        conn.execute_batch(
            "DROP TABLE agent_workspaces;
             DROP TABLE workspaces;
             CREATE TABLE workspaces (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL
             );
             CREATE TABLE agent_workspaces (
               agent_group_id TEXT PRIMARY KEY,
               workspace_id   TEXT NOT NULL
             );
             INSERT INTO agent_groups (id, name, folder, created_at)
               VALUES ('ag-a', 'A', 'ag-a', 'now');
             INSERT INTO workspaces (id, name, created_at) VALUES ('ws-1', 'One', 'now');
             INSERT INTO agent_workspaces (agent_group_id, workspace_id) VALUES ('ag-a', 'ws-1');",
        )
        .expect("seed old shape");
        assert_eq!(
            pk_of(&conn, "agent_workspaces", "workspace_id"),
            0,
            "old PK"
        );

        migrate(&conn).expect("migrate");

        // The owner column now exists, and agent_workspaces has the composite PK with its
        // one membership row carried over.
        assert!(pk_of(&conn, "workspaces", "owner_agent_id") >= 0); // column resolves
        assert!(
            pk_of(&conn, "agent_workspaces", "workspace_id") > 0,
            "composite PK"
        );
        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM agent_workspaces WHERE agent_group_id='ag-a' AND workspace_id='ws-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(kept, 1, "membership row carried over");

        // Idempotent: a second migrate is a no-op.
        migrate(&conn).expect("migrate again");
        assert!(pk_of(&conn, "agent_workspaces", "workspace_id") > 0);
    }
}
