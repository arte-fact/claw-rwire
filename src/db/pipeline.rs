//! A workspace's workflow pipeline (the `pipeline_steps` table, Phase F): the ordered steps a
//! task flows through. A `stage` step is one agent run; a `loop` step is a bounded
//! produce ⇄ validate cycle (driven in F-2). A workspace with **no rows** runs the implicit
//! default — a single `Execute` stage → done — so existing tasks behave exactly as before.

use rusqlite::{Connection, params};

/// What kind of step this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Stage,
    Loop,
}

impl StepKind {
    fn from_str(value: &str) -> Self {
        match value {
            "loop" => StepKind::Loop,
            _ => StepKind::Stage,
        }
    }
}

/// One step of a workspace pipeline. The loop fields are unused for `Stage` steps (and in F-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The row id (empty for the implicit default step — it has no row).
    pub id: String,
    pub position: i64,
    pub kind: StepKind,
    pub name: String,
    /// The producer/stage agent, or `None` to use the workspace owner.
    pub agent: Option<String>,
    /// The step's role-preamble override, or `None` to use the default executor preamble.
    pub prompt: Option<String>,
    pub validator_kind: Option<String>,
    pub validator_agent: Option<String>,
    pub validator_prompt: Option<String>,
    pub max_iterations: i64,
    pub on_exhaust: String,
}

const SELECT: &str = "SELECT id, position, kind, name, agent_id, prompt, validator_kind, \
                      validator_agent_id, validator_prompt, max_iterations, on_exhaust \
                      FROM pipeline_steps";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Step> {
    Ok(Step {
        id: row.get(0)?,
        position: row.get(1)?,
        kind: StepKind::from_str(&row.get::<_, String>(2)?),
        name: row.get(3)?,
        agent: row.get(4)?,
        prompt: row.get(5)?,
        validator_kind: row.get(6)?,
        validator_agent: row.get(7)?,
        validator_prompt: row.get(8)?,
        max_iterations: row.get(9)?,
        on_exhaust: row.get(10)?,
    })
}

/// The default pipeline when a workspace has configured none: a single `Execute` stage whose
/// agent is the owner (`None`) and whose role preamble is the default executor.
fn default_steps() -> Vec<Step> {
    vec![Step {
        id: String::new(),
        position: 0,
        kind: StepKind::Stage,
        name: "Execute".to_owned(),
        agent: None,
        prompt: None,
        validator_kind: None,
        validator_agent: None,
        validator_prompt: None,
        max_iterations: 1,
        on_exhaust: "human".to_owned(),
    }]
}

/// A workspace's **configured** steps in order — the raw rows (empty when none configured). The
/// editor shows these; the engine uses [`steps_for`] (which falls back to the default).
pub fn configured_steps(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Step>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 ORDER BY position ASC"
    ))?;
    stmt.query_map(params![workspace_id], from_row)?.collect()
}

/// A workspace's pipeline steps in order — the configured ones, or the default single stage when
/// none are configured. Always non-empty.
pub fn steps_for(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Step>> {
    let rows = configured_steps(conn, workspace_id)?;
    Ok(if rows.is_empty() {
        default_steps()
    } else {
        rows
    })
}

/// The editable fields of a step — the form payload for insert/update.
pub struct StepFields<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub agent: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub validator_kind: Option<&'a str>,
    pub validator_agent: Option<&'a str>,
    pub validator_prompt: Option<&'a str>,
    pub max_iterations: i64,
    pub on_exhaust: &'a str,
}

/// Insert a step at `position`. `id` is caller-minted.
pub fn insert(
    conn: &Connection,
    id: &str,
    workspace_id: &str,
    position: i64,
    fields: &StepFields,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pipeline_steps
           (id, workspace_id, position, kind, name, agent_id, prompt,
            validator_kind, validator_agent_id, validator_prompt, max_iterations, on_exhaust)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            workspace_id,
            position,
            fields.kind,
            fields.name,
            fields.agent,
            fields.prompt,
            fields.validator_kind,
            fields.validator_agent,
            fields.validator_prompt,
            fields.max_iterations,
            fields.on_exhaust,
        ],
    )?;
    Ok(())
}

/// Insert a plain stage. A thin `insert` wrapper kept as a test helper (the seed no longer uses
/// stage steps — Studio's pipeline is two loop steps). `id` is caller-minted.
#[cfg(test)]
pub fn insert_stage(
    conn: &Connection,
    id: &str,
    workspace_id: &str,
    position: i64,
    name: &str,
    agent: Option<&str>,
    prompt: Option<&str>,
) -> rusqlite::Result<()> {
    insert(
        conn,
        id,
        workspace_id,
        position,
        &StepFields {
            kind: "stage",
            name,
            agent,
            prompt,
            validator_kind: None,
            validator_agent: None,
            validator_prompt: None,
            max_iterations: 1,
            on_exhaust: "human",
        },
    )
}

/// Update a step's editable fields (kind/name/agent/prompt/validator/loop bounds).
pub fn update(conn: &Connection, id: &str, fields: &StepFields) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE pipeline_steps SET kind = ?2, name = ?3, agent_id = ?4, prompt = ?5,
            validator_kind = ?6, validator_agent_id = ?7, validator_prompt = ?8,
            max_iterations = ?9, on_exhaust = ?10
          WHERE id = ?1",
        params![
            id,
            fields.kind,
            fields.name,
            fields.agent,
            fields.prompt,
            fields.validator_kind,
            fields.validator_agent,
            fields.validator_prompt,
            fields.max_iterations,
            fields.on_exhaust,
        ],
    )?;
    Ok(())
}

/// Delete a step.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM pipeline_steps WHERE id = ?1", params![id])?;
    Ok(())
}

/// Set a step's position (the editor swaps two to reorder).
pub fn set_position(conn: &Connection, id: &str, position: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE pipeline_steps SET position = ?2 WHERE id = ?1",
        params![id, position],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, workspaces};

    #[test]
    fn empty_workspace_returns_the_default_single_stage() {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "WS").expect("ws");
        let steps = steps_for(&conn, "ws").expect("steps");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].name, "Execute");
        assert_eq!(steps[0].agent, None);
    }

    #[test]
    fn configured_steps_come_back_in_order() {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "WS").expect("ws");
        insert_stage(&conn, "s2", "ws", 1, "Refine", None, Some("polish it")).expect("s2");
        insert_stage(&conn, "s1", "ws", 0, "Execute", None, None).expect("s1");
        let steps = steps_for(&conn, "ws").expect("steps");
        assert_eq!(
            steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Execute", "Refine"]
        );
        assert_eq!(steps[1].prompt.as_deref(), Some("polish it"));
    }

    #[test]
    fn insert_update_delete_and_reorder() {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "WS").expect("ws");
        insert_stage(&conn, "a", "ws", 0, "A", None, None).expect("a");
        insert(
            &conn,
            "b",
            "ws",
            1,
            &StepFields {
                kind: "loop",
                name: "B",
                agent: None,
                prompt: None,
                validator_kind: Some("agent"),
                validator_agent: None,
                validator_prompt: None,
                max_iterations: 3,
                on_exhaust: "fail",
            },
        )
        .expect("b");
        let steps = configured_steps(&conn, "ws").expect("steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].kind, StepKind::Loop);
        assert_eq!(steps[1].max_iterations, 3);

        // Reorder: swap positions.
        set_position(&conn, "a", 1).expect("a->1");
        set_position(&conn, "b", 0).expect("b->0");
        let names: Vec<String> = configured_steps(&conn, "ws")
            .expect("steps")
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["B".to_owned(), "A".to_owned()]);

        // Update + delete.
        update(
            &conn,
            "a",
            &StepFields {
                kind: "stage",
                name: "A2",
                agent: None,
                prompt: Some("do it well"),
                validator_kind: None,
                validator_agent: None,
                validator_prompt: None,
                max_iterations: 1,
                on_exhaust: "human",
            },
        )
        .expect("update");
        delete(&conn, "b").expect("delete");
        let steps = configured_steps(&conn, "ws").expect("steps");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].name, "A2");
        assert_eq!(steps[0].prompt.as_deref(), Some("do it well"));
    }
}
