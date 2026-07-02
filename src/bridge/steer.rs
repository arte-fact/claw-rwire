//! Steer (R4): the operator leaves a message on a worker's thread; the agent picks it up at
//! its **next pass boundary** — never mid-run, so in-flight work is untouched.
//!
//! Delivery has two shapes, decided at send time:
//! - the agent has open work in this workspace → the message queues, and the next producer
//!   launch injects it as a trailing user turn (see `scheduler::consume_steers`);
//! - the agent is idle here → there is no upcoming pass to ride, so the message materialises
//!   as a **task** assigned to the agent (the scheduler starts the turn now).

use rwire::SharedServerState;

use crate::db::{agent_messages, tasks};

use super::log_line;

/// Queue (or materialise) a steer message for `(agent, workspace)` — the conn-only core, so
/// the decision is unit-testable. Returns whether the message became a task.
pub(super) fn deliver_or_queue(
    conn: &rusqlite::Connection,
    agent_id: &str,
    workspace_id: &str,
    body: &str,
) -> bool {
    let body = body.trim();
    if body.is_empty() {
        return false;
    }
    let Ok(id) = crate::id::new_id() else {
        log_line("steer: could not mint a message id");
        return false;
    };
    if let Err(err) = agent_messages::queue(conn, &id, agent_id, workspace_id, body) {
        log_line(format!("steer: queue failed: {err}"));
        return false;
    }
    let busy = tasks::has_open_for_agent(conn, agent_id, workspace_id).unwrap_or(false);
    if busy {
        return false; // rides the next pass of the open work
    }
    // Idle: the message becomes a task assigned to the agent — its own turn, starting now.
    let Ok(task_id) = crate::id::new_id() else {
        return false;
    };
    let title = first_line_title(body);
    match tasks::create(
        conn,
        &tasks::NewTask {
            id: &task_id,
            workspace_id,
            agent: Some(agent_id),
            title: &title,
            spec: body,
            schedule: None,
            origin_chat: None,
            max_retries: None,
        },
    ) {
        Ok(()) => {
            let _ = agent_messages::mark_delivered(conn, &id);
            true
        }
        Err(err) => {
            log_line(format!("steer: create task failed: {err}"));
            false
        }
    }
}

/// The bridge handler: persist + decide, then reflect on the open thread and the board.
pub fn send_message(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent_id: &str,
    workspace_id: &str,
    body: &str,
) {
    let became_task = deliver_or_queue(conn, agent_id, workspace_id, body);
    super::agent_thread::refresh_if_open(conn, shared);
    if became_task {
        super::tasks::project_if_active(conn, shared, workspace_id);
    }
}

/// The message's first line, trimmed to a card-title length.
fn first_line_title(body: &str) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    let title: String = line.chars().take(60).collect();
    if title.is_empty() {
        "Operator message".to_owned()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, open_in_memory, workspaces};

    fn seeded() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        agents::create(&conn, "ag-1", "Forge", None, None).expect("agent");
        workspaces::create(&conn, "ws", "Studio").expect("ws");
        conn
    }

    #[test]
    fn a_message_to_an_idle_agent_becomes_its_task_and_is_delivered() {
        let conn = seeded();
        assert!(deliver_or_queue(
            &conn,
            "ag-1",
            "ws",
            "polish the README\nwith examples"
        ));
        let lane = tasks::list_for_workspace(&conn, "ws").expect("lane");
        assert_eq!(lane.len(), 1);
        assert_eq!(lane[0].title, "polish the README");
        assert_eq!(lane[0].agent.as_deref(), Some("ag-1"));
        assert!(
            agent_messages::undelivered(&conn, "ag-1", "ws")
                .expect("q")
                .is_empty(),
            "the task IS the delivery"
        );
    }

    #[test]
    fn a_message_to_a_busy_agent_queues_for_the_next_pass() {
        let conn = seeded();
        tasks::create(
            &conn,
            &tasks::NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: Some("ag-1"),
                title: "in flight",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("task");
        assert!(!deliver_or_queue(&conn, "ag-1", "ws", "also add tests"));
        assert_eq!(
            tasks::list_for_workspace(&conn, "ws").expect("lane").len(),
            1,
            "no extra task"
        );
        let queued = agent_messages::undelivered(&conn, "ag-1", "ws").expect("q");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].body, "also add tests");
    }

    #[test]
    fn blank_messages_are_ignored() {
        let conn = seeded();
        assert!(!deliver_or_queue(&conn, "ag-1", "ws", "   \n "));
        assert!(
            agent_messages::recent(&conn, "ag-1", "ws", 10)
                .expect("recent")
                .is_empty()
        );
    }
}
