//! Resolve a chat to everything the provider needs to call its endpoint:
//! chat → agent group → endpoint row + model + key. Mirrors rust-claw's
//! `providers::resolution`, with each missing link a distinct error.

use rusqlite::Connection;

use super::endpoints::{self, Endpoint};
use super::{agent_prompts, agent_tools, agents, workspaces};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInference {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// The agent's display name — used to seed a fresh workspace brief.
    pub agent_name: String,
    /// The id of the agent's workspace (dir name under `<data>/workspaces/`), or `None`
    /// for a pure chat agent with no workspace. The worker resolves the dir for the coder
    /// tools + the `AGENT.md` project brief from this.
    pub workspace: Option<String>,
    /// The agent's identity prompt ("who am I"), or `None` if unset. Composed between the
    /// base prompt and the workspace brief.
    pub agent_prompt: Option<String>,
    /// The agent's enabled tool names, or `None` if it has never been configured (the
    /// run then applies its default selection). Gates which tools the model is offered.
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolutionError {
    #[error("chat {chat:?} is not linked to an agent")]
    NoAgent { chat: String },
    #[error("agent {0:?} does not exist")]
    UnknownAgent(String),
    #[error("agent {agent:?} has no endpoint configured")]
    NoEndpoint { agent: String },
    #[error("endpoint {0:?} does not exist")]
    UnknownEndpoint(String),
    #[error("agent {agent:?} has no model configured")]
    NoModel { agent: String },
    #[error("endpoint {endpoint:?} names api_key_env {var:?} but it is not set")]
    MissingKeyEnv { endpoint: String, var: String },
    #[error("db error: {0}")]
    Db(String),
}

/// Stored key wins; an `api_key_env` reference must resolve; keyless is valid (local
/// servers usually take no auth).
pub fn api_key_for(
    endpoint: &Endpoint,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, ResolutionError> {
    if let Some(key) = &endpoint.api_key {
        return Ok(Some(key.clone()));
    }
    match &endpoint.api_key_env {
        None => Ok(None),
        Some(var) => env(var)
            .map(Some)
            .ok_or_else(|| ResolutionError::MissingKeyEnv {
                endpoint: endpoint.name.clone(),
                var: var.clone(),
            }),
    }
}

pub fn resolve_for_chat(
    conn: &Connection,
    chat_id: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedInference, ResolutionError> {
    let agent = agents::agent_for_chat(conn, chat_id)
        .map_err(db)?
        .ok_or_else(|| ResolutionError::NoAgent {
            chat: chat_id.to_owned(),
        })?;
    // A plain chat turn runs in the agent's own workspace (its home) — the one it owns.
    let workspace = workspaces::owned_by(conn, &agent.id).map_err(db)?;
    inference_for(conn, &agent, workspace, env)
}

/// Resolve for a specific agent running in a specific workspace — a **task run**. The agent
/// supplies the endpoint / model / key / identity prompt / tools; the workspace is the
/// task's (not the agent's home), so a member runs in the workspace it was assigned to.
pub fn resolve_for_agent(
    conn: &Connection,
    agent_id: &str,
    workspace_id: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedInference, ResolutionError> {
    let agent = agents::get(conn, agent_id)
        .map_err(db)?
        .ok_or_else(|| ResolutionError::UnknownAgent(agent_id.to_owned()))?;
    inference_for(conn, &agent, Some(workspace_id.to_owned()), env)
}

/// Assemble the inference for an already-resolved agent and a chosen workspace: look up
/// its endpoint + model + key, identity prompt, and enabled tools.
fn inference_for(
    conn: &Connection,
    agent: &agents::AgentGroup,
    workspace: Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ResolvedInference, ResolutionError> {
    let endpoint_name = agent
        .endpoint
        .clone()
        .ok_or_else(|| ResolutionError::NoEndpoint {
            agent: agent.name.clone(),
        })?;
    let endpoint = endpoints::get(conn, &endpoint_name)
        .map_err(db)?
        .ok_or(ResolutionError::UnknownEndpoint(endpoint_name))?;
    let model = agent
        .model
        .clone()
        .ok_or_else(|| ResolutionError::NoModel {
            agent: agent.name.clone(),
        })?;
    let api_key = api_key_for(&endpoint, env)?;
    let tools = agent_tools::selection(conn, &agent.id).map_err(db)?;
    let agent_prompt = agent_prompts::get(conn, &agent.id).map_err(db)?;
    Ok(ResolvedInference {
        base_url: endpoint.base_url,
        model,
        api_key,
        agent_name: agent.name.clone(),
        workspace,
        agent_prompt,
        tools,
    })
}

fn db(err: rusqlite::Error) -> ResolutionError {
    ResolutionError::Db(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, chat, endpoints, open_in_memory};

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn seeded() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        endpoints::upsert(
            &conn,
            "default",
            "http://localhost:8000/v1",
            Some("sk"),
            None,
        )
        .expect("endpoint");
        let agent = agents::ensure_default(&conn, Some("default"), Some("gemma")).expect("agent");
        chat::create_chat(&conn, "chat-1", "New chat").expect("chat");
        agents::link(&conn, "chat-1", &agent.id).expect("link");
        conn
    }

    #[test]
    fn resolves_a_linked_chat() {
        let conn = seeded();
        let inf = resolve_for_chat(&conn, "chat-1", no_env).expect("resolve");
        assert_eq!(inf.base_url, "http://localhost:8000/v1");
        assert_eq!(inf.model, "gemma");
        assert_eq!(inf.api_key.as_deref(), Some("sk"));
        // No workspace assigned and no identity prompt set → both resolve to None.
        assert_eq!(inf.workspace, None);
        assert_eq!(inf.agent_prompt, None);
    }

    #[test]
    fn resolves_the_agents_workspace_and_identity_prompt() {
        use crate::db::{agent_prompts, agents, workspaces};
        let conn = seeded();
        let agent = agents::first(&conn).expect("agent").expect("some");
        workspaces::create(&conn, "ws-1", "Project").expect("ws");
        workspaces::set_owner(&conn, "ws-1", &agent.id).expect("own");
        agent_prompts::set(&conn, &agent.id, "You are precise.").expect("prompt");
        let inf = resolve_for_chat(&conn, "chat-1", no_env).expect("resolve");
        assert_eq!(inf.workspace.as_deref(), Some("ws-1"));
        assert_eq!(inf.agent_prompt.as_deref(), Some("You are precise."));
    }

    #[test]
    fn resolve_for_agent_uses_the_given_workspace_not_the_home() {
        use crate::db::workspaces;
        let conn = seeded();
        let agent = agents::first(&conn).expect("agent").expect("some");
        // The agent owns "home" but the task runs in "shared".
        workspaces::create(&conn, "home", "Home").expect("home");
        workspaces::set_owner(&conn, "home", &agent.id).expect("own");
        workspaces::create(&conn, "shared", "Shared").expect("shared");
        let inf = resolve_for_agent(&conn, &agent.id, "shared", no_env).expect("resolve");
        assert_eq!(inf.workspace.as_deref(), Some("shared"));
        assert_eq!(inf.model, "gemma");
    }

    #[test]
    fn resolve_for_agent_rejects_an_unknown_agent() {
        let conn = seeded();
        assert_eq!(
            resolve_for_agent(&conn, "ghost", "ws", no_env),
            Err(ResolutionError::UnknownAgent("ghost".to_owned()))
        );
    }

    #[test]
    fn unlinked_chat_has_no_agent() {
        let conn = open_in_memory().expect("db");
        chat::create_chat(&conn, "lonely", "New chat").expect("chat");
        assert_eq!(
            resolve_for_chat(&conn, "lonely", no_env),
            Err(ResolutionError::NoAgent {
                chat: "lonely".to_owned()
            })
        );
    }

    #[test]
    fn api_key_resolution_prefers_stored_then_env_then_keyless() {
        let stored = Endpoint {
            name: "a".to_owned(),
            base_url: String::new(),
            api_key: Some("sk-stored".to_owned()),
            api_key_env: Some("IGNORED".to_owned()),
        };
        assert_eq!(
            api_key_for(&stored, no_env).expect("ok"),
            Some("sk-stored".to_owned())
        );

        let env_backed = Endpoint {
            api_key: None,
            api_key_env: Some("OPENROUTER_API_KEY".to_owned()),
            ..stored.clone()
        };
        assert_eq!(
            api_key_for(&env_backed, |v| (v == "OPENROUTER_API_KEY")
                .then(|| "sk-env".to_owned()))
            .expect("ok"),
            Some("sk-env".to_owned())
        );
        assert_eq!(
            api_key_for(&env_backed, no_env),
            Err(ResolutionError::MissingKeyEnv {
                endpoint: "a".to_owned(),
                var: "OPENROUTER_API_KEY".to_owned()
            })
        );

        let keyless = Endpoint {
            api_key: None,
            api_key_env: None,
            ..stored
        };
        assert_eq!(api_key_for(&keyless, no_env).expect("ok"), None);
    }
}
