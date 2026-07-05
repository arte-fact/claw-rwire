//! The native agent: turns a chat transcript into OpenAI-compatible
//! `/chat/completions` requests and loops over tool calls until the model answers in
//! text. Blocking (`ureq`), run on a worker thread by the bridge so it never blocks
//! the command loop. This module holds the wire types + message building; the HTTP and
//! the tool loop live in [`client`].

mod client;

pub use client::{context_length, list_models, run_turn, run_turn_steered};

use serde::{Deserialize, Serialize};

use crate::state::{ChatMessage as Turn, MessageRole};

/// The default system prompt until per-agent prompts exist.
pub const SYSTEM_PROMPT: &str = "You are claw, a concise and helpful personal assistant. \
Use your tools when they help you answer accurately.";

/// Role preamble for a **chat (concierge) turn** — the user's single point of contact.
/// Layered above the agent's identity prompt + workspace brief. Keeps the concierge from
/// doing workspace work itself or fabricating the result of delegated tasks (the failure
/// mode where it claimed a task was done and invented file contents fetched over the web).
pub const CONCIERGE_PROMPT: &str = "You are the concierge: the user's single point of \
contact. Answer quick questions directly. For anything that touches a workspace's files, \
runs multiple steps, or belongs to a project, DELEGATE it with the `ask` tool to the right \
workspace — do not attempt the file work yourself. Delegated work runs asynchronously on a \
task board: never claim a delegated task is finished and never invent its result; you will \
be notified when it completes. Call tools using the provided tool-call mechanism, never by \
writing the call out as text.";

/// Role preamble for a **task execution turn** — an agent running an assigned task inside its
/// workspace directory. It must actually use its file tools, not just describe the change, and
/// verify its own work before claiming done (the failure modes seen in live runs: claiming a file
/// was written without writing it, shipping code that doesn't compile, and editing other tasks'
/// files).
pub const EXECUTOR_PROMPT: &str = "You are executing an assigned task inside this workspace's \
directory. You have real file tools — use `write`/`edit`/`read`/`bash` to make the change on \
disk; do not merely describe what should be done or print file contents for the user to copy. \
Stay within the scope of this task: change only the files it calls for, and don't rewrite work \
that belongs to other tasks. Paths are resolved from the workspace root, so keep a code project's \
files together under `projects/<project-name>/` (e.g. `projects/foo/src/lib.rs`) — never write \
source files like `src/lib.rs` at the workspace root, even if the task spec names a bare path. \
Before reporting done, verify your own work — if the project has a \
build or tests (e.g. `cargo build`/`cargo test`), run them and make sure they pass; never claim a \
file was written or a step finished without having actually done it. When finished, state briefly \
what you changed and that it builds and tests clean. Work only within the workspace. Call tools \
using the provided tool-call mechanism, never by writing the call out as text.";

/// Role preamble for a **validation turn** — a reviewer judging a producer's output inside a
/// loop step. It reads (read-only) and then issues a structured verdict via the `verdict` tool.
/// Hardened against weak-model failure modes seen live: reading files at wrong paths, passing code
/// that never compiled, and stating the verdict in prose instead of calling the tool.
pub const VALIDATOR_PROMPT: &str = "You are reviewing completed work against its task spec. Read \
the actual files it produced — they live under this workspace's directory, so read them at their \
real workspace-relative paths; if a read fails, fix the path and try again rather than guessing or \
giving up. Judge correctness, completeness, and adherence to the workspace's conventions, and \
check that the work actually builds: look for evidence the producer ran the project's build and \
tests and they passed — if there's no such evidence, or it shows failures, that's a `revise`, not \
a `pass`. Always record your decision by calling the `verdict` tool exactly once: `pass` if it \
meets the spec, `revise` with concrete, actionable feedback if it needs another pass, or `reject` ONLY \
when the task itself is impossible or the work must be thrown away entirely — a reject ends the \
task and cancels everything depending on it, so a fixable flaw (a wrong name, a missing wire-up, \
a failing test) is ALWAYS `revise`, never `reject`. When in doubt, revise. State your decision \
through the tool, never only in prose. Be specific and fair; don't rewrite the work yourself.";

/// Role preamble for an **owner planning turn** — turning a delegated request into task cards.
/// The owner plans; it does not do the work in this turn.
pub const OWNER_PLANNER_PROMPT: &str = "You are the owner of this workspace, planning a \
delegated request. Break it into one or more concrete tasks with the `create_task` tool: \
assign each to the member whose persona best fits (omit to keep it yourself), chain dependent \
steps with blockers, and set a cron schedule for anything recurring. When the tasks build a \
code project, give file paths in each spec under a shared `projects/<project-name>/` directory \
(e.g. `projects/foo/src/lib.rs`), not bare `src/...` — every step's tools resolve paths from the \
workspace root, so a bare `src/…` would scatter files there instead of inside the project. Do \
not do the work in this turn — only plan it. Then confirm the plan in one or two sentences. Call \
tools using the provided tool-call mechanism, never by writing the call out as text.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// The function half of a `tool_calls` entry — name plus a JSON-string argument blob,
/// exactly as the OpenAI wire format carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// One tool call the model requested. Round-trips the wire shape both directions
/// (parsed from a response, echoed back in the next request).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".to_owned()
}

/// A wire message for the request body. `content` is absent for an assistant turn that
/// only calls tools; `tool_calls` carries those calls; `tool_call_id` ties a `tool`
/// result back to its call.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::text(Role::System, content)
    }

    /// An assistant turn that only requested tools (no prose).
    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: None,
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    /// A tool's result, addressed to the call that produced it.
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
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

/// One tool the agent ran this turn — persisted as a card and (flattened) replayed to
/// the model on later turns.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub name: String,
    pub arguments: String,
    pub result: String,
    pub ok: bool,
}

/// A live event emitted *during* a turn, so the UI can show real-time agent activity rather
/// than only the final result. `run_turn` calls the [`EventSink`] with these as they happen.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A tool call is about to run (`arguments` is the raw JSON the model sent).
    ToolStarted { name: String, arguments: String },
    /// The current tool call finished (matched to the in-flight one for the run).
    ToolFinished { ok: bool },
    /// A chunk of streamed reply text (Tier 2 — emitted only on the streaming path).
    Text(String),
}

/// The sink `run_turn` calls with live events. Pass a no-op closure (`&mut |_| {}`) to disable.
pub type EventSink<'a> = &'a mut dyn FnMut(AgentEvent);

/// The outcome of a full turn: the tools it ran (in order), the final reply text, and the
/// `prompt_tokens` the endpoint reported for the final request — the real context size, used
/// to decide when to fold the thread into rolling memory. `None` when the server omits usage.
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub invocations: Vec<ToolInvocation>,
    pub reply: Option<String>,
    pub prompt_tokens: Option<u32>,
}

/// One model round's output: optional prose, any tool calls it wants run, and the request's
/// `prompt_tokens` from the response `usage` (the OpenAI-compatible token accounting).
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub prompt_tokens: Option<u32>,
}

/// Build the base chat-completion messages: system prompt, then the transcript with
/// adjacent same-role turns coalesced and any trailing assistant turn dropped (an
/// endpoint rejects a list ending in the assistant's own message). The loop appends
/// its in-turn tool messages onto this.
#[must_use]
pub fn build_messages(system: &str, turns: &[Turn]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(turns.len() + 1);
    if !system.is_empty() {
        messages.push(ChatMessage::system(system));
    }
    for turn in turns {
        let role = match turn.role {
            MessageRole::User => Role::User,
            MessageRole::Agent => Role::Assistant,
        };
        match messages.last_mut() {
            Some(last) if last.role == role => {
                if let Some(content) = last.content.as_mut() {
                    content.push('\n');
                    content.push_str(&turn.text);
                }
            }
            _ => messages.push(ChatMessage::text(role, turn.text.clone())),
        }
    }
    while matches!(messages.last(), Some(m) if m.role == Role::Assistant && m.tool_calls.is_empty())
    {
        messages.pop();
    }
    messages
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
            .map(|m| (m.role, m.content.as_deref().unwrap_or("")))
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
    }

    #[test]
    fn tool_messages_serialize_with_the_right_fields() {
        let calls = vec![ToolCall {
            id: "call_1".to_owned(),
            kind: "function".to_owned(),
            function: FunctionCall {
                name: "search".to_owned(),
                arguments: r#"{"query":"rust"}"#.to_owned(),
            },
        }];
        let assistant = serde_json::to_value(ChatMessage::assistant_tool_calls(calls)).unwrap();
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "search");
        assert!(assistant.get("content").is_none());

        let result = serde_json::to_value(ChatMessage::tool_result("call_1", "ok")).unwrap();
        assert_eq!(result["role"], "tool");
        assert_eq!(result["tool_call_id"], "call_1");
        assert_eq!(result["content"], "ok");
    }
}
