//! HTTP to the OpenAI-compatible endpoint: a one-round completion, the multi-round
//! tool loop on top of it, and model discovery. All blocking (`ureq`).

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::resolution::ResolvedInference;
use crate::tools::{ToolDef, Toolbox, VERDICT};

use super::{
    AgentError, AgentEvent, ChatMessage, Completion, EventSink, FunctionCall, Role, ToolCall,
    ToolInvocation, TurnResult,
};

/// Fail fast when the host is unreachable or the wrong scheme is configured.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-request deadline — generous for generation, not a silent hang. 120s proved too tight
/// live: a validator reviewing a large deliverable on a loaded provider timed out mid-review
/// (ureq's `timeout` covers the whole response read, so it can abort a healthy stream).
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);
/// Cap on assistant↔tool round-trips per turn, so a confused model can't loop forever. Sized for
/// multi-step task work — a researcher searches/fetches, *writes* the report, then reads it back;
/// a builder writes files, then builds and tests; a reviewer lists/reads/greps the tree, runs the
/// build and tests, *then* calls `verdict`. At 6 a search-heavy agent hit the cap before writing
/// its file; at 10 the DB showed reviewers spending every round inspecting a multi-file crate and
/// hitting the cap *before* the `verdict` round — the forced no-tools answer carried no verdict, so
/// the loop defaulted to "revise" and never converged (tasks stuck `awaiting`). 20 gives thorough
/// reviews room to inspect **and** decide; `RUN_BUDGET` remains the wall-clock backstop.
const MAX_TOOL_ROUNDS: usize = 20;
/// Whole-turn wall-clock budget across all rounds + tool calls. Once spent, the next round is asked
/// to answer in text (no tools) so the turn always terminates. Raised alongside the round cap so a
/// legitimately long turn (more rounds, a slow `cargo build`/`test`) isn't cut off mid-work before
/// its rounds are used.
const RUN_BUDGET: Duration = Duration::from_secs(300);
/// Minimum gap between streamed-text flushes — coalesces tokens so the UI re-renders a few
/// times a second, not once per token.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Run a full turn: send the conversation with the toolbox's tools, execute any calls
/// the model makes, feed results back, and repeat until it answers in prose (or the
/// round/budget caps hit). Returns the tools it ran and the final reply.
pub fn run_turn(
    inf: &ResolvedInference,
    messages: Vec<ChatMessage>,
    toolbox: &dyn Toolbox,
) -> Result<TurnResult, AgentError> {
    run_loop(inf, messages, toolbox, &mut |_| {}, false, None)
}

/// As [`run_turn`], but streams progress through `sink` (tool calls as they run, the reply as
/// it arrives), plus **mid-run steering** (R5): `steer` is polled between tool
/// rounds; any strings it returns are injected as user turns before the next round, so an
/// operator note lands in seconds instead of at the next launch. The poller owns the
/// epoch/cancel discipline (a superseded run's poll must return nothing).
pub fn run_turn_steered(
    inf: &ResolvedInference,
    messages: Vec<ChatMessage>,
    toolbox: &dyn Toolbox,
    sink: EventSink,
    steer: &dyn Fn() -> Vec<String>,
) -> Result<TurnResult, AgentError> {
    run_loop(inf, messages, toolbox, sink, true, Some(steer))
}

/// The shared tool loop. `stream` selects the streaming completion (token-by-token text +
/// `Text` events) vs the plain blocking one; tool events fire either way (to a no-op sink for
/// non-streaming callers).
fn run_loop(
    inf: &ResolvedInference,
    mut messages: Vec<ChatMessage>,
    toolbox: &dyn Toolbox,
    sink: EventSink,
    stream: bool,
    steer: Option<&dyn Fn() -> Vec<String>>,
) -> Result<TurnResult, AgentError> {
    let defs = toolbox.defs();
    // The closing round is **reserved for `verdict`** when the toolbox advertises it (a validator
    // run): once the loop ends — round cap hit or wall-clock spent — that tool is the *only* one
    // offered, so a reviewer that spent every round inspecting still renders a decision instead of
    // being force-answered with none (which the scheduler reads as "no verdict" → a wasted revise).
    // A toolbox without `verdict` closes with no tools (a plain text answer), as before.
    let verdict_only: Vec<ToolDef> = defs.iter().filter(|d| d.name == VERDICT).cloned().collect();
    let mut invocations = Vec::new();
    let started = Instant::now();

    for round in 0..MAX_TOOL_ROUNDS {
        // Out of wall-clock: stop offering the full toolset and go straight to the closing round.
        if started.elapsed() >= RUN_BUDGET {
            break;
        }
        // Mid-run steering (R5): between rounds — never before the first, whose messages were
        // composed at launch — drain the operator's queue into the conversation.
        if round > 0
            && let Some(poll) = steer
        {
            for body in poll() {
                messages.push(ChatMessage::text(
                    Role::User,
                    format!("Message from the operator (steer, mid-run): {body}"),
                ));
            }
        }
        let completion = complete(inf, &messages, &defs, sink, stream)?;
        if completion.tool_calls.is_empty() {
            return Ok(TurnResult {
                invocations,
                reply: completion.content,
                prompt_tokens: completion.prompt_tokens,
            });
        }
        messages.push(ChatMessage::assistant_tool_calls(
            completion.tool_calls.clone(),
        ));
        run_calls(
            &completion.tool_calls,
            toolbox,
            &mut messages,
            &mut invocations,
            sink,
        );
    }

    // The reserved closing round: `verdict`-only for a validator, no tools for everyone else.
    // Execute any call it makes so `parse_verdict` sees the decision; terminate the turn either way.
    let completion = complete(inf, &messages, &verdict_only, sink, stream)?;
    if !completion.tool_calls.is_empty() {
        messages.push(ChatMessage::assistant_tool_calls(
            completion.tool_calls.clone(),
        ));
        run_calls(
            &completion.tool_calls,
            toolbox,
            &mut messages,
            &mut invocations,
            sink,
        );
    }
    Ok(TurnResult {
        invocations,
        reply: completion.content,
        prompt_tokens: completion.prompt_tokens,
    })
}

/// Execute one round's tool calls: fire progress events, run each through the toolbox, record the
/// invocation, and append its result to `messages` for the next round.
fn run_calls(
    calls: &[ToolCall],
    toolbox: &dyn Toolbox,
    messages: &mut Vec<ChatMessage>,
    invocations: &mut Vec<ToolInvocation>,
    sink: EventSink,
) {
    for call in calls {
        sink(AgentEvent::ToolStarted {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        });
        let outcome = toolbox.call(&call.function.name, &call.function.arguments);
        sink(AgentEvent::ToolFinished { ok: outcome.ok });
        invocations.push(ToolInvocation {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
            result: outcome.result.clone(),
            ok: outcome.ok,
        });
        messages.push(ChatMessage::tool_result(call.id.clone(), outcome.result));
    }
}

/// Pick the streaming or blocking completion for one round.
fn complete(
    inf: &ResolvedInference,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    sink: EventSink,
    stream: bool,
) -> Result<Completion, AgentError> {
    if stream {
        complete_turn_streamed(inf, messages, tools, sink)
    } else {
        complete_turn(inf, messages, tools)
    }
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

/// One model round, **streamed**: the same request with `stream: true`, parsed token-by-token
/// so reply text emits as `Text` events. Falls back to the blocking [`complete_turn`] if the
/// endpoint doesn't actually stream (no SSE data), so tasks never break on an odd server.
fn complete_turn_streamed(
    inf: &ResolvedInference,
    messages: &[ChatMessage],
    tools: &[ToolDef],
    sink: EventSink,
) -> Result<Completion, AgentError> {
    let url = format!("{}/chat/completions", inf.base_url.trim_end_matches('/'));
    let mut request = http().post(&url).set("Content-Type", "application/json");
    if let Some(key) = &inf.api_key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let mut body = json!({
        "model": inf.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(tool_wire).collect());
    }
    match request.send_json(body) {
        Ok(response) => {
            let (completion, saw_stream) = parse_stream(response.into_reader(), sink)?;
            if saw_stream {
                Ok(completion)
            } else {
                // The server ignored `stream` — do a normal blocking round instead.
                complete_turn(inf, messages, tools)
            }
        }
        Err(ureq::Error::Status(status, response)) => Err(AgentError::Api {
            status,
            body: truncate(&response.into_string().unwrap_or_default(), 500),
        }),
        Err(ureq::Error::Transport(transport)) => Err(AgentError::Http(transport.to_string())),
    }
}

/// Accumulate an SSE stream into a [`Completion`], emitting reply text as `Text` events (batched
/// ~[`FLUSH_INTERVAL`] to avoid render storms). The `bool` is whether it looked like a stream
/// at all (any `data:` line) — `false` signals the caller to fall back to blocking.
fn parse_stream(
    reader: impl std::io::Read,
    sink: EventSink,
) -> Result<(Completion, bool), AgentError> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(reader);
    let mut content = String::new();
    let mut tools: Vec<ToolAccum> = Vec::new();
    let mut prompt_tokens = None;
    let mut pending = String::new();
    let mut last_flush = Instant::now();
    let mut saw_stream = false;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| AgentError::Http(err.to_string()))?;
        if read == 0 {
            break;
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        saw_stream = true;
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) else {
            continue;
        };
        if let Some(usage) = chunk.usage {
            prompt_tokens = usage.prompt_tokens.or(prompt_tokens);
        }
        for choice in chunk.choices {
            if let Some(text) = choice.delta.content {
                content.push_str(&text);
                pending.push_str(&text);
                if last_flush.elapsed() >= FLUSH_INTERVAL {
                    sink(AgentEvent::Text(std::mem::take(&mut pending)));
                    last_flush = Instant::now();
                }
            }
            for delta in choice.delta.tool_calls.unwrap_or_default() {
                let slot = tool_slot(&mut tools, delta.index);
                if let Some(id) = delta.id.filter(|id| !id.is_empty()) {
                    slot.id = id;
                }
                if let Some(function) = delta.function {
                    if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                        slot.name = name;
                    }
                    if let Some(arguments) = function.arguments {
                        slot.arguments.push_str(&arguments);
                    }
                }
            }
        }
    }
    if !pending.is_empty() {
        sink(AgentEvent::Text(pending));
    }
    let content = {
        let trimmed = content.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    let tool_calls: Vec<ToolCall> = tools
        .into_iter()
        .enumerate()
        .filter(|(_, accum)| !accum.name.is_empty())
        .map(|(index, accum)| ToolCall {
            id: if accum.id.is_empty() {
                format!("call_{index}")
            } else {
                accum.id
            },
            kind: "function".to_owned(),
            function: FunctionCall {
                name: accum.name,
                arguments: accum.arguments,
            },
        })
        .collect();
    // Same leaked-tool-call recovery as the blocking path (deepseek markup in the content).
    if tool_calls.is_empty()
        && let Some(text) = &content
    {
        let recovered = recover_leaked_tool_calls(text);
        if !recovered.is_empty() {
            return Ok((
                Completion {
                    content: leading_prose(text),
                    tool_calls: recovered,
                    prompt_tokens,
                },
                saw_stream,
            ));
        }
    }
    Ok((
        Completion {
            content,
            tool_calls,
            prompt_tokens,
        },
        saw_stream,
    ))
}

/// Grow `tools` to cover `index` and return the slot to accumulate into.
fn tool_slot(tools: &mut Vec<ToolAccum>, index: usize) -> &mut ToolAccum {
    if index >= tools.len() {
        tools.resize_with(index + 1, ToolAccum::default);
    }
    &mut tools[index]
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
    let content = message
        .content
        .map(|content| content.trim().to_owned())
        .filter(|content| !content.is_empty());
    let prompt_tokens = parsed.usage.and_then(|usage| usage.prompt_tokens);
    let tool_calls = message.tool_calls.unwrap_or_default();
    // Some models emit tool calls as *text* in the content instead of the structured
    // `tool_calls` field (e.g. deepseek's `…invoke name="x"…` markup). When the structured
    // field is empty, recover them from the content so the loop actually runs them rather
    // than leaking the markup to the user as a final answer.
    if tool_calls.is_empty()
        && let Some(text) = &content
    {
        let recovered = recover_leaked_tool_calls(text);
        if !recovered.is_empty() {
            return Ok(Completion {
                content: leading_prose(text),
                tool_calls: recovered,
                prompt_tokens,
            });
        }
    }
    Ok(Completion {
        content,
        tool_calls,
        prompt_tokens,
    })
}

/// The model's context window (max tokens) from `GET /models`, if the server advertises it —
/// OpenRouter and some others expose `context_length` (or `top_provider.context_length`) as an
/// extension; base OpenAI does not. `None` when the model isn't found or no length is given.
pub fn context_length(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> Result<Option<u32>, AgentError> {
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
            Ok(parsed
                .data
                .into_iter()
                .find(|entry| entry.id == model)
                .and_then(|entry| entry.context_length()))
        }
        Err(ureq::Error::Status(status, response)) => Err(AgentError::Api {
            status,
            body: truncate(&response.into_string().unwrap_or_default(), 500),
        }),
        Err(ureq::Error::Transport(transport)) => Err(AgentError::Http(transport.to_string())),
    }
}

/// Recover tool calls a model serialised into assistant text instead of the `tool_calls`
/// field: the `…invoke name="x"…parameter name="k"…>v</…` markup (deepseek "DSML" and the
/// Anthropic-style `<invoke>` form) and `<tool_call>{json}</tool_call>` blocks.
fn recover_leaked_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = recover_invoke_markup(content);
    calls.extend(recover_tool_call_json(content));
    for (index, call) in calls.iter_mut().enumerate() {
        call.id = format!("recovered_{index}");
    }
    calls
}

/// Parse `invoke name="NAME"` blocks, each with `parameter name="KEY" …>VALUE</…` children,
/// into tool calls. Delimiter-agnostic — keys on the `invoke`/`parameter` markers, so it
/// works across the `<｜｜DSML｜｜…>` and plain `<invoke>` variants.
fn recover_invoke_markup(content: &str) -> Vec<ToolCall> {
    const MARK: &str = "invoke name=\"";
    let starts: Vec<usize> = content.match_indices(MARK).map(|(i, _)| i).collect();
    let mut calls = Vec::new();
    for (idx, &start) in starts.iter().enumerate() {
        let name_at = start + MARK.len();
        let Some(quote) = content[name_at..].find('"') else {
            continue;
        };
        let name = content[name_at..name_at + quote].trim();
        if name.is_empty() {
            continue;
        }
        let block_end = starts.get(idx + 1).copied().unwrap_or(content.len());
        let block = &content[name_at + quote..block_end];
        calls.push(ToolCall {
            id: String::new(),
            kind: "function".to_owned(),
            function: super::FunctionCall {
                name: name.to_owned(),
                arguments: recover_params(block),
            },
        });
    }
    calls
}

/// Pull `parameter name="KEY" …>VALUE</…` pairs out of one invoke block into a JSON object.
/// Values are taken between the parameter tag's `>` and the next `<` — fine for the paths /
/// urls / strings these calls carry.
fn recover_params(block: &str) -> String {
    const MARK: &str = "parameter name=\"";
    let mut obj = serde_json::Map::new();
    let mut rest = block;
    while let Some(pos) = rest.find(MARK) {
        let after = &rest[pos + MARK.len()..];
        let Some(quote) = after.find('"') else {
            break;
        };
        let key = after[..quote].to_owned();
        let tail = &after[quote..];
        let Some(gt) = tail.find('>') else {
            break;
        };
        let value_region = &tail[gt + 1..];
        let value_end = value_region.find('<').unwrap_or(value_region.len());
        obj.insert(
            key,
            Value::String(value_region[..value_end].trim().to_owned()),
        );
        rest = &value_region[value_end..];
    }
    Value::Object(obj).to_string()
}

/// Parse `<tool_call>{ "name": …, "arguments": … }</tool_call>` blocks (Hermes/Qwen style).
fn recover_tool_call_json(content: &str) -> Vec<ToolCall> {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut calls = Vec::new();
    let mut rest = content;
    while let Some(open) = rest.find(OPEN) {
        let after = &rest[open + OPEN.len()..];
        let Some(close) = after.find(CLOSE) else {
            break;
        };
        if let Ok(value) = serde_json::from_str::<Value>(after[..close].trim())
            && let Some(name) = value.get("name").and_then(Value::as_str)
        {
            let arguments = value
                .get("arguments")
                .or_else(|| value.get("parameters"))
                .map(|args| {
                    args.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| args.to_string())
                })
                .unwrap_or_else(|| "{}".to_owned());
            calls.push(ToolCall {
                id: String::new(),
                kind: "function".to_owned(),
                function: super::FunctionCall {
                    name: name.to_owned(),
                    arguments,
                },
            });
        }
        rest = &after[close..];
    }
    calls
}

/// The prose before any leaked tool-call markup, so a recovered turn keeps the model's
/// preamble (if any) and drops the machinery. `None` if nothing meaningful precedes it.
fn leading_prose(content: &str) -> Option<String> {
    let cut = ["invoke name=\"", "<tool_call>", "<｜｜", "<tool_calls"]
        .iter()
        .filter_map(|marker| content.find(marker))
        .min()
        .unwrap_or(content.len());
    let prose = content[..cut]
        .trim_end_matches(['<', '\n', ' ', '\t'])
        .trim();
    (!prose.is_empty()).then(|| prose.to_owned())
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
    #[serde(default)]
    usage: Option<Usage>,
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

/// The OpenAI-compatible token accounting block. Fields are optional — some servers omit it.
#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
}

// ── Streaming (SSE) deltas ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FnDelta>,
}

#[derive(Deserialize, Default)]
struct FnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Accumulates a streamed tool call across its deltas (name first, arguments in pieces).
#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    /// OpenRouter-style top-level context window.
    #[serde(default)]
    context_length: Option<u32>,
    /// Some providers nest it under `top_provider.context_length` instead.
    #[serde(default)]
    top_provider: Option<TopProvider>,
}

impl ModelEntry {
    /// The context window, preferring the top-level field, then the nested provider field.
    fn context_length(&self) -> Option<u32> {
        self.context_length
            .or_else(|| self.top_provider.as_ref().and_then(|tp| tp.context_length))
    }
}

#[derive(Deserialize)]
struct TopProvider {
    #[serde(default)]
    context_length: Option<u32>,
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

    #[test]
    fn recovers_deepseek_dsml_tool_calls_leaked_into_content() {
        // The exact shape deepseek-v4-flash emitted in the real session (fullwidth `｜`).
        let leaked = "Now let me read the files:\n\
            <｜｜DSML｜｜tool_calls>\n\
            <｜｜DSML｜｜invoke name=\"read\">\n\
            <｜｜DSML｜｜parameter name=\"path\" string=\"true\">projects/isola/src/main.rs</｜｜DSML｜｜parameter>\n\
            </｜｜DSML｜｜invoke>\n\
            <｜｜DSML｜｜invoke name=\"fetch\">\n\
            <｜｜DSML｜｜parameter name=\"url\" string=\"true\">https://example.com/x.rs</｜｜DSML｜｜parameter>\n\
            </｜｜DSML｜｜invoke>\n\
            </｜｜DSML｜｜tool_calls>";
        let calls = recover_leaked_tool_calls(leaked);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "read");
        assert_eq!(
            calls[0].function.arguments,
            r#"{"path":"projects/isola/src/main.rs"}"#
        );
        assert_eq!(calls[1].function.name, "fetch");
        assert_eq!(
            calls[1].function.arguments,
            r#"{"url":"https://example.com/x.rs"}"#
        );
        // Recovered calls get synthetic, distinct ids.
        assert_eq!(calls[0].id, "recovered_0");
        assert_eq!(calls[1].id, "recovered_1");
    }

    #[test]
    fn parse_completion_recovers_leaked_calls_and_keeps_preamble() {
        let body = "{\"choices\":[{\"message\":{\"content\":\"Reading it now:\\n\
            <｜｜DSML｜｜invoke name=\\\"read\\\">\
            <｜｜DSML｜｜parameter name=\\\"path\\\" string=\\\"true\\\">a.txt</｜｜DSML｜｜parameter>\
            </｜｜DSML｜｜invoke>\"}}]}";
        let completion = parse_completion(body).unwrap();
        assert_eq!(
            completion.tool_calls.len(),
            1,
            "leaked call must be recovered"
        );
        assert_eq!(completion.tool_calls[0].function.name, "read");
        assert_eq!(completion.content.as_deref(), Some("Reading it now:"));
    }

    #[test]
    fn recovers_hermes_style_tool_call_json() {
        let leaked = r#"<tool_call>{"name": "write", "arguments": {"path": "x.md", "content": "hi"}}</tool_call>"#;
        let calls = recover_leaked_tool_calls(leaked);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write");
        assert!(calls[0].function.arguments.contains("\"path\":\"x.md\""));
    }

    #[test]
    fn ordinary_prose_is_not_mistaken_for_a_tool_call() {
        let calls = recover_leaked_tool_calls("Here is a normal answer with no tool markup.");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_completion_reads_prompt_tokens_from_usage() {
        let body = r#"{"choices":[{"message":{"content":"hi"}}],
            "usage":{"prompt_tokens":1234,"completion_tokens":7,"total_tokens":1241}}"#;
        assert_eq!(parse_completion(body).unwrap().prompt_tokens, Some(1234));
        // Missing usage → None (turn-count fallback territory).
        let no_usage = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        assert_eq!(parse_completion(no_usage).unwrap().prompt_tokens, None);
    }

    #[test]
    fn model_entry_context_length_prefers_top_level_then_nested() {
        let list: ModelList = serde_json::from_str(
            r#"{"data":[
                {"id":"a","context_length":128000},
                {"id":"b","top_provider":{"context_length":32768}},
                {"id":"c"}
            ]}"#,
        )
        .unwrap();
        let by_id = |id: &str| {
            list.data
                .iter()
                .find(|m| m.id == id)
                .unwrap()
                .context_length()
        };
        assert_eq!(by_id("a"), Some(128000));
        assert_eq!(by_id("b"), Some(32768), "falls back to top_provider");
        assert_eq!(by_id("c"), None, "no context advertised");
    }
}
