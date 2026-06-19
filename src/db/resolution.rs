//! Resolve a chat to everything the provider needs to call its endpoint:
//! chat → agent group → endpoint row + model + key. Mirrors rust-claw's
//! `providers::resolution`, with each missing link a distinct error.

use rusqlite::Connection;

use super::agents;
use super::endpoints::{self, Endpoint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInference {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolutionError {
    #[error("chat {chat:?} is not linked to an agent")]
    NoAgent { chat: String },
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
    Ok(ResolvedInference {
        base_url: endpoint.base_url,
        model,
        api_key,
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
