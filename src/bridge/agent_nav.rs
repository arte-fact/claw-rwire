//! The agent-list nav projection (Phase 2): the concierge (web agent) pinned on top, then every
//! other agent grouped under the workspace it belongs to, each carrying a live status (idle /
//! working / awaiting) and an approval badge. Derived from the registry (agents + workspace
//! membership) and per-agent in-flight task counts.
//!
//! `project` is piggy-backed on `review::project_awaiting` (task lifecycle) and
//! `admin::project_registry` (membership/agent edits), so the nav stays live on both axes.

use std::collections::HashMap;

use rwire::{ChangeSet, SharedServerState};

use crate::db::{agents, tasks, workspaces};
use crate::state::{AgentNav, AgentNavGroup, AgentNavRow, AgentStatus, App};

/// An agent's status from its `(running, awaiting)` in-flight counts — awaiting wins, since it's
/// the state that needs the operator.
fn status_of(running: i64, awaiting: i64) -> AgentStatus {
    if awaiting > 0 {
        AgentStatus::Awaiting
    } else if running > 0 {
        AgentStatus::Working
    } else {
        AgentStatus::Idle
    }
}

/// Build the nav model: resolve the concierge, then group the remaining members by workspace,
/// stamping each agent with its status + awaiting count.
fn build(conn: &rusqlite::Connection) -> AgentNav {
    let names: HashMap<String, String> = agents::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();
    let inflight: HashMap<String, (i64, i64)> = tasks::count_inflight_by_agent(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, running, awaiting)| (id, (running, awaiting)))
        .collect();
    let ws_awaiting: HashMap<String, i64> = tasks::count_awaiting_by_workspace(conn)
        .unwrap_or_default()
        .into_iter()
        .collect();

    let row = |id: &str| {
        let (running, awaiting) = inflight.get(id).copied().unwrap_or((0, 0));
        AgentNavRow {
            id: id.to_owned(),
            name: names.get(id).cloned().unwrap_or_else(|| id.to_owned()),
            status: status_of(running, awaiting),
            awaiting: awaiting.max(0) as usize,
        }
    };

    let concierge_id = agents::web_agent(conn).unwrap_or_default();
    let concierge = concierge_id.as_deref().map(row);

    let groups = workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|ws| {
            let agents = workspaces::members_of(conn, &ws.id)
                .unwrap_or_default()
                .into_iter()
                .filter(|member| Some(member.as_str()) != concierge_id.as_deref())
                .map(|member| row(&member))
                .collect();
            AgentNavGroup {
                awaiting: ws_awaiting.get(&ws.id).copied().unwrap_or(0).max(0) as usize,
                workspace_id: ws.id,
                workspace_name: ws.name,
                agents,
            }
        })
        .collect();

    AgentNav { concierge, groups }
}

/// Project the agent-list nav into `App.agent_nav`. Cheap (a few small grouped queries); called on
/// every task lifecycle change and every registry edit so status dots + badges stay live.
pub fn project(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let nav = build(conn);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_AGENT_NAV]),
        move |app| app.agent_nav = nav,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{NewTask, TaskStatus};
    use crate::db::{agents, chat, endpoints, open_in_memory};

    /// Seed the showcase-like topology: claw (concierge) owns Studio + is in Research; Forge in
    /// Studio; Scout owns Research; Critic in both.
    fn seeded() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "claw", "claw", Some("e"), Some("m")).expect("claw");
        agents::create(&conn, "forge", "Forge", Some("e"), Some("m")).expect("forge");
        agents::create(&conn, "scout", "Scout", Some("e"), Some("m")).expect("scout");
        agents::create(&conn, "critic", "Critic", Some("e"), Some("m")).expect("critic");
        workspaces::create(&conn, "studio", "Studio").expect("studio");
        workspaces::create(&conn, "research", "Research").expect("research");
        workspaces::set_owner(&conn, "studio", "claw").expect("own studio");
        workspaces::set_owner(&conn, "research", "scout").expect("own research");
        workspaces::add_member(&conn, "forge", "studio").expect("forge");
        workspaces::add_member(&conn, "critic", "studio").expect("critic s");
        workspaces::add_member(&conn, "claw", "research").expect("claw r");
        workspaces::add_member(&conn, "critic", "research").expect("critic r");
        // Route the (web) channel at claw so it resolves as the concierge.
        chat::create_chat(&conn, "c1", "New chat").expect("chat");
        agents::relink_web_chats(&conn, "claw").expect("relink");
        conn
    }

    fn names(rows: &[AgentNavRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn pins_the_concierge_and_excludes_it_from_groups() {
        let conn = seeded();
        let nav = build(&conn);
        assert_eq!(
            nav.concierge.as_ref().map(|r| r.name.as_str()),
            Some("claw")
        );
        // claw owns Studio + is in Research, but is never listed inside a group.
        for group in &nav.groups {
            assert!(
                !group.agents.iter().any(|a| a.id == "claw"),
                "concierge excluded from {}",
                group.workspace_name
            );
        }
        let studio = nav
            .groups
            .iter()
            .find(|g| g.workspace_id == "studio")
            .unwrap();
        assert_eq!(names(&studio.agents), vec!["Critic", "Forge"]); // sorted by id
        let research = nav
            .groups
            .iter()
            .find(|g| g.workspace_id == "research")
            .unwrap();
        assert_eq!(names(&research.agents), vec!["Critic", "Scout"]);
    }

    #[test]
    fn stamps_working_and_awaiting_status_with_badges() {
        let conn = seeded();
        // Forge has a running task in Studio.
        tasks::create(
            &conn,
            &NewTask {
                id: "t-run",
                workspace_id: "studio",
                agent: Some("forge"),
                title: "build",
                spec: "x",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("run task");
        tasks::set_status(&conn, "t-run", TaskStatus::Running).expect("run");
        // Scout has an awaiting task in Research (as owner, unassigned).
        tasks::create(
            &conn,
            &NewTask {
                id: "t-await",
                workspace_id: "research",
                agent: None,
                title: "survey",
                spec: "y",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("await task");
        tasks::set_status(&conn, "t-await", TaskStatus::Awaiting).expect("park");

        let nav = build(&conn);
        let forge = nav
            .groups
            .iter()
            .flat_map(|g| &g.agents)
            .find(|a| a.id == "forge")
            .unwrap();
        assert_eq!(forge.status, AgentStatus::Working);
        let scout = nav
            .groups
            .iter()
            .flat_map(|g| &g.agents)
            .find(|a| a.id == "scout")
            .unwrap();
        assert_eq!(scout.status, AgentStatus::Awaiting);
        assert_eq!(scout.awaiting, 1);
        // The Research group header carries the workspace's awaiting total.
        let research = nav
            .groups
            .iter()
            .find(|g| g.workspace_id == "research")
            .unwrap();
        assert_eq!(research.awaiting, 1);
    }
}
