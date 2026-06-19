//! The seam between the pipeline subsystem and the rwire UI.
//!
//! A single background thread owns the central-DB connection (sync `rusqlite`,
//! kept off the async UI thread) and a command inbox. UI handlers post `Command`s
//! via [`send`]; the thread mutates the DB and projects read-models back into shared
//! `App` state with narrow `ChangeSet`s, so only the affected renderers re-run.
//!
//! Phase 2 ships a stand-in "echo" agent in place of the real provider loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use rwire::{ChangeSet, SharedServerState};

mod admin;

use crate::agent;
use crate::db::{self, agents, chat, endpoints, resolution};
use crate::id;
use crate::state::{ActivityEntry, ActivityKind, App, ChatSummary, LogLine, MessageRole};

pub enum Command {
    /// Create a conversation with a caller-minted id, so the handler can navigate
    /// to `/chat/{id}` immediately (before this async create completes).
    NewChat {
        id: String,
    },
    OpenChat(String),
    Submit {
        chat: String,
        text: String,
    },
    /// A worker thread's result from the agent endpoint, posted back so the (DB-owning)
    /// bridge thread persists it and projects.
    AgentReply {
        chat: String,
        result: Result<String, String>,
    },
    // ── System Map admin (registry CRUD) ──
    UpsertEndpoint {
        name: String,
        base_url: String,
        api_key: Option<String>,
    },
    DeleteEndpoint {
        name: String,
    },
    UpsertAgent {
        id: Option<String>,
        name: String,
        endpoint: Option<String>,
        model: Option<String>,
    },
    DeleteAgent {
        id: String,
    },
    SetWebAgent {
        id: String,
    },
    /// Fetch an endpoint's advertised models into `App.endpoint_models` (the agent
    /// form's model dropdown).
    DiscoverModels {
        endpoint: String,
    },
    /// Close the System Map drawer (sent on navigation so it doesn't persist across
    /// route changes — `editing` is shared state).
    CloseDrawer,
}

static TX: OnceLock<Sender<Command>> = OnceLock::new();

/// Post a command to the pipeline. Dropped if the bridge failed to start.
pub fn send(command: Command) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(command);
    }
}

static LOG_SINK: OnceLock<Arc<SharedServerState>> = OnceLock::new();
const LOG_CAP: usize = 200;

/// Record an operational log line: print it and append to `App.logs` (newest first,
/// capped) for the Logs viewer. Must NOT be called from inside an
/// `update_shared_changed` closure — it takes the shared lock itself.
pub fn log_line(message: impl Into<String>) {
    let message = message.into();
    eprintln!("{message}");
    if let Some(shared) = LOG_SINK.get() {
        shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_LOGS]), |app| {
            app.logs.insert(
                0,
                LogLine {
                    time: now_hm(),
                    message,
                },
            );
            app.logs.truncate(LOG_CAP);
        });
    }
}

pub fn spawn(shared: Arc<SharedServerState>, central_db: PathBuf) {
    let (tx, rx) = mpsc::channel();
    let _ = TX.set(tx);
    let _ = LOG_SINK.set(shared.clone());
    let conn = match db::open(&central_db) {
        Ok(conn) => conn,
        Err(err) => {
            log_line(format!("bridge: cannot open central db: {err}"));
            return;
        }
    };
    // Prime shared state *before* the server serves any connection, so the first
    // render already has the chats + registry. Otherwise a connection that paints
    // before the projection lands shows blank data, and a client-side tab switch —
    // which doesn't touch those fields — never re-renders it (needs a reload).
    boot(&conn, &shared);
    std::thread::spawn(move || {
        while let Ok(command) = rx.recv() {
            handle(&conn, &shared, command);
        }
    });
}

/// Prime the projection from the DB and select the most recent thread. On first
/// run (no threads yet) seed one so the operator lands in a ready chat.
fn boot(conn: &rusqlite::Connection, shared: &SharedServerState) {
    if chat::list_chats(conn).map(|chats| chats.is_empty()) == Ok(true)
        && let Ok(id) = id::new_id()
    {
        let _ = chat::create_chat(conn, &id, "New chat");
    }
    seed_provider(conn);
    link_chats_to_default_agent(conn);
    admin::project_registry(conn, shared);
    let chats = chat::list_chats(conn).unwrap_or_default();
    let selected = chats.first().map(|chat| chat.id.clone());
    let messages = selected
        .as_deref()
        .map(|id| chat::load_thread(conn, id).unwrap_or_default())
        .unwrap_or_default();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
            App::FIELD_PIPELINE_READY,
        ]),
        |app| {
            app.chats = chats;
            app.open_chat = selected;
            app.messages = messages;
            app.pipeline_ready = true;
        },
    );
}

/// First-boot bootstrap: when no endpoint exists yet, seed a `default` endpoint +
/// agent from `.env` so the chat talks to a real model out of the box. Once any
/// endpoint exists the DB is the source of record and the System Map admin owns it —
/// env never clobbers UI edits on restart. No env config → echo fallback.
fn seed_provider(conn: &rusqlite::Connection) {
    match endpoints::is_empty(conn) {
        Ok(true) => {}
        _ => return, // already configured (admin-set) — or a read error; don't touch it
    }
    let (Ok(base_url), Ok(model)) = (env("CLAW_ENDPOINT"), env("CLAW_MODEL")) else {
        return;
    };
    let api_key = env("CLAW_API_KEY").ok().filter(|key| !key.is_empty());
    if let Err(err) = endpoints::upsert(conn, "default", &base_url, api_key.as_deref(), None) {
        log_line(format!("bridge: seed endpoint failed: {err}"));
        return;
    }
    match agents::ensure_default(conn, Some("default"), Some(&model)) {
        Ok(_) => println!("Agent: native endpoint {base_url} (model {model})"),
        Err(err) => log_line(format!("bridge: seed agent failed: {err}")),
    }
}

/// Read a non-empty, trimmed env var.
fn env(key: &str) -> Result<String, ()> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(())
}

/// Bind every web chat to the default agent group (idempotent) so a turn can resolve
/// its endpoint. Covers chats inherited from an earlier echo-only run.
fn link_chats_to_default_agent(conn: &rusqlite::Connection) {
    let Ok(Some(agent)) = agents::first(conn) else {
        return;
    };
    if let Ok(chats) = chat::list_chats(conn) {
        for summary in &chats {
            let _ = agents::link(conn, &summary.id, &agent.id);
        }
    }
}

fn handle(conn: &rusqlite::Connection, shared: &SharedServerState, command: Command) {
    match command {
        Command::NewChat { id } => new_chat(conn, shared, &id),
        Command::OpenChat(id) => open_chat(conn, shared, &id),
        Command::Submit { chat, text } => submit(conn, shared, &chat, &text),
        Command::AgentReply { chat, result } => agent_reply(conn, shared, &chat, result),
        Command::UpsertEndpoint {
            name,
            base_url,
            api_key,
        } => admin::upsert_endpoint(conn, shared, &name, &base_url, api_key),
        Command::DeleteEndpoint { name } => admin::delete_endpoint(conn, shared, &name),
        Command::UpsertAgent {
            id,
            name,
            endpoint,
            model,
        } => admin::upsert_agent(conn, shared, id, &name, endpoint, model),
        Command::DeleteAgent { id } => admin::delete_agent(conn, shared, &id),
        Command::SetWebAgent { id } => admin::set_web_agent(conn, shared, &id),
        Command::DiscoverModels { endpoint } => admin::discover_models(conn, shared, &endpoint),
        Command::CloseDrawer => close_drawer(shared),
    }
}

/// Close the System Map drawer. Idempotent — re-rendering the (absent) drawer when no
/// view subscribes to `editing` is a no-op.
fn close_drawer(shared: &SharedServerState) {
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_EDITING]), |app| {
        app.editing = None;
    });
}

fn new_chat(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    if let Err(err) = chat::create_chat(conn, id, "New chat") {
        log_line(format!("bridge: create chat failed: {err}"));
        return;
    }
    if let Ok(Some(agent)) = agents::first(conn) {
        let _ = agents::link(conn, id, &agent.id);
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let id = id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
            App::FIELD_SENDING,
        ]),
        |app| {
            app.chats = chats;
            app.open_chat = Some(id);
            app.messages = Vec::new();
            app.sending = false;
        },
    );
}

fn open_chat(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let messages = chat::load_thread(conn, id).unwrap_or_default();
    let id = id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
            App::FIELD_SENDING,
        ]),
        |app| {
            app.open_chat = Some(id);
            app.messages = messages;
            app.sending = false;
        },
    );
}

fn submit(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str, text: &str) {
    if let Err(err) = chat::append_message(conn, chat_id, MessageRole::User, text) {
        log_line(format!("bridge: append user message failed: {err}"));
        return;
    }
    let _ = chat::title_if_default(conn, chat_id, &first_line_title(text));

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
    let turns = chat::load_thread(conn, chat_id).unwrap_or_default();
    let messages = agent::build_messages(agent::SYSTEM_PROMPT, &turns);
    let chats = chat::list_chats(conn).unwrap_or_default();
    let proj_chat = chat_id.to_owned();
    let title = title_of(&chats, &proj_chat);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_MESSAGES,
            App::FIELD_SENDING,
            App::FIELD_ERROR,
            App::FIELD_RUNNING,
            App::FIELD_ACTIVITY,
        ]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(proj_chat.as_str()) {
                app.messages = turns;
            }
            app.sending = true;
            app.error = None;
            app.running.push(proj_chat.clone());
            push_activity(app, title, ActivityKind::Started, String::new());
        },
    );

    let chat = chat_id.to_owned();
    std::thread::spawn(move || {
        let result = agent::complete(&inference, &messages).map_err(|err| err.to_string());
        send(Command::AgentReply { chat, result });
    });
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
fn agent_reply(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    chat_id: &str,
    result: Result<String, String>,
) {
    match result {
        Ok(reply) => {
            if let Err(err) = chat::append_message(conn, chat_id, MessageRole::Agent, &reply) {
                log_line(format!("bridge: append agent message failed: {err}"));
            }
            project_reply(conn, shared, chat_id);
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
                    push_activity(app, title, ActivityKind::Failed, message);
                },
            );
        }
    }
}

/// Reload the thread + sidebar and clear `sending`/`error` after a reply landed.
fn project_reply(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str) {
    let chats = chat::list_chats(conn).unwrap_or_default();
    let messages = chat::load_thread(conn, chat_id).unwrap_or_default();
    let title = title_of(&chats, chat_id);
    let chat_id = chat_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_MESSAGES,
            App::FIELD_SENDING,
            App::FIELD_ERROR,
            App::FIELD_RUNNING,
            App::FIELD_ACTIVITY,
        ]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(chat_id.as_str()) {
                app.messages = messages;
            }
            app.sending = false;
            app.error = None;
            app.running.retain(|id| id != &chat_id);
            push_activity(app, title, ActivityKind::Finished, String::new());
        },
    );
}

/// Newest-first, capped activity log.
const ACTIVITY_CAP: usize = 50;

fn push_activity(app: &mut App, title: String, kind: ActivityKind, detail: String) {
    app.activity.insert(
        0,
        ActivityEntry {
            time: now_hm(),
            title,
            kind,
            detail,
        },
    );
    app.activity.truncate(ACTIVITY_CAP);
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

/// `HH:MM` (UTC), matching the transcript timestamps.
fn now_hm() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60)
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
    let line = text.trim().lines().next().unwrap_or_default().trim();
    let truncated: String = line.chars().take(40).collect();
    if truncated.is_empty() {
        "New chat".to_owned()
    } else {
        truncated
    }
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
