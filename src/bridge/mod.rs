//! The seam between the pipeline subsystem and the rwire UI.
//!
//! A single background thread owns the central-DB connection (sync `rusqlite`,
//! kept off the async UI thread) and a command inbox. UI handlers post `Command`s
//! via [`send`]; the thread mutates the DB and projects read-models back into shared
//! `App` state with narrow `ChangeSet`s, so only the affected renderers re-run.
//!
//! Phase 2 ships a stand-in "echo" agent in place of the real provider loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use rwire::{ChangeSet, SharedServerState};

mod admin;
mod files;
mod tasks;

use crate::agent;
use crate::db::{self, agents, chat, endpoints, resolution, workspaces};
use crate::id;
use crate::mcp::{self, McpClient};
use crate::state::{ActivityEntry, ActivityKind, App, ChatSummary, LogLine, MessageRole};
use crate::tools::{
    CoderToolbox, CompositeToolbox, EmptyToolbox, FilteredToolbox, McpToolbox, Toolbox,
    WORKSPACE_TOOLS,
};
use crate::workspace;

pub enum Command {
    /// Create a conversation with a caller-minted id, so the handler can navigate
    /// to `/chat/{id}` immediately (before this async create completes).
    NewChat {
        id: String,
    },
    OpenChat(String),
    /// Hide a thread from the active list (sets `archived_at`); re-projects both lists.
    ArchiveChat(String),
    /// Restore an archived thread to the active list (clears `archived_at`).
    UnarchiveChat(String),
    /// Permanently delete a thread and its transcript (gated behind a UI confirm).
    DeleteChat(String),
    /// Fill `App.archived` when the sidebar's "Archived" section is first expanded.
    ListArchived,
    Submit {
        chat: String,
        text: String,
    },
    /// A worker thread's full turn (tool calls + final reply) from the agent, posted
    /// back so the (DB-owning) bridge thread persists it and projects.
    AgentReply {
        chat: String,
        result: Result<agent::TurnResult, String>,
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
        /// The agent's identity prompt from the drawer's System-prompt textarea
        /// (operator-owned config, stored in `agent_prompts`).
        prompt: String,
        /// The enabled tool names ticked in the drawer's tool picker.
        tools: Vec<String>,
    },
    DeleteAgent {
        id: String,
    },
    /// Read an existing agent's identity prompt + workspace assignment into the open
    /// drawer draft (prefill).
    LoadAgentPrompt {
        id: String,
    },
    /// Read an existing agent's enabled tools into the open drawer draft (prefill).
    LoadAgentTools {
        id: String,
    },
    SetWebAgent {
        id: String,
    },
    // ── Workspace management (the workspace Team tab + the dashboard's create) ──
    /// Create a new workspace named `name` (the dashboard's create affordance).
    CreateWorkspace {
        name: String,
    },
    /// Rename a workspace (its Team tab).
    RenameWorkspace {
        id: String,
        name: String,
    },
    /// Set (or clear, with `None`) a workspace's owner — its domain lead / home agent.
    SetWorkspaceOwner {
        id: String,
        owner: Option<String>,
    },
    /// Add / remove a member (the team) of a workspace.
    AddMember {
        id: String,
        agent: String,
    },
    RemoveMember {
        id: String,
        agent: String,
    },
    /// Delete a workspace + its directory (refused while members are still assigned).
    DeleteWorkspace {
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
    /// Set the workspace the current route is in (`/w/:wid…`), or `None` off any workspace
    /// route — projected into `App.active_workspace` for the sidebar + workspace views.
    SetActiveWorkspace {
        id: Option<String>,
    },
    /// Load a workspace's `AGENT.md` brief into `App.workspace_brief` (the Overview editor).
    LoadBrief {
        workspace: String,
    },
    // ── Task board (the workspace Tasks tab) ──
    /// Project a workspace's task lane into `App.tasks` (its Tasks tab opening).
    LoadTasks {
        workspace: String,
    },
    /// Queue a task in a workspace, assigned to `agent` (a member; `None` = the owner).
    CreateTask {
        workspace: String,
        title: String,
        spec: String,
        agent: Option<String>,
    },
    /// Cancel a task (its lane re-projects).
    CancelTask {
        id: String,
    },
    /// Save a workspace's `AGENT.md` brief (the Overview editor).
    SaveBrief {
        workspace: String,
        content: String,
    },
    // ── Files view (browse + edit a workspace) ──
    /// List a directory in a workspace (the `/w/:wid/files` tab + breadcrumb navigation).
    ListDir {
        workspace: String,
        path: String,
    },
    /// Read a file into the editor pane.
    OpenFile {
        workspace: String,
        path: String,
    },
    /// Save (or create) a file with the given content.
    WriteFile {
        workspace: String,
        path: String,
        content: String,
    },
    /// Create a directory.
    NewFolder {
        workspace: String,
        path: String,
    },
    /// Delete a file or directory.
    DeleteEntry {
        workspace: String,
        path: String,
    },
    /// Rename/move an entry.
    RenameEntry {
        workspace: String,
        from: String,
        to: String,
    },
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

/// The web MCP client (fetch/search/…), connected once at boot. `None` if its binary
/// is missing or the handshake failed — the agent then runs without web tools.
static WEB_MCP: OnceLock<Arc<McpClient>> = OnceLock::new();

/// Spawn the web MCP server and cache it. Best-effort, like rust-claw's connect: a
/// failure logs and leaves the agent tool-less rather than aborting boot.
fn connect_web_mcp() {
    let command = env("CLAW_WEB_MCP_CMD").unwrap_or_else(|()| mcp::DEFAULT_WEB_COMMAND.to_owned());
    match McpClient::spawn(mcp::WEB_SERVER_NAME, &command, &[], &web_mcp_env()) {
        Ok(client) => {
            log_line(format!(
                "mcp: web server connected — {} tools",
                client.tools().len()
            ));
            let _ = WEB_MCP.set(Arc::new(client));
        }
        Err(err) => {
            log_line(format!(
                "mcp: web server unavailable ({err}); running without web tools"
            ));
        }
    }
}

/// Extra environment for the web MCP child. `CLAW_WEB_MCP_LD_PATH` (set in `.env` or
/// the real environment) is prepended to the inherited `LD_LIBRARY_PATH` so the
/// server's bundled headless Chromium can find libraries the app's own (cargo-injected)
/// library path omits — `libasound` on this machine. Empty when the var is unset, so
/// the child just inherits the parent environment unchanged.
fn web_mcp_env() -> Vec<(String, String)> {
    let Ok(extra) = env("CLAW_WEB_MCP_LD_PATH") else {
        return Vec::new();
    };
    let inherited = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let value = if inherited.is_empty() {
        extra
    } else {
        format!("{extra}:{inherited}")
    };
    vec![("LD_LIBRARY_PATH".to_owned(), value)]
}

/// The connected web MCP client, if any — a run builds its toolbox from this.
pub(crate) fn web_mcp() -> Option<Arc<McpClient>> {
    WEB_MCP.get().cloned()
}

/// Every selectable tool as `(group, name)` — the built-in workspace tools plus the
/// connected web MCP server's tools. The drawer's tool picker and the per-agent persistence
/// both build from this single source of truth.
pub(crate) fn tool_catalog() -> Vec<(String, String)> {
    let mut catalog: Vec<(String, String)> = WORKSPACE_TOOLS
        .iter()
        .map(|name| ("workspace".to_owned(), (*name).to_owned()))
        .collect();
    if let Some(client) = web_mcp() {
        for tool in client.tools() {
            catalog.push(("web".to_owned(), tool.name.clone()));
        }
    }
    catalog
}

/// The data dir (parent of `central.db`), under which agent workspaces live. Set once at
/// boot so a worker thread can resolve `<data>/workspaces/<folder>` without re-plumbing.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The base data dir, defaulting to `.` if boot hasn't set it (only in unit tests).
fn data_dir() -> PathBuf {
    DATA_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn spawn(shared: Arc<SharedServerState>, central_db: PathBuf) {
    let (tx, rx) = mpsc::channel();
    let _ = TX.set(tx);
    let _ = LOG_SINK.set(shared.clone());
    let _ = DATA_DIR.set(
        central_db
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    );
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
    connect_web_mcp();
    admin::project_tool_catalog(&shared);
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
    migrate_workspaces(conn);
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

/// Give every agent a workspace it **owns** (its home), idempotent. An agent that predates
/// the workspace model — or one inherited from a shared rust-claw central.db — gets a
/// workspace whose id is its legacy `folder`, so its existing directory and `AGENT.md` (now
/// the workspace's project brief) carry over untouched. `set_owner` also makes it a member.
/// The per-agent identity prompt starts empty (the run falls back to base + brief until the
/// operator sets one). Agents that already own a workspace are left alone.
fn migrate_workspaces(conn: &rusqlite::Connection) {
    let Ok(agents) = agents::list(conn) else {
        return;
    };
    for agent in agents {
        if workspaces::owned_by(conn, &agent.id)
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        let _ = workspaces::create(conn, &agent.folder, &agent.name);
        let _ = workspaces::set_owner(conn, &agent.folder, &agent.id);
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
        Command::ArchiveChat(id) => archive_chat(conn, shared, &id),
        Command::UnarchiveChat(id) => unarchive_chat(conn, shared, &id),
        Command::DeleteChat(id) => delete_chat(conn, shared, &id),
        Command::ListArchived => list_archived(conn, shared),
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
            prompt,
            tools,
        } => admin::upsert_agent(conn, shared, id, &name, endpoint, model, prompt, tools),
        Command::DeleteAgent { id } => admin::delete_agent(conn, shared, &id),
        Command::LoadAgentPrompt { id } => admin::load_agent_prompt(conn, shared, &id),
        Command::LoadAgentTools { id } => admin::load_agent_tools(conn, shared, &id),
        Command::SetWebAgent { id } => admin::set_web_agent(conn, shared, &id),
        Command::CreateWorkspace { name } => admin::create_workspace(conn, shared, &name),
        Command::RenameWorkspace { id, name } => admin::rename_workspace(conn, shared, &id, &name),
        Command::SetWorkspaceOwner { id, owner } => {
            admin::set_workspace_owner(conn, shared, &id, owner)
        }
        Command::AddMember { id, agent } => admin::add_member(conn, shared, &id, &agent),
        Command::RemoveMember { id, agent } => admin::remove_member(conn, shared, &id, &agent),
        Command::DeleteWorkspace { id } => admin::delete_workspace(conn, shared, &id),
        Command::DiscoverModels { endpoint } => admin::discover_models(conn, shared, &endpoint),
        Command::CloseDrawer => close_drawer(shared),
        Command::SetActiveWorkspace { id } => set_active_workspace(shared, id),
        Command::LoadBrief { workspace } => files::load_brief(conn, shared, &workspace),
        Command::LoadTasks { workspace } => tasks::load(conn, shared, &workspace),
        Command::CreateTask {
            workspace,
            title,
            spec,
            agent,
        } => tasks::create(conn, shared, &workspace, &title, &spec, agent),
        Command::CancelTask { id } => tasks::cancel(conn, shared, &id),
        Command::SaveBrief { workspace, content } => {
            files::save_brief(conn, shared, &workspace, &content)
        }
        Command::ListDir { workspace, path } => files::list_dir(conn, shared, &workspace, &path),
        Command::OpenFile { workspace, path } => files::open_file(conn, shared, &workspace, &path),
        Command::WriteFile {
            workspace,
            path,
            content,
        } => files::write_file(conn, shared, &workspace, &path, &content),
        Command::NewFolder { workspace, path } => {
            files::new_folder(conn, shared, &workspace, &path)
        }
        Command::DeleteEntry { workspace, path } => {
            files::delete_entry(conn, shared, &workspace, &path)
        }
        Command::RenameEntry {
            workspace,
            from,
            to,
        } => files::rename_entry(conn, shared, &workspace, &from, &to),
    }
}

/// Close the System Map drawer. Idempotent — re-rendering the (absent) drawer when no
/// view subscribes to `editing` is a no-op.
fn close_drawer(shared: &SharedServerState) {
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_EDITING]), |app| {
        app.editing = None;
    });
}

/// Project the route's workspace into `App.active_workspace` (the sidebar highlight + the
/// workspace header/dashboard/Team source). Routed through the bridge so the change
/// broadcasts like every other projection.
fn set_active_workspace(shared: &SharedServerState, id: Option<String>) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_ACTIVE_WORKSPACE,
            App::FIELD_WORKSPACE_DELETE_ARMED,
        ]),
        move |app| {
            app.active_workspace = id;
            app.workspace_delete_armed = false; // a fresh view never opens mid-confirm
        },
    );
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

/// Archive a thread, then re-project the active + archived lists. If the archived
/// thread was the open one, fall back to the newest remaining active chat (or an
/// empty surface) so the selection never points at a row that's gone from the list.
fn archive_chat(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    if let Err(err) = chat::archive_chat(conn, id) {
        log_line(format!("bridge: archive chat failed: {err}"));
        return;
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let archived = chat::list_archived_chats(conn).unwrap_or_default();
    let fallback = chats.first().map(|chat| chat.id.clone());
    let fallback_messages = fallback
        .as_deref()
        .map(|id| chat::load_thread(conn, id).unwrap_or_default())
        .unwrap_or_default();
    let id = id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_ARCHIVED,
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
        ]),
        |app| {
            app.chats = chats;
            app.archived = archived;
            if app.open_chat.as_deref() == Some(id.as_str()) {
                app.open_chat = fallback;
                app.messages = fallback_messages;
            }
        },
    );
}

/// Restore an archived thread, then re-project both lists. Selection is untouched —
/// restoring doesn't remove the open thread.
fn unarchive_chat(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    if let Err(err) = chat::unarchive_chat(conn, id) {
        log_line(format!("bridge: unarchive chat failed: {err}"));
        return;
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let archived = chat::list_archived_chats(conn).unwrap_or_default();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_CHATS, App::FIELD_ARCHIVED]),
        |app| {
            app.chats = chats;
            app.archived = archived;
        },
    );
}

/// Permanently delete a thread, then re-project both lists and clear the pending
/// confirm. If the deleted thread was somehow open, fall back to the newest active
/// chat so the surface never points at a row that no longer exists.
fn delete_chat(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    if let Err(err) = chat::delete_chat(conn, id) {
        log_line(format!("bridge: delete chat failed: {err}"));
        return;
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let archived = chat::list_archived_chats(conn).unwrap_or_default();
    let fallback = chats.first().map(|chat| chat.id.clone());
    let fallback_messages = fallback
        .as_deref()
        .map(|id| chat::load_thread(conn, id).unwrap_or_default())
        .unwrap_or_default();
    let id = id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_CHATS,
            App::FIELD_ARCHIVED,
            App::FIELD_PENDING_DELETE,
            App::FIELD_OPEN_CHAT,
            App::FIELD_MESSAGES,
        ]),
        |app| {
            app.chats = chats;
            app.archived = archived;
            app.pending_delete = None;
            if app.open_chat.as_deref() == Some(id.as_str()) {
                app.open_chat = fallback;
                app.messages = fallback_messages;
            }
        },
    );
}

/// Fill `App.archived` for the sidebar's "Archived" section (lazy: sent when it opens).
fn list_archived(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let archived = chat::list_archived_chats(conn).unwrap_or_default();
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_ARCHIVED]), |app| {
        app.archived = archived;
    });
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
    // The agent's workspace (if it has one): scaffold the dir + starter AGENT.md, then read
    // AGENT.md as the shared project-brief layer. The system prompt is layered —
    // base + the agent's identity prompt + the workspace brief. Best-effort: a scaffold
    // failure just drops the brief layer.
    let workspace_dir = inference
        .workspace
        .as_deref()
        .map(|id| workspace::dir_for(&data_dir(), id));
    if let Some(dir) = &workspace_dir
        && let Err(err) = workspace::scaffold(dir, &inference.agent_name)
    {
        log_line(format!("bridge: workspace scaffold failed: {err}"));
    }
    let brief = workspace_dir.as_deref().and_then(workspace::read_agent_md);
    let system_prompt = workspace::compose_system_prompt(
        agent::SYSTEM_PROMPT,
        inference.agent_prompt.clone(),
        brief,
    );

    // Two views of the transcript: the display thread (chat turns + tool cards) for the
    // UI, and the model view (tools folded into text notes) for the request.
    let display = chat::load_thread(conn, chat_id).unwrap_or_default();
    let model_turns = chat::load_model_turns(conn, chat_id).unwrap_or_default();
    let messages = agent::build_messages(&system_prompt, &model_turns);
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
                app.messages = display;
            }
            app.sending = true;
            app.error = None;
            app.running.push(proj_chat.clone());
            push_activity(app, title, ActivityKind::Started, String::new());
        },
    );

    // Build the run's toolbox from the agent's tool selection: web MCP tools and/or the
    // workspace coder tools, each filtered to the enabled set.
    let toolbox = build_toolbox(inference.tools.clone(), workspace_dir);
    let chat = chat_id.to_owned();
    std::thread::spawn(move || {
        let result =
            agent::run_turn(&inference, messages, toolbox.as_ref()).map_err(|err| err.to_string());
        send(Command::AgentReply { chat, result });
    });
}

/// Assemble the toolbox for a run from the agent's enabled tool set. `selection` is the
/// configured names, or `None` for an unconfigured agent — which defaults to all web/MCP
/// tools (coder tools off), preserving the pre-selection behaviour. Each source is wrapped
/// in a [`FilteredToolbox`] so only enabled tools are advertised and callable.
fn build_toolbox(selection: Option<Vec<String>>, workspace: Option<PathBuf>) -> Box<dyn Toolbox> {
    let client = web_mcp();
    let allowed: Vec<String> = match selection {
        Some(names) => names,
        None => client
            .as_ref()
            .map(|c| c.tools().iter().map(|tool| tool.name.clone()).collect())
            .unwrap_or_default(),
    };
    if allowed.is_empty() {
        return Box::new(EmptyToolbox);
    }

    let mut boxes: Vec<Box<dyn Toolbox>> = Vec::new();
    // Workspace tools, only if the agent has a workspace and at least one is selected
    // (else don't touch the filesystem).
    if let Some(workspace) = workspace
        && WORKSPACE_TOOLS
            .iter()
            .any(|tool| allowed.iter().any(|name| name == tool))
    {
        let coder: Box<dyn Toolbox> = Box::new(CoderToolbox::new(workspace));
        boxes.push(Box::new(FilteredToolbox::new(coder, allowed.clone())));
    }
    // Web/MCP tools, only if connected and at least one of theirs is selected.
    if let Some(client) = client {
        let mcp: Box<dyn Toolbox> = Box::new(McpToolbox::new(client));
        let advertises = mcp
            .defs()
            .iter()
            .any(|def| allowed.iter().any(|name| name == &def.name));
        if advertises {
            boxes.push(Box::new(FilteredToolbox::new(mcp, allowed.clone())));
        }
    }
    match boxes.len() {
        0 => Box::new(EmptyToolbox),
        1 => boxes.pop().expect("len 1"),
        _ => Box::new(CompositeToolbox::new(boxes)),
    }
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
    result: Result<agent::TurnResult, String>,
) {
    match result {
        Ok(turn) => {
            // Persist the tool cards (in order), then the final reply — `load_thread`
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
