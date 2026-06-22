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

/// Start the owner's planning turn for a delegated request. Resolves the workspace owner,
/// builds its planning toolbox (over a private DB connection), and runs the turn on a worker
/// thread that posts `Command::AskPlanned` back.
pub fn ask(conn: &rusqlite::Connection, workspace: &str, request: &str, origin_chat: &str) {
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
    let system_prompt = run_system_prompt(
        agent::OWNER_PLANNER_PROMPT,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        super::memory_layer(conn, &inference.agent_id),
    );
    let brief = ChatMessage {
        role: MessageRole::User,
        text: format!(
            "A request was delegated to you to plan for this workspace.\n\nRequest: {request}\n\n\
             Break it into one or more tasks with create_task — assign a member by persona \
             where it helps, chain dependent steps with blockers, and set a schedule for \
             recurring work. Then confirm the plan in one or two sentences.",
        ),
        time: now_hm(),
    };
    let messages = agent::build_messages(&system_prompt, std::slice::from_ref(&brief));
    let toolbox = PlanningToolbox::new(planning_conn, workspace, Some(origin_chat.to_owned()));

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
        move |app| push_activity(app, title, kind, detail),
    );
}
