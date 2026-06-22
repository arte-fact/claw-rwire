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

const SELECT: &str = "SELECT position, kind, name, agent_id, prompt, validator_kind, \
                      validator_agent_id, validator_prompt, max_iterations, on_exhaust \
                      FROM pipeline_steps";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Step> {
    Ok(Step {
        position: row.get(0)?,
        kind: StepKind::from_str(&row.get::<_, String>(1)?),
        name: row.get(2)?,
        agent: row.get(3)?,
        prompt: row.get(4)?,
        validator_kind: row.get(5)?,
        validator_agent: row.get(6)?,
        validator_prompt: row.get(7)?,
        max_iterations: row.get(8)?,
        on_exhaust: row.get(9)?,
    })
}

/// The default pipeline when a workspace has configured none: a single `Execute` stage whose
/// agent is the owner (`None`) and whose role preamble is the default executor.
fn default_steps() -> Vec<Step> {
    vec![Step {
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

/// A workspace's pipeline steps in order — the configured ones, or the default single stage when
/// none are configured. Always non-empty.
pub fn steps_for(conn: &Connection, workspace_id: &str) -> rusqlite::Result<Vec<Step>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE workspace_id = ?1 ORDER BY position ASC"
    ))?;
    let rows: Vec<Step> = stmt
        .query_map(params![workspace_id], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(if rows.is_empty() {
        default_steps()
    } else {
        rows
    })
}

/// Insert a step (used by the seed + the editor). `id` is caller-minted.
pub fn insert_stage(
    conn: &Connection,
    id: &str,
    workspace_id: &str,
    position: i64,
    name: &str,
    agent: Option<&str>,
    prompt: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pipeline_steps (id, workspace_id, position, kind, name, agent_id, prompt)
         VALUES (?1, ?2, ?3, 'stage', ?4, ?5, ?6)",
        params![id, workspace_id, position, name, agent, prompt],
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
}
