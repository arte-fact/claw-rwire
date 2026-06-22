//! The per-agent identity prompt (the `agent_prompts` table). Operator-owned and kept out
//! of the workspace, so a shared workspace's `AGENT.md` stays the project brief while each
//! agent keeps a distinct persona. Absent/empty means "no identity layer" — the run then
//! composes from the base prompt + the workspace brief alone.

use rusqlite::{Connection, OptionalExtension, params};

/// The default identity persona for the **concierge / workspace owner** — the do-everything
/// generalist that fields chat, plans workspaces, and executes its own tasks. Role-neutral
/// (the run's role preamble supplies the behaviour), so it reads sensibly in every context.
#[must_use]
pub fn default_owner_body(name: &str) -> String {
    format!(
        "You are {name}, a capable and careful generalist — part software engineer, part \
         researcher, part operator. You write clean, idiomatic code, keep changes small and \
         well-tested, follow the workspace's conventions, and explain briefly what you did."
    )
}

/// The default identity persona for a **newly-created agent** (a workspace member / specialist).
/// Doubles as the routing description the owner's `create_task` enum shows.
#[must_use]
pub fn default_member_body(name: &str) -> String {
    format!(
        "You are {name}, a capable specialist. Do focused, high-quality work: make small, \
         well-tested changes, follow the workspace's conventions, and say briefly what you \
         changed and why."
    )
}

/// The agent's identity prompt, or `None` if unset.
pub fn get(conn: &Connection, agent_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT body FROM agent_prompts WHERE agent_group_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )
    .optional()
}

/// Set the agent's identity prompt. A blank/whitespace body clears it (deletes the row),
/// so "no identity" round-trips as `None` rather than an empty string.
pub fn set(conn: &Connection, agent_id: &str, body: &str) -> rusqlite::Result<()> {
    if body.trim().is_empty() {
        conn.execute(
            "DELETE FROM agent_prompts WHERE agent_group_id = ?1",
            params![agent_id],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO agent_prompts (agent_group_id, body, updated_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(agent_group_id)
           DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        params![agent_id, body],
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
    fn unset_is_none() {
        let conn = seeded();
        assert_eq!(get(&conn, "ag-1").expect("get"), None);
    }

    #[test]
    fn set_then_get_round_trips_and_updates() {
        let conn = seeded();
        set(&conn, "ag-1", "You are a careful reviewer.").expect("set");
        assert_eq!(
            get(&conn, "ag-1").expect("get"),
            Some("You are a careful reviewer.".to_owned())
        );
        set(&conn, "ag-1", "You are a planner.").expect("update");
        assert_eq!(
            get(&conn, "ag-1").expect("get"),
            Some("You are a planner.".to_owned())
        );
    }

    #[test]
    fn blank_body_clears_the_prompt() {
        let conn = seeded();
        set(&conn, "ag-1", "something").expect("set");
        set(&conn, "ag-1", "   \n\t").expect("clear");
        assert_eq!(get(&conn, "ag-1").expect("get"), None);
    }
}
