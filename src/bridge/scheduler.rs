//! The task scheduler. A periodic tick scans every workspace and, for each **idle** lane,
//! launches its next ready task on a worker thread — so a workspace runs one task at a
//! time (sequential), while workspaces run in parallel. The worker runs the assigned
//! agent's turn in the workspace dir and posts `Command::TaskFinished` back, which the
//! (DB-owning) bridge thread persists and projects.

use std::time::Duration;

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::tasks::{self, Task, TaskStatus};
use crate::db::{resolution, workspaces};
use crate::state::{ActivityKind, App, ChatMessage, MessageRole};
use crate::workspace;

use super::{Command, build_toolbox, data_dir, log_line, run_system_prompt, send};

/// Default seconds between scheduler scans.
const DEFAULT_TICK_SECS: u64 = 3;

/// Start the background ticker: post a `SchedulerTick` every N seconds for the bridge to
/// service. N defaults to [`DEFAULT_TICK_SECS`], overridable with `CLAW_SCHEDULER_SECS`
/// (`0` disables the scheduler — tasks then stay queued, the C-1 manual behaviour).
pub fn spawn_ticker() {
    let secs = std::env::var("CLAW_SCHEDULER_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TICK_SECS);
    if secs == 0 {
        log_line("scheduler: disabled (CLAW_SCHEDULER_SECS=0)");
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(secs));
            send(Command::SchedulerTick);
        }
    });
}

/// One scan: for every workspace with no running task, launch its next ready task.
pub fn tick(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let Ok(workspaces) = workspaces::list(conn) else {
        return;
    };
    for ws in workspaces {
        if tasks::running_in(conn, &ws.id).ok().flatten().is_some() {
            continue;
        }
        if let Ok(Some(task)) = tasks::next_ready(conn, &ws.id) {
            start(conn, shared, task);
        }
    }
}

/// Claim a task for its lane (flip to `running`), reflect it, then dispatch the worker. A
/// task that can't be dispatched at all (no assignable agent, unresolved endpoint) is
/// finished `failed` immediately rather than left stuck `running`.
fn start(conn: &rusqlite::Connection, shared: &SharedServerState, task: Task) {
    if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Running) {
        return log_line(format!("scheduler: claim {} failed: {err}", task.id));
    }
    project_lane(conn, shared, &task.workspace_id);
    activity(
        shared,
        task.title.clone(),
        ActivityKind::Started,
        String::new(),
    );
    if let Err(reason) = launch(conn, &task) {
        send(Command::TaskFinished {
            id: task.id,
            result: Err(reason),
        });
    }
}

/// Build the run and spawn the worker thread. `Err` (with a reason) means the task could
/// not be started — [`start`] turns that into a failed finish.
fn launch(conn: &rusqlite::Connection, task: &Task) -> Result<(), String> {
    let agent_id = assignee(conn, task)
        .ok_or_else(|| "no agent assigned and the workspace has no owner".to_owned())?;
    let inference = resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, |key| {
        std::env::var(key).ok()
    })
    .map_err(|err| err.to_string())?;

    let workspace_dir = workspace::dir_for(&data_dir(), &task.workspace_id);
    let system_prompt = run_system_prompt(
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
    );
    // The task spec is the run's single user turn.
    let spec = ChatMessage {
        role: MessageRole::User,
        text: task.spec.clone(),
        time: super::now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&spec));
    let toolbox = build_toolbox(inference.tools.clone(), Some(workspace_dir));

    let id = task.id.clone();
    std::thread::spawn(move || {
        let result =
            agent::run_turn(&inference, messages, toolbox.as_ref()).map_err(|err| err.to_string());
        send(Command::TaskFinished { id, result });
    });
    Ok(())
}

/// The agent that runs a task: its explicit assignee (a member), else the workspace owner.
fn assignee(conn: &rusqlite::Connection, task: &Task) -> Option<String> {
    if let Some(agent) = &task.agent {
        return Some(agent.clone());
    }
    workspaces::get(conn, &task.workspace_id)
        .ok()
        .flatten()
        .and_then(|ws| ws.owner)
}

/// Persist a finished task (done or failed), then re-project its lane + push an activity
/// entry. The worker's final reply (or the error) is stored as the task's `result`.
pub fn finished(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    result: Result<agent::TurnResult, String>,
) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    let (status, stored, kind, detail) = match result {
        Ok(turn) => {
            let reply = turn.reply.unwrap_or_default();
            (
                TaskStatus::Done,
                reply,
                ActivityKind::Finished,
                String::new(),
            )
        }
        Err(err) => (TaskStatus::Failed, err.clone(), ActivityKind::Failed, err),
    };
    if let Err(err) = tasks::finish(conn, id, status, Some(&stored)) {
        log_line(format!("scheduler: finish {id} failed: {err}"));
    }
    project_lane(conn, shared, &task.workspace_id);
    activity(shared, task.title, kind, detail);
}

/// Re-project a lane into `App.tasks`, but only when the operator is actually viewing that
/// workspace — a background lane must not clobber the lane on screen.
fn project_lane(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = super::tasks::lane_views(conn, workspace);
    let workspace = workspace.to_owned();
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_TASKS]), move |app| {
        if app.active_workspace.as_deref() == Some(workspace.as_str()) {
            app.tasks = views;
        }
    });
}

fn activity(shared: &SharedServerState, title: String, kind: ActivityKind, detail: String) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ACTIVITY]),
        move |app| {
            super::push_activity(app, title, kind, detail);
        },
    );
}
