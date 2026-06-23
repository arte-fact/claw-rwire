//! Delegation: a concierge `ask` becomes a workspace **owner's planning turn**. The owner
//! runs `run_turn` with the [`PlanningToolbox`] (which queues task cards), on its own worker
//! thread — the same shape as a chat turn or a task run. The scheduler then drains the cards.

use rwire::SharedServerState;

use crate::agent;
use crate::db::{self, resolution, workspaces};
use crate::state::{ActivityKind, App, ChatMessage, MessageRole};
use crate::tools::PlanningToolbox;
use crate::workspace;

use super::{Command, data_dir, log_line, now_hm, push_activity, run_system_prompt, send};

/// Start a delegated request as a reviewable **Plan** task (R3): create the plan gate task, then
/// run the owner's planning turn, which proposes tasks as held children of the plan.
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
    launch_planning(conn, workspace, &plan_id, &plan_brief(request), origin_chat);
}

/// Re-plan a rejected-for-revision plan: re-run the owner's planning turn into the same plan task
/// with the reviewer's feedback. The held children were already cleared by the caller.
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
/// thread, proposing tasks as held children of `plan_id`. Posts `Command::AskPlanned` back.
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
    let system_prompt = run_system_prompt(
        &role,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        super::memory_layer(conn, &inference.agent_id),
    );
    let brief = ChatMessage {
        role: MessageRole::User,
        text: brief_text.to_owned(),
        time: now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&brief));
    let toolbox = PlanningToolbox::new(
        planning_conn,
        workspace,
        Some(origin_chat.to_owned()),
        Some(plan_id.to_owned()),
    );

    let workspace = workspace.to_owned();
    let origin_chat = origin_chat.to_owned();
    std::thread::spawn(move || {
        let result = agent::run_turn(&inference, messages, &toolbox)
            .map(|turn| turn.reply.unwrap_or_default())
            .map_err(|err| err.to_string());
        send(Command::AskPlanned {
            workspace,
            origin_chat,
            result,
        });
    });
}

/// The owner's planning instruction for a fresh request.
fn plan_brief(request: &str) -> String {
    format!(
        "A request was delegated to you to plan for this workspace.\n\nRequest: {request}\n\n\
         Break it into one or more tasks with create_task — assign a member by persona where it \
         helps, chain dependent steps with blockers, and set a schedule for recurring work. The \
         tasks you create are a **proposal** the user reviews and approves before they run. Then \
         confirm the plan in one or two sentences."
    )
}

/// The owner's re-planning instruction after the user requested changes.
fn revise_brief(request: &str, feedback: &str) -> String {
    format!(
        "Your earlier plan for this request was sent back for changes.\n\nRequest: {request}\n\n\
         Reviewer feedback: {feedback}\n\nPropose a revised set of tasks with create_task \
         addressing the feedback, then confirm the plan in one or two sentences."
    )
}

/// The owner's planning turn finished: re-project the lane (so freshly queued cards show)
/// and record the plan in the Activity log. The per-task completion notice is the
/// scheduler's job; this marks the planning itself.
pub fn planned(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    workspace: &str,
    origin_chat: &str,
    result: Result<String, String>,
) {
    super::tasks::project_if_active(conn, shared, workspace);
    // The plan parked at a human gate — refresh the Review inbox + badge so it shows.
    super::review::project_count(conn, shared);
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
