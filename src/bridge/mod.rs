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
mod agent_nav;
mod agent_thread;
mod boot;
mod chat_turn;
pub(crate) use chat_turn::wrap_memory;
mod files;
mod live;
pub(crate) use live::{clear_live_turn, live_progress, push_activity};
mod memory;
mod notify;
mod orchestrate;
pub(crate) mod preview;
mod prompts;
mod replay;
mod requests;
mod review;
mod scheduler;
mod seed;
mod steer;
pub(crate) use tasks::status_digest;
mod tasks;
mod workflow;

use crate::agent;
use crate::db::{self, agents, chat, resolution, workspaces};
use crate::mcp::{self, McpClient};
use crate::state::{App, LogLine, RunKind, RunRole};
use crate::tools::{
    CoderToolbox, CompositeToolbox, Delegate, EmptyToolbox, FilteredToolbox, McpToolbox, Toolbox,
    WORKSPACE_TOOLS,
};
use crate::workspace;

pub enum Command {
    /// Permanently delete an archived thread and its transcript (gated behind a UI confirm).
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
    /// Toggle a workspace's auto-approve flag — when on, delegated work skips the R3 plan gate.
    SetWorkspaceAutoApprove {
        id: String,
        on: bool,
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
    /// Project one task's detail (spec + run chain) — the detail view opening.
    LoadTaskDetail {
        task_id: String,
    },
    /// Project a request's page (`/w/:wid/requests/:rid` opening, G2).
    LoadRequestDetail {
        request_id: String,
    },
    /// Project a project's page (`/w/:wid/projects/:pid` opening, P1).
    LoadProjectDetail {
        workspace: String,
        project: String,
    },
    /// Set (or clear, with an empty slug) a project's kind override (P1).
    SetProjectKind {
        workspace: String,
        project: String,
        slug: String,
    },
    /// The request-level macro review's verdict came back (G3, worker → bridge).
    RequestValidated {
        request_id: String,
        decision: String,
        note: String,
    },
    /// The request page's explicit acts (G3).
    RequestDeliver {
        id: String,
    },
    RequestAbandon {
        id: String,
    },
    RequestRollback {
        id: String,
    },
    RequestPush {
        id: String,
    },
    /// Restore one file to its state before a pass commit (a run's diff view, G3). The task
    /// routes the restore into its lane's tree (G4).
    RestoreFile {
        workspace: String,
        task: String,
        repo: String,
        sha: String,
        path: String,
    },
    /// Open an agent's workspace thread (`/w/:wid/agent/:aid`) — project its merged
    /// tasks/runs/live timeline scoped to that workspace.
    OpenAgentThread {
        agent: String,
        workspace: String,
    },
    /// "Load older" on the agent thread (R5): grow the open thread's window by one page.
    ExpandAgentThread,
    /// Leaving the agent pages — clear the open agent so the nav highlight follows the route.
    CloseAgentThread,
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
        /// Per-task auto-retry budget (`None` = the global default).
        max_retries: Option<i64>,
    },
    /// Apply the edit drawer's changes to a task (title / spec / assignee / schedule /
    /// max-retries / blockers), then re-project the lane.
    UpdateTask {
        id: String,
        title: String,
        spec: String,
        agent: Option<String>,
        schedule: Option<String>,
        max_retries: Option<i64>,
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
    /// Archive a terminal task — hide it from the board to declutter it (the run chain is kept).
    ArchiveTask {
        id: String,
    },
    /// Restore an archived task to the board.
    UnarchiveTask {
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
        /// The memory layer the run launched with (R3.5 M4) — echoed back by the worker so the
        /// recorded run carries its provenance. `None` for validators (memory-free by design).
        memory: Option<String>,
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
        plan_id: String,
        result: Result<String, String>,
    },
    /// The concierge's notify turn produced a completion message — append it to the origin
    /// chat and project (no error banner / activity: a background notice is unobtrusive).
    Notified {
        chat: String,
        text: String,
    },
    /// A summarizer worker folded older context into an agent's rolling memory — persist the
    /// new summary and how far it has consumed the transcript. `workspace` picks the scope row:
    /// `None` is the agent's global (concierge) memory, `Some` a workspace-scoped one. `note`
    /// (the human fold line) and `source` (the task/chat it came from) go to the journal.
    UpdateMemory {
        agent_id: String,
        workspace: Option<String>,
        summary: String,
        summarized_through: i64,
        note: String,
        source: Option<String>,
    },
    /// A mid-run steer poll consumed messages (R5) — re-project the open thread so the
    /// "queued" tag flips to delivered while the run is still going.
    SteerConsumed,
    /// Steer (R4): the operator leaves a message on a worker's thread. Queued now; delivered
    /// at the agent's next pass boundary (or, to an idle agent, materialised as a task).
    SteerAgent {
        agent: String,
        workspace: String,
        body: String,
    },
    /// Save a workspace's `STANDARDS.md` — the validator standards memo (the Overview editor).
    /// Save the workspace's machine-check command (L1: Settings tab).
    SaveChecks {
        workspace: String,
        command: String,
    },
    SaveStandards {
        workspace: String,
        content: String,
    },
    /// Save a workspace's `AGENT.md` brief (the Overview editor).
    SaveBrief {
        workspace: String,
        content: String,
    },
    // ── Files view (browse + edit a workspace) ──
    /// Select a directory + (re)project the tree (the `/w/:wid/files` tab + breadcrumb navigation).
    ListDir {
        workspace: String,
        path: String,
    },
    /// Toggle a folder's expansion in the tree (and select it as the create target).
    ToggleDir {
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
    let command =
        boot::env("CLAW_WEB_MCP_CMD").unwrap_or_else(|()| mcp::DEFAULT_WEB_COMMAND.to_owned());
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
    let Ok(extra) = boot::env("CLAW_WEB_MCP_LD_PATH") else {
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
    boot::boot(&conn, &shared);
    boot::log_sandbox_mode();
    connect_web_mcp();
    admin::project_tool_catalog(&shared);
    std::thread::spawn(move || {
        while let Ok(command) = rx.recv() {
            handle(&conn, &shared, command);
        }
    });
    scheduler::spawn_ticker();
}

fn handle(conn: &rusqlite::Connection, shared: &SharedServerState, command: Command) {
    match command {
        Command::DeleteChat(id) => delete_chat(conn, shared, &id),
        Command::ListArchived => list_archived(conn, shared),
        Command::Submit { chat, text } => chat_turn::submit(conn, shared, &chat, &text),
        Command::AgentReply { chat, result } => chat_turn::agent_reply(conn, shared, &chat, result),
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
        Command::SetWorkspaceAutoApprove { id, on } => {
            admin::set_workspace_auto_approve(conn, shared, &id, on)
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
        Command::LoadTaskDetail { task_id } => tasks::load_detail(conn, shared, &task_id),
        Command::LoadRequestDetail { request_id } => {
            requests::load_detail(conn, shared, &request_id)
        }
        Command::LoadProjectDetail { workspace, project } => {
            requests::view::load_project_detail(conn, shared, &workspace, &project)
        }
        Command::SetProjectKind {
            workspace,
            project,
            slug,
        } => requests::view::set_project_kind(conn, shared, &workspace, &project, &slug),
        Command::RequestValidated {
            request_id,
            decision,
            note,
        } => requests::control::on_validated(conn, shared, &request_id, &decision, &note),
        Command::RequestDeliver { id } => requests::control::deliver_now(conn, shared, &id),
        Command::RequestAbandon { id } => requests::control::abandon(conn, shared, &id),
        Command::RequestRollback { id } => requests::control::rollback(conn, shared, &id),
        Command::RequestPush { id } => requests::control::push(conn, shared, &id),
        Command::RestoreFile {
            workspace,
            task,
            repo,
            sha,
            path,
        } => requests::control::restore_file(conn, shared, &workspace, &task, &repo, &sha, &path),
        Command::OpenAgentThread { agent, workspace } => {
            agent_thread::load(conn, shared, &agent, &workspace)
        }
        Command::ExpandAgentThread => agent_thread::expand(conn, shared),
        Command::CloseAgentThread => agent_thread::close(shared),
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
            review::project_awaiting(conn, shared);
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
            max_retries,
        } => tasks::create(
            conn,
            shared,
            tasks::Create {
                workspace,
                title,
                spec,
                agent,
                schedule,
                max_retries,
            },
        ),
        Command::UpdateTask {
            id,
            title,
            spec,
            agent,
            schedule,
            max_retries,
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
                max_retries,
                blockers,
            },
        ),
        Command::CancelTask { id } => tasks::cancel(conn, shared, &id),
        Command::StopTask { id } => tasks::stop(conn, shared, &id),
        Command::StartTask { id } => tasks::resume(conn, shared, &id),
        Command::RestartTask { id } => tasks::restart(conn, shared, &id),
        Command::ArchiveTask { id } => tasks::archive(conn, shared, &id),
        Command::UnarchiveTask { id } => tasks::unarchive(conn, shared, &id),
        Command::SchedulerTick => scheduler::tick(conn, shared),
        Command::AgentProgress { ctx, event } => live_progress(shared, ctx, event),
        Command::TaskFinished {
            id,
            role,
            epoch,
            result,
            memory,
        } => scheduler::finished(conn, shared, &id, role, epoch, result, memory),
        Command::Ask {
            workspace,
            request,
            origin_chat,
        } => orchestrate::ask(conn, &workspace, &request, &origin_chat),
        Command::AskPlanned {
            workspace,
            origin_chat,
            plan_id,
            result,
        } => orchestrate::planned(conn, shared, &workspace, &origin_chat, &plan_id, result),
        Command::Notified { chat, text } => chat_turn::notified(conn, shared, &chat, &text),
        Command::UpdateMemory {
            agent_id,
            workspace,
            summary,
            summarized_through,
            note,
            source,
        } => {
            let scope = workspace.as_deref();
            if let Err(err) =
                db::agent_memory::set(conn, &agent_id, scope, &summary, summarized_through)
            {
                log_line(format!("bridge: update memory failed: {err}"));
            }
            memory::journal(
                conn,
                &agent_id,
                scope,
                db::agent_memory_log::MemoryEventKind::Fold,
                source.as_deref(),
                &note,
                &summary,
            );
            project_memory_surfaces(conn, shared);
        }
        Command::SteerConsumed => agent_thread::refresh_if_open(conn, shared),
        Command::SteerAgent {
            agent,
            workspace,
            body,
        } => steer::send_message(conn, shared, &agent, &workspace, &body),
        Command::SaveChecks { workspace, command } => {
            files::save_checks(conn, shared, &workspace, &command)
        }
        Command::SaveStandards { workspace, content } => {
            files::save_standards(conn, shared, &workspace, &content)
        }
        Command::SaveBrief { workspace, content } => {
            files::save_brief(conn, shared, &workspace, &content)
        }
        Command::ListDir { workspace, path } => files::list_dir(conn, shared, &workspace, &path),
        Command::ToggleDir { workspace, path } => {
            files::toggle_dir(conn, shared, &workspace, &path)
        }
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
    run_system_prompt_with_project(base, workspace_dir, agent_name, agent_prompt, None, memory)
}

/// [`run_system_prompt`] plus the project layer (P1): kind conventions + machine contracts
/// for the specific repo a task works in. Task/planning runs resolve it from the task's
/// target project; chat turns (concierge, not project-scoped) pass `None`.
fn run_system_prompt_with_project(
    base: &str,
    workspace_dir: Option<&Path>,
    agent_name: &str,
    agent_prompt: Option<String>,
    project: Option<String>,
    memory: Option<String>,
) -> String {
    if let Some(dir) = workspace_dir
        && let Err(err) = workspace::scaffold(dir, agent_name)
    {
        log_line(format!("bridge: workspace scaffold failed: {err}"));
    }
    let brief = workspace_dir.and_then(workspace::read_agent_md);
    workspace::compose_system_prompt(base, agent_prompt, brief, project, memory)
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

/// Project the concierge's global memory for the chat page's fold line + card (R3.5 M3) —
/// parity with the agent page: the chat is the concierge's thread, so it shows what the
/// concierge remembers. Cheap (two small reads); called after turns and memory mutations.
fn project_chat_memory(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let Ok(Some(agent_id)) = agents::web_agent(conn) else {
        return;
    };
    let agent_name = agents::get(conn, &agent_id)
        .ok()
        .flatten()
        .map(|agent| agent.name)
        .unwrap_or_else(|| "the concierge".to_owned());
    let row = db::agent_memory::get(conn, &agent_id, None).unwrap_or_default();
    let last_fold = db::agent_memory_log::recent(conn, &agent_id, None, 50)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find(|event| event.kind == db::agent_memory_log::MemoryEventKind::Fold)
        .map(|event| format!("{} — {}", hh_mm(&event.created_at), event.note))
        .unwrap_or_default();
    let memory = crate::state::ChatMemory {
        agent_id,
        agent_name,
        summary: row.summary,
        paused: row.paused,
        last_fold,
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_CHAT_MEMORY]),
        move |app| app.chat_memory = Some(memory),
    );
}

/// Project the System page's memory overview (R3.5 M5): every (agent, workspace) scope row
/// with its size, freshness, and pause flag. Small (one row per scope); refreshed with the
/// chat-memory projection so a fold landing anywhere keeps the overview honest.
fn project_memory_overview(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let agents_by_id: Vec<(String, String)> = agents::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();
    let workspaces_by_id: Vec<(String, String)> = workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|ws| (ws.id, ws.name))
        .collect();
    let name_of = |pairs: &[(String, String)], id: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| id.to_owned())
    };
    let rows: Vec<crate::state::MemoryOverviewRow> = db::agent_memory::list_all(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let (scope, route) = if row.workspace_id.is_empty() {
                ("global".to_owned(), "/".to_owned())
            } else {
                (
                    name_of(&workspaces_by_id, &row.workspace_id),
                    format!("/w/{}/agent/{}", row.workspace_id, row.agent_id),
                )
            };
            crate::state::MemoryOverviewRow {
                agent_name: name_of(&agents_by_id, &row.agent_id),
                scope,
                route,
                chars: row.summary.chars().count(),
                updated: row
                    .updated_at
                    .get(..16)
                    .unwrap_or_default()
                    .replace('T', " "),
                paused: row.paused,
            }
        })
        .collect();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_MEMORY_OVERVIEW]),
        move |app| app.memory_overview = rows,
    );
}

/// Re-project every surface that shows memory — the open agent thread (a fold can land while
/// its page is open), the chat card, and the System overview. Called after any memory mutation.
fn project_memory_surfaces(conn: &rusqlite::Connection, shared: &SharedServerState) {
    agent_thread::refresh_if_open(conn, shared);
    project_chat_memory(conn, shared);
    project_memory_overview(conn, shared);
}

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
pub(super) fn warm_context_cache(inference: &resolution::ResolvedInference) {
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
/// The execution strategy for a run's `bash` (and machine checks), from the resolved
/// [`SandboxMode`]. In `Isola` mode the sandbox is named per lane ([`crate::sandbox::name_for`]:
/// `claw-<ws>` canonical, `claw-<ws>-<req8>` for a request tree) and the mount is the lane root
/// itself — every lane gets its own sandbox, torn down with the tree.
pub(crate) fn exec_strategy_for(workspace: &Path) -> crate::tools::ExecStrategy {
    use crate::sandbox::SandboxMode;
    use crate::tools::ExecStrategy;
    match SandboxMode::resolve(boot::env("CLAW_SANDBOX").ok().as_deref()).effective() {
        SandboxMode::Isola => ExecStrategy::Isola {
            sandbox: crate::sandbox::name_for(workspace),
            mount: workspace.to_path_buf(),
        },
        SandboxMode::Host => ExecStrategy::Host {
            root: workspace.to_path_buf(),
        },
    }
}

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
        let exec = exec_strategy_for(&workspace);
        let coder: Box<dyn Toolbox> = Box::new(CoderToolbox::with_exec(workspace, exec));
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

/// `2026-06-22T08:00:…Z` → `08:00` — the transcript-grain stamp every projection shows.
pub(super) fn hh_mm(stamp: &str) -> String {
    stamp.get(11..16).unwrap_or_default().to_owned()
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
