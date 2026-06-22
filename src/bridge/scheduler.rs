//! The task scheduler. A periodic tick scans every workspace and, for each **idle** lane,
//! launches its next ready task on a worker thread — so a workspace runs one task at a
//! time (sequential), while workspaces run in parallel. The worker runs the assigned
//! agent's turn in the workspace dir and posts `Command::TaskFinished` back, which the
//! (DB-owning) bridge thread persists and projects.

use std::time::Duration;

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::pipeline::{self, Step, StepKind};
use crate::db::resolution::{self, ResolvedInference};
use crate::db::tasks::{self, Task, TaskStatus};
use crate::db::{task_runs, workspaces};
use crate::state::{ActivityKind, App, ChatMessage, MessageRole, RunRole};
use crate::tools::{CompositeToolbox, Toolbox, ValidatorToolbox};
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
    launch_current(conn, &task);
}

/// Launch the task's **current step's** producer/stage run on a worker thread (self-posting
/// `TaskFinished`). The run role is `Producer` for a loop step (its validator runs next) or
/// `Stage` for a plain step. The agent runs with the step's role preamble + chained context.
fn launch_current(conn: &rusqlite::Connection, task: &Task) {
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let Some(step) = steps.get(task.step_position.max(0) as usize).cloned() else {
        return send(Command::TaskFinished {
            id: task.id.clone(),
            role: RunRole::Stage,
            result: Err("the pipeline has no step at this position".to_owned()),
        });
    };
    let role = if step.kind == StepKind::Loop {
        RunRole::Producer
    } else {
        RunRole::Stage
    };
    let prepared = (|| -> Result<_, String> {
        let agent_id = step_agent(conn, task, &step)
            .ok_or_else(|| "no agent for this step and the workspace has no owner".to_owned())?;
        let inference =
            resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, env_lookup)
                .map_err(|err| err.to_string())?;
        let workspace_dir = workspace::dir_for(&data_dir(), &task.workspace_id);
        let preamble = step
            .prompt
            .clone()
            .unwrap_or_else(|| super::role_prompt(conn, crate::state::RunKind::Task));
        let system_prompt = run_system_prompt(
            &preamble,
            Some(&workspace_dir),
            &inference.agent_name,
            inference.agent_prompt.clone(),
            super::memory_layer(conn, &inference.agent_id),
        );
        let prior = task_runs::list_for_task(conn, &task.id).unwrap_or_default();
        let user = ChatMessage {
            role: MessageRole::User,
            text: step_context(&task.spec, &prior, &step.name),
            time: super::now_hm(),
        };
        let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&user));
        let toolbox = build_toolbox(inference.tools.clone(), Some(workspace_dir), true);
        Ok((inference, messages, toolbox))
    })();
    spawn_run(task.id.clone(), role, prepared);
}

/// Launch a loop step's **validator** run: a read-only review of the producer's `output` that
/// must call the `verdict` tool. Self-posting `TaskFinished { role: Validator }`.
fn launch_validator(conn: &rusqlite::Connection, task: &Task, step: &Step, output: &str) {
    let prepared = (|| -> Result<_, String> {
        let agent_id = step
            .validator_agent
            .clone()
            .or_else(|| owner_of(conn, &task.workspace_id))
            .ok_or_else(|| "no validator agent and the workspace has no owner".to_owned())?;
        let inference =
            resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, env_lookup)
                .map_err(|err| err.to_string())?;
        let workspace_dir = workspace::dir_for(&data_dir(), &task.workspace_id);
        let preamble = step
            .validator_prompt
            .clone()
            .unwrap_or_else(|| super::role_prompt(conn, crate::state::RunKind::Validator));
        let system_prompt = run_system_prompt(
            &preamble,
            Some(&workspace_dir),
            &inference.agent_name,
            inference.agent_prompt.clone(),
            super::memory_layer(conn, &inference.agent_id),
        );
        let user = ChatMessage {
            role: MessageRole::User,
            text: validator_context(&task.spec, output),
            time: super::now_hm(),
        };
        let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&user));
        // Read-only: the workspace `read` tool plus the `verdict` tool — no writing.
        let read = build_toolbox(Some(vec!["read".to_owned()]), Some(workspace_dir), false);
        let toolbox: Box<dyn Toolbox> = Box::new(CompositeToolbox::new(vec![
            Box::new(ValidatorToolbox),
            read,
        ]));
        Ok((inference, messages, toolbox))
    })();
    spawn_run(task.id.clone(), RunRole::Validator, prepared);
}

type Prepared = Result<(ResolvedInference, Vec<agent::ChatMessage>, Box<dyn Toolbox>), String>;

/// Spawn the worker for a prepared run, or post the preparation failure straight back.
fn spawn_run(id: String, role: RunRole, prepared: Prepared) {
    match prepared {
        Ok((inference, messages, toolbox)) => {
            std::thread::spawn(move || {
                let result = agent::run_turn(&inference, messages, toolbox.as_ref())
                    .map_err(|err| err.to_string());
                send(Command::TaskFinished { id, role, result });
            });
        }
        Err(reason) => send(Command::TaskFinished {
            id,
            role,
            result: Err(reason),
        }),
    }
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The agent that runs a step: the step's own agent, else the task's assignee (back-compat for
/// the default single stage), else the workspace owner.
fn step_agent(conn: &rusqlite::Connection, task: &Task, step: &Step) -> Option<String> {
    step.agent
        .clone()
        .or_else(|| task.agent.clone())
        .or_else(|| owner_of(conn, &task.workspace_id))
}

fn owner_of(conn: &rusqlite::Connection, workspace_id: &str) -> Option<String> {
    workspaces::get(conn, workspace_id)
        .ok()
        .flatten()
        .and_then(|ws| ws.owner)
}

/// The user turn for a producer/stage run: the spec, the outputs of earlier work steps, and
/// (on a loop re-run) the reviewer's latest "revise" feedback to address.
fn step_context(spec: &str, prior: &[task_runs::TaskRun], step_name: &str) -> String {
    let mut text = format!("Task: {spec}");
    let outputs: Vec<&str> = prior
        .iter()
        .filter(|run| run.ok && (run.role == "stage" || run.role == "producer"))
        .filter_map(|run| run.output.as_deref())
        .filter(|out| !out.trim().is_empty())
        .collect();
    if !outputs.is_empty() {
        text.push_str("\n\nWork produced so far:");
        for (index, out) in outputs.iter().enumerate() {
            text.push_str(&format!("\n\n--- step {} ---\n{out}", index + 1));
        }
    }
    if let Some(feedback) = prior
        .iter()
        .rev()
        .find(|run| run.role == "validator" && run.verdict.as_deref() == Some("revise"))
        .and_then(|run| run.feedback.as_deref())
        .filter(|fb| !fb.trim().is_empty())
    {
        text.push_str(&format!(
            "\n\nThe reviewer asked you to revise. Feedback to address:\n{feedback}"
        ));
    }
    text.push_str(&format!(
        "\n\nYour step: \"{step_name}\". {}",
        if outputs.is_empty() {
            "Do it and briefly report what you did."
        } else {
            "Build on / revise the work above and briefly report what you did."
        }
    ));
    text
}

/// The user turn for a validator run: the spec and the producer's output to review.
fn validator_context(spec: &str, output: &str) -> String {
    format!(
        "Task spec:\n{spec}\n\nThe assignee produced this for review:\n\n{output}\n\n\
         Review it against the spec (read the files if needed) and call the `verdict` tool."
    )
}

/// The most recent successful producer/stage output — the work an `approve` accepts.
fn last_producer_output(conn: &rusqlite::Connection, task_id: &str) -> String {
    task_runs::list_for_task(conn, task_id)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|run| run.ok && (run.role == "stage" || run.role == "producer"))
        .and_then(|run| run.output.clone())
        .unwrap_or_default()
}

fn role_str(role: RunRole) -> &'static str {
    match role {
        RunRole::Stage => "stage",
        RunRole::Producer => "producer",
        RunRole::Validator => "validator",
    }
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

/// A run finished: record it in the chain, then route by what ran (`role`).
/// - **Stage** → advance to the next step, or finish (done/failed) after the last.
/// - **Producer** → on success, run the loop's validator; on failure, fail the task.
/// - **Validator** → route on the `verdict`: pass → advance past the loop; revise → re-run the
///   producer with feedback (up to `max_iterations`, then on-exhaust); reject → terminal.
pub fn finished(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    role: RunRole,
    result: Result<agent::TurnResult, String>,
) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let step = steps.get(task.step_position.max(0) as usize).cloned();
    let (ok, output, invocations) = match result {
        Ok(turn) => (true, turn.reply.unwrap_or_default(), turn.invocations),
        Err(err) => (false, err, Vec::new()),
    };
    let verdict = if matches!(role, RunRole::Validator) && ok {
        crate::tools::parse_verdict(&invocations)
    } else {
        None
    };

    // Record the run in the chain (context source + audit trail).
    let agent = step.as_ref().and_then(|s| match role {
        RunRole::Validator => s
            .validator_agent
            .clone()
            .or_else(|| owner_of(conn, &task.workspace_id)),
        _ => step_agent(conn, &task, s),
    });
    if let Ok(run_id) = crate::id::new_id() {
        let _ = task_runs::append(
            conn,
            &task_runs::NewRun {
                id: &run_id,
                task_id: id,
                position: task.step_position,
                role: role_str(role),
                agent: agent.as_deref(),
                output: Some(&output),
                verdict: verdict.as_ref().map(|(decision, _)| decision.as_str()),
                feedback: verdict.as_ref().map(|(_, feedback)| feedback.as_str()),
                iteration: task.iteration,
                ok,
            },
        );
    }

    match role {
        RunRole::Stage => match next_action(task.step_position, steps.len(), ok) {
            Next::Advance(next_position) => advance_to(conn, shared, &task, next_position),
            _ if ok => terminal_finish(conn, shared, &task, TaskStatus::Done, &output),
            _ => terminal_finish(conn, shared, &task, TaskStatus::Failed, &output),
        },
        RunRole::Producer => {
            if !ok {
                terminal_finish(conn, shared, &task, TaskStatus::Failed, &output);
            } else if let Some(step) = &step {
                launch_validator(conn, &task, step, &output);
                super::tasks::project_if_active(conn, shared, &task.workspace_id);
            }
        }
        RunRole::Validator => {
            if !ok {
                return terminal_finish(conn, shared, &task, TaskStatus::Failed, &output);
            }
            let Some(step) = step else { return };
            route_verdict(conn, shared, &task, &step, &steps, verdict);
        }
    }
}

/// Route a validator's verdict (defaulting a missing verdict to `revise`, so a confused
/// reviewer is bounded by the iteration cap rather than passing silently).
fn route_verdict(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    step: &Step,
    steps: &[Step],
    verdict: Option<(String, String)>,
) {
    let (decision, feedback) = verdict.unwrap_or_else(|| {
        (
            "revise".to_owned(),
            "(the reviewer returned no verdict)".to_owned(),
        )
    });
    match decision.as_str() {
        "pass" => match next_action(task.step_position, steps.len(), true) {
            Next::Advance(next_position) => advance_to(conn, shared, task, next_position),
            _ => terminal_finish(
                conn,
                shared,
                task,
                TaskStatus::Done,
                &last_producer_output(conn, &task.id),
            ),
        },
        "reject" => terminal_finish(conn, shared, task, TaskStatus::Rejected, &feedback),
        _ => {
            // revise: iterate the producer if there's budget left, else exhaust.
            if task.iteration + 1 < step.max_iterations {
                let _ = tasks::set_iteration(conn, &task.id, task.iteration + 1);
                if let Ok(Some(again)) = tasks::get(conn, &task.id) {
                    launch_current(conn, &again);
                }
                super::tasks::project_if_active(conn, shared, &task.workspace_id);
                activity(
                    shared,
                    task.title.clone(),
                    ActivityKind::Started,
                    format!("revise {}", task.iteration + 2),
                );
            } else if step.on_exhaust == "fail" {
                terminal_finish(
                    conn,
                    shared,
                    task,
                    TaskStatus::Failed,
                    &format!(
                        "not passed after {} attempts — {feedback}",
                        step.max_iterations
                    ),
                );
            } else {
                park_awaiting(conn, shared, task);
            }
        }
    }
}

/// Move a task to its next step and immediately launch it (the task holds the lane).
fn advance_to(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    next_position: i64,
) {
    if let Err(err) = tasks::set_step(conn, &task.id, next_position) {
        log_line(format!("scheduler: advance {} failed: {err}", task.id));
    }
    if let Ok(Some(advanced)) = tasks::get(conn, &task.id) {
        launch_current(conn, &advanced);
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
    activity(
        shared,
        task.title.clone(),
        ActivityKind::Started,
        format!("step {}", next_position + 1),
    );
}

/// Finish a task terminally (done / failed / rejected) — or, recurrent + done, re-enter at the
/// next slot. Re-projects the lane, notifies the origin chat, and logs activity.
fn terminal_finish(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    status: TaskStatus,
    output: &str,
) {
    if let Some(next_run) = reschedule_at(task, status) {
        if let Err(err) = tasks::reschedule(conn, &task.id, &next_run, Some(output)) {
            log_line(format!("scheduler: reschedule {} failed: {err}", task.id));
        }
    } else if let Err(err) = tasks::finish(conn, &task.id, status, Some(output)) {
        log_line(format!("scheduler: finish {} failed: {err}", task.id));
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
    if let Some(chat) = &task.origin_chat {
        super::notify::completion(chat, &task.title, status.as_str(), output);
    }
    let (kind, detail) = if status == TaskStatus::Done {
        (ActivityKind::Finished, String::new())
    } else {
        (ActivityKind::Failed, output.to_owned())
    };
    activity(shared, task.title.clone(), kind, detail);
}

/// Park a task at a human gate (loop on-exhaust = human). Frees the lane; the review inbox
/// (F-3) lets the user resolve it.
fn park_awaiting(conn: &rusqlite::Connection, shared: &SharedServerState, task: &Task) {
    if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Awaiting) {
        log_line(format!("scheduler: park {} failed: {err}", task.id));
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
    activity(
        shared,
        task.title.clone(),
        ActivityKind::Started,
        "awaiting review".to_owned(),
    );
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
