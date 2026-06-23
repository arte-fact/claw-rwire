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
mod notify;
mod orchestrate;
mod prompts;
mod review;
mod scheduler;
mod seed;
mod tasks;
mod workflow;

use crate::agent;
use crate::db::{self, agents, chat, endpoints, resolution, workspaces};
use crate::id;
use crate::mcp::{self, McpClient};
use crate::state::{
    ActivityEntry, ActivityKind, App, ChatSummary, LogLine, MessageRole, RunKind, RunRole,
};
use crate::tools::{
    CoderToolbox, CompositeToolbox, ConciergeToolbox, Delegate, EmptyToolbox, FilteredToolbox,
    McpToolbox, Toolbox, WORKSPACE_TOOLS,
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
    /// Project every workspace's lane summary into `App.dashboard_lanes` (the dashboard opening).
    LoadDashboard,
    /// Project the review inbox (tasks parked at a human gate) — the `/review` page opening.
    LoadReview,
    /// Project one task's detail (spec + run chain) — the detail view opening.
    LoadTaskDetail {
        task_id: String,
    },
    /// Project the active workspace's pipeline steps + edit lock — the Workflow tab opening.
    LoadWorkflow {
        workspace: String,
    },
    /// Append a pipeline step (`kind` = `stage`/`loop`) to a workspace.
    AddStep {
        workspace: String,
        kind: String,
    },
    /// Persist a pipeline step's edited fields.
    UpdateStep {
        workspace: String,
        id: String,
        form: crate::state::StepForm,
    },
    /// Delete a pipeline step.
    DeleteStep {
        workspace: String,
        id: String,
    },
    /// Move a pipeline step up (`up = true`) or down.
    MoveStep {
        workspace: String,
        id: String,
        up: bool,
    },
    /// A human reviewer's verdict from the inbox/detail view: drives the loop.
    ReviewVerdict {
        task_id: String,
        decision: String,
        feedback: String,
    },
    /// Compose the Prompts inspector's preview for an agent + run-kind (read-only).
    LoadPrompt {
        agent: Option<String>,
        run: RunKind,
    },
    /// Project the editable global role-preamble defaults (the Prompts defaults section).
    LoadSettings,
    /// Set (or, blank, reset) a role preamble override.
    SetRolePrompt {
        run: RunKind,
        value: String,
    },
    /// Queue a task in a workspace, assigned to `agent` (a member; `None` = the owner). A
    /// `schedule` (cron string) makes it recurrent.
    CreateTask {
        workspace: String,
        title: String,
        spec: String,
        agent: Option<String>,
        schedule: Option<String>,
    },
    /// Apply the edit drawer's changes to a task (title / spec / assignee / schedule /
    /// blockers), then re-project the lane.
    UpdateTask {
        id: String,
        title: String,
        spec: String,
        agent: Option<String>,
        schedule: Option<String>,
        blockers: Vec<String>,
    },
    /// Cancel a task (its lane re-projects).
    CancelTask {
        id: String,
    },
    /// Stop (pause) a task → `stopped`: frees the lane and abandons any in-flight run.
    StopTask {
        id: String,
    },
    /// Start (resume) a stopped task → `pending`: continues from the current step.
    StartTask {
        id: String,
    },
    /// Restart a task → `pending` from a clean slate (step 0, fresh attempt). Works from a
    /// stopped or terminal task.
    RestartTask {
        id: String,
    },
    /// Periodic scheduler scan: launch the next ready task in each idle workspace lane.
    SchedulerTick,
    /// A live agent event during a task run (a tool call starting/finishing, or streamed text),
    /// folded into the real-time activity feed. Carries pre-resolved linked context.
    AgentProgress {
        ctx: crate::state::ProgressCtx,
        event: agent::AgentEvent,
    },
    /// A worker thread's finished task run, posted back for the bridge to persist + project.
    /// `role` tells the engine whether a stage / producer / validator just ran (Phase F).
    /// `epoch` is the dispatch guard captured at launch — a result whose epoch no longer matches
    /// the task's `run_epoch` (stopped / cancelled / restarted / re-dispatched) is dropped.
    TaskFinished {
        id: String,
        role: RunRole,
        epoch: i64,
        result: Result<agent::TurnResult, String>,
    },
    /// The concierge delegated a request to a workspace owner (the `ask` tool): run the
    /// owner's planning turn, which queues task cards.
    Ask {
        workspace: String,
        request: String,
        origin_chat: String,
    },
    /// The owner's planning turn finished: re-project the lane and record the plan. The
    /// notify-on-completion is the task scheduler's job; this just surfaces the planning.
    AskPlanned {
        workspace: String,
        origin_chat: String,
        result: Result<String, String>,
    },
    /// The concierge's notify turn produced a completion message — append it to the origin
    /// chat and project (no error banner / activity: a background notice is unobtrusive).
    Notified {
        chat: String,
        text: String,
    },
    /// A summarizer worker folded older turns into an agent's rolling memory — persist the
    /// new summary and how far it has consumed the transcript.
    UpdateMemory {
        agent_id: String,
        summary: String,
        summarized_through: i64,
    },
    /// Operator memory controls (the Prompts inspector's memory panel): overwrite the summary
    /// (keeping the marker), clear it (reset summary + marker), or pause/resume auto-summary.
    SetMemory {
        agent_id: String,
        summary: String,
    },
    ClearMemory {
        agent_id: String,
    },
    SetMemoryPaused {
        agent_id: String,
        paused: bool,
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
    scheduler::spawn_ticker();
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
    // Fresh-DB showcase: a small maker team (agents + workspaces + briefs + welcome chat) so
    // first boot demonstrates the loop. Before migrate_workspaces, which would otherwise give
    // the member-only specialist its own home.
    seed::seed_showcase(conn, &data_dir());
    migrate_workspaces(conn);
    link_chats_to_default_agent(conn);
    // After the web chat is linked, so `web_agent` resolves the concierge to seed.
    seed_owner_persona(conn);
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
        // Give an agent an owned home only if it belongs to no workspace at all (a legacy /
        // unattached agent). Member-only agents — e.g. a showcase specialist — keep none.
        if workspaces::is_attached(conn, &agent.id).unwrap_or(false) {
            continue;
        }
        let _ = workspaces::create(conn, &agent.folder, &agent.name);
        let _ = workspaces::set_owner(conn, &agent.folder, &agent.id);
    }
}

/// Give the concierge / workspace owner (the web agent) a default identity persona if it has
/// none, so it's never a blank agent — the diagnosed failure where the owner had no persona,
/// so it read as faceless and `create_task` auto-routing had nothing to describe it by.
/// Idempotent: only fills a missing persona, never overwrites an operator-set one.
fn seed_owner_persona(conn: &rusqlite::Connection) {
    let Ok(Some(agent_id)) = agents::web_agent(conn) else {
        return;
    };
    if db::agent_prompts::get(conn, &agent_id)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let name = agents::get(conn, &agent_id)
        .ok()
        .flatten()
        .map(|agent| agent.name)
        .unwrap_or_else(|| "claw".to_owned());
    let _ = db::agent_prompts::set(
        conn,
        &agent_id,
        &db::agent_prompts::default_owner_body(&name),
    );
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
        Command::LoadDashboard => tasks::project_dashboard(conn, shared),
        Command::LoadReview => review::load_inbox(conn, shared),
        Command::LoadTaskDetail { task_id } => tasks::load_detail(conn, shared, &task_id),
        Command::LoadWorkflow { workspace } => workflow::load(conn, shared, &workspace),
        Command::AddStep { workspace, kind } => workflow::add(conn, shared, &workspace, &kind),
        Command::UpdateStep {
            workspace,
            id,
            form,
        } => workflow::update(conn, shared, &workspace, &id, &form),
        Command::DeleteStep { workspace, id } => workflow::remove(conn, shared, &workspace, &id),
        Command::MoveStep { workspace, id, up } => {
            workflow::move_step(conn, shared, &workspace, &id, up)
        }
        Command::ReviewVerdict {
            task_id,
            decision,
            feedback,
        } => {
            scheduler::human_verdict(conn, shared, &task_id, &decision, &feedback);
            review::load_inbox(conn, shared);
            tasks::load_detail(conn, shared, &task_id);
        }
        Command::LoadPrompt { agent, run } => prompts::load(conn, shared, agent, run),
        Command::LoadSettings => prompts::load_settings(conn, shared),
        Command::SetRolePrompt { run, value } => prompts::set_role(conn, shared, run, &value),
        Command::CreateTask {
            workspace,
            title,
            spec,
            agent,
            schedule,
        } => tasks::create(conn, shared, &workspace, &title, &spec, agent, schedule),
        Command::UpdateTask {
            id,
            title,
            spec,
            agent,
            schedule,
            blockers,
        } => tasks::update(
            conn,
            shared,
            &id,
            tasks::Edit {
                title,
                spec,
                agent,
                schedule,
                blockers,
            },
        ),
        Command::CancelTask { id } => tasks::cancel(conn, shared, &id),
        Command::StopTask { id } => tasks::stop(conn, shared, &id),
        Command::StartTask { id } => tasks::resume(conn, shared, &id),
        Command::RestartTask { id } => tasks::restart(conn, shared, &id),
        Command::SchedulerTick => scheduler::tick(conn, shared),
        Command::AgentProgress { ctx, event } => live_progress(shared, ctx, event),
        Command::TaskFinished {
            id,
            role,
            epoch,
            result,
        } => scheduler::finished(conn, shared, &id, role, epoch, result),
        Command::Ask {
            workspace,
            request,
            origin_chat,
        } => orchestrate::ask(conn, &workspace, &request, &origin_chat),
        Command::AskPlanned {
            workspace,
            origin_chat,
            result,
        } => orchestrate::planned(conn, shared, &workspace, &origin_chat, result),
        Command::Notified { chat, text } => notified(conn, shared, &chat, &text),
        Command::UpdateMemory {
            agent_id,
            summary,
            summarized_through,
        } => {
            if let Err(err) = db::agent_memory::set(conn, &agent_id, &summary, summarized_through) {
                log_line(format!("bridge: update memory failed: {err}"));
            }
        }
        Command::SetMemory { agent_id, summary } => {
            // Keep the marker so the windowing stays consistent with the edited summary.
            let marker = db::agent_memory::get(conn, &agent_id)
                .map(|m| m.summarized_through)
                .unwrap_or(0);
            if let Err(err) = db::agent_memory::set(conn, &agent_id, summary.trim(), marker) {
                log_line(format!("bridge: set memory failed: {err}"));
            }
        }
        Command::ClearMemory { agent_id } => {
            // Reset the summary and the marker — the next turn re-windows from scratch.
            if let Err(err) = db::agent_memory::set(conn, &agent_id, "", 0) {
                log_line(format!("bridge: clear memory failed: {err}"));
            }
        }
        Command::SetMemoryPaused { agent_id, paused } => {
            if let Err(err) = db::agent_memory::set_paused(conn, &agent_id, paused) {
                log_line(format!("bridge: pause memory failed: {err}"));
            }
        }
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
    // The agent's workspace (if it has one): scaffold the dir + read AGENT.md as the
    // shared project-brief layer, composing the layered system prompt.
    let workspace_dir = inference
        .workspace
        .as_deref()
        .map(|id| workspace::dir_for(&data_dir(), id));
    // The agent's rolling memory: its summary frames the system prompt, and its marker
    // windows the transcript so only the turns after the condensed portion are sent raw.
    let memory = db::agent_memory::get(conn, &inference.agent_id).unwrap_or_default();
    let role = role_prompt(conn, RunKind::Chat);
    let system_prompt = run_system_prompt(
        &role,
        workspace_dir.as_deref(),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        memory.text().map(|summary| wrap_memory(&summary)),
    );

    // Two views of the transcript: the display thread (chat turns + tool cards) for the
    // UI, and the model view (tools folded into notes, windowed past the summary) for the request.
    let display = chat::load_thread(conn, chat_id).unwrap_or_default();
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
            push_activity(app, title, ActivityKind::Started, String::new(), None, None);
        },
    );

    // Build the run's toolbox from the agent's tool selection: web MCP tools and/or the
    // workspace coder tools, each filtered to the enabled set. The chat agent is the
    // concierge, so it also gets the `ask` delegation tool over every workspace.
    let selected = build_toolbox(inference.tools.clone(), workspace_dir, false);
    let concierge = ConciergeToolbox::new(chat_id, delegates(conn));
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

/// Scaffold a run's workspace dir (best-effort) and compose its layered system prompt:
/// the role `base` (concierge / executor / planner) + the agent's identity prompt + the
/// workspace's `AGENT.md` brief + the agent's rolling `memory`. Shared by chat turns and
/// task/planning runs. `None` workspace → just the base + identity (+ memory) layers.
fn run_system_prompt(
    base: &str,
    workspace_dir: Option<&Path>,
    agent_name: &str,
    agent_prompt: Option<String>,
    memory: Option<String>,
) -> String {
    if let Some(dir) = workspace_dir
        && let Err(err) = workspace::scaffold(dir, agent_name)
    {
        log_line(format!("bridge: workspace scaffold failed: {err}"));
    }
    let brief = workspace_dir.and_then(workspace::read_agent_md);
    workspace::compose_system_prompt(base, agent_prompt, brief, memory)
}

/// `settings` keys for the editable role preambles (Phase E configurability).
pub(super) const ROLE_CONCIERGE_KEY: &str = "role.concierge";
pub(super) const ROLE_EXECUTOR_KEY: &str = "role.executor";
pub(super) const ROLE_PLANNER_KEY: &str = "role.planner";
pub(super) const ROLE_VALIDATOR_KEY: &str = "role.validator";

/// The role preamble for a run-kind — the operator's override from `settings`, else the
/// compiled-in constant. Used by every run site so the live prompt and the inspector agree.
fn role_prompt(conn: &rusqlite::Connection, run: RunKind) -> String {
    let (key, default) = match run {
        RunKind::Chat => (ROLE_CONCIERGE_KEY, agent::CONCIERGE_PROMPT),
        RunKind::Task => (ROLE_EXECUTOR_KEY, agent::EXECUTOR_PROMPT),
        RunKind::Planning => (ROLE_PLANNER_KEY, agent::OWNER_PLANNER_PROMPT),
        RunKind::Validator => (ROLE_VALIDATOR_KEY, agent::VALIDATOR_PROMPT),
    };
    db::settings::get_or(conn, key, default)
}

/// The agent's rolling memory wrapped as a system-prompt layer, or `None` when it has no
/// memory yet. Built from `agent_memory`; injected into every run for that agent.
fn memory_layer(conn: &rusqlite::Connection, agent_id: &str) -> Option<String> {
    db::agent_memory::get(conn, agent_id)
        .ok()
        .and_then(|memory| memory.text())
        .map(|summary| wrap_memory(&summary))
}

/// Frame a rolling summary as the "working memory" system-prompt layer.
fn wrap_memory(summary: &str) -> String {
    format!(
        "## Working memory\n\nA running summary of earlier context (older messages have been \
         condensed here):\n\n{summary}"
    )
}

/// How many recent transcript turns the concierge sends raw; older ones live in the summary.
const MEMORY_WINDOW: usize = 12;
/// Turn-count summarize trigger — the **fallback** when the endpoint reports no token usage.
const SUMMARIZE_AT: usize = 24;
/// Token-based summarize trigger: fold once a request's `prompt_tokens` exceeds this fraction
/// of the model's context window. The primary path when usage + a context size are known.
const SUMMARIZE_FRACTION: f64 = 0.6;
/// Assumed context window when the server reports usage but not a context length yet.
const DEFAULT_CONTEXT_TOKENS: u32 = 8192;

/// Cache of each `(endpoint, model)`'s context window, so the token-budget check doesn't refetch
/// `/models` every turn. Warmed off the bridge thread by the turn worker.
static CONTEXT_CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<String, u32>>> =
    OnceLock::new();

fn context_cache_key(inference: &resolution::ResolvedInference) -> String {
    format!("{}\u{0}{}", inference.base_url, inference.model)
}

/// The cached context window for this inference's `(endpoint, model)`, if known yet.
fn cached_context(inference: &resolution::ResolvedInference) -> Option<u32> {
    CONTEXT_CACHE
        .get()?
        .lock()
        .ok()?
        .get(&context_cache_key(inference))
        .copied()
}

/// Fetch and cache the model's context window from `/models` (best-effort; servers that don't
/// advertise it are simply not cached). Runs on a worker thread — never the bridge thread.
fn warm_context_cache(inference: &resolution::ResolvedInference) {
    if cached_context(inference).is_some() {
        return;
    }
    if let Ok(Some(length)) = agent::context_length(
        &inference.base_url,
        inference.api_key.as_deref(),
        &inference.model,
    ) && let Ok(mut map) = CONTEXT_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
    {
        map.insert(context_cache_key(inference), length);
    }
}

/// Assemble the toolbox for a run from the agent's enabled tool set. `selection` is the
/// configured names; `None` means the agent was never configured, so a default applies:
/// the web/MCP tools, plus — when `workspace_default` is set (a **task executor**, which is
/// workspace work by definition) — the workspace file tools. Chat (concierge) runs pass
/// `false`, so an unconfigured concierge stays web-only and delegates rather than writing
/// files. Each source is wrapped in a [`FilteredToolbox`] so only enabled tools are callable.
fn build_toolbox(
    selection: Option<Vec<String>>,
    workspace: Option<PathBuf>,
    workspace_default: bool,
) -> Box<dyn Toolbox> {
    let client = web_mcp();
    let allowed: Vec<String> = match selection {
        Some(names) => names,
        None => {
            let mut names: Vec<String> = client
                .as_ref()
                .map(|c| c.tools().iter().map(|tool| tool.name.clone()).collect())
                .unwrap_or_default();
            if workspace_default && workspace.is_some() {
                names.extend(WORKSPACE_TOOLS.iter().map(|tool| (*tool).to_owned()));
            }
            names
        }
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
    let memory = db::agent_memory::get(conn, &inference.agent_id).unwrap_or_default();
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
                    send(Command::UpdateMemory {
                        agent_id,
                        summary,
                        summarized_through: new_marker,
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
fn notified(conn: &rusqlite::Connection, shared: &SharedServerState, chat_id: &str, text: &str) {
    if let Err(err) = chat::append_message(conn, chat_id, MessageRole::Agent, text) {
        log_line(format!("bridge: append notify message failed: {err}"));
        return;
    }
    let chats = chat::list_chats(conn).unwrap_or_default();
    let messages = chat::load_thread(conn, chat_id).unwrap_or_default();
    let chat_id = chat_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_CHATS, App::FIELD_MESSAGES]),
        |app| {
            app.chats = chats;
            if app.open_chat.as_deref() == Some(chat_id.as_str()) {
                app.messages = messages;
            }
        },
    );
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

/// Newest-first, capped activity log.
const ACTIVITY_CAP: usize = 50;

fn push_activity(
    app: &mut App,
    title: String,
    kind: ActivityKind,
    detail: String,
    task_id: Option<String>,
    workspace_id: Option<String>,
) {
    app.activity.insert(
        0,
        ActivityEntry {
            time: now_hm(),
            title,
            kind,
            detail,
            task_id,
            workspace_id,
        },
    );
    app.activity.truncate(ACTIVITY_CAP);
}

/// Cap on the live agent-activity ring.
const LIVE_CAP: usize = 60;

/// Fold a live agent event (a tool call starting/finishing) into `App.live_activity` — the
/// real-time feed. `ToolStarted` adds an in-flight entry; `ToolFinished` flips the matching one.
fn live_progress(
    shared: &SharedServerState,
    ctx: crate::state::ProgressCtx,
    event: agent::AgentEvent,
) {
    use agent::AgentEvent;
    match event {
        AgentEvent::ToolStarted { name, arguments } => {
            let entry = crate::state::LiveEvent {
                time: now_hms(),
                agent: ctx.agent,
                workspace_id: ctx.workspace_id,
                workspace: ctx.workspace,
                task_id: ctx.task_id,
                task: ctx.task,
                action: describe_tool(&name, &arguments),
                in_progress: true,
                ok: false,
            };
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_ACTIVITY]),
                move |app| {
                    app.live_activity.insert(0, entry);
                    app.live_activity.truncate(LIVE_CAP);
                },
            );
        }
        AgentEvent::ToolFinished { ok } => {
            let task_id = ctx.task_id;
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_ACTIVITY]),
                move |app| {
                    if let Some(entry) = app
                        .live_activity
                        .iter_mut()
                        .find(|e| e.task_id == task_id && e.in_progress)
                    {
                        entry.in_progress = false;
                        entry.ok = ok;
                    }
                },
            );
        }
        AgentEvent::Text(chunk) => {
            let task_id = ctx.task_id;
            shared.update_shared_changed::<App>(
                ChangeSet::from_fields(&[App::FIELD_LIVE_TEXT]),
                move |app| match app.live_text.iter_mut().find(|t| t.task_id == task_id) {
                    Some(buffer) => buffer.text.push_str(&chunk),
                    None => {
                        app.live_text.push(crate::state::LiveText {
                            task_id,
                            text: chunk,
                        });
                        // Bound it — a handful of concurrently-streaming tasks at most.
                        if app.live_text.len() > 8 {
                            app.live_text.remove(0);
                        }
                    }
                },
            );
        }
    }
}

/// Clear a task's streamed-text buffer — its run is now recorded, so the persisted transcript
/// (not the live typewriter) is the source of truth.
pub(super) fn clear_live_text(shared: &SharedServerState, task_id: &str) {
    let task_id = task_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_LIVE_TEXT]),
        move |app| app.live_text.retain(|t| t.task_id != task_id),
    );
}

/// A short neutral phrase for a tool call (the UI adds the ▷/✓/✗ prefix from the flags).
fn describe_tool(name: &str, arguments: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    let path = value.get("path").and_then(serde_json::Value::as_str);
    match name {
        "write" => format!("write {}", path.unwrap_or("a file")),
        "edit" => format!("edit {}", path.unwrap_or("a file")),
        "read" => format!("read {}", path.unwrap_or("a file")),
        "bash" => {
            let cmd = value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let snippet: String = cmd.chars().take(48).collect();
            format!("bash: {snippet}")
        }
        "verdict" => "review".to_owned(),
        other => other.to_owned(),
    }
}

/// `HH:MM:SS` (UTC) — seconds matter for the live feed (events land seconds apart).
fn now_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
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
