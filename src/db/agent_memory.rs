//! Per-agent rolling memory (the `agent_memory` table): a summarized recollection of the
//! agent's own thread, plus how far that summary has consumed the raw transcript. Injected
//! into the agent's runs as a "working memory" layer so context survives across turns/tasks
//! without sending the whole (growing) transcript every time.

use rusqlite::{Connection, OptionalExtension, params};

/// An agent's rolling memory: the `summary` text, `summarized_through` (the id of the last
/// chat message folded into it — turns after this are sent raw), and whether auto-summarization
/// is `paused` (the operator froze it).
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

/// The agent's memory, or a default (empty summary, marker 0, not paused) if it has none yet.
pub fn get(conn: &Connection, agent_id: &str) -> rusqlite::Result<Memory> {
    conn.query_row(
        "SELECT summary, summarized_through_id, paused
           FROM agent_memory WHERE agent_group_id = ?1",
        params![agent_id],
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

/// Freeze or resume an agent's auto-summarization (operator control). Upserts so it works
/// before any summary exists.
pub fn set_paused(conn: &Connection, agent_id: &str, paused: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_memory (agent_group_id, summary, summarized_through_id, paused, updated_at)
         VALUES (?1, '', 0, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(agent_group_id) DO UPDATE SET
           paused = excluded.paused, updated_at = excluded.updated_at",
        params![agent_id, i64::from(paused)],
    )?;
    Ok(())
}

/// Replace an agent's rolling memory (summary + how far it has consumed the transcript).
pub fn set(
    conn: &Connection,
    agent_id: &str,
    summary: &str,
    summarized_through: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_memory (agent_group_id, summary, summarized_through_id, updated_at)
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(agent_group_id) DO UPDATE SET
           summary = excluded.summary,
           summarized_through_id = excluded.summarized_through_id,
           updated_at = excluded.updated_at",
        params![agent_id, summary, summarized_through],
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
        let memory = get(&conn, "ag-1").expect("get");
        assert_eq!(memory, Memory::default());
        assert_eq!(memory.text(), None);
    }

    #[test]
    fn set_then_get_round_trips_and_upserts() {
        let conn = seeded();
        set(&conn, "ag-1", "remembered the plan", 42).expect("set");
        let memory = get(&conn, "ag-1").expect("get");
        assert_eq!(memory.summary, "remembered the plan");
        assert_eq!(memory.summarized_through, 42);
        assert_eq!(memory.text().as_deref(), Some("remembered the plan"));

        // A second write replaces (advancing the marker).
        set(&conn, "ag-1", "remembered more", 99).expect("update");
        let memory = get(&conn, "ag-1").expect("get");
        assert_eq!(memory.summary, "remembered more");
        assert_eq!(memory.summarized_through, 99);
    }

    #[test]
    fn blank_summary_reads_as_no_layer() {
        let conn = seeded();
        set(&conn, "ag-1", "   \n\t", 5).expect("set");
        assert_eq!(get(&conn, "ag-1").expect("get").text(), None);
    }

    #[test]
    fn pause_round_trips_and_survives_a_summary_write() {
        let conn = seeded();
        set_paused(&conn, "ag-1", true).expect("pause");
        assert!(get(&conn, "ag-1").expect("get").paused);
        // A later summarizer write must not unfreeze the operator's pause.
        set(&conn, "ag-1", "folded something", 10).expect("set");
        let memory = get(&conn, "ag-1").expect("get");
        assert!(memory.paused, "pause preserved across a summary write");
        assert_eq!(memory.summary, "folded something");
        set_paused(&conn, "ag-1", false).expect("resume");
        assert!(!get(&conn, "ag-1").expect("get").paused);
    }
}
