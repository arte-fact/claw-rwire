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

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<AgentGroup>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY created_at ASC"))?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

/// Create an agent group (admin). `folder` is the id, which is unique by construction;
/// claw-rwire's web chats don't use the workspace folder.
pub fn create(
    conn: &Connection,
    id: &str,
    name: &str,
    endpoint: Option<&str>,
    model: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_groups (id, name, folder, endpoint, model, created_at)
         VALUES (?1, ?2, ?1, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![id, name, endpoint, model],
    )?;
    Ok(())
}

pub fn update(
    conn: &Connection,
    id: &str,
    name: &str,
    endpoint: Option<&str>,
    model: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_groups SET name = ?2, endpoint = ?3, model = ?4 WHERE id = ?1",
        params![id, name, endpoint, model],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM agent_groups WHERE id = ?1", params![id])?;
    Ok(())
}

/// The agent group every web chat currently routes to, if any (they share one).
pub fn web_agent(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT m.agent_group_id
           FROM messaging_group_agents m
           JOIN messaging_groups g ON g.id = m.messaging_group_id
          WHERE g.channel_type = 'web'
          ORDER BY m.created_at ASC
          LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
}

/// Point every web chat at `agent_id` (the Channels→Agent link). Replaces existing
/// web links so the channel routes to exactly one agent.
pub fn relink_web_chats(conn: &Connection, agent_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM messaging_group_agents
          WHERE messaging_group_id IN
            (SELECT id FROM messaging_groups WHERE channel_type = 'web')",
        [],
    )?;
    let chat_ids: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT id FROM messaging_groups WHERE channel_type = 'web'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for chat_id in chat_ids {
        link(conn, &chat_id, agent_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{chat, endpoints, open_in_memory};

    #[test]
    fn ensure_default_is_idempotent_and_carries_endpoint_model() {
        let conn = open_in_memory().expect("db");
        // The endpoint must exist first (agent_groups.endpoint is a FK).
        endpoints::upsert(&conn, "default", "http://x/v1", None, None).expect("endpoint");
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

    #[test]
    fn create_update_delete_and_list() {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e1", "http://e1/v1", None, None).expect("e1");
        create(&conn, "ag-1", "Writer", Some("e1"), Some("m1")).expect("create");
        assert_eq!(
            get(&conn, "ag-1").expect("get"),
            Some(AgentGroup {
                id: "ag-1".to_owned(),
                name: "Writer".to_owned(),
                endpoint: Some("e1".to_owned()),
                model: Some("m1".to_owned()),
            })
        );
        update(&conn, "ag-1", "Editor", Some("e1"), Some("m2")).expect("update");
        let updated = get(&conn, "ag-1").expect("get").expect("some");
        assert_eq!(updated.name, "Editor");
        assert_eq!(updated.model.as_deref(), Some("m2"));
        assert_eq!(list(&conn).expect("list").len(), 1);
        delete(&conn, "ag-1").expect("delete");
        assert!(list(&conn).expect("list").is_empty());
    }

    #[test]
    fn relink_web_chats_points_every_web_chat_at_one_agent() {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        create(&conn, "ag-a", "A", Some("e"), Some("m")).expect("a");
        create(&conn, "ag-b", "B", Some("e"), Some("m")).expect("b");
        chat::create_chat(&conn, "c1", "x").expect("c1");
        chat::create_chat(&conn, "c2", "y").expect("c2");

        relink_web_chats(&conn, "ag-a").expect("relink a");
        assert_eq!(web_agent(&conn).expect("web"), Some("ag-a".to_owned()));
        assert_eq!(agent_for_chat(&conn, "c1").expect("c1").unwrap().id, "ag-a");

        // Re-pointing the channel moves every web chat, leaving no duplicate links.
        relink_web_chats(&conn, "ag-b").expect("relink b");
        assert_eq!(web_agent(&conn).expect("web"), Some("ag-b".to_owned()));
        assert_eq!(agent_for_chat(&conn, "c2").expect("c2").unwrap().id, "ag-b");
    }
}
