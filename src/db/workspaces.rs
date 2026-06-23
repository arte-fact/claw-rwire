//! First-class workspaces (the `workspaces` table), each with an **owner** agent (its domain
//! lead + home), plus **membership** (`agent_workspaces`, M:N) — the team whose tasks an agent
//! can be assigned and whose files it may touch. An agent owns at most one workspace; it can
//! be a member of several. The owner is implicitly a member of its own.

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    /// When set, a delegated request skips the R3 plan gate and the owner's tasks queue directly
    /// (no human review). Off by default — delegation produces a reviewable plan.
    pub auto_approve: bool,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        owner: row.get(2)?,
        auto_approve: row.get::<_, i64>(3)? != 0,
    })
}

const SELECT: &str = "SELECT id, name, owner_agent_id, auto_approve FROM workspaces";

/// Set (or clear) a workspace's auto-approve flag — when on, delegated work skips the plan gate.
pub fn set_auto_approve(conn: &Connection, id: &str, on: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE workspaces SET auto_approve = ?2 WHERE id = ?1",
        params![id, i64::from(on)],
    )?;
    Ok(())
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

pub fn rename(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE workspaces SET name = ?2 WHERE id = ?1",
        params![id, name],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Workspace>> {
    conn.query_row(&format!("{SELECT} WHERE id = ?1"), params![id], from_row)
        .optional()
}

/// Delete a workspace row. The caller is responsible for checking no agents are still
/// members (FK enforcement is off) and for removing the directory on disk.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Workspace>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY created_at ASC"))?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

// ── Owner (the workspace's domain lead / home agent) ────────────────────────────

/// Set the workspace's owner, also making the owner a member (the owner is implicitly a
/// member of its own workspace). An agent owns at most one workspace — the partial unique
/// index enforces it — so `set_owner` first clears any other workspace this agent owns.
pub fn set_owner(conn: &Connection, workspace_id: &str, agent_id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE workspaces SET owner_agent_id = NULL WHERE owner_agent_id = ?1",
        params![agent_id],
    )?;
    tx.execute(
        "UPDATE workspaces SET owner_agent_id = ?2 WHERE id = ?1",
        params![workspace_id, agent_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO agent_workspaces (agent_group_id, workspace_id) VALUES (?1, ?2)",
        params![agent_id, workspace_id],
    )?;
    tx.commit()
}

/// Clear a workspace's owner (it becomes unowned). Membership is left intact — the
/// ex-owner stays on the team unless removed separately.
pub fn clear_owner(conn: &Connection, workspace_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE workspaces SET owner_agent_id = NULL WHERE id = ?1",
        params![workspace_id],
    )?;
    Ok(())
}

/// The id of the workspace `agent_id` owns (its home), if any.
pub fn owned_by(conn: &Connection, agent_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM workspaces WHERE owner_agent_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )
    .optional()
}

// ── Membership (the team; M:N) ──────────────────────────────────────────────────

/// Whether the agent belongs to (owns or is a member of) any workspace at all — used to
/// decide if a legacy/unattached agent needs an auto-created home (member-only agents don't).
pub fn is_attached(conn: &Connection, agent_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_workspaces WHERE agent_group_id = ?1)",
        params![agent_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
}

/// Add `agent_id` to `workspace_id`'s team (idempotent).
pub fn add_member(conn: &Connection, agent_id: &str, workspace_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO agent_workspaces (agent_group_id, workspace_id) VALUES (?1, ?2)",
        params![agent_id, workspace_id],
    )?;
    Ok(())
}

/// Remove `agent_id` from `workspace_id`'s team. (Callers should not remove the owner.)
pub fn remove_member(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM agent_workspaces WHERE agent_group_id = ?1 AND workspace_id = ?2",
        params![agent_id, workspace_id],
    )?;
    Ok(())
}

/// The agent ids on `workspace_id`'s team.
pub fn members_of(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT agent_group_id FROM agent_workspaces WHERE workspace_id = ?1 ORDER BY agent_group_id",
    )?;
    let rows = stmt.query_map(params![workspace_id], |row| row.get(0))?;
    rows.collect()
}

/// The names of the agents on `workspace_id`'s team, sorted — for display.
pub fn agents_in(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT a.name FROM agent_workspaces w
           JOIN agent_groups a ON a.id = w.agent_group_id
          WHERE w.workspace_id = ?1
          ORDER BY a.name",
    )?;
    let rows = stmt.query_map(params![workspace_id], |row| row.get(0))?;
    rows.collect()
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
    fn auto_approve_defaults_off_and_round_trips() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws1");
        assert!(!get(&conn, "ws-1").expect("get").unwrap().auto_approve);
        set_auto_approve(&conn, "ws-1", true).expect("set on");
        assert!(get(&conn, "ws-1").expect("get").unwrap().auto_approve);
        set_auto_approve(&conn, "ws-1", false).expect("set off");
        assert!(!get(&conn, "ws-1").expect("get").unwrap().auto_approve);
    }

    #[test]
    fn membership_is_many_to_many() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws1");
        create(&conn, "ws-2", "Two").expect("ws2");
        assert!(members_of(&conn, "ws-1").expect("none yet").is_empty());
        // One agent on several workspaces; one workspace with several agents.
        add_member(&conn, "ag-a", "ws-1").expect("a1");
        add_member(&conn, "ag-a", "ws-2").expect("a2");
        add_member(&conn, "ag-b", "ws-1").expect("b1");
        add_member(&conn, "ag-a", "ws-1").expect("idempotent");
        assert_eq!(
            members_of(&conn, "ws-1").expect("ws1"),
            vec!["ag-a".to_owned(), "ag-b".to_owned()]
        );
        assert_eq!(
            members_of(&conn, "ws-2").expect("ws2"),
            vec!["ag-a".to_owned()]
        );
        assert_eq!(
            agents_in(&conn, "ws-1").expect("names"),
            vec!["Coder".to_owned(), "Writer".to_owned()]
        );
    }

    #[test]
    fn remove_member_drops_one_membership() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws");
        add_member(&conn, "ag-a", "ws-1").expect("add");
        remove_member(&conn, "ag-a", "ws-1").expect("remove");
        assert!(members_of(&conn, "ws-1").expect("empty").is_empty());
        assert!(agents_in(&conn, "ws-1").expect("empty").is_empty());
    }

    #[test]
    fn owner_is_unique_per_agent_and_implies_membership() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws1");
        create(&conn, "ws-2", "Two").expect("ws2");
        assert_eq!(owned_by(&conn, "ag-a").expect("none"), None);

        set_owner(&conn, "ws-1", "ag-a").expect("own ws1");
        assert_eq!(
            owned_by(&conn, "ag-a").expect("some"),
            Some("ws-1".to_owned())
        );
        // Owning implies membership.
        assert_eq!(
            members_of(&conn, "ws-1").expect("member"),
            vec!["ag-a".to_owned()]
        );
        assert_eq!(
            get(&conn, "ws-1").expect("ws").unwrap().owner.as_deref(),
            Some("ag-a")
        );

        // An agent owns at most one workspace: owning ws-2 releases ws-1.
        set_owner(&conn, "ws-2", "ag-a").expect("own ws2");
        assert_eq!(
            owned_by(&conn, "ag-a").expect("some"),
            Some("ws-2".to_owned())
        );
        assert_eq!(get(&conn, "ws-1").expect("ws").unwrap().owner, None);
    }

    #[test]
    fn clear_owner_unsets_the_owner_keeping_membership() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws");
        set_owner(&conn, "ws-1", "ag-a").expect("own");
        clear_owner(&conn, "ws-1").expect("clear");
        assert_eq!(get(&conn, "ws-1").expect("get").unwrap().owner, None);
        assert_eq!(owned_by(&conn, "ag-a").expect("none"), None);
        // The ex-owner stays on the team.
        assert_eq!(
            members_of(&conn, "ws-1").expect("member"),
            vec!["ag-a".to_owned()]
        );
    }

    #[test]
    fn two_agents_own_different_workspaces() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("ws1");
        create(&conn, "ws-2", "Two").expect("ws2");
        set_owner(&conn, "ws-1", "ag-a").expect("a");
        set_owner(&conn, "ws-2", "ag-b").expect("b");
        assert_eq!(owned_by(&conn, "ag-a").expect("a"), Some("ws-1".to_owned()));
        assert_eq!(owned_by(&conn, "ag-b").expect("b"), Some("ws-2".to_owned()));
    }

    #[test]
    fn rename_changes_the_name_and_get_reads_it() {
        let conn = seeded();
        create(&conn, "ws-1", "Old").expect("create");
        rename(&conn, "ws-1", "New").expect("rename");
        assert_eq!(get(&conn, "ws-1").expect("get").unwrap().name, "New");
    }

    #[test]
    fn delete_removes_the_workspace() {
        let conn = seeded();
        create(&conn, "ws-1", "One").expect("create");
        delete(&conn, "ws-1").expect("delete");
        assert_eq!(get(&conn, "ws-1").expect("get"), None);
        assert!(list(&conn).expect("list").is_empty());
    }
}
