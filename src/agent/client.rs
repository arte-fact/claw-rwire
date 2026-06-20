//! HTTP to the OpenAI-compatible endpoint: a one-round completion, the multi-round
//! tool loop on top of it, and model discovery. All blocking (`ureq`).

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::resolution::ResolvedInference;
use crate::tools::{ToolDef, Toolbox};

use super::{AgentError, ChatMessage, Completion, ToolCall, ToolInvocation, TurnResult};

/// Fail fast when the host is unreachable or the wrong scheme is configured.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-request deadline — generous for generation, not a silent hang.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
/// Cap on assistant↔tool round-trips per turn, so a confused model can't loop forever.
const MAX_TOOL_ROUNDS: usize = 6;
/// Whole-turn wall-clock budget across all rounds + tool calls. Once spent, the next
/// round is asked to answer in text (no tools) so the turn always terminates.
const RUN_BUDGET: Duration = Duration::from_secs(180);

/// Run a full turn: send the conversation with the toolbox's tools, execute any calls
/// the model makes, feed results back, and repeat until it answers in prose (or the
/// round/budget caps hit). Returns the tools it ran and the final reply.
pub fn run_turn(
    inf: &ResolvedInference,
    mut messages: Vec<ChatMessage>,
    toolbox: &dyn Toolbox,
) -> Result<TurnResult, AgentError> {
    let defs = toolbox.defs();
    let mut invocations = Vec::new();
    let started = Instant::now();

    for _ in 0..MAX_TOOL_ROUNDS {
        let within_budget = started.elapsed() < RUN_BUDGET;
        let tools: &[ToolDef] = if within_budget { &defs } else { &[] };
        let completion = complete_turn(inf, &messages, tools)?;

        if completion.tool_calls.is_empty() || !within_budget {
            return Ok(TurnResult {
                invocations,
                reply: completion.content,
            });
        }

        messages.push(ChatMessage::assistant_tool_calls(
            completion.tool_calls.clone(),
        ));
        for call in &completion.tool_calls {
            let outcome = toolbox.call(&call.function.name, &call.function.arguments);
            invocations.push(ToolInvocation {
                name: call.function.name.clone(),
                arguments: call.function.arguments.clone(),
                result: outcome.result.clone(),
                ok: outcome.ok,
            });
            messages.push(ChatMessage::tool_result(call.id.clone(), outcome.result));
        }
    }

    // Round cap reached with tools still pending — one final no-tools call for an answer.
    let completion = complete_turn(inf, &messages, &[])?;
    Ok(TurnResult {
        invocations,
        reply: completion.content,
    })
}

/// One model round. Sends `tools` only when non-empty (an endpoint without tool support
/// then sees a plain request). Returns prose and/or tool calls.
fn complete_turn(
    inf: &ResolvedInference,
    messages: &[ChatMessage],
    tools: &[ToolDef],
) -> Result<Completion, AgentError> {
    let url = format!("{}/chat/completions", inf.base_url.trim_end_matches('/'));
    let mut request = http().post(&url).set("Content-Type", "application/json");
    if let Some(key) = &inf.api_key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let mut body = json!({ "model": inf.model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(tool_wire).collect());
    }
    match request.send_json(body) {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|err| AgentError::Http(err.to_string()))?;
            parse_completion(&text)
        }
        Err(ureq::Error::Status(status, response)) => Err(AgentError::Api {
            status,
            body: truncate(&response.into_string().unwrap_or_default(), 500),
        }),
        Err(ureq::Error::Transport(transport)) => Err(AgentError::Http(transport.to_string())),
    }
}

/// Discover the model ids an endpoint advertises (`GET /v1/models`), sorted + deduped —
/// populates the System Map's agent model dropdown.
pub fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, AgentError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = http().get(&url);
    if let Some(key) = api_key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    match request.call() {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|err| AgentError::Http(err.to_string()))?;
            let parsed: ModelList = serde_json::from_str(&text)
                .map_err(|err| AgentError::Malformed(err.to_string()))?;
            let mut ids: Vec<String> = parsed.data.into_iter().map(|model| model.id).collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        }
        Err(ureq::Error::Status(status, response)) => Err(AgentError::Api {
            status,
            body: truncate(&response.into_string().unwrap_or_default(), 500),
        }),
        Err(ureq::Error::Transport(transport)) => Err(AgentError::Http(transport.to_string())),
    }
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout(RESPONSE_TIMEOUT)
        .build()
}

fn tool_wire(def: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": def.name,
            "description": def.description,
            "parameters": def.parameters,
        }
    })
}

fn parse_completion(body: &str) -> Result<Completion, AgentError> {
    let parsed: ChatResponse =
        serde_json::from_str(body).map_err(|err| AgentError::Malformed(err.to_string()))?;
    let message = parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message)
        .ok_or(AgentError::Empty)?;
    Ok(Completion {
        content: message
            .content
            .map(|content| content.trim().to_owned())
            .filter(|content| !content.is_empty()),
        tool_calls: message.tool_calls.unwrap_or_default(),
    })
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
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_content_and_rejects_empty() {
        let ok = r#"{"choices":[{"message":{"role":"assistant","content":"  hi  "}}]}"#;
        assert_eq!(parse_completion(ok).unwrap().content.as_deref(), Some("hi"));

        let empty = r#"{"choices":[{"message":{"content":""}}]}"#;
        assert_eq!(parse_completion(empty).unwrap().content, None);

        assert!(matches!(
            parse_completion("not json"),
            Err(AgentError::Malformed(_))
        ));
    }

    #[test]
    fn parse_extracts_tool_calls() {
        let body = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_1","type":"function","function":{"name":"search","arguments":"{\"query\":\"rust\"}"}}
        ]}}]}"#;
        let completion = parse_completion(body).unwrap();
        assert!(completion.content.is_none());
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].function.name, "search");
        assert_eq!(completion.tool_calls[0].id, "call_1");
    }
}
