//! The chat turn: a user message becomes a model request (identity + brief + budgeted
//! memory + the windowed transcript + the live status digest), streams back into the open
//! thread, and — past the window — folds into the concierge's rolling summary. Split from
//! `bridge/mod.rs`; every fn here runs on the bridge thread except the spawned agent turn.

use std::sync::{Mutex, OnceLock};

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::{self, chat, resolution, workspaces};
use crate::state::{ActivityKind, App, ChatSummary, MessageRole, RunKind, TranscriptItem};
use crate::tools::{CompositeToolbox, ConciergeToolbox, EmptyToolbox, Toolbox};
use crate::workspace;

use super::{
    Command, DEFAULT_CONTEXT_TOKENS, Delegate, build_toolbox, cached_context, data_dir, log_line,
    memory, now_hm, push_activity, role_prompt, run_system_prompt, send, tasks, warm_context_cache,
};

/// The default display window — turns projected before the reader scrolls up. Small on purpose: the
/// concierge thread grows without bound, so a full load would balloon every projection. `CLAW_CHAT_
/// WINDOW` overrides it (mainly for exercising the sentinel in tests). Mirrors the agent thread's
/// page size in spirit.
const CHAT_WINDOW: usize = 50;

fn default_chat_window() -> usize {
    std::env::var("CLAW_CHAT_WINDOW")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(CHAT_WINDOW)
}

/// The open concierge thread's `(chat_id, window)`, mirrored bridge-side so a "load older" expansion
/// survives background refreshes and resets when the reader switches threads (a differing chat_id).
static OPEN_CHAT: OnceLock<Mutex<Option<(String, usize)>>> = OnceLock::new();

fn open_chat_slot() -> &'static Mutex<Option<(String, usize)>> {
    OPEN_CHAT.get_or_init(|| Mutex::new(None))
}

/// The window to project `chat_id` with — the stored size when it's the open thread, else a fresh
/// default. Switching threads (a new chat_id) resets the expansion lazily, with no switch hook.
fn chat_window(chat_id: &str) -> usize {
    let mut slot = open_chat_slot().lock().unwrap_or_else(|e| e.into_inner());
    match slot.as_ref() {
        Some((id, window)) if id == chat_id => *window,
        _ => {
            let window = default_chat_window();
            *slot = Some((chat_id.to_owned(), window));
            window
        }
    }
}

/// Project `chat_id`'s windowed transcript: the newest `chat_window` turns oldest-first + the count
/// of older turns past the fold (the sentinel's `next`). The single seam every projection site uses.
pub(super) fn windowed_thread(
    conn: &rusqlite::Connection,
    chat_id: &str,
) -> (Vec<TranscriptItem>, usize) {
    chat::load_thread_windowed(conn, chat_id, chat_window(chat_id)).unwrap_or_default()
}

/// Seamless history (P4a): the top sentinel scrolled into view — grow the open thread's window by a
/// page and re-project it. No-op when nothing is open. The grown window sticks across background
/// refreshes; switching threads resets it.
pub(super) fn expand(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let chat_id = {
        let mut slot = open_chat_slot().lock().unwrap_or_else(|e| e.into_inner());
        let Some((id, window)) = slot.as_mut() else {
            return;
        };
        *window += default_chat_window();
        id.clone()
    };
    let (messages, truncated) = windowed_thread(conn, &chat_id);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_MESSAGES, App::FIELD_MESSAGES_TRUNCATED]),
        |app| {
            if app.open_chat.as_deref() == Some(chat_id.as_str()) {
                app.messages = messages;
                app.messages_truncated = truncated;
            }
        },
    );
}

pub(super) fn submit(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    chat_id: &str,
    text: &str,
) {
    if let Err(err) = chat::append_message(conn, chat_id, MessageRole::User, text) {
        log_line(format!("bridge: append user message failed: {err}"));
        return;
    }
    let _ = chat::title_if_default(conn, chat_id, &first_line_title(text));
    dispatch(conn, shared, chat_id, text);
}

/// Boot recovery: a concierge turn runs on a worker thread that dies with the process, so a
/// restart mid-turn leaves the user's message with no reply (observed live — two turns lost
/// across a day's deploys). Re-dispatch every chat whose last message is still inbound; the
/// message is already persisted, so this dispatches without re-appending. The macro-review
/// recovery's missing sibling ([`super::requests::control::recover_validations`]).
pub(super) fn recover_pending_turns(conn: &rusqlite::Connection, shared: &SharedServerState) {
    for chat_id in chat::chats_awaiting_reply(conn).unwrap_or_default() {
        let Some(text) = chat::last_user_text(conn, &chat_id).unwrap_or_default() else {
            continue;
        };
        log_line(format!(
            "chat: re-dispatching {chat_id} — a turn was interrupted by a restart"
        ));
        dispatch(conn, shared, &chat_id, &text);
    }
}

/// Resolve the chat's endpoint and run the turn (native worker or echo fallback). The user
/// message must already be persisted — both [`submit`] and [`recover_pending_turns`] append
/// (or find) it first, so the transcript the turn reads is complete.
fn dispatch(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str, text: &str) {
    match resolution::resolve_for_chat(conn, chat_id, |key| std::env::var(key).ok()) {
        Ok(inference) => dispatch_native(conn, shared, chat_id, inference),
        Err(_) => reply_echo(conn, shared, chat_id, text),
    }
}

/// Native path: show the user's turn + spinner now, then run the (blocking) endpoint
/// call on a worker thread that posts the result back as `Command::AgentReply`. The
/// bridge thread stays free to handle other commands meanwhile.
fn dispatch_native(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    chat_id: &str,
    inference: resolution::ResolvedInference,
) {
    // The agent's workspace (if it has one): scaffold the dir + read AGENT.md as the
    // shared project-brief layer, composing the layered system prompt.
    let workspace_dir = inference
        .workspace
        .as_deref()
        .map(|id| workspace::dir_for(&data_dir(), id));
    // The concierge's rolling memory (its **global** scope row — one conversation, one memory):
    // the summary frames the system prompt, and the marker windows the transcript so only the
    // turns after the condensed portion are sent raw.
    let memory = db::agent_memory::get(conn, &inference.agent_id, None).unwrap_or_default();
    let role = role_prompt(conn, RunKind::Chat);

    // Two views of the transcript: the display thread (chat turns + tool cards) for the
    // UI, and the model view (tools folded into notes, windowed past the summary) for the request.
    let (display, display_truncated) = windowed_thread(conn, chat_id);
    let model_turns: Vec<crate::state::ChatMessage> =
        chat::model_turns_after(conn, chat_id, memory.summarized_through)
            .unwrap_or_default()
            .into_iter()
            .map(|turn| crate::state::ChatMessage {
                role: turn.role,
                text: turn.text,
                time: String::new(),
            })
            .collect();
    // F→U→W→M: the summary layer takes only what the budget leaves once the identity and the
    // raw tail are placed and the concierge's working headroom is reserved.
    let unit_chars = model_turns
        .iter()
        .map(|turn| turn.text.chars().count() + 16)
        .sum();
    let memory_text = memory::fill_for_launch(
        &inference,
        RunKind::Chat,
        &role,
        workspace_dir.as_deref(),
        unit_chars,
        None,
        memory.text().as_deref(),
    )
    .map(|summary| wrap_memory(&summary));
    let system_prompt = run_system_prompt(
        &role,
        workspace_dir.as_deref(),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        memory_text,
    );
    // The live status digest (R5): present state is looked up fresh each turn, never
    // remembered. Appended as the **newest turn** — buried in the system prompt the model
    // narrated from stale conversation history anyway (live-tested); the tail position is the
    // one it cannot skip.
    let mut model_turns = model_turns;
    if let Some(digest) = tasks::status_digest(conn) {
        model_turns.push(crate::state::ChatMessage {
            role: MessageRole::User,
            text: digest,
            time: String::new(),
        });
    }
    let messages = agent::build_messages(&system_prompt, &model_turns);
    let chats = chat::list_chats(conn).unwrap_or_default();
    let proj_chat = chat_id.to_owned();
    let title = title_of(&chats, &proj_chat);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_MESSAGES,
            App::FIELD_MESSAGES_TRUNCATED,
            App::FIELD_SENDING,
            App::FIELD_ERROR,
            App::FIELD_RUNNING,
            App::FIELD_ACTIVITY,
        ]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(proj_chat.as_str()) {
                app.messages = display;
                app.messages_truncated = display_truncated;
            }
            app.sending = true;
            app.error = None;
            app.running.push(proj_chat.clone());
            push_activity(app, title, ActivityKind::Started, String::new(), None, None);
        },
    );

    // Build the run's toolbox from the agent's tool selection: web MCP tools and/or the
    // workspace coder tools, each filtered to the enabled set. The chat agent is the
    // concierge, so it also gets the `ask` delegation tool over every workspace.
    let selected = build_toolbox(inference.tools.clone(), workspace_dir, false);
    let concierge = ConciergeToolbox::new(chat_id, delegates(conn), data_dir());
    let toolbox: Box<dyn Toolbox> =
        Box::new(CompositeToolbox::new(vec![Box::new(concierge), selected]));
    let chat = chat_id.to_owned();
    std::thread::spawn(move || {
        let result =
            agent::run_turn(&inference, messages, toolbox.as_ref()).map_err(|err| err.to_string());
        send(Command::AgentReply { chat, result });
        // Warm the model's context-window cache off the bridge thread (cached after the
        // first call), so the token-budget summarize check has a real budget to compare to.
        warm_context_cache(&inference);
    });
}

/// The concierge's delegation targets: every workspace, with the first line of its `AGENT.md`
/// brief as the routing description the model picks by.
fn delegates(conn: &rusqlite::Connection) -> Vec<Delegate> {
    workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|ws| {
            let brief = workspace::read_agent_md(&workspace::dir_for(&data_dir(), &ws.id))
                .unwrap_or_default();
            Delegate {
                id: ws.id,
                name: ws.name,
                brief,
            }
        })
        .collect()
}

/// Echo fallback (no endpoint configured): synchronous stand-in reply.
fn reply_echo(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str, text: &str) {
    let reply = echo_reply(text);
    if let Err(err) = chat::append_message(conn, chat_id, MessageRole::Agent, &reply) {
        log_line(format!("bridge: append agent message failed: {err}"));
    }
    project_reply(conn, shared, chat_id);
}

/// Persist a worker's endpoint result and project. On error, surface a transient
/// banner instead of writing a bogus assistant turn into the transcript.
pub(super) fn agent_reply(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    chat_id: &str,
    result: Result<agent::TurnResult, String>,
) {
    match result {
        Ok(turn) => {
            // Persist the tool cards (in order), then the final reply — `windowed_thread`
            // reads them back interleaved.
            for tool in &turn.invocations {
                if let Err(err) = chat::append_tool(
                    conn,
                    chat_id,
                    &tool.name,
                    &tool.arguments,
                    &tool.result,
                    tool.ok,
                ) {
                    log_line(format!("bridge: append tool turn failed: {err}"));
                }
            }
            if let Some(reply) = turn.reply.as_deref().filter(|r| !r.trim().is_empty())
                && let Err(err) = chat::append_message(conn, chat_id, MessageRole::Agent, reply)
            {
                log_line(format!("bridge: append agent message failed: {err}"));
            }
            project_reply(conn, shared, chat_id);
            // Fold older turns into the concierge's rolling memory once context gets large
            // (by the request's real token count, else a turn-count fallback).
            maybe_summarize(conn, chat_id, turn.prompt_tokens);
        }
        Err(message) => {
            log_line(format!("bridge: agent turn failed: {message}"));
            let title = chat_title(conn, chat_id);
            let chat = chat_id.to_owned();
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[
                    App::FIELD_SENDING,
                    App::FIELD_ERROR,
                    App::FIELD_RUNNING,
                    App::FIELD_ACTIVITY,
                ]),
                |app| {
                    app.sending = false;
                    app.error = Some(message.clone());
                    app.running.retain(|id| id != &chat);
                    push_activity(app, title, ActivityKind::Failed, message, None, None);
                },
            );
        }
    }
}

/// The instruction for a summarization turn — fold an excerpt into the running summary.
const SUMMARIZE_PROMPT: &str = "You maintain a running summary of a long conversation so it \
can continue without the full transcript. Merge the existing summary with the new excerpt \
into one concise summary. Preserve concrete facts, decisions, file/task/workspace names, \
commitments, and any open or unfinished threads; drop pleasantries. Write terse notes, not \
prose, and output only the summary.";

/// How many recent transcript turns the concierge sends raw; older ones live in the summary.
const MEMORY_WINDOW: usize = 12;

/// Turn-count summarize trigger — the **fallback** when the endpoint reports no token usage.
const SUMMARIZE_AT: usize = 24;

/// Token-based summarize trigger: fold once a request's `prompt_tokens` exceeds this fraction
/// of the model's context window. The primary path when usage + a context size are known.
const SUMMARIZE_FRACTION: f64 = 0.6;

/// Fold the concierge's older turns into its rolling memory when the thread gets large,
/// advancing the marker so the raw tail shrinks back to [`MEMORY_WINDOW`]. The trigger is
/// **token-based** when the endpoint reports `prompt_tokens` (over [`SUMMARIZE_FRACTION`] of
/// the model's context window), and **falls back** to a turn count ([`SUMMARIZE_AT`]) when it
/// doesn't. Runs the summarization on a worker thread (one LLM call); best-effort.
fn maybe_summarize(conn: &rusqlite::Connection, chat_id: &str, prompt_tokens: Option<u32>) {
    let Ok(inference) = resolution::resolve_for_chat(conn, chat_id, |key| std::env::var(key).ok())
    else {
        return;
    };
    let memory = db::agent_memory::get(conn, &inference.agent_id, None).unwrap_or_default();
    if memory.paused {
        return; // operator froze this agent's memory
    }
    let tail =
        chat::model_turns_after(conn, chat_id, memory.summarized_through).unwrap_or_default();
    // Need at least a window plus something to fold.
    if tail.len() <= MEMORY_WINDOW {
        return;
    }
    let over = match prompt_tokens {
        Some(tokens) => {
            let budget = cached_context(&inference).unwrap_or(DEFAULT_CONTEXT_TOKENS);
            f64::from(tokens) > SUMMARIZE_FRACTION * f64::from(budget)
        }
        None => tail.len() > SUMMARIZE_AT,
    };
    if !over {
        return;
    }
    let fold_count = tail.len() - MEMORY_WINDOW;
    let batch = &tail[..fold_count];
    let Some(new_marker) = batch.last().map(|turn| turn.id) else {
        return;
    };
    let excerpt = render_turns(batch);
    let previous = memory.summary;
    let agent_id = inference.agent_id.clone();
    let chat = chat_id.to_owned();
    std::thread::spawn(move || {
        let user = crate::state::ChatMessage {
            role: MessageRole::User,
            text: format!(
                "Existing summary (may be empty):\n{previous}\n\n\
                 --- New conversation excerpt to fold in ---\n{excerpt}"
            ),
            time: now_hm(),
        };
        let messages = agent::build_messages(SUMMARIZE_PROMPT, std::slice::from_ref(&user));
        match agent::run_turn(&inference, messages, &EmptyToolbox) {
            Ok(turn) => {
                if let Some(summary) = turn
                    .reply
                    .map(|reply| reply.trim().to_owned())
                    .filter(|reply| !reply.is_empty())
                {
                    let plural = if fold_count == 1 { "" } else { "s" };
                    send(Command::UpdateMemory {
                        agent_id,
                        workspace: None,
                        summary,
                        summarized_through: new_marker,
                        note: format!("condensed {fold_count} older message{plural} into memory"),
                        source: Some(chat),
                    });
                }
            }
            Err(err) => log_line(format!("bridge: summarize failed: {err}")),
        }
    });
}

/// Render model turns as labelled lines for the summarizer's excerpt.
fn render_turns(turns: &[chat::ModelTurn]) -> String {
    turns
        .iter()
        .map(|turn| {
            let who = match turn.role {
                MessageRole::User => "User",
                MessageRole::Agent => "Assistant",
            };
            format!("{who}: {}", turn.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Append the concierge's completion notice to its origin chat and project it (refresh the
/// sidebar + the thread if it's the open one). Unlike `agent_reply` this raises no error
/// banner and logs no activity — it's an unobtrusive background notice.
pub(super) fn notified(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    chat_id: &str,
    text: &str,
) {
    if let Err(err) = chat::append_message(conn, chat_id, MessageRole::Agent, text) {
        log_line(format!("bridge: append notify message failed: {err}"));
        return;
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let (messages, truncated) = windowed_thread(conn, chat_id);
    let chat_id = chat_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_MESSAGES,
            App::FIELD_MESSAGES_TRUNCATED,
        ]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(chat_id.as_str()) {
                app.messages = messages;
                app.messages_truncated = truncated;
            }
        },
    );
}

/// Reload the thread + sidebar and clear `sending`/`error` after a reply landed.
fn project_reply(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str) {
    let chats = chat::list_chats(conn).unwrap_or_default();
    let (messages, truncated) = windowed_thread(conn, chat_id);
    let title = title_of(&chats, chat_id);
    let chat_id = chat_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_MESSAGES,
            App::FIELD_MESSAGES_TRUNCATED,
            App::FIELD_SENDING,
            App::FIELD_ERROR,
            App::FIELD_RUNNING,
            App::FIELD_ACTIVITY,
        ]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(chat_id.as_str()) {
                app.messages = messages;
                app.messages_truncated = truncated;
            }
            app.sending = false;
            app.error = None;
            app.running.retain(|id| id != &chat_id);
            push_activity(
                app,
                title,
                ActivityKind::Finished,
                String::new(),
                None,
                None,
            );
        },
    );
}

/// The stand-in agent: echoes the message back as markdown so the renderer's
/// formatting (bold, blockquote, emphasis) is exercised end to end.
fn echo_reply(text: &str) -> String {
    let trimmed = text.trim();
    format!(
        "**echo** — you said:\n\n> {}\n\n_{} characters_",
        trimmed,
        trimmed.chars().count()
    )
}

fn first_line_title(text: &str) -> String {
    let title = crate::text::first_line_capped(text, 40);
    if title.is_empty() {
        "New chat".to_owned()
    } else {
        title
    }
}

/// Frame a rolling summary as the "working memory" system-prompt layer.
pub(crate) fn wrap_memory(summary: &str) -> String {
    format!(
        "## Working memory\n\nA running summary of earlier context (older messages have been \
         condensed here):\n\n{summary}"
    )
}

fn title_of(chats: &[ChatSummary], chat_id: &str) -> String {
    chats
        .iter()
        .find(|chat| chat.id == chat_id)
        .map_or_else(|| "chat".to_owned(), |chat| chat.title.clone())
}

fn chat_title(conn: &rusqlite::Connection, chat_id: &str) -> String {
    title_of(&chat::list_chats(conn).unwrap_or_default(), chat_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_reply_quotes_and_counts() {
        let reply = echo_reply("  hi  ");
        assert!(reply.contains("> hi"));
        assert!(reply.contains("2 characters"));
    }

    #[test]
    fn first_line_title_truncates_and_defaults() {
        assert_eq!(first_line_title("hello world"), "hello world");
        assert_eq!(first_line_title("  \n  "), "New chat");
        let long = "x".repeat(80);
        assert_eq!(first_line_title(&long).chars().count(), 40);
    }
}
