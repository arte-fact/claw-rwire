//! What happens after a run comes back: verdict routing, the human/plan gates, lane
//! advancement, terminal bookkeeping, and relaunch. Split from the scheduler's mod.rs —
//! everything here runs on the bridge thread and calls back into the launch fns via `super`.

use rwire::SharedServerState;

use crate::db::pipeline::{self, Step};
use crate::db::tasks::{self, Task, TaskStatus};
use crate::db::{task_runs, workspaces};
use crate::state::{ActivityKind, MessageRole};

use super::{Next, activity, launch_current, log_line, next_action, reschedule_at};

/// Route a validator's verdict (defaulting a missing verdict to `revise`, so a confused
/// reviewer is bounded by the iteration cap rather than passing silently). `by_human` is set when
/// a person resolved the gate from the board — their `revise` always re-runs the producer,
/// overriding the agent loop's exhausted cap (otherwise an escalated task would re-park forever
/// with no correction run).
pub(super) fn route_verdict(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    step: &Step,
    steps: &[Step],
    verdict: Option<(String, String)>,
    by_human: bool,
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
                &super::last_producer_output(conn, &task.id),
            ),
        },
        "reject" => terminal_finish(conn, shared, task, TaskStatus::Rejected, &feedback),
        _ => {
            // revise: re-run the producer. A human reviewer (gate step, or a person resolving an
            // escalation) is never capped — they decide when to stop; an agent validator is bounded
            // by `max_iterations`, then on-exhaust.
            if revise_reruns(
                by_human,
                step.validator_kind.as_deref(),
                task.iteration,
                step.max_iterations,
            ) {
                let _ = tasks::set_iteration(conn, &task.id, task.iteration + 1);
                if let Ok(Some(again)) = tasks::get(conn, &task.id) {
                    relaunch(conn, &again);
                }
                super::super::tasks::project_if_active(conn, shared, &task.workspace_id);
                super::super::tasks::refresh_detail_if_open(conn, shared, &task.id);
                activity(
                    shared,
                    task,
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
                // on_exhaust = human: escalate to a reviewer, saying *why* (the reviewer couldn't
                // pass it after N attempts, with the last feedback) so the gate isn't opaque.
                let reason = format!(
                    "escalated — not approved after {} review attempt(s): {}",
                    step.max_iterations,
                    if feedback.trim().is_empty() {
                        "no clear feedback from the reviewer"
                    } else {
                        feedback.trim()
                    }
                );
                park_awaiting(conn, shared, task, Some(&reason));
            }
        }
    }
}

/// What a finished loop producer should do at its step's validator boundary — a pure decision so
/// it can be unit-tested. An agent validator runs a review; a human validator parks for the inbox,
/// unless the workspace auto-approves (R3), in which case the gate passes automatically.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GateAction {
    LaunchAgentReview,
    ParkForHuman,
    AutoPass,
}

pub(super) fn gate_action(validator_kind: Option<&str>, auto_approve: bool) -> GateAction {
    match validator_kind {
        Some("human") if auto_approve => GateAction::AutoPass,
        Some("human") => GateAction::ParkForHuman,
        _ => GateAction::LaunchAgentReview,
    }
}

/// Whether a `revise` verdict should re-run the producer (vs. exhaust → on-exhaust). A pure
/// decision so it can be unit-tested. A person resolving the gate (`by_human`) or a human-validator
/// step is uncapped; an agent loop is bounded by `max_iterations`. The escalation bug was that a
/// human's `revise` on an exhausted *agent* loop hit neither uncapped case and so never re-ran.
pub(super) fn revise_reruns(
    by_human: bool,
    validator_kind: Option<&str>,
    iteration: i64,
    max_iterations: i64,
) -> bool {
    by_human || validator_kind == Some("human") || iteration + 1 < max_iterations
}

/// Whether the task's workspace auto-approves human gates (the R3 `auto_approve` flag).
pub(super) fn workspace_auto_approves(conn: &rusqlite::Connection, workspace_id: &str) -> bool {
    workspaces::get(conn, workspace_id)
        .ok()
        .flatten()
        .is_some_and(|ws| ws.auto_approve)
}

/// Auto-pass a human gate for an auto-approve workspace: record an `(auto) pass` validator run (so
/// the run chain shows the gate was auto-approved, not silently skipped), then route the loop
/// exactly like a human/agent `pass`. Mirrors [`human_verdict`]'s record-then-route.
pub(super) fn auto_pass_gate(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    step: &Step,
    steps: &[Step],
) {
    if let Ok(run_id) = crate::id::new_id() {
        let _ = task_runs::append(
            conn,
            &task_runs::NewRun {
                id: &run_id,
                task_id: &task.id,
                position: task.step_position,
                role: "validator",
                agent: None,
                output: Some("(auto) pass"),
                verdict: Some("pass"),
                feedback: Some("(auto-approved)"),
                iteration: task.iteration,
                ok: true,
                transcript: None,
                attempt: task.attempt,
                memory: None,
            },
        );
    }
    route_verdict(
        conn,
        shared,
        task,
        step,
        steps,
        Some(("pass".to_owned(), "(auto-approved)".to_owned())),
        false,
    );
}

/// Whether a task being (re)launched can claim the lane now, or must queue. A task resumed from a
/// human gate may find the lane already held by another task the scheduler started while it waited;
/// launching anyway would run two workers against the one shared workspace tree at once.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Dispatch {
    LaunchNow,
    Queue,
}

pub(super) fn dispatch_when(lane_busy: bool) -> Dispatch {
    if lane_busy {
        Dispatch::Queue
    } else {
        Dispatch::LaunchNow
    }
}

/// (Re)launch the task's current step if the lane is free, else leave it `pending` for the
/// scheduler to start once the lane frees — preserving one-running-task-per-workspace across the
/// review-gate resume path. The normal mid-pipeline advance sees only this task on the lane, so it
/// launches immediately as before.
pub(super) fn relaunch(conn: &rusqlite::Connection, task: &Task) {
    let lane_busy = tasks::other_running_in(conn, &task.workspace_id, &task.id)
        .ok()
        .flatten()
        .is_some();
    match dispatch_when(lane_busy) {
        Dispatch::LaunchNow => {
            if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Running) {
                log_line(format!("scheduler: claim {} failed: {err}", task.id));
            }
            launch_current(conn, task);
        }
        Dispatch::Queue => {
            if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Pending) {
                log_line(format!("scheduler: requeue {} failed: {err}", task.id));
            }
        }
    }
}

/// Move a task to its next step and (re)launch it. The task holds the lane in the normal flow; on
/// the human-gate resume path [`relaunch`] queues instead if another task grabbed the lane.
pub(super) fn advance_to(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    next_position: i64,
) {
    if let Err(err) = tasks::set_step(conn, &task.id, next_position) {
        log_line(format!("scheduler: advance {} failed: {err}", task.id));
    }
    if let Ok(Some(advanced)) = tasks::get(conn, &task.id) {
        relaunch(conn, &advanced);
    }
    super::super::tasks::project_if_active(conn, shared, &task.workspace_id);
    super::super::tasks::refresh_detail_if_open(conn, shared, &task.id);
    activity(
        shared,
        task,
        ActivityKind::Started,
        format!("step {}", next_position + 1),
    );
}

/// Finish a task terminally (done / failed / rejected) — or, recurrent + done, re-enter at the
/// next slot. Re-projects the lane, notifies the origin chat, and logs activity.
pub(super) fn terminal_finish(
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
    // The producer-side fold (R3): a completed task — the worker's natural turn boundary — is
    // condensed into its producing agents' (agent, workspace) memory. Failures fold too: what
    // broke and why is exactly what the next task must remember.
    super::memory::fold_finished_task(conn, task, status);
    // A terminal failure strands every task waiting on this one (the ready-scan needs each blocker
    // `done`) — cancel them so they don't sit `pending` forever, invisibly.
    if matches!(status, TaskStatus::Failed | TaskStatus::Rejected) {
        cancel_stranded_dependents(conn, shared, task, status);
    }
    super::super::tasks::project_if_active(conn, shared, &task.workspace_id);
    super::super::tasks::refresh_detail_if_open(conn, shared, &task.id);
    // A request task landing may complete the whole request — delivery v0 (G1) merges the
    // request branch to develop per repo and drops the receipt into the origin chat.
    super::super::requests::maybe_deliver(conn, task);
    if let Some(chat) = &task.origin_chat {
        super::super::notify::completion(chat, &task.title, status.as_str(), output);
    }
    let (kind, detail) = if status == TaskStatus::Done {
        (ActivityKind::Finished, String::new())
    } else {
        (ActivityKind::Failed, output.to_owned())
    };
    activity(shared, task, kind, detail);
}

/// Cancel the not-yet-started tasks stranded by `task`'s terminal failure, then surface it: one
/// Activity line and a cheap ping to the origin chat (not an LLM turn), so a dead dependency is
/// visible instead of a silently stuck `pending` card.
pub(super) fn cancel_stranded_dependents(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    status: TaskStatus,
) {
    let reason = format!(
        "cancelled — blocked by \"{}\" which ended {}",
        task.title,
        status.as_str()
    );
    match tasks::cancel_dependents(conn, &task.id, &reason) {
        Ok(ids) if !ids.is_empty() => {
            activity(
                shared,
                task,
                ActivityKind::Failed,
                format!("{} dependent task(s) cancelled", ids.len()),
            );
            if let Some(chat) = &task.origin_chat {
                let _ = crate::db::chat::append_message(
                    conn,
                    chat,
                    MessageRole::Agent,
                    &format!(
                        "⚠️ {} dependent task(s) were cancelled because **{}** ended {}.",
                        ids.len(),
                        task.title,
                        status.as_str()
                    ),
                );
            }
        }
        Ok(_) => {}
        Err(err) => log_line(format!("scheduler: cascade {} failed: {err}", task.id)),
    }
}

/// Park a task at a human gate (a human validator, or a loop's on-exhaust = human). Frees the
/// lane; the workspace board lets the user resolve it. `reason` is set on an **escalation** (the loop
/// exhausted its attempts) — it's recorded on the task and surfaced in the ping + activity so the
/// gate isn't a silent `awaiting` with no explanation.
pub(super) fn park_awaiting(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    reason: Option<&str>,
) {
    if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Awaiting) {
        log_line(format!("scheduler: park {} failed: {err}", task.id));
    }
    if let Some(reason) = reason {
        let _ = tasks::set_result(conn, &task.id, reason);
    }
    // Ping the origin chat (a cheap, always-works notice, not an LLM turn): on the first park, or
    // whenever a task escalates — an escalation can land at iteration > 0, which would otherwise
    // park silently.
    if (task.iteration == 0 || reason.is_some())
        && let Some(chat) = &task.origin_chat
    {
        let why = reason
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        let _ = crate::db::chat::append_message(
            conn,
            chat,
            MessageRole::Agent,
            &format!(
                "🔔 Task **{}** needs your review{why} — open its workspace board (the \
                 \"Needs you\" filter) to approve or send it back.",
                task.title
            ),
        );
    }
    super::super::tasks::project_if_active(conn, shared, &task.workspace_id);
    super::super::tasks::refresh_detail_if_open(conn, shared, &task.id);
    super::super::review::project_awaiting(conn, shared);
    activity(
        shared,
        task,
        ActivityKind::Started,
        reason.unwrap_or("awaiting review").to_owned(),
    );
}

/// Apply a human reviewer's verdict (from the board / Peek modal) to a task parked at a human
/// gate: record it as a validator run and drive the loop exactly like an agent verdict.
pub fn human_verdict(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task_id: &str,
    decision: &str,
    feedback: &str,
) {
    let Ok(Some(task)) = tasks::get(conn, task_id) else {
        return;
    };
    if task.status != TaskStatus::Awaiting {
        return; // not at a gate (already resolved) — ignore a stale click
    }
    // A reviewable Plan task (R3) gates its held children rather than a pipeline run.
    if task.is_plan {
        return plan_verdict(conn, shared, &task, decision, feedback);
    }
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let Some(step) = steps.get(task.step_position.max(0) as usize).cloned() else {
        return;
    };
    if let Ok(run_id) = crate::id::new_id() {
        let _ = task_runs::append(
            conn,
            &task_runs::NewRun {
                id: &run_id,
                task_id,
                position: task.step_position,
                role: "validator",
                agent: None, // the human
                output: Some(&format!("(you) {decision}")),
                verdict: Some(decision),
                feedback: Some(feedback),
                iteration: task.iteration,
                ok: true,
                transcript: None,
                attempt: task.attempt,
                memory: None,
            },
        );
    }
    // Route like an agent verdict, but flagged `by_human` so a `revise` always re-runs the producer
    // even past an agent loop's exhausted cap (the escalation case). The advance/revise paths go
    // through `relaunch`, which re-claims the lane (status → running) only if it's free — otherwise
    // it queues the task, so a resume never starts a second worker alongside one the scheduler
    // launched while this task was parked. (The pre-verdict `task` carries the right step/iteration.)
    route_verdict(
        conn,
        shared,
        &task,
        &step,
        &steps,
        Some((decision.to_owned(), feedback.to_owned())),
        true,
    );
    super::super::review::project_awaiting(conn, shared);
}

/// Apply a verdict to a reviewable **Plan** task (R3): approve releases its held children (they
/// start running), reject cancels them, and request-changes re-runs the owner's planning turn with
/// the feedback (re-proposing into the same plan).
pub(super) fn plan_verdict(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    decision: &str,
    feedback: &str,
) {
    match decision {
        "pass" => {
            // Approving a plan whose proposal is empty (mid-replan, or the planner never
            // proposed) must NOT close the gate — it would finish "done" having materialized
            // nothing, the silent "plan didn't land in the workspace" failure. Keep it parked.
            let proposed = tasks::proposal::get(conn, &task.id).unwrap_or_default();
            if proposed.is_empty() {
                log_line(format!(
                    "scheduler: plan {} approved with no proposal — kept at the gate",
                    task.id
                ));
                activity(
                    shared,
                    task,
                    ActivityKind::Started,
                    "no proposal to approve yet — still planning".to_owned(),
                );
            } else {
                // Approve (R3 Option A): materialise the proposal into real queued tasks now —
                // nothing existed on the board before this.
                match tasks::proposal::materialize(conn, &task.id, task.origin_chat.as_deref()) {
                    Ok(count) => log_line(format!("scheduler: plan {} → {count} task(s)", task.id)),
                    Err(err) => {
                        log_line(format!("scheduler: materialize {} failed: {err}", task.id))
                    }
                }
                let _ = tasks::finish(conn, &task.id, TaskStatus::Done, Some("approved"));
                // Approving the plan opens the Request (G1): mint its branch, go executing.
                super::super::requests::on_plan_approved(conn, task);
                activity(
                    shared,
                    task,
                    ActivityKind::Finished,
                    "plan approved".to_owned(),
                );
            }
        }
        "reject" => {
            // Nothing was created — just discard the proposal and close the gate. The
            // request closes with it: no branch was ever cut.
            let _ = tasks::proposal::clear(conn, &task.id);
            let _ = tasks::finish(conn, &task.id, TaskStatus::Rejected, Some(feedback));
            super::super::requests::on_plan_rejected(conn, task);
            activity(
                shared,
                task,
                ActivityKind::Failed,
                "plan rejected".to_owned(),
            );
        }
        _ => {
            // revise: discard the current proposal and re-plan with the feedback; the plan stays
            // at the gate for another review.
            let _ = tasks::proposal::clear(conn, &task.id);
            super::super::orchestrate::replan(conn, task, feedback);
            activity(
                shared,
                task,
                ActivityKind::Started,
                "re-planning".to_owned(),
            );
        }
    }
    super::super::tasks::project_if_active(conn, shared, &task.workspace_id);
    super::super::tasks::refresh_detail_if_open(conn, shared, &task.id);
    super::super::review::project_awaiting(conn, shared);
}
