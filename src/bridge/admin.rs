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
    let creating = id.is_none();
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
    // The identity prompt is operator-owned config (not a workspace file). On a blank *edit*
    // it clears (run falls back to base + brief); on a blank *create* we seed a default
    // specialist persona so a new agent is never personaless (it also feeds task auto-routing).
    // Workspace ownership + membership are managed on the workspace's Team tab, not here.
    let prompt = if creating && prompt.trim().is_empty() {
        agent_prompts::default_member_body(name)
    } else {
        prompt
    };
    if let Err(err) = agent_prompts::set(conn, &id, &prompt) {
        return surface_error(shared, format!("save agent prompt: {err}"));
    }
    project_registry(conn, shared);
}

/// Create a new workspace (the dashboard's create affordance), minting an id.
pub fn create_workspace(conn: &rusqlite::Connection, shared: &SharedServerState, name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return surface_error(shared, "workspace name can't be empty".to_owned());
    }
    match crate::id::new_id() {
        Ok(id) => {
            if let Err(err) = workspaces::create(conn, &id, name) {
                return surface_error(shared, format!("create workspace: {err}"));
            }
        }
        Err(err) => return surface_error(shared, format!("create workspace: {err}")),
    }
    project_registry(conn, shared);
}

/// Rename a workspace (its Team tab).
pub fn rename_workspace(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    name: &str,
) {
    let name = name.trim();
    if name.is_empty() {
        return surface_error(shared, "workspace name can't be empty".to_owned());
    }
    if let Err(err) = workspaces::rename(conn, id, name) {
        return surface_error(shared, format!("rename workspace: {err}"));
    }
    project_registry(conn, shared);
}

/// Set (or clear) a workspace's owner. Setting also makes the owner a member.
pub fn set_workspace_owner(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    owner: Option<String>,
) {
    let result = match owner.filter(|value| !value.trim().is_empty()) {
        Some(owner) => workspaces::set_owner(conn, id, &owner),
        None => workspaces::clear_owner(conn, id),
    };
    if let Err(err) = result {
        return surface_error(shared, format!("set workspace owner: {err}"));
    }
    project_registry(conn, shared);
}

/// Add a member to a workspace's team.
pub fn add_member(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str, agent: &str) {
    if let Err(err) = workspaces::add_member(conn, agent, id) {
        return surface_error(shared, format!("add member: {err}"));
    }
    project_registry(conn, shared);
}

/// Remove a member from a workspace's team. The owner can't be removed (it's the home
/// agent); clear or change the owner first.
pub fn remove_member(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    agent: &str,
) {
    if workspaces::get(conn, id)
        .ok()
        .flatten()
        .and_then(|w| w.owner)
        .as_deref()
        == Some(agent)
    {
        return surface_error(
            shared,
            "the owner is always a member — change the owner first".to_owned(),
        );
    }
    if let Err(err) = workspaces::remove_member(conn, agent, id) {
        return surface_error(shared, format!("remove member: {err}"));
    }
    project_registry(conn, shared);
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
