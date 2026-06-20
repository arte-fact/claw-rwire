//! First-class workspaces (the `workspaces` table) and the agent→workspace mapping
//! (`agent_workspaces`). A workspace is a named directory under `<data>/workspaces/<id>`
//! that one or more agents share; an agent may have none. claw resolves an agent's
//! workspace through this mapping rather than the legacy `agent_groups.folder` column,
//! which lets distinct agents collaborate on one shared project.

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
    })
}

/// Create a workspace (idempotent on id — a re-create never renames an existing one). The
/// `id` is the directory name under `<data>/workspaces/`.
pub fn create(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO workspaces (id, name, created_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![id, name],
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Workspace>> {
    let mut stmt = conn.prepare("SELECT id, name FROM workspaces ORDER BY created_at ASC")?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

/// Assign `agent_id` to `workspace_id`, replacing any prior assignment. Multiple agents
/// may point at one workspace (sharing).
pub fn assign(conn: &Connection, agent_id: &str, workspace_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agent_workspaces (agent_group_id, workspace_id) VALUES (?1, ?2)
         ON CONFLICT(agent_group_id) DO UPDATE SET workspace_id = excluded.workspace_id",
        params![agent_id, workspace_id],
    )?;
    Ok(())
}

/// The id of the workspace an agent works in, if any.
pub fn for_agent(conn: &Connection, agent_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT workspace_id FROM agent_workspaces WHERE agent_group_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, endpoints, open_in_memory};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-a", "Coder", Some("e"), Some("m")).expect("a");
        agents::create(&conn, "ag-b", "Writer", Some("e"), Some("m")).expect("b");
        conn
    }

    #[test]
    fn create_is_idempotent_and_lists_in_creation_order() {
        let conn = seeded();
        create(&conn, "ws-1", "Project One").expect("create");
        create(&conn, "ws-1", "renamed-on-recreate?").expect("re-create is a no-op");
        create(&conn, "ws-2", "Project Two").expect("create");
        let listed = list(&conn).expect("list");
        let ids: Vec<&str> = listed.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["ws-1", "ws-2"]);
        // A re-create with a different name must not overwrite the existing one.
        assert_eq!(listed[0].name, "Project One");
    }

    #[test]
    fn assign_maps_an_agent_and_replaces_on_reassign() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws1");
        create(&conn, "ws-2", "Two").expect("ws2");
        assert_eq!(for_agent(&conn, "ag-a").expect("none yet"), None);
        assign(&conn, "ag-a", "ws-1").expect("assign");
        assert_eq!(
            for_agent(&conn, "ag-a").expect("some"),
            Some("ws-1".to_owned())
        );
        // Re-assigning replaces, never duplicates (PK on agent_group_id).
        assign(&conn, "ag-a", "ws-2").expect("reassign");
        assert_eq!(
            for_agent(&conn, "ag-a").expect("some"),
            Some("ws-2".to_owned())
        );
    }

    #[test]
    fn two_agents_can_share_one_workspace() {
        let conn = seeded();
        create(&conn, "ws-shared", "Shared").expect("ws");
        assign(&conn, "ag-a", "ws-shared").expect("a");
        assign(&conn, "ag-b", "ws-shared").expect("b");
        assert_eq!(
            for_agent(&conn, "ag-a").expect("a"),
            Some("ws-shared".to_owned())
        );
        assert_eq!(
            for_agent(&conn, "ag-b").expect("b"),
            Some("ws-shared".to_owned())
        );
    }
}
