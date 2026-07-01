//! The agent-thread projection (Phase 1, read-only): an agent's whole working life merged into
//! one time-ordered timeline. It reads the agent's tasks ([`tasks::list_for_agent`]) and each
//! task's runs, reusing the Peek modal's [`run_view`](super::tasks::run_view) mapping so a run
//! reads identically on both surfaces. Live runs are surfaced as [`AgentLive`] markers; the live
//! items themselves stream through `App.live_turns`, keyed by task id.
//!
//! `load` opens an agent's thread (route navigation); `refresh_if_open` re-projects it after any
//! task lifecycle change (piggy-backed on `tasks::refresh_detail_if_open`) so the page stays live.

use rusqlite::Connection;
use rwire::{ChangeSet, SharedServerState};

use crate::db::tasks::TaskStatus;
use crate::db::{agents, pipeline, task_runs, tasks};
use crate::state::{AgentLive, AgentThread, AgentThreadItem, App};

use super::tasks::{agent_names, run_view};

/// The current step label for a running task's live entry — the pipeline step name at the task's
/// position, with the loop iteration appended past the first (matching the Peek live entry's tag).
fn step_tag(conn: &Connection, workspace: &str, step_position: i64, iteration: i64) -> String {
    let steps = pipeline::steps_for(conn, workspace).unwrap_or_default();
    let mut tag = steps
        .get(step_position.max(0) as usize)
        .map(|step| step.name.clone())
        .unwrap_or_else(|| "Running".to_owned());
    if iteration > 0 {
        tag.push_str(&format!(" · iter {}", iteration + 1));
    }
    tag
}

/// Build an agent's thread: its identity plus, for each task it's the effective assignee of, a
/// task marker followed by that task's runs (oldest first). Running tasks also emit a live marker.
/// `None` if the agent id doesn't resolve.
fn build(conn: &Connection, agent_id: &str) -> Option<AgentThread> {
    let agent = agents::get(conn, agent_id).ok().flatten()?;
    let names = agent_names(conn);
    let mut items = Vec::new();
    let mut live = Vec::new();
    for task in tasks::list_for_agent(conn, agent_id).unwrap_or_default() {
        items.push(AgentThreadItem::Task {
            id: task.id.clone(),
            workspace_id: task.workspace_id.clone(),
            title: task.title.clone(),
            status: task.status.as_str().to_owned(),
        });
        for run in task_runs::list_for_task(conn, &task.id).unwrap_or_default() {
            items.push(AgentThreadItem::Run(run_view(&names, run)));
        }
        // A task parked at a human gate gets its inline review composer, right where its work sits —
        // approval home is the worker's own thread (open question 5).
        if task.status == TaskStatus::Awaiting {
            items.push(AgentThreadItem::ReviewGate {
                task_id: task.id.clone(),
            });
        }
        if task.status == TaskStatus::Running {
            live.push(AgentLive {
                task_id: task.id.clone(),
                title: task.title,
                tag: step_tag(conn, &task.workspace_id, task.step_position, task.iteration),
            });
        }
    }
    Some(AgentThread {
        agent_id: agent.id,
        agent_name: agent.name,
        items,
        live,
    })
}

/// Open (or re-open) an agent's thread — set as the open agent and project its timeline. Called
/// when a `/agent/:id` route resolves.
pub fn load(conn: &Connection, shared: &SharedServerState, agent_id: &str) {
    let thread = build(conn, agent_id);
    let agent_id = agent_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_OPEN_AGENT, App::FIELD_AGENT_THREAD]),
        move |app| {
            app.open_agent = Some(agent_id);
            app.agent_thread = thread;
        },
    );
}

/// Re-project the open agent's thread after a task lifecycle change, so a run finishing / a task
/// advancing updates the page live. A cheap no-op when no agent thread is on screen (`open_agent`
/// is `None`); when set, it rebuilds that one agent's bounded timeline in place.
pub fn refresh_if_open(conn: &Connection, shared: &SharedServerState) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_AGENT_THREAD]),
        |app| {
            if let Some(agent_id) = app.open_agent.clone() {
                app.agent_thread = build(conn, &agent_id);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::task_runs::NewRun;
    use crate::db::tasks::NewTask;
    use crate::db::{endpoints, open_in_memory, workspaces};
    use crate::state::AgentThreadItem;

    fn seeded() -> Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("e");
        agents::create(&conn, "ag-a", "Forge", Some("e"), Some("m")).expect("a");
        workspaces::create(&conn, "ws", "Studio").expect("ws");
        workspaces::set_owner(&conn, "ws", "ag-a").expect("own");
        conn
    }

    #[test]
    fn build_projects_identity_and_returns_none_for_unknown_agent() {
        let conn = seeded();
        let thread = build(&conn, "ag-a").expect("thread");
        assert_eq!(thread.agent_id, "ag-a");
        assert_eq!(thread.agent_name, "Forge");
        assert!(build(&conn, "nope").is_none(), "unknown agent → None");
    }

    #[test]
    fn build_interleaves_task_markers_with_their_runs_in_order() {
        let conn = seeded();
        tasks::create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "Build a highlighter",
                spec: "do it",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");
        task_runs::append(
            &conn,
            &NewRun {
                id: "r1",
                task_id: "t1",
                position: 0,
                role: "producer",
                agent: Some("ag-a"),
                output: Some("first cut"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt: 0,
            },
        )
        .expect("run");

        let thread = build(&conn, "ag-a").expect("thread");
        // The task marker leads, its run follows.
        match &thread.items[0] {
            AgentThreadItem::Task { id, title, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(title, "Build a highlighter");
            }
            other => panic!("expected a task marker first, got {other:?}"),
        }
        match &thread.items[1] {
            AgentThreadItem::Run(run) => {
                assert_eq!(run.output, "first cut");
                assert_eq!(run.agent, "Forge", "the agent id resolves to its name");
            }
            other => panic!("expected a run second, got {other:?}"),
        }
        assert!(thread.live.is_empty(), "a pending task isn't live");
    }

    #[test]
    fn build_emits_a_review_gate_after_an_awaiting_tasks_runs() {
        let conn = seeded();
        tasks::create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "Parked task",
                spec: "do it",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");
        tasks::set_status(&conn, "t1", TaskStatus::Awaiting).expect("park");

        let thread = build(&conn, "ag-a").expect("thread");
        assert!(
            thread.items.iter().any(|item| matches!(
                item,
                AgentThreadItem::ReviewGate { task_id } if task_id == "t1"
            )),
            "an awaiting task surfaces its inline review gate"
        );
        // A running/pending task must NOT get a gate.
        tasks::set_status(&conn, "t1", TaskStatus::Running).expect("run");
        let thread = build(&conn, "ag-a").expect("thread");
        assert!(
            !thread
                .items
                .iter()
                .any(|item| matches!(item, AgentThreadItem::ReviewGate { .. })),
            "a running task has no review gate"
        );
    }

    #[test]
    fn build_marks_a_running_task_live() {
        let conn = seeded();
        tasks::create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "Running task",
                spec: "do it",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");
        tasks::set_status(&conn, "t1", TaskStatus::Running).expect("run");

        let thread = build(&conn, "ag-a").expect("thread");
        assert_eq!(thread.live.len(), 1);
        assert_eq!(thread.live[0].task_id, "t1");
        assert_eq!(thread.live[0].title, "Running task");
    }
}
