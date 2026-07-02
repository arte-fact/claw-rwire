//! Per-(agent, workspace) rolling memory (the `agent_memory` table): a summarized recollection
//! of the agent's own work in one scope, plus how far that summary has consumed the raw
//! transcript. Injected into the agent's runs as a "working memory" layer so context survives
//! across turns/tasks without sending the whole (growing) history every time.
//!
//! The scope key mirrors responsibility scope (the R3 context model): `None` workspace is the
//! agent's **global** row — the concierge's one conversation; `Some(workspace)` scopes a
//! producer/planner's memory to that workspace, so an agent in two workspaces holds two
//! independent memories and nothing leaks across. Stored as `''` (not NULL) so the composite
//! primary key upserts correctly.

use rusqlite::{Connection, OptionalExtension, params};

/// An agent's rolling memory in one scope: the `summary` text, `summarized_through` (the id of
/// the last chat message folded into it — global row only; workspace rows fold whole tasks and
/// keep it 0), and whether auto-summarization is `paused` (the operator froze it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pub summary: String,
    pub summarized_through: i64,
    pub paused: bool,
}

impl Memory {
    /// The summary trimmed, or `None` when empty — the caller skips the memory layer then.
    pub fn text(&self) -> Option<String> {
        let trimmed = self.summary.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
}

/// The stored scope key: `None` (the global row) is encoded as the empty string.
fn scope_key(workspace: Option<&str>) -> &str {
    workspace.unwrap_or("")
}

/// The agent's memory in `workspace` scope (`None` = its global row), or a default (empty
/// summary, marker 0, not paused) if it has none yet.
pub fn get(conn: &Connection, agent_id: &str, workspace: Option<&str>) -> rusqlite::Result<Memory> {
    conn.query_row(
        "SELECT summary, summarized_through_id, paused
           FROM agent_memory WHERE agent_group_id = ?1 AND workspace_id = ?2",
        params![agent_id, scope_key(workspace)],
        |row| {
            Ok(Memory {
                summary: row.get(0)?,
                summarized_through: row.get(1)?,
                paused: row.get::<_, i64>(2)? != 0,
            })
        },
    )
    .optional()
    .map(Option::unwrap_or_default)
}

/// Freeze or resume auto-summarization for one scope (operator control). Upserts so it works
/// before any summary exists.
pub fn set_paused(
    conn: &Connection,
    agent_id: &str,
    workspace: Option<&str>,
    paused: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_memory
           (agent_group_id, workspace_id, summary, summarized_through_id, paused, updated_at)
         VALUES (?1, ?2, '', 0, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(agent_group_id, workspace_id) DO UPDATE SET
           paused = excluded.paused, updated_at = excluded.updated_at",
        params![agent_id, scope_key(workspace), i64::from(paused)],
    )?;
    Ok(())
}

/// One scope row for the memory overview (R3.5 M5): whose memory, where, how big, how fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub agent_id: String,
    /// `""` = the agent's global (concierge) row.
    pub workspace_id: String,
    pub summary: String,
    pub updated_at: String,
    pub paused: bool,
}

/// Every memory row in the system, agent-then-workspace ordered — the overview's source.
pub fn list_all(conn: &Connection) -> rusqlite::Result<Vec<ScopeRow>> {
    let mut stmt = conn.prepare(
        "SELECT agent_group_id, workspace_id, summary, updated_at, paused
           FROM agent_memory ORDER BY agent_group_id ASC, workspace_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ScopeRow {
            agent_id: row.get(0)?,
            workspace_id: row.get(1)?,
            summary: row.get(2)?,
            updated_at: row.get(3)?,
            paused: row.get::<_, i64>(4)? != 0,
        })
    })?;
    rows.collect()
}

/// Replace one scope's rolling memory (summary + how far it has consumed the transcript).
pub fn set(
    conn: &Connection,
    agent_id: &str,
    workspace: Option<&str>,
    summary: &str,
    summarized_through: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_memory
           (agent_group_id, workspace_id, summary, summarized_through_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(agent_group_id, workspace_id) DO UPDATE SET
           summary = excluded.summary,
           summarized_through_id = excluded.summarized_through_id,
           updated_at = excluded.updated_at",
        params![agent_id, scope_key(workspace), summary, summarized_through],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, endpoints, open_in_memory};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-1", "A", Some("e"), Some("m")).expect("create");
        conn
    }

    #[test]
    fn missing_memory_is_default_empty() {
        let conn = seeded();
        let memory = get(&conn, "ag-1", None).expect("get");
        assert_eq!(memory, Memory::default());
        assert_eq!(memory.text(), None);
    }

    #[test]
    fn set_then_get_round_trips_and_upserts() {
        let conn = seeded();
        set(&conn, "ag-1", None, "remembered the plan", 42).expect("set");
        let memory = get(&conn, "ag-1", None).expect("get");
        assert_eq!(memory.summary, "remembered the plan");
        assert_eq!(memory.summarized_through, 42);
        assert_eq!(memory.text().as_deref(), Some("remembered the plan"));

        // A second write replaces (advancing the marker).
        set(&conn, "ag-1", None, "remembered more", 99).expect("update");
        let memory = get(&conn, "ag-1", None).expect("get");
        assert_eq!(memory.summary, "remembered more");
        assert_eq!(memory.summarized_through, 99);
    }

    #[test]
    fn workspace_scopes_are_independent_of_each_other_and_of_the_global_row() {
        let conn = seeded();
        set(&conn, "ag-1", None, "global recollection", 7).expect("global");
        set(&conn, "ag-1", Some("ws-a"), "knows workspace A", 0).expect("ws-a");
        set(&conn, "ag-1", Some("ws-b"), "knows workspace B", 0).expect("ws-b");

        assert_eq!(
            get(&conn, "ag-1", None).expect("get").summary,
            "global recollection"
        );
        assert_eq!(
            get(&conn, "ag-1", Some("ws-a")).expect("get").summary,
            "knows workspace A"
        );
        assert_eq!(
            get(&conn, "ag-1", Some("ws-b")).expect("get").summary,
            "knows workspace B"
        );
        // Overwriting one scope leaves the others untouched — the hard wall between workspaces.
        set(&conn, "ag-1", Some("ws-a"), "revised A", 0).expect("rewrite");
        assert_eq!(
            get(&conn, "ag-1", Some("ws-b")).expect("get").summary,
            "knows workspace B"
        );
        assert_eq!(
            get(&conn, "ag-1", None).expect("get").summary,
            "global recollection"
        );
    }

    #[test]
    fn blank_summary_reads_as_no_layer() {
        let conn = seeded();
        set(&conn, "ag-1", None, "   \n\t", 5).expect("set");
        assert_eq!(get(&conn, "ag-1", None).expect("get").text(), None);
    }

    #[test]
    fn pause_round_trips_and_survives_a_summary_write() {
        let conn = seeded();
        set_paused(&conn, "ag-1", None, true).expect("pause");
        assert!(get(&conn, "ag-1", None).expect("get").paused);
        // A later summarizer write must not unfreeze the operator's pause.
        set(&conn, "ag-1", None, "folded something", 10).expect("set");
        let memory = get(&conn, "ag-1", None).expect("get");
        assert!(memory.paused, "pause preserved across a summary write");
        assert_eq!(memory.summary, "folded something");
        set_paused(&conn, "ag-1", None, false).expect("resume");
        assert!(!get(&conn, "ag-1", None).expect("get").paused);
    }

    #[test]
    fn pause_is_per_scope() {
        let conn = seeded();
        set_paused(&conn, "ag-1", Some("ws-a"), true).expect("pause ws-a");
        assert!(get(&conn, "ag-1", Some("ws-a")).expect("get").paused);
        assert!(!get(&conn, "ag-1", None).expect("get").paused);
        assert!(!get(&conn, "ag-1", Some("ws-b")).expect("get").paused);
    }

    #[test]
    fn legacy_single_key_table_is_rekeyed_with_rows_becoming_global() {
        // Simulate a pre-R3 DB: the old single-PK table with one populated row, then migrate.
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE agent_groups (id TEXT PRIMARY KEY, name TEXT);
             INSERT INTO agent_groups (id, name) VALUES ('ag-1', 'A');
             CREATE TABLE agent_memory (
               agent_group_id        TEXT PRIMARY KEY REFERENCES agent_groups(id),
               summary               TEXT NOT NULL DEFAULT '',
               summarized_through_id INTEGER NOT NULL DEFAULT 0,
               paused                INTEGER NOT NULL DEFAULT 0,
               updated_at            TEXT NOT NULL
             );
             INSERT INTO agent_memory VALUES ('ag-1', 'old recollection', 12, 1, 't');",
        )
        .expect("legacy shape");
        crate::db::rekey_agent_memory_by_workspace(&conn).expect("rekey");
        // The old row is now the agent's global row, contents intact (incl. the pause).
        let memory = get(&conn, "ag-1", None).expect("get");
        assert_eq!(memory.summary, "old recollection");
        assert_eq!(memory.summarized_through, 12);
        assert!(memory.paused);
        // The new key works: a workspace row coexists with it.
        set(&conn, "ag-1", Some("ws-a"), "scoped", 0).expect("set scoped");
        assert_eq!(
            get(&conn, "ag-1", None).expect("get").summary,
            "old recollection"
        );
        // Running the migration again is a no-op (idempotent on every open).
        crate::db::rekey_agent_memory_by_workspace(&conn).expect("rekey twice");
        assert_eq!(
            get(&conn, "ag-1", Some("ws-a")).expect("get").summary,
            "scoped"
        );
    }
}
