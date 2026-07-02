//! Steer messages (R4): operator → agent notes, scoped like the thread to (agent, workspace).
//! A message is **queued** on send and **delivered** at the agent's next pass boundary — a
//! producer launch injects every undelivered message as a user turn (and marks it delivered);
//! a message to an idle agent is materialised as a task instead. In-flight work is never
//! touched: delivery only ever happens at a launch the scheduler was making anyway.

use rusqlite::{Connection, params};

/// One steer message: what the operator said, when, and whether an agent pass has seen it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub body: String,
    /// ISO-8601 UTC send stamp — the thread orders the turn by it.
    pub created_at: String,
    /// When a pass consumed it; `None` = still queued.
    pub delivered_at: Option<String>,
}

/// Queue a message for `(agent, workspace)`.
pub fn queue(
    conn: &Connection,
    id: &str,
    agent_id: &str,
    workspace_id: &str,
    body: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_messages (id, agent_group_id, workspace_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![id, agent_id, workspace_id, body],
    )?;
    Ok(())
}

/// The scope's undelivered messages, oldest first — what the next pass injects.
pub fn undelivered(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, body, created_at, delivered_at FROM agent_messages
          WHERE agent_group_id = ?1 AND workspace_id = ?2 AND delivered_at IS NULL
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![agent_id, workspace_id], from_row)?;
    rows.collect()
}

/// Mark one message delivered (consumed by a pass or materialised as a task).
pub fn mark_delivered(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_messages SET delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
          WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// The scope's most recent `limit` messages (queued and delivered), oldest first — the thread
/// projection merges them as operator turns.
pub fn recent(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, body, created_at, delivered_at FROM agent_messages
          WHERE agent_group_id = ?1 AND workspace_id = ?2
          ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![agent_id, workspace_id, limit as i64], from_row)?;
    let mut messages: Vec<Message> = rows.collect::<Result<_, _>>()?;
    messages.reverse();
    Ok(messages)
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        body: row.get(1)?,
        created_at: row.get(2)?,
        delivered_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, open_in_memory, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        agents::create(&conn, "ag-1", "A", None, None).expect("agent");
        workspaces::create(&conn, "ws", "W").expect("ws");
        workspaces::create(&conn, "elsewhere", "E").expect("elsewhere");
        conn
    }

    #[test]
    fn queue_undelivered_deliver_round_trip() {
        let conn = seeded();
        queue(&conn, "m1", "ag-1", "ws", "focus on tests").expect("m1");
        queue(&conn, "m2", "ag-1", "ws", "and keep it small").expect("m2");
        queue(&conn, "m3", "ag-1", "elsewhere", "other scope").expect("m3");

        let pending = undelivered(&conn, "ag-1", "ws").expect("undelivered");
        assert_eq!(pending.len(), 2, "scoped to (agent, workspace)");
        assert_eq!(pending[0].body, "focus on tests");
        assert!(pending[0].delivered_at.is_none());

        mark_delivered(&conn, "m1").expect("deliver");
        let pending = undelivered(&conn, "ag-1", "ws").expect("undelivered");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "m2");

        // `recent` shows both, oldest first, with the delivery stamp on the consumed one.
        let all = recent(&conn, "ag-1", "ws", 10).expect("recent");
        assert_eq!(all.len(), 2);
        assert!(all[0].delivered_at.is_some());
        assert!(all[1].delivered_at.is_none());
    }
}
