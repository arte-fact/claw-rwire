//! A reviewable plan's **proposal** (R3, Option A): the structured task breakdown the owner's
//! planning turn emits, stored as JSON on the plan task's `proposal` column *before* any child
//! tasks exist. Approving the plan materialises these into real queued tasks; revising or
//! rejecting just clears them — the board never holds a half-created task to undo.

use rusqlite::{Connection, OptionalExtension, params};

use super::{NewTask, create_planned};
use crate::db::{agents, workspaces};
use crate::id;

/// One proposed task in a plan under review. `blockers` reference *other proposed tasks by title*
/// (no ids exist yet); they're resolved to real ids when the plan is materialised.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalItem {
    pub title: String,
    pub spec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}

/// The plan's current proposal — empty when nothing has been proposed yet (or it was cleared).
pub fn get(conn: &Connection, plan_id: &str) -> rusqlite::Result<Vec<ProposalItem>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT proposal FROM tasks WHERE id = ?1",
            params![plan_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default())
}

/// Append items to the plan's proposal, returning the new running total. The planning turn calls
/// this (via the `propose_tasks` tool) as it breaks the request down.
pub fn append(conn: &Connection, plan_id: &str, items: &[ProposalItem]) -> rusqlite::Result<usize> {
    let mut current = get(conn, plan_id)?;
    current.extend_from_slice(items);
    write(conn, plan_id, Some(&current))?;
    Ok(current.len())
}

/// Discard the plan's proposal (revise / reject) — there are no child rows to cancel.
pub fn clear(conn: &Connection, plan_id: &str) -> rusqlite::Result<()> {
    write(conn, plan_id, None)
}

fn write(conn: &Connection, plan_id: &str, items: Option<&[ProposalItem]>) -> rusqlite::Result<()> {
    let json = items.map(|items| serde_json::to_string(items).unwrap_or_default());
    conn.execute(
        "UPDATE tasks SET proposal = ?2,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id = ?1",
        params![plan_id, json],
    )?;
    Ok(())
}

/// Materialise an approved plan: turn its proposal into real queued tasks (one `create_planned`
/// each, tagged with `plan_id` for traceability), then wire `blockers` by matching proposed
/// titles to the freshly-minted ids. Clears the proposal once consumed. Returns how many ran.
pub fn materialize(
    conn: &Connection,
    plan_id: &str,
    origin_chat: Option<&str>,
) -> rusqlite::Result<usize> {
    let items = get(conn, plan_id)?;
    let Some(plan) = super::get(conn, plan_id)? else {
        return Ok(0);
    };
    let members = member_names(conn, &plan.workspace_id);

    // First pass: create each task, remembering proposed-title → new id for blocker wiring.
    let mut by_title: Vec<(String, String)> = Vec::new();
    let mut created: Vec<(String, Vec<String>)> = Vec::new();
    for item in &items {
        let agent_id = item.agent.as_deref().and_then(|name| {
            members
                .iter()
                .find(|(_, n)| n == name)
                .map(|(id, _)| id.clone())
        });
        let Ok(new_id) = id::new_id() else { continue };
        let new = NewTask {
            id: &new_id,
            workspace_id: &plan.workspace_id,
            agent: agent_id.as_deref(),
            title: &item.title,
            spec: &item.spec,
            schedule: item.schedule.as_deref(),
            origin_chat,
            max_retries: None,
        };
        create_planned(conn, &new, plan_id)?;
        // A plan born from an ask carries its Request (G1) — every materialised child
        // inherits it, so the lane can scope to the request and delivery can find them.
        if let Some(request_id) = &plan.request_id {
            super::set_request(conn, &new_id, request_id)?;
        }
        by_title.push((item.title.clone(), new_id.clone()));
        created.push((new_id, item.blockers.clone()));
    }

    // Second pass: resolve blockers by title (an unknown title is skipped, not fatal).
    for (task_id, blockers) in &created {
        for blocker in blockers {
            if let Some((_, blocker_id)) = by_title.iter().find(|(title, _)| title == blocker) {
                let _ = super::add_blocker(conn, task_id, blocker_id);
            }
        }
    }

    clear(conn, plan_id)?;
    Ok(created.len())
}

/// The workspace's members as `(id, name)` — the routing candidates a proposal's `agent` names.
fn member_names(conn: &Connection, workspace_id: &str) -> Vec<(String, String)> {
    workspaces::members_of(conn, workspace_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            agents::get(conn, &id)
                .ok()
                .flatten()
                .map(|a| (a.id, a.name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{TaskStatus, create_plan, list_for_workspace};
    use crate::db::{agents, endpoints, open_in_memory, workspaces};

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "owner", "Lead", Some("e"), Some("m")).expect("owner");
        agents::create(&conn, "writer", "Writer", Some("e"), Some("m")).expect("writer");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        workspaces::set_owner(&conn, "ws", "owner").expect("own");
        workspaces::add_member(&conn, "writer", "ws").expect("member");
        create_plan(&conn, "plan1", "ws", "Plan: x", "do x", None).expect("plan");
        conn
    }

    /// The real tasks the plan materialised (tagged with its id), via the public lane query.
    fn plan_children(conn: &Connection) -> Vec<super::super::Task> {
        list_for_workspace(conn, "ws")
            .expect("lane")
            .into_iter()
            .filter(|task| task.plan_id.as_deref() == Some("plan1"))
            .collect()
    }

    fn item(title: &str, blockers: &[&str], agent: Option<&str>) -> ProposalItem {
        ProposalItem {
            title: title.to_owned(),
            spec: format!("do {title}"),
            agent: agent.map(str::to_owned),
            schedule: None,
            blockers: blockers.iter().map(|b| (*b).to_owned()).collect(),
        }
    }

    #[test]
    fn append_accumulates_and_get_round_trips() {
        let conn = seeded();
        assert_eq!(
            append(&conn, "plan1", &[item("A", &[], None)]).expect("a"),
            1
        );
        assert_eq!(
            append(&conn, "plan1", &[item("B", &["A"], None)]).expect("b"),
            2
        );
        let proposal = get(&conn, "plan1").expect("get");
        assert_eq!(proposal.len(), 2);
        assert_eq!(proposal[1].blockers, vec!["A".to_owned()]);
    }

    #[test]
    fn clear_empties_the_proposal_without_touching_the_board() {
        let conn = seeded();
        append(&conn, "plan1", &[item("A", &[], None)]).expect("a");
        clear(&conn, "plan1").expect("clear");
        assert!(get(&conn, "plan1").expect("get").is_empty());
        // No child rows ever existed to leave behind.
        assert_eq!(plan_children(&conn).len(), 0);
    }

    #[test]
    fn materialize_creates_pending_children_and_wires_blockers_by_title() {
        let conn = seeded();
        append(
            &conn,
            "plan1",
            &[
                item("Core", &[], None),
                item("UI", &["Core"], Some("Writer")),
            ],
        )
        .expect("propose");

        let count = materialize(&conn, "plan1", Some("chat-1")).expect("materialize");
        assert_eq!(count, 2);

        let kids = plan_children(&conn);
        assert_eq!(kids.len(), 2);
        // Materialised children are queued (not held) and tagged with the plan.
        assert!(kids.iter().all(|k| k.status == TaskStatus::Pending));
        assert!(kids.iter().all(|k| k.plan_id.as_deref() == Some("plan1")));
        // The named assignee resolved to the member; the unassigned task went to the owner (NULL).
        let ui = kids.iter().find(|k| k.title == "UI").expect("ui");
        assert_eq!(ui.agent.as_deref(), Some("writer"));
        // The proposal is consumed once materialised.
        assert!(get(&conn, "plan1").expect("get").is_empty());

        // "UI" is blocked until "Core" is done → only "Core" is ready to run.
        let ready = crate::db::tasks::next_ready(&conn, "ws", None)
            .expect("ready")
            .expect("some");
        assert_eq!(ready.title, "Core");
    }

    #[test]
    fn materialize_of_an_empty_proposal_is_a_no_op() {
        let conn = seeded();
        assert_eq!(materialize(&conn, "plan1", None).expect("none"), 0);
    }
}
