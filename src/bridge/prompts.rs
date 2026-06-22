//! The Prompts inspector projection: assemble the 4-layer system prompt for a chosen agent +
//! run-kind, read-only (no scaffold, no side effects), so the operator can see exactly what
//! the model receives. Mirrors `run_system_prompt`'s layering without the file writes.

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::{self, agent_prompts, agents, workspaces};
use crate::state::{App, PromptPreview, RunKind};
use crate::workspace;

/// The role preamble for a run-kind — the same constants the live run sites pass.
fn role_preamble(run: RunKind) -> &'static str {
    match run {
        RunKind::Chat => agent::CONCIERGE_PROMPT,
        RunKind::Task => agent::EXECUTOR_PROMPT,
        RunKind::Planning => agent::OWNER_PLANNER_PROMPT,
    }
}

/// Compose and project the preview for `agent` (defaulting to the concierge) running as `run`,
/// using the agent's home/owned workspace as the representative brief source.
pub fn load(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    agent: Option<String>,
    run: RunKind,
) {
    let Some(agent_id) = resolve_agent(conn, agent) else {
        return;
    };
    let agent_name = agents::get(conn, &agent_id)
        .ok()
        .flatten()
        .map(|a| a.name)
        .unwrap_or_else(|| agent_id.clone());

    let role = role_preamble(run);
    let persona = agent_prompts::get(conn, &agent_id).ok().flatten();
    let workspace_id = workspaces::owned_by(conn, &agent_id).ok().flatten();
    let workspace_name = workspace_id
        .as_ref()
        .and_then(|id| workspaces::get(conn, id).ok().flatten())
        .map(|ws| ws.name)
        .unwrap_or_else(|| "(no workspace)".to_owned());
    let brief = workspace_id
        .as_ref()
        .map(|id| workspace::dir_for(&super::data_dir(), id))
        .and_then(|dir| workspace::read_agent_md(&dir));
    let mem = db::agent_memory::get(conn, &agent_id).unwrap_or_default();
    let memory = mem.text();

    let assembled = workspace::compose_system_prompt(
        role,
        persona.clone(),
        brief.clone(),
        memory.as_deref().map(super::wrap_memory),
    );
    let memory_meta = if mem.summary.trim().is_empty() {
        "no memory yet".to_owned()
    } else {
        format!(
            "{} chars · condensed through msg #{}",
            mem.summary.chars().count(),
            mem.summarized_through
        )
    };
    let preview = PromptPreview {
        agent_name,
        run: run.label().to_owned(),
        workspace: workspace_name,
        role: role.to_owned(),
        persona: persona.unwrap_or_default(),
        brief: brief.unwrap_or_default(),
        memory: mem.summary,
        assembled,
        memory_meta,
        memory_paused: mem.paused,
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_PROMPT_AGENT,
            App::FIELD_PROMPT_RUN,
            App::FIELD_PROMPT_PREVIEW,
        ]),
        move |app| {
            app.prompt_agent = Some(agent_id);
            app.prompt_run = run;
            app.prompt_preview = Some(preview);
        },
    );
}

/// The selected agent, else the concierge (web agent), else the first agent.
fn resolve_agent(conn: &rusqlite::Connection, agent: Option<String>) -> Option<String> {
    agent
        .filter(|id| !id.trim().is_empty())
        .or_else(|| agents::web_agent(conn).ok().flatten())
        .or_else(|| agents::first(conn).ok().flatten().map(|a| a.id))
}
