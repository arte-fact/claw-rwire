//! The Prompts inspector projection: assemble the 4-layer system prompt for a chosen agent +
//! run-kind, read-only (no scaffold, no side effects), so the operator can see exactly what
//! the model receives. Mirrors `run_system_prompt`'s layering without the file writes.

use rwire::{ChangeSet, SharedServerState};

use crate::db::{self, agent_prompts, agents, workspaces};
use crate::state::{App, PromptPreview, RoleDefaults, RunKind};
use crate::workspace;

/// Project the effective (override-or-default) role preambles for the defaults editor.
pub fn load_settings(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let defaults = RoleDefaults {
        concierge: super::role_prompt(conn, RunKind::Chat),
        executor: super::role_prompt(conn, RunKind::Task),
        planner: super::role_prompt(conn, RunKind::Planning),
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ROLE_DEFAULTS]),
        move |app| app.role_defaults = defaults,
    );
}

/// Set (or, with a blank value, reset) a role preamble override, then re-project the defaults.
pub fn set_role(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    run: RunKind,
    value: &str,
) {
    let key = match run {
        RunKind::Chat => super::ROLE_CONCIERGE_KEY,
        RunKind::Task => super::ROLE_EXECUTOR_KEY,
        RunKind::Planning => super::ROLE_PLANNER_KEY,
        RunKind::Validator => super::ROLE_VALIDATOR_KEY,
    };
    if let Err(err) = db::settings::set(conn, key, value) {
        super::log_line(format!("bridge: set role prompt failed: {err}"));
    }
    load_settings(conn, shared);
}

/// Compose and project the preview for `agent` (defaulting to the concierge) running as `run`,
/// using the agent's home/owned workspace as the representative brief source. The role layer
/// uses the operator's override (`settings`) so the preview matches the live run exactly.
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

    let role = super::role_prompt(conn, run);
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
        &role,
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
        role,
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
