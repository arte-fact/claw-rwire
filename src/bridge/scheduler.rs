//! The task scheduler. A periodic tick scans every workspace and, for each **idle** lane,
//! launches its next ready task on a worker thread — so a workspace runs one task at a
//! time (sequential), while workspaces run in parallel. The worker runs the assigned
//! agent's turn in the workspace dir and posts `Command::TaskFinished` back, which the
//! (DB-owning) bridge thread persists and projects.

use std::time::Duration;

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::pipeline::{self, Step};
use crate::db::tasks::{self, Task, TaskStatus};
use crate::db::{resolution, task_runs, workspaces};
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
    if let Err(reason) = launch_step(conn, &task) {
        send(Command::TaskFinished {
            id: task.id,
            result: Err(reason),
        });
    }
}

/// Run the task's **current pipeline step** on a worker thread: the step's agent (or owner)
/// runs with the step's role preamble and the **chained context** (the task spec + earlier
/// steps' outputs). `Err` means the step couldn't be started — the caller fails the task.
fn launch_step(conn: &rusqlite::Connection, task: &Task) -> Result<(), String> {
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let position = task.step_position.max(0) as usize;
    let step = steps
        .get(position)
        .ok_or_else(|| "the pipeline has no step at this position".to_owned())?;

    let agent_id = step_agent(conn, task, step)
        .ok_or_else(|| "no agent for this step and the workspace has no owner".to_owned())?;
    let inference = resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, |key| {
        std::env::var(key).ok()
    })
    .map_err(|err| err.to_string())?;

    let workspace_dir = workspace::dir_for(&data_dir(), &task.workspace_id);
    // The step's role-preamble override, else the default executor preamble.
    let role = step
        .prompt
        .clone()
        .unwrap_or_else(|| super::role_prompt(conn, crate::state::RunKind::Task));
    let system_prompt = run_system_prompt(
        &role,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        super::memory_layer(conn, &inference.agent_id),
    );
    let prior = task_runs::list_for_task(conn, &task.id).unwrap_or_default();
    let context = step_context(&task.spec, &prior, &step.name);
    let user = ChatMessage {
        role: MessageRole::User,
        text: context,
        time: super::now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&user));
    let toolbox = build_toolbox(inference.tools.clone(), Some(workspace_dir), true);

    let id = task.id.clone();
    std::thread::spawn(move || {
        let result =
            agent::run_turn(&inference, messages, toolbox.as_ref()).map_err(|err| err.to_string());
        send(Command::TaskFinished { id, result });
    });
    Ok(())
}

/// The agent that runs a step: the step's own agent, else the task's assignee (back-compat for
/// the default single stage), else the workspace owner.
fn step_agent(conn: &rusqlite::Connection, task: &Task, step: &Step) -> Option<String> {
    step.agent
        .clone()
        .or_else(|| task.agent.clone())
        .or_else(|| {
            workspaces::get(conn, &task.workspace_id)
                .ok()
                .flatten()
                .and_then(|ws| ws.owner)
        })
}

/// The user turn for a step: the task spec plus the outputs of earlier steps (the chain).
fn step_context(spec: &str, prior: &[task_runs::TaskRun], step_name: &str) -> String {
    let mut text = format!("Task: {spec}");
    let outputs: Vec<&str> = prior
        .iter()
        .filter(|run| run.ok)
        .filter_map(|run| run.output.as_deref())
        .filter(|out| !out.trim().is_empty())
        .collect();
    if outputs.is_empty() {
        text.push_str(&format!(
            "\n\nYour step: \"{step_name}\". Do it and briefly report what you did."
        ));
    } else {
        text.push_str("\n\nWork produced by earlier steps:");
        for (index, out) in outputs.iter().enumerate() {
            text.push_str(&format!("\n\n--- step {} ---\n{out}", index + 1));
        }
        text.push_str(&format!(
            "\n\nYour step: \"{step_name}\". Build on the work above and briefly report what you did."
        ));
    }
    text
}

/// What to do with a task after a stage run finishes — a pure decision, unit-tested.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    Advance(i64),
    Done,
    Fail,
}

fn next_action(step_position: i64, step_count: usize, ok: bool) -> Next {
    if !ok {
        Next::Fail
    } else if step_position + 1 < step_count as i64 {
        Next::Advance(step_position + 1)
    } else {
        Next::Done
    }
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

/// A step run finished: record it in the chain, then **advance the pipeline** — run the next
/// stage (the task stays running), or finish (done/failed) after the last. A failed step ends
/// the task; a recurrent task that completes its pipeline re-enters at the next slot.
pub fn finished(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    result: Result<agent::TurnResult, String>,
) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    let (ok, output) = match result {
        Ok(turn) => (true, turn.reply.unwrap_or_default()),
        Err(err) => (false, err),
    };

    // Record this step's run in the chain (the context source + audit trail).
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let agent = steps
        .get(task.step_position.max(0) as usize)
        .and_then(|step| step_agent(conn, &task, step));
    if let Ok(run_id) = crate::id::new_id() {
        let _ = task_runs::append(
            conn,
            &task_runs::NewRun {
                id: &run_id,
                task_id: id,
                position: task.step_position,
                role: "stage",
                agent: agent.as_deref(),
                output: Some(&output),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok,
            },
        );
    }

    match next_action(task.step_position, steps.len(), ok) {
        Next::Advance(next_position) => {
            if let Err(err) = tasks::set_step(conn, id, next_position) {
                log_line(format!("scheduler: advance {id} failed: {err}"));
            }
            // Immediately launch the next step — the task holds the lane through its pipeline.
            if let Ok(Some(advanced)) = tasks::get(conn, id)
                && let Err(reason) = launch_step(conn, &advanced)
            {
                send(Command::TaskFinished {
                    id: id.to_owned(),
                    result: Err(reason),
                });
            }
            super::tasks::project_if_active(conn, shared, &task.workspace_id);
            activity(
                shared,
                task.title,
                ActivityKind::Started,
                format!("step {}", next_position + 1),
            );
        }
        terminal => {
            let status = if matches!(terminal, Next::Done) {
                TaskStatus::Done
            } else {
                TaskStatus::Failed
            };
            // Recurrent + done → re-enter at the next slot (resets to step 0); else terminal.
            if let Some(next_run) = reschedule_at(&task, status) {
                if let Err(err) = tasks::reschedule(conn, id, &next_run, Some(&output)) {
                    log_line(format!("scheduler: reschedule {id} failed: {err}"));
                }
            } else if let Err(err) = tasks::finish(conn, id, status, Some(&output)) {
                log_line(format!("scheduler: finish {id} failed: {err}"));
            }
            super::tasks::project_if_active(conn, shared, &task.workspace_id);
            if let Some(chat) = &task.origin_chat {
                super::notify::completion(chat, &task.title, status.as_str(), &output);
            }
            let (kind, detail) = if ok {
                (ActivityKind::Finished, String::new())
            } else {
                (ActivityKind::Failed, output.clone())
            };
            activity(shared, task.title, kind, detail);
        }
    }
}

fn activity(shared: &SharedServerState, title: String, kind: ActivityKind, detail: String) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ACTIVITY]),
        move |app| {
            super::push_activity(app, title, kind, detail);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_action_advances_through_steps_then_completes() {
        assert_eq!(next_action(0, 2, true), Next::Advance(1)); // step 0 of 2 → advance
        assert_eq!(next_action(1, 2, true), Next::Done); // last step → done
        assert_eq!(next_action(0, 1, true), Next::Done); // single (default) stage → done
        assert_eq!(next_action(0, 3, false), Next::Fail); // failure ends regardless of position
    }

    #[test]
    fn step_context_chains_prior_outputs() {
        let prior = vec![task_runs::TaskRun {
            id: "r1".to_owned(),
            position: 0,
            role: "stage".to_owned(),
            agent: None,
            output: Some("built the thing".to_owned()),
            verdict: None,
            feedback: None,
            iteration: 0,
            ok: true,
        }];
        let ctx = step_context("make X", &prior, "Refine");
        assert!(ctx.contains("Task: make X"));
        assert!(ctx.contains("built the thing"));
        assert!(ctx.contains("Refine"));
        // No prior runs → the simpler first-step prompt.
        let first = step_context("make X", &[], "Execute");
        assert!(!first.contains("earlier steps"));
    }
}
