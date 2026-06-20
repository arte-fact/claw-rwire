//! Per-agent tool selection (the `agent_tools` table). Replaces the coarse
//! `tool_profile` with an explicit enabled set: the operator ticks individual tools
//! (web/MCP + coder) in the agent drawer. An agent with no rows is "unconfigured" — the
//! run then falls back to a default set rather than running with nothing.

use rusqlite::{Connection, params};

/// Replace an agent's tool selection. `catalog` is every selectable tool paired with
/// whether it's enabled; writing a row for each (even the disabled ones) marks the agent
/// "configured", so an all-off selection is honoured rather than treated as default.
pub fn set_tools(
    conn: &Connection,
    agent_id: &str,
    catalog: &[(String, bool)],
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM agent_tools WHERE agent_group_id = ?1",
        params![agent_id],
    )?;
    for (name, enabled) in catalog {
        tx.execute(
            "INSERT INTO agent_tools (agent_group_id, tool_name, enabled) VALUES (?1, ?2, ?3)",
            params![agent_id, name, i64::from(*enabled)],
        )?;
    }
    tx.commit()
}

/// The agent's selection: `None` if it has no rows (unconfigured — caller applies its
/// default), otherwise the list of enabled tool names (possibly empty).
pub fn selection(conn: &Connection, agent_id: &str) -> rusqlite::Result<Option<Vec<String>>> {
    let total: i64 = conn.query_row(
        "SELECT count(*) FROM agent_tools WHERE agent_group_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )?;
    if total == 0 {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT tool_name FROM agent_tools WHERE agent_group_id = ?1 AND enabled = 1
         ORDER BY tool_name",
    )?;
    let rows = stmt.query_map(params![agent_id], |row| row.get(0))?;
    Ok(Some(rows.collect::<Result<_, _>>()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, endpoints, open_in_memory};

    fn seeded() -> (Connection, String) {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-1", "A", Some("e"), Some("m")).expect("create");
        (conn, "ag-1".to_owned())
    }

    #[test]
    fn unconfigured_is_none() {
        let (conn, id) = seeded();
        assert_eq!(selection(&conn, &id).expect("sel"), None);
    }

    #[test]
    fn set_then_selection_returns_only_enabled() {
        let (conn, id) = seeded();
        set_tools(
            &conn,
            &id,
            &[
                ("search".to_owned(), true),
                ("bash".to_owned(), false),
                ("read".to_owned(), true),
            ],
        )
        .expect("set");
        assert_eq!(
            selection(&conn, &id).expect("sel"),
            Some(vec!["read".to_owned(), "search".to_owned()])
        );
    }

    #[test]
    fn all_disabled_is_configured_empty_not_none() {
        let (conn, id) = seeded();
        set_tools(&conn, &id, &[("search".to_owned(), false)]).expect("set");
        assert_eq!(selection(&conn, &id).expect("sel"), Some(Vec::new()));
    }

    #[test]
    fn set_replaces_prior_selection() {
        let (conn, id) = seeded();
        set_tools(&conn, &id, &[("search".to_owned(), true)]).expect("set1");
        set_tools(&conn, &id, &[("read".to_owned(), true)]).expect("set2");
        assert_eq!(
            selection(&conn, &id).expect("sel"),
            Some(vec!["read".to_owned()])
        );
    }
}
