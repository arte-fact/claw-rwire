//! The memory journal (the `agent_memory_log` table, R3.5 M1): an append-only record of every
//! memory mutation in one (agent, workspace) scope — folds, operator edits/clears, pause
//! toggles — each with the summary *after* the event. Provenance for "what does this agent
//! know and where did it come from", the source of the thread's fold-event lines, and the
//! substrate for future undo/diff. Pruned to the last [`KEEP`] rows per scope on write.

use rusqlite::{Connection, params};

/// How many journal rows each (agent, workspace) scope keeps.
const KEEP: usize = 50;

/// What kind of mutation a journal row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEventKind {
    /// An automatic fold — a completed task (workspace scope) or older chat turns (global
    /// scope) condensed into the summary.
    Fold,
    /// The operator overwrote the summary.
    Edit,
    /// The operator reset the summary.
    Clear,
    /// The operator froze / resumed auto-folding.
    Pause,
    Resume,
}

impl MemoryEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryEventKind::Fold => "fold",
            MemoryEventKind::Edit => "edit",
            MemoryEventKind::Clear => "clear",
            MemoryEventKind::Pause => "pause",
            MemoryEventKind::Resume => "resume",
        }
    }

    /// Reads a stored kind; an unrecognized value (a future writer) degrades to `Edit`, the
    /// most neutral rendering.
    pub fn from_str(value: &str) -> Self {
        match value {
            "fold" => MemoryEventKind::Fold,
            "clear" => MemoryEventKind::Clear,
            "pause" => MemoryEventKind::Pause,
            "resume" => MemoryEventKind::Resume,
            _ => MemoryEventKind::Edit,
        }
    }
}

/// One journal row: what happened to the scope's memory, when, and what the summary was after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEvent {
    pub id: i64,
    pub kind: MemoryEventKind,
    /// The task/chat the event came from; `None` = the operator.
    pub source: Option<String>,
    /// The human line the thread renders ("condensed 'Build a color highlighter' into memory").
    pub note: String,
    /// The full summary after this event (undo/diff material — not rendered in the thread).
    pub summary: String,
    pub created_at: String,
}

/// Record a mutation of one scope's memory and prune that scope to its last [`KEEP`] rows.
pub fn append(
    conn: &Connection,
    agent_id: &str,
    workspace: Option<&str>,
    kind: MemoryEventKind,
    source: Option<&str>,
    note: &str,
    summary: &str,
) -> rusqlite::Result<()> {
    let scope = workspace.unwrap_or("");
    conn.execute(
        "INSERT INTO agent_memory_log
           (agent_group_id, workspace_id, kind, source, note, summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![agent_id, scope, kind.as_str(), source, note, summary],
    )?;
    conn.execute(
        "DELETE FROM agent_memory_log
          WHERE agent_group_id = ?1 AND workspace_id = ?2
            AND id NOT IN (SELECT id FROM agent_memory_log
                            WHERE agent_group_id = ?1 AND workspace_id = ?2
                            ORDER BY id DESC LIMIT ?3)",
        params![agent_id, scope, KEEP as i64],
    )?;
    Ok(())
}

/// The scope's most recent `limit` events, oldest first (ready for a chronological merge).
pub fn recent(
    conn: &Connection,
    agent_id: &str,
    workspace: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<MemoryEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, source, note, summary, created_at FROM agent_memory_log
          WHERE agent_group_id = ?1 AND workspace_id = ?2
          ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![agent_id, workspace.unwrap_or(""), limit as i64],
        |row| {
            Ok(MemoryEvent {
                id: row.get(0)?,
                kind: MemoryEventKind::from_str(&row.get::<_, String>(1)?),
                source: row.get(2)?,
                note: row.get(3)?,
                summary: row.get(4)?,
                created_at: row.get(5)?,
            })
        },
    )?;
    let mut events: Vec<MemoryEvent> = rows.collect::<Result<_, _>>()?;
    events.reverse();
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, open_in_memory};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        agents::create(&conn, "ag-1", "A", None, None).expect("agent");
        conn
    }

    #[test]
    fn append_then_recent_round_trips_oldest_first() {
        let conn = seeded();
        append(
            &conn,
            "ag-1",
            Some("ws"),
            MemoryEventKind::Fold,
            Some("t1"),
            "condensed 'Build' into memory",
            "knows the build",
        )
        .expect("fold");
        append(
            &conn,
            "ag-1",
            Some("ws"),
            MemoryEventKind::Edit,
            None,
            "memory edited",
            "operator's version",
        )
        .expect("edit");
        let events = recent(&conn, "ag-1", Some("ws"), 10).expect("recent");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, MemoryEventKind::Fold);
        assert_eq!(events[0].source.as_deref(), Some("t1"));
        assert_eq!(events[1].kind, MemoryEventKind::Edit);
        assert_eq!(events[1].source, None);
        assert_eq!(events[1].summary, "operator's version");
    }

    #[test]
    fn scopes_are_isolated() {
        let conn = seeded();
        append(
            &conn,
            "ag-1",
            Some("ws-a"),
            MemoryEventKind::Fold,
            None,
            "a",
            "",
        )
        .expect("a");
        append(&conn, "ag-1", None, MemoryEventKind::Clear, None, "g", "").expect("g");
        assert_eq!(recent(&conn, "ag-1", Some("ws-a"), 10).expect("a").len(), 1);
        assert_eq!(recent(&conn, "ag-1", Some("ws-b"), 10).expect("b").len(), 0);
        let global = recent(&conn, "ag-1", None, 10).expect("g");
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].kind, MemoryEventKind::Clear);
    }

    #[test]
    fn each_scope_prunes_to_its_last_keep_rows() {
        let conn = seeded();
        for i in 0..(KEEP + 5) {
            append(
                &conn,
                "ag-1",
                Some("ws"),
                MemoryEventKind::Fold,
                None,
                &format!("fold {i}"),
                "",
            )
            .expect("append");
        }
        // An untouched sibling scope is not pruned by ws's writes.
        append(&conn, "ag-1", None, MemoryEventKind::Edit, None, "kept", "").expect("global");
        let events = recent(&conn, "ag-1", Some("ws"), KEEP * 2).expect("recent");
        assert_eq!(events.len(), KEEP, "pruned to the cap");
        assert_eq!(events[0].note, "fold 5", "the oldest rows fell off");
        assert_eq!(recent(&conn, "ag-1", None, 10).expect("g").len(), 1);
    }

    #[test]
    fn unknown_kind_degrades_to_edit() {
        assert_eq!(MemoryEventKind::from_str("archive"), MemoryEventKind::Edit);
        for kind in [
            MemoryEventKind::Fold,
            MemoryEventKind::Edit,
            MemoryEventKind::Clear,
            MemoryEventKind::Pause,
            MemoryEventKind::Resume,
        ] {
            assert_eq!(MemoryEventKind::from_str(kind.as_str()), kind);
        }
    }
}
