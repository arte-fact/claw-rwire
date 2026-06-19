//! The native agent: turns a chat transcript into an OpenAI-compatible
//! `/chat/completions` request and returns the reply text. No tools, non-streaming —
//! the conversational core. The HTTP call is blocking (`ureq`), run on a worker
//! thread by the bridge so it never blocks the command loop.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::resolution::ResolvedInference;
use crate::state::{ChatMessage as Turn, MessageRole};

const TIMEOUT: Duration = Duration::from_secs(300);

/// The default system prompt until per-agent prompts exist.
pub const SYSTEM_PROMPT: &str = "You are claw, a concise and helpful personal assistant.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A wire message for the request body (`{role, content}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("request failed: {0}")]
    Http(String),
    #[error("endpoint returned {status}: {body}")]
    Api { status: u16, body: String },
    #[error("malformed response: {0}")]
    Malformed(String),
    #[error("the model returned an empty reply")]
    Empty,
}

/// Build the chat-completion message list: system prompt, then the transcript with
/// adjacent same-role turns coalesced and any trailing assistant turn dropped (an
/// endpoint rejects a list ending in the assistant's own message).
#[must_use]
pub fn build_messages(system: &str, turns: &[Turn]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(turns.len() + 1);
    if !system.is_empty() {
        messages.push(ChatMessage {
            role: Role::System,
            content: system.to_owned(),
        });
    }
    for turn in turns {
        let role = match turn.role {
            MessageRole::User => Role::User,
            MessageRole::Agent => Role::Assistant,
        };
        match messages.last_mut() {
            Some(last) if last.role == role => {
                last.content.push('\n');
                last.content.push_str(&turn.text);
            }
            _ => messages.push(ChatMessage {
                role,
                content: turn.text.clone(),
            }),
        }
    }
    while matches!(messages.last(), Some(message) if message.role == Role::Assistant) {
        messages.pop();
    }
    messages
}

/// Call the endpoint and return the assistant's reply text. Blocking.
pub fn complete(inf: &ResolvedInference, messages: &[ChatMessage]) -> Result<String, AgentError> {
    let url = format!("{}/chat/completions", inf.base_url.trim_end_matches('/'));
    let http = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let mut request = http.post(&url).set("Content-Type", "application/json");
    if let Some(key) = &inf.api_key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let body = serde_json::json!({ "model": inf.model, "messages": messages });
    match request.send_json(body) {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|err| AgentError::Http(err.to_string()))?;
            parse_reply(&text)
        }
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(AgentError::Api {
                status,
                body: truncate(&body, 500),
            })
        }
        Err(ureq::Error::Transport(transport)) => Err(AgentError::Http(transport.to_string())),
    }
}

fn parse_reply(body: &str) -> Result<String, AgentError> {
    let parsed: ChatResponse =
        serde_json::from_str(body).map_err(|err| AgentError::Malformed(err.to_string()))?;
    parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .map(|content| content.trim().to_owned())
        .filter(|content| !content.is_empty())
        .ok_or(AgentError::Empty)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: MessageRole, text: &str) -> Turn {
        Turn {
            role,
            text: text.to_owned(),
            time: "00:00".to_owned(),
        }
    }

    fn shape(messages: &[ChatMessage]) -> Vec<(Role, &str)> {
        messages
            .iter()
            .map(|m| (m.role, m.content.as_str()))
            .collect()
    }

    #[test]
    fn maps_roles_and_prepends_the_system_prompt() {
        let turns = [
            turn(MessageRole::User, "hi"),
            turn(MessageRole::Agent, "hello"),
            turn(MessageRole::User, "how are you?"),
        ];
        assert_eq!(
            shape(&build_messages("sys", &turns)),
            vec![
                (Role::System, "sys"),
                (Role::User, "hi"),
                (Role::Assistant, "hello"),
                (Role::User, "how are you?"),
            ]
        );
    }

    #[test]
    fn coalesces_adjacent_same_role_turns() {
        let turns = [
            turn(MessageRole::User, "one"),
            turn(MessageRole::User, "two"),
            turn(MessageRole::Agent, "ok"),
            turn(MessageRole::User, "three"),
        ];
        assert_eq!(
            shape(&build_messages("", &turns)),
            vec![
                (Role::User, "one\ntwo"),
                (Role::Assistant, "ok"),
                (Role::User, "three"),
            ]
        );
    }

    #[test]
    fn drops_a_trailing_assistant_turn() {
        let turns = [
            turn(MessageRole::User, "hi"),
            turn(MessageRole::Agent, "reply"),
        ];
        let messages = build_messages("sys", &turns);
        assert_eq!(
            shape(&messages),
            vec![(Role::System, "sys"), (Role::User, "hi")]
        );
        assert!(!matches!(messages.last(), Some(m) if m.role == Role::Assistant));
    }

    #[test]
    fn parse_reply_extracts_first_choice_and_rejects_empty() {
        let ok = r#"{"choices":[{"message":{"role":"assistant","content":"  hi  "}}]}"#;
        assert_eq!(parse_reply(ok).expect("content"), "hi");
        let empty = r#"{"choices":[{"message":{"content":""}}]}"#;
        assert!(matches!(parse_reply(empty), Err(AgentError::Empty)));
        assert!(matches!(
            parse_reply("not json"),
            Err(AgentError::Malformed(_))
        ));
    }
}
