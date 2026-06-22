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
    // Keep the dashboard's all-lanes view live regardless of the open route.
    super::tasks::project_dashboard(conn, shared);
}

/// Claim a task for its lane (flip to `running`), reflect it, then dispatch the worker. A
/// task that can't be dispatched at all (no assignable agent, unresolved endpoint) is
/// finished `failed` immediately rather than left stuck `running`.
fn start(conn: &rusqlite::Connection, shared: &SharedServerState, task: Task) {
    if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Running) {
        return log_line(format!("scheduler: claim {} failed: {err}", task.id));
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
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
        agent::EXECUTOR_PROMPT,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        super::memory_layer(conn, &inference.agent_id),
    );
    // The task spec is the run's single user turn.
    let spec = ChatMessage {
        role: MessageRole::User,
        text: task.spec.clone(),
        time: super::now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&spec));
    // A task is workspace work: default to the workspace file tools when the agent has no
    // explicit selection (so the owner, often unconfigured, can actually write files).
    let toolbox = build_toolbox(inference.tools.clone(), Some(workspace_dir), true);

    let id = task.id.clone();
    std::thread::spawn(move || {
        let result =
            agent::run_turn(&inference, messages, toolbox.as_ref()).map_err(|err| err.to_string());
        send(Command::TaskFinished { id, result });
    });
    Ok(())
}

/// The next-run timestamp for a recurrent task that just succeeded, or `None` to finish it
/// (non-recurrent, failed, or a missing/unparseable cron). Failures finish so a broken
/// recurrent task doesn't hammer its lane every tick.
fn reschedule_at(task: &Task, status: TaskStatus) -> Option<String> {
    if !matches!(task.kind, crate::db::tasks::TaskKind::Recurrent) || status != TaskStatus::Done {
        return None;
    }
    let cron = crate::schedule::Cron::parse(task.schedule.as_deref()?)?;
    let next = cron.next_after(now_unix())?;
    Some(crate::schedule::to_rfc3339_millis(next))
}

/// Wall-clock now as unix seconds (UTC).
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
    // A recurrent task that succeeded re-enters its lane at the next scheduled slot rather
    // than going terminal; a failed (or non-recurrent) one finishes. Skip-to-next-slot: we
    // always compute the next fire strictly after now, never replaying missed occurrences.
    if let Some(next_run) = reschedule_at(&task, status) {
        if let Err(err) = tasks::reschedule(conn, id, &next_run, Some(&stored)) {
            log_line(format!("scheduler: reschedule {id} failed: {err}"));
        }
    } else if let Err(err) = tasks::finish(conn, id, status, Some(&stored)) {
        log_line(format!("scheduler: finish {id} failed: {err}"));
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
    // Notify the chat the delegation came from, in the concierge's own voice.
    if let Some(chat) = &task.origin_chat {
        super::notify::completion(chat, &task.title, status.as_str(), &stored);
    }
    activity(shared, task.title, kind, detail);
}

fn activity(shared: &SharedServerState, title: String, kind: ActivityKind, detail: String) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ACTIVITY]),
        move |app| {
            super::push_activity(app, title, kind, detail);
        },
    );
}
