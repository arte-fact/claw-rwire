//! Delegation: a concierge `ask` becomes a workspace **owner's planning turn**. The owner runs
//! `run_turn` with the [`PlanningToolbox`] (which emits a reviewable **proposal**, R3 Option A),
//! on its own worker thread — the same shape as a chat turn or a task run. Approving the plan
//! materialises the proposal into real tasks the scheduler then drains.

use rwire::SharedServerState;

use crate::agent;
use crate::db::{self, resolution, workspaces};
use crate::state::{ActivityKind, App, ChatMessage, MessageRole};
use crate::tools::PlanningToolbox;
use crate::workspace;

use super::{Command, data_dir, log_line, memory, now_hm, push_activity, run_system_prompt, send};

/// Start a delegated request (R3, Option A). Always a reviewable **Plan** task: create the plan
/// gate, then run the owner's planning turn, which emits a structured **proposal** (no task rows
/// yet). The user approves to materialise it. A workspace with `auto_approve` set materialises the
/// proposal immediately (see [`planned`]) — and its per-task human sign-off gates also pass
/// automatically (see `scheduler::gate_action`), so the workspace runs hands-off end to end.
pub fn ask(conn: &rusqlite::Connection, workspace: &str, request: &str, origin_chat: &str) {
    let Ok(plan_id) = crate::id::new_id() else {
        return log_line("orchestrate: could not mint a plan id".to_owned());
    };
    if let Err(err) = crate::db::tasks::create_plan(
        conn,
        &plan_id,
        workspace,
        &plan_title(request),
        request,
        Some(origin_chat),
    ) {
        return log_line(format!("orchestrate: create plan failed: {err}"));
    }
    let auto_approve = workspaces::get(conn, workspace)
        .ok()
        .flatten()
        .is_some_and(|ws| ws.auto_approve);
    let brief = if auto_approve {
        direct_brief(request)
    } else {
        plan_brief(request)
    };
    launch_planning(conn, workspace, &plan_id, &brief, origin_chat);
}

/// Re-plan a sent-back plan: re-run the owner's planning turn into the same plan task with the
/// reviewer's feedback. The previous proposal was already cleared by the caller.
pub fn replan(conn: &rusqlite::Connection, plan: &crate::db::tasks::Task, feedback: &str) {
    let origin = plan.origin_chat.as_deref().unwrap_or("");
    launch_planning(
        conn,
        &plan.workspace_id,
        &plan.id,
        &revise_brief(&plan.spec, feedback),
        origin,
    );
}

/// Resolve the workspace owner and run its planning turn (the `brief` instruction) on a worker
/// thread, appending its proposal to `plan_id`. Posts `Command::AskPlanned` back.
fn launch_planning(
    conn: &rusqlite::Connection,
    workspace: &str,
    plan_id: &str,
    brief_text: &str,
    origin_chat: &str,
) {
    let owner = match workspaces::get(conn, workspace).ok().flatten() {
        Some(ws) => ws.owner,
        None => return log_line(format!("orchestrate: unknown workspace {workspace:?}")),
    };
    let Some(owner) = owner else {
        return log_line(format!(
            "orchestrate: workspace {workspace:?} has no owner to plan"
        ));
    };
    let inference =
        match resolution::resolve_for_agent(conn, &owner, workspace, |key| std::env::var(key).ok())
        {
            Ok(inference) => inference,
            Err(err) => return log_line(format!("orchestrate: resolve owner failed: {err}")),
        };
    // The planning turn writes cards through its own connection (the bridge owns `conn`).
    let planning_conn = match db::open(&data_dir().join("central.db")) {
        Ok(planning_conn) => planning_conn,
        Err(err) => return log_line(format!("orchestrate: open planning db failed: {err}")),
    };
    let workspace_dir = workspace::dir_for(&data_dir(), workspace);
    let role = super::role_prompt(conn, crate::state::RunKind::Planning);
    // Planning reads the same (agent, workspace) memory row as the owner's producer runs —
    // same memory, different hat: plans build on what shipped and what failed there. Budgeted
    // F→U→W→M like every other run; the planning unit is one brief, so memory gets real room.
    let row = db::agent_memory::get(conn, &inference.agent_id, Some(workspace)).unwrap_or_default();
    let memory_text = memory::fill_for_launch(
        &inference,
        crate::state::RunKind::Planning,
        &role,
        Some(&workspace_dir),
        brief_text.chars().count(),
        None,
        row.text().as_deref(),
    )
    .map(|text| memory::wrap_workspace_memory(&text));
    let system_prompt = run_system_prompt(
        &role,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        memory_text,
    );
    let brief = ChatMessage {
        role: MessageRole::User,
        text: brief_text.to_owned(),
        time: now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&brief));
    let toolbox = PlanningToolbox::new(planning_conn, workspace, Some(plan_id.to_owned()));

    let workspace = workspace.to_owned();
    let origin_chat = origin_chat.to_owned();
    let plan_id = plan_id.to_owned();
    std::thread::spawn(move || {
        let result = agent::run_turn(&inference, messages, &toolbox)
            .map(|turn| turn.reply.unwrap_or_default())
            .map_err(|err| err.to_string());
        send(Command::AskPlanned {
            workspace,
            origin_chat,
            plan_id,
            result,
        });
        // Keep the context-window cache warm from planning workers too (per-process cache).
        super::warm_context_cache(&inference);
    });
}

/// The owner's planning instruction for an auto-approve workspace — the proposal runs immediately
/// (no review gate), so it's a plain "break it into tasks and confirm".
fn direct_brief(request: &str) -> String {
    format!(
        "A request was delegated to you to plan for this workspace.\n\nRequest: {request}\n\n\
         Break it into one or more tasks and call propose_tasks once with the whole list — assign a \
         member by persona where it helps, chain dependent steps with blockers (by task title), and \
         set a schedule for recurring work. Then confirm the plan in one or two sentences."
    )
}

/// The owner's planning instruction for a fresh request.
fn plan_brief(request: &str) -> String {
    format!(
        "A request was delegated to you to plan for this workspace.\n\nRequest: {request}\n\n\
         Break it into one or more tasks and call propose_tasks once with the whole list — assign a \
         member by persona where it helps, chain dependent steps with blockers (by task title), and \
         set a schedule for recurring work. The proposal is reviewed and approved by the user before \
         anything runs. Then confirm the plan in one or two sentences."
    )
}

/// The owner's re-planning instruction after the user requested changes.
fn revise_brief(request: &str, feedback: &str) -> String {
    format!(
        "Your earlier plan for this request was sent back for changes.\n\nRequest: {request}\n\n\
         Reviewer feedback: {feedback}\n\nCall propose_tasks with a revised set of tasks addressing \
         the feedback, then confirm the plan in one or two sentences."
    )
}

/// The owner's planning turn finished. For an auto-approve workspace, materialise the proposal
/// immediately (hands-off); otherwise the plan stays parked at the review gate. Either way,
/// re-project the lane + the sidebar "needs you" badge and record the plan in the Activity log.
pub fn planned(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    origin_chat: &str,
    plan_id: &str,
    result: Result<String, String>,
) {
    let auto_approve = workspaces::get(conn, workspace)
        .ok()
        .flatten()
        .is_some_and(|ws| ws.auto_approve);
    if auto_approve && result.is_ok() {
        match db::tasks::proposal::materialize(conn, plan_id, Some(origin_chat)) {
            Ok(count) => {
                let _ = db::tasks::finish(
                    conn,
                    plan_id,
                    db::tasks::TaskStatus::Done,
                    Some("auto-approved"),
                );
                log_line(format!(
                    "orchestrate: auto-approved plan {plan_id} → {count} task(s)"
                ));
            }
            Err(err) => log_line(format!(
                "orchestrate: auto-materialize {plan_id} failed: {err}"
            )),
        }
    }
    super::tasks::project_if_active(conn, shared, workspace);
    // The plan parked at a human gate — refresh the sidebar "needs you" badge so it shows.
    super::review::project_awaiting(conn, shared);
    // A re-planned proposal must land in the *open* Peek too — without this, the operator who
    // just clicked "Request changes" stares at an empty plan forever (the live-repro bug).
    super::tasks::refresh_detail_if_open(conn, shared, plan_id);
    let _ = origin_chat; // the notify turn (C-3b) will speak into this thread
    let title = format!("planning · {workspace}");
    let (kind, detail) = match result {
        Ok(plan) => (ActivityKind::Finished, plan),
        Err(err) => {
            log_line(format!("orchestrate: planning {workspace:?} failed: {err}"));
            (ActivityKind::Failed, err)
        }
    };
    shared.update_shared_changed::<App>(
        rwire::ChangeSet::from_fields(&[App::FIELD_ACTIVITY]),
        move |app| push_activity(app, title, kind, detail, None, None),
    );
}

/// A short title for a plan task — `Plan: <first line of the request>`, capped.
fn plan_title(request: &str) -> String {
    let line = request.trim().lines().next().unwrap_or("request").trim();
    let capped: String = line.chars().take(60).collect();
    format!("Plan: {capped}")
}
