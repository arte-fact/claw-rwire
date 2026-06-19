//! Central-DB queries for `agent_groups` and the chat↔agent link
//! (`messaging_group_agents`). claw-rwire runs a single default agent for the web
//! surface; the System Map admin will later create more.

use rusqlite::{Connection, OptionalExtension, params};

/// The singleton web agent. A fixed id keeps `ensure_default` idempotent without
/// minting randomness in DB code.
const DEFAULT_ID: &str = "ag-default";
const DEFAULT_NAME: &str = "claw";
const DEFAULT_FOLDER: &str = "claw";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGroup {
    pub id: String,
    pub name: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<AgentGroup> {
    Ok(AgentGroup {
        id: row.get(0)?,
        name: row.get(1)?,
        endpoint: row.get(2)?,
        model: row.get(3)?,
    })
}

const SELECT: &str = "SELECT id, name, endpoint, model FROM agent_groups";

/// The first agent group (any), if one exists — respects an agent inherited from a
/// shared rust-claw central.db.
pub fn first(conn: &Connection) -> rusqlite::Result<Option<AgentGroup>> {
    conn.query_row(
        &format!("{SELECT} ORDER BY created_at ASC LIMIT 1"),
        [],
        from_row,
    )
    .optional()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<AgentGroup>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), params![id], from_row)
        .optional()
}

/// Return the existing agent group, or create the default one wired to
/// `endpoint`/`model`. Called at boot.
pub fn ensure_default(
    conn: &Connection,
    endpoint: Option<&str>,
    model: Option<&str>,
) -> rusqlite::Result<AgentGroup> {
    if let Some(group) = first(conn)? {
        return Ok(group);
    }
    conn.execute(
        "INSERT INTO agent_groups (id, name, folder, endpoint, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![DEFAULT_ID, DEFAULT_NAME, DEFAULT_FOLDER, endpoint, model],
    )?;
    get(conn, DEFAULT_ID).map(|group| group.expect("agent group just inserted"))
}

/// Bind a chat to an agent group (idempotent on the pair). The link id is derived
/// from the pair so re-linking is a no-op.
pub fn link(conn: &Connection, chat_id: &str, agent_group_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO messaging_group_agents
           (id, messaging_group_id, agent_group_id, created_at)
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            format!("{chat_id}:{agent_group_id}"),
            chat_id,
            agent_group_id
        ],
    )?;
    Ok(())
}

/// The agent group bound to a chat (lowest `priority` wins), if any.
pub fn agent_for_chat(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<AgentGroup>> {
    conn.query_row(
        "SELECT a.id, a.name, a.endpoint, a.model
           FROM messaging_group_agents m
           JOIN agent_groups a ON a.id = m.agent_group_id
          WHERE m.messaging_group_id = ?1
          ORDER BY m.priority ASC, m.created_at ASC
          LIMIT 1",
        params![chat_id],
        from_row,
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{chat, endpoints, open_in_memory};

    #[test]
    fn ensure_default_is_idempotent_and_carries_endpoint_model() {
        let conn = open_in_memory().expect("db");
        // The endpoint must exist first (agent_groups.endpoint is a FK).
        endpoints::insert(&conn, "default", "http://x/v1", None, None).expect("endpoint");
        let a = ensure_default(&conn, Some("default"), Some("m1")).expect("ensure");
        assert_eq!(a.endpoint.as_deref(), Some("default"));
        assert_eq!(a.model.as_deref(), Some("m1"));
        // Second call returns the same row, not a duplicate.
        let b = ensure_default(&conn, Some("other"), Some("m2")).expect("ensure2");
        assert_eq!(a, b);
    }

    #[test]
    fn linking_a_chat_resolves_back_to_its_agent() {
        let conn = open_in_memory().expect("db");
        let agent = ensure_default(&conn, None, Some("m")).expect("ensure");
        chat::create_chat(&conn, "chat-1", "New chat").expect("chat");
        assert!(agent_for_chat(&conn, "chat-1").expect("none yet").is_none());
        link(&conn, "chat-1", &agent.id).expect("link");
        link(&conn, "chat-1", &agent.id).expect("re-link is a no-op");
        assert_eq!(agent_for_chat(&conn, "chat-1").expect("some"), Some(agent));
    }
}
