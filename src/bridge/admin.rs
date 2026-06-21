//! System Map registry CRUD, run on the bridge thread (it owns the DB connection).
//! Each command writes `central.db` then re-projects the read-models; a failure (e.g.
//! deleting a still-referenced row) surfaces as the transient `App.error` banner
//! rather than crashing the bridge.

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::{agent_prompts, agent_tools, agents, endpoints, resolution, workspaces};
use crate::state::{AgentView, App, Editing, EndpointView, ToolCatalogEntry, WorkspaceView};
use crate::workspace;

/// Load the registry into the shared read-models. Called at boot and after each edit.
pub fn project_registry(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let endpoint_views: Vec<EndpointView> = endpoints::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|endpoint| EndpointView {
            name: endpoint.name,
            base_url: endpoint.base_url,
            has_key: endpoint.api_key.is_some(),
            api_key_env: endpoint.api_key_env,
        })
        .collect();
    let agent_views: Vec<AgentView> = agents::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|agent| AgentView {
            id: agent.id,
            name: agent.name,
            endpoint: agent.endpoint,
            model: agent.model,
        })
        .collect();
    let web_agent = agents::web_agent(conn).unwrap_or_default();
    let workspace_views: Vec<WorkspaceView> = workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|workspace| {
            let members = workspaces::members_of(conn, &workspace.id).unwrap_or_default();
            let member_names = workspaces::agents_in(conn, &workspace.id).unwrap_or_default();
            WorkspaceView {
                id: workspace.id,
                name: workspace.name,
                owner: workspace.owner,
                members,
                member_names,
            }
        })
        .collect();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_ENDPOINTS,
            App::FIELD_AGENTS,
            App::FIELD_WEB_AGENT,
            App::FIELD_WORKSPACES,
        ]),
        |app| {
            app.endpoints = endpoint_views;
            app.agents = agent_views;
            app.web_agent = web_agent;
            app.workspaces = workspace_views;
        },
    );
}

pub fn upsert_endpoint(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    name: &str,
    base_url: &str,
    api_key: Option<String>,
) {
    // Blank key on edit keeps the existing secret (the form never shows it); a new
    // value replaces it; a brand-new endpoint with no key is keyless.
    let api_key = match api_key.filter(|key| !key.trim().is_empty()) {
        Some(key) => Some(key),
        None => endpoints::get(conn, name)
            .ok()
            .flatten()
            .and_then(|existing| existing.api_key),
    };
    if let Err(err) = endpoints::upsert(conn, name, base_url, api_key.as_deref(), None) {
        return surface_error(shared, format!("save endpoint '{name}': {err}"));
    }
    project_registry(conn, shared);
}

pub fn delete_endpoint(conn: &rusqlite::Connection, shared: &SharedServerState, name: &str) {
    if let Err(err) = endpoints::delete(conn, name) {
        return surface_error(
            shared,
            format!("delete endpoint '{name}': {err} (an agent may still use it)"),
        );
    }
    project_registry(conn, shared);
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_agent(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: Option<String>,
    name: &str,
    endpoint: Option<String>,
    model: Option<String>,
    prompt: String,
    tools: Vec<String>,
) {
    let endpoint = endpoint.filter(|value| !value.trim().is_empty());
    let model = model.filter(|value| !value.trim().is_empty());
    // Create or update the row, then persist the tool selection and AGENT.md. `id` is the
    // existing id on edit, or a freshly minted one on create.
    let id = match &id {
        Some(id) => match agents::update(conn, id, name, endpoint.as_deref(), model.as_deref()) {
            Ok(()) => id.clone(),
            Err(err) => return surface_error(shared, format!("save agent '{name}': {err}")),
        },
        None => match crate::id::new_id() {
            Ok(new_id) => {
                if let Err(err) =
                    agents::create(conn, &new_id, name, endpoint.as_deref(), model.as_deref())
                {
                    return surface_error(shared, format!("save agent '{name}': {err}"));
                }
                new_id
            }
            Err(err) => return surface_error(shared, format!("create agent: {err}")),
        },
    };
    // Write a row for every catalog tool (enabled iff selected), so the agent is marked
    // "configured" and an all-off selection is honoured rather than treated as default.
    let catalog: Vec<(String, bool)> = super::tool_catalog()
        .into_iter()
        .map(|(_, tool)| {
            let enabled = tools.iter().any(|selected| selected == &tool);
            (tool, enabled)
        })
        .collect();
    if let Err(err) = agent_tools::set_tools(conn, &id, &catalog) {
        return surface_error(shared, format!("save agent tools: {err}"));
    }
    // The identity prompt is operator-owned config (not a workspace file); a blank one
    // clears it so the run falls back to the base prompt + the workspace brief. Workspace
    // ownership + membership are managed on the Workspaces tier, not here.
    if let Err(err) = agent_prompts::set(conn, &id, &prompt) {
        return surface_error(shared, format!("save agent prompt: {err}"));
    }
    project_registry(conn, shared);
}

/// Create a workspace (minting an id) or update an existing one — name, owner, and members.
/// The owner is always also a member; saving replaces the membership set with `members`
/// (plus the owner), so the Workspaces-tier drawer is the single source of truth for the team.
pub fn upsert_workspace(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: Option<String>,
    name: &str,
    owner: Option<String>,
    members: Vec<String>,
) {
    let name = name.trim();
    if name.is_empty() {
        return surface_error(shared, "workspace name can't be empty".to_owned());
    }
    let owner = owner.filter(|value| !value.trim().is_empty());
    let workspace_id = match &id {
        Some(id) => {
            if let Err(err) = workspaces::rename(conn, id, name) {
                return surface_error(shared, format!("save workspace: {err}"));
            }
            id.clone()
        }
        None => match crate::id::new_id() {
            Ok(new_id) => {
                if let Err(err) = workspaces::create(conn, &new_id, name) {
                    return surface_error(shared, format!("create workspace: {err}"));
                }
                new_id
            }
            Err(err) => return surface_error(shared, format!("create workspace: {err}")),
        },
    };
    if let Err(err) = set_team(conn, &workspace_id, owner.as_deref(), &members) {
        return surface_error(shared, format!("save workspace team: {err}"));
    }
    project_registry(conn, shared);
}

/// Replace a workspace's owner + membership. The owner (if any) is always a member; any
/// agent not in the new set is removed from the team.
fn set_team(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    owner: Option<&str>,
    members: &[String],
) -> rusqlite::Result<()> {
    if let Some(owner) = owner {
        workspaces::set_owner(conn, workspace_id, owner)?; // also adds owner as a member
    }
    for member in members {
        workspaces::add_member(conn, member, workspace_id)?;
    }
    // Drop anyone no longer on the team (never the owner — it's kept as a member above).
    let keep: std::collections::HashSet<&str> =
        members.iter().map(String::as_str).chain(owner).collect();
    for existing in workspaces::members_of(conn, workspace_id)? {
        if !keep.contains(existing.as_str()) {
            workspaces::remove_member(conn, &existing, workspace_id)?;
        }
    }
    Ok(())
}

/// Delete a workspace and its directory. Refused while agents are still assigned (they'd
/// be orphaned — FK enforcement is off), so the operator re-points them first.
pub fn delete_workspace(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    match workspaces::agents_in(conn, id) {
        Ok(agents) if !agents.is_empty() => {
            return surface_error(
                shared,
                format!(
                    "delete workspace: reassign its agent(s) first — {}",
                    agents.join(", ")
                ),
            );
        }
        Err(err) => return surface_error(shared, format!("delete workspace: {err}")),
        Ok(_) => {}
    }
    if let Err(err) = workspaces::delete(conn, id) {
        return surface_error(shared, format!("delete workspace: {err}"));
    }
    // Remove the workspace directory (best-effort; absence is fine — it may never have run).
    let dir = workspace::dir_for(&super::data_dir(), id);
    if let Err(err) = std::fs::remove_dir_all(&dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        super::log_line(format!(
            "admin: removing workspace dir '{id}' failed: {err}"
        ));
    }
    project_registry(conn, shared);
}

/// Read an agent's identity prompt into the open draft (mirrors `discover_models`). Called
/// when the drawer opens an existing agent, so the System-prompt textarea is prefilled.
/// (Workspace ownership/membership lives on the Workspaces tier, not the agent draft.)
pub fn load_agent_prompt(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let prompt = agent_prompts::get(conn, id)
        .ok()
        .flatten()
        .unwrap_or_default();
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_EDITING]), |app| {
        if let Some(Editing::Agent(draft)) = app.editing.as_mut()
            && draft.id.as_deref() == Some(id)
        {
            draft.prompt = prompt;
        }
    });
}

/// Read an agent's enabled tools into the open draft. An unconfigured agent defaults to
/// all web/MCP tools pre-checked (coder off), matching the run's default selection.
pub fn load_agent_tools(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let tools = match agent_tools::selection(conn, id) {
        Ok(Some(enabled)) => enabled,
        Ok(None) => super::tool_catalog()
            .into_iter()
            .filter(|(group, _)| group == "web")
            .map(|(_, name)| name)
            .collect(),
        Err(_) => return,
    };
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_EDITING]), |app| {
        if let Some(Editing::Agent(draft)) = app.editing.as_mut()
            && draft.id.as_deref() == Some(id)
        {
            draft.tools = tools;
        }
    });
}

/// Project the selectable-tool catalog (coder + connected web tools) into `App` once at
/// boot, so the agent drawer's tool picker has a source to render from.
pub fn project_tool_catalog(shared: &SharedServerState) {
    let catalog: Vec<ToolCatalogEntry> = super::tool_catalog()
        .into_iter()
        .map(|(group, name)| ToolCatalogEntry { group, name })
        .collect();
    shared
        .update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_TOOL_CATALOG]), |app| {
            app.tool_catalog = catalog
        });
}

pub fn delete_agent(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    if let Err(err) = agents::delete(conn, id) {
        return surface_error(
            shared,
            format!("delete agent: {err} (re-point the Web channel to another agent first)"),
        );
    }
    project_registry(conn, shared);
}

pub fn set_web_agent(conn: &rusqlite::Connection, shared: &SharedServerState, agent_id: &str) {
    if let Err(err) = agents::relink_web_chats(conn, agent_id) {
        return surface_error(shared, format!("route the Web channel: {err}"));
    }
    project_registry(conn, shared);
}

/// Fetch the endpoint's advertised models into `App.endpoint_models`. A lookup or
/// network failure just yields an empty list (the form falls back to free text) — a
/// missing model list shouldn't block editing an agent.
pub fn discover_models(conn: &rusqlite::Connection, shared: &SharedServerState, endpoint: &str) {
    let models = match endpoints::get(conn, endpoint) {
        Ok(Some(row)) => {
            let api_key = resolution::api_key_for(&row, |var| std::env::var(var).ok())
                .ok()
                .flatten();
            agent::list_models(&row.base_url, api_key.as_deref()).unwrap_or_default()
        }
        _ => Vec::new(),
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ENDPOINT_MODELS]),
        |app| app.endpoint_models = models,
    );
}

fn surface_error(shared: &SharedServerState, message: String) {
    crate::bridge::log_line(format!("bridge admin: {message}"));
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_ERROR]), |app| {
        app.error = Some(message);
    });
}
