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
        validator: super::role_prompt(conn, RunKind::Validator),
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
    resolver: Option<super::Resolver>,
) {
    let key = match run {
        RunKind::Chat => super::ROLE_CONCIERGE_KEY,
        RunKind::Task => super::ROLE_EXECUTOR_KEY,
        RunKind::Planning => super::ROLE_PLANNER_KEY,
        RunKind::Validator => super::ROLE_VALIDATOR_KEY,
    };
    let result = db::settings::set(conn, key, value);
    load_settings(conn, shared);
    match result {
        Ok(()) => super::resolve_ok(resolver),
        Err(err) => super::resolve_err(resolver, format!("could not save the role prompt: {err}")),
    }
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
    // The R3 injection matrix, mirrored so the preview matches the live run exactly: chat reads
    // the agent's global row, task/planning read the (representative) workspace's row, and a
    // validator run composes **no** memory layer at all.
    let (memory_applies, memory_scope) = match run {
        RunKind::Chat => (true, None),
        RunKind::Task | RunKind::Planning => (true, workspace_id.clone()),
        RunKind::Validator => (false, None),
    };
    let mem = if memory_applies {
        db::agent_memory::get(conn, &agent_id, memory_scope.as_deref()).unwrap_or_default()
    } else {
        db::agent_memory::Memory::default()
    };
    let memory = mem.text();

    let assembled = workspace::compose_system_prompt(
        &role,
        persona.clone(),
        brief.clone(),
        None,
        memory.as_deref().map(super::wrap_memory),
    );
    let memory_meta = if !memory_applies {
        "validators run memory-free — fresh eyes per deliverable".to_owned()
    } else if mem.summary.trim().is_empty() {
        format!("no memory yet · {}", scope_label(memory_scope.as_deref()))
    } else {
        format!(
            "{} chars · condensed through msg #{} · {}",
            mem.summary.chars().count(),
            mem.summarized_through,
            scope_label(memory_scope.as_deref())
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
        memory_scope,
        memory_applies,
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

/// A short human label for a memory scope — the panel's meta line.
fn scope_label(workspace: Option<&str>) -> String {
    match workspace {
        Some(id) => format!("scope: workspace {id}"),
        None => "scope: global".to_owned(),
    }
}

/// The selected agent, else the concierge (web agent), else the first agent.
fn resolve_agent(conn: &rusqlite::Connection, agent: Option<String>) -> Option<String> {
    agent
        .filter(|id| !id.trim().is_empty())
        .or_else(|| agents::web_agent(conn).ok().flatten())
        .or_else(|| agents::first(conn).ok().flatten().map(|a| a.id))
}
