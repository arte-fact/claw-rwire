//! Application state. Everything lives in the process-global `App`
//! (`#[storage(shared)]`) — the projection the pipeline writes into and every
//! connection reads. View-local flags (the mobile nav toggle, the System Map drawer
//! target) live here too rather than in a per-connection `#[storage(memory)]` state:
//! a *shared*-state renderer nested inside a *memory*-state renderer doesn't receive
//! the shared cache, and the sidebar's status + thread list are shared-state renderers
//! that sit inside the drawer. For a single operator, sharing these across tabs is
//! benign (the desktop sidebar ignores `nav_open` via a breakpoint override anyway).

use rwire::State;

/// The top-level navigation surfaces (ARCHITECTURE: chat + the System Map admin +
/// activity/logs). Defines the drawer's nav menu; the rwire router renders each
/// path's view and the active item is derived from the live `CurrentRoute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Chat,
    Dashboard,
    Review,
    Archived,
    Prompts,
    System,
    Activity,
    Logs,
}

impl Tab {
    /// The app-level surfaces shown in the nav footer. Chat is intentionally absent — the
    /// conversation list above the nav *is* the chat surface. Dashboard leads (the
    /// cross-workspace overview); Files is gone (it's now a per-workspace tab).
    pub const NAV: [Tab; 7] = [
        Tab::Dashboard,
        Tab::Review,
        Tab::Archived,
        Tab::Prompts,
        Tab::System,
        Tab::Activity,
        Tab::Logs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Dashboard => "Dashboard",
            Tab::Review => "Review",
            Tab::Archived => "Archived",
            Tab::Prompts => "Prompts",
            Tab::System => "System",
            Tab::Activity => "Activity",
            Tab::Logs => "Logs",
        }
    }

    /// The URL path this tab lives at. Chat is the app root.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Chat => "/",
            Tab::Dashboard => "/dashboard",
            Tab::Review => "/review",
            Tab::Archived => "/archived",
            Tab::Prompts => "/prompts",
            Tab::System => "/system",
            Tab::Activity => "/activity",
            Tab::Logs => "/logs",
        }
    }

    /// The leading icon for this tab's nav row, so the nav rail is iconned uniformly
    /// (matching New chat, the row actions, and Log out).
    pub fn icon(self) -> rwire::icons::Icon {
        use rwire::icons::Icon;
        match self {
            Tab::Chat => Icon::MessageSquare,
            Tab::Dashboard => Icon::Home,
            Tab::Review => Icon::Check,
            Tab::Archived => Icon::Archive,
            Tab::Prompts => Icon::Terminal,
            Tab::System => Icon::Cpu,
            Tab::Activity => Icon::Activity,
            Tab::Logs => Icon::FileText,
        }
    }
}

/// One row in the chat sidebar (projected from `messaging_groups` / web threads). The
/// busy indicator is derived from `App.running`, not stored here.
#[derive(Debug, Clone, Default)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
}

/// One run-lifecycle event for the Activity board (newest first, capped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// `HH:MM` (UTC).
    pub time: String,
    /// The conversation's title at the time of the event.
    pub title: String,
    pub kind: ActivityKind,
    /// Extra context — the failure message for `Failed`, empty otherwise.
    pub detail: String,
    /// Linked context (task lifecycle events): the task + workspace, so the row deep-links to
    /// the task detail. `None` for chat events.
    pub task_id: Option<String>,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Started,
    Finished,
    Failed,
}

/// One live agent action (a tool call, in flight or finished) for the real-time feed — the
/// Activity board's live section and a running task's detail. Carries linked context (agent ·
/// workspace · task) so the feed can deep-link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEvent {
    /// `HH:MM:SS` (UTC).
    pub time: String,
    pub agent: String,
    pub workspace_id: String,
    pub workspace: String,
    pub task_id: String,
    pub task: String,
    /// What the agent is doing / did — e.g. "writing src/main.rs" → "wrote src/main.rs".
    pub action: String,
    /// True while the tool is running; false once it's finished.
    pub in_progress: bool,
    /// The tool's success, once finished.
    pub ok: bool,
}

/// The streamed reply text for a currently-running task — grows token-by-token as the model
/// types, shown in the task detail's Live card. Cleared when the run is recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveText {
    pub task_id: String,
    pub text: String,
}

/// The linked context for a run's live events — resolved once when the run launches and carried
/// on every `AgentProgress` so the bridge never re-queries per event (Tier 2 streams fast).
#[derive(Debug, Clone)]
pub struct ProgressCtx {
    pub task_id: String,
    pub task: String,
    pub agent: String,
    pub workspace_id: String,
    pub workspace: String,
}

/// One captured operational log line for the Logs viewer (newest first, capped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// `HH:MM` (UTC).
    pub time: String,
    pub message: String,
}

/// Who authored a transcript turn. `in` rows are the operator; `out` rows are the
/// agent (the web channel's two `web_messages` directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Agent,
}

/// One rendered turn in the open thread.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub text: String,
    /// `HH:MM` (UTC) for the transcript timestamp.
    pub time: String,
}

/// One item in the rendered thread: a chat turn or a tool-call card, interleaved in
/// the order they happened.
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    Message(ChatMessage),
    Tool(ToolCardView),
}

/// A tool invocation rendered as a collapsible card: what ran, with what arguments,
/// the (full) result behind a disclosure, and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCardView {
    pub name: String,
    pub arguments: String,
    pub result: String,
    pub ok: bool,
    /// `HH:MM` (UTC).
    pub time: String,
}

/// An endpoint row for the System Map. The secret is never projected — only whether
/// one is set — so the read-model can't leak it to the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointView {
    pub name: String,
    pub base_url: String,
    pub has_key: bool,
    pub api_key_env: Option<String>,
}

/// An agent row for the System Map, with its endpoint link and model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

/// A workspace row for the System Map's Workspaces tier. A workspace is a shareable
/// directory under `<data>/workspaces/<id>` with an **owner** (its domain lead) and a team
/// of **members**. `owner` is the owning agent id; `members` the member agent ids;
/// `member_names` the members' display names (for the card summary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceView {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub members: Vec<String>,
    pub member_names: Vec<String>,
    /// When set, delegated work skips the R3 plan-review gate (tasks queue directly).
    pub auto_approve: bool,
}

/// One task card in a workspace's lane (projected from the `tasks` table). `agent` is the
/// assignee's display name (or `None` = the owner); `status` is the lifecycle string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    pub spec: String,
    pub agent: Option<String>,
    /// The assignee's raw agent id (for pre-selecting the edit drawer's reassign control).
    pub agent_id: Option<String>,
    pub status: String,
    pub recurrent: bool,
    /// The cron schedule (recurrent tasks), shown on the card + edited in the drawer.
    pub schedule: Option<String>,
    /// Ids of tasks blocking this one (its dependencies), for the drawer's blocker field.
    pub blockers: Vec<String>,
    /// Next scheduled fire (`HH:MM` UTC, or a date) for a recurrent task awaiting its slot.
    pub next_run: Option<String>,
    /// A short snippet of the last run's result/error, shown on terminal cards.
    pub result: Option<String>,
    /// The current pipeline step (the kanban column the card lives in, Phase F).
    pub step_position: i64,
    /// The iteration within a loop step (shown on the card when > 0).
    pub iteration: i64,
    /// Auto-retries consumed so far (shown on the card while a failed run is retrying).
    pub auto_retries: i64,
    /// The per-task auto-retry budget override, prefilled in the edit drawer (`None` = default).
    pub max_retries: Option<i64>,
    /// The manual-restart generation (shown as "attempt N" once > 0).
    pub attempt: i64,
    /// When a retry is scheduled next (`HH:MM`), if the task is holding on a backoff.
    pub next_retry: Option<String>,
}

/// One workspace's lane at a glance, for the global dashboard's all-lanes view: the title of
/// the task currently running (if any) and how many are queued behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneSummary {
    pub workspace_id: String,
    pub running: Option<String>,
    pub queued: usize,
    /// Tasks parked at a human gate in this workspace — the "needs you" count.
    pub awaiting: usize,
}

/// Which kind of run the Prompts inspector previews — picks the role-preamble layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunKind {
    /// A chat turn (the concierge).
    #[default]
    Chat,
    /// A task execution turn (a producer in a workspace pipeline).
    Task,
    /// An owner planning turn.
    Planning,
    /// A validation turn (a reviewer judging a producer's output in a loop step).
    Validator,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            RunKind::Chat => "Chat",
            RunKind::Task => "Task",
            RunKind::Planning => "Planning",
            RunKind::Validator => "Validator",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RunKind::Chat => "chat",
            RunKind::Task => "task",
            RunKind::Planning => "planning",
            RunKind::Validator => "validator",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "task" => RunKind::Task,
            "planning" => RunKind::Planning,
            "validator" => RunKind::Validator,
            _ => RunKind::Chat,
        }
    }

    pub const ALL: [RunKind; 4] = [
        RunKind::Chat,
        RunKind::Task,
        RunKind::Planning,
        RunKind::Validator,
    ];
}

/// Which run just finished, so the pipeline engine knows how to route the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRole {
    /// A plain stage (or a loop's producer that has no validator after it).
    Stage,
    /// A loop step's producer — its validator runs next.
    Producer,
    /// A loop step's validator — its `verdict` routes the loop.
    Validator,
}

/// The Prompts inspector's read-only assembly: each of the 4 system-prompt layers (empty =
/// absent) for the selected agent + run-kind, plus the composed result the model would get.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptPreview {
    /// The agent + run-kind + representative workspace this preview is for (header context).
    pub agent_name: String,
    pub run: String,
    pub workspace: String,
    pub role: String,
    pub persona: String,
    pub brief: String,
    pub memory: String,
    pub assembled: String,
    /// Memory metadata for the panel: a size/marker line and whether it's paused.
    pub memory_meta: String,
    pub memory_paused: bool,
}

/// The effective (override-or-default) role preambles — the editable global behaviour rules
/// shown on the Prompts view's "Global role defaults" section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleDefaults {
    pub concierge: String,
    pub executor: String,
    pub planner: String,
    pub validator: String,
}

/// One task parked at a human gate, shown in the cross-workspace Review inbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewItem {
    pub task_id: String,
    pub title: String,
    /// The workspace id (for links) and display name.
    pub workspace_id: String,
    pub workspace: String,
    pub spec: String,
    /// The producer's latest output — the work to review (fallback when `run` is absent).
    pub output: String,
    /// The producer's latest run — its tool transcript + output, so the inbox shows the full
    /// story (what files/commands), not just a snippet. `None` if no run was recorded yet.
    pub run: Option<RunView>,
    /// Whether this is a reviewable **Plan** task (R3) — the inbox shows its proposed tasks.
    pub is_plan: bool,
    /// For a plan: the proposed (held) child tasks the user is approving.
    pub proposed: Vec<ProposedTask>,
}

/// One proposed task in a plan under review (R3) — shown as a card in the Review inbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposedTask {
    pub title: String,
    pub spec: String,
    /// The assignee's display name, or `None` for the owner.
    pub agent: Option<String>,
    /// Cron schedule for a recurring proposed task, if any.
    pub schedule: Option<String>,
}

/// One run in a task's chain, for the detail view's transcript (Phase F-4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunView {
    /// `stage` / `producer` / `validator`.
    pub role: String,
    /// The agent's display name, or "you" for a human verdict.
    pub agent: String,
    pub output: String,
    pub verdict: Option<String>,
    pub feedback: Option<String>,
    pub iteration: i64,
    pub ok: bool,
    /// The run's tool calls (from the persisted transcript) — the full story of what it did.
    pub tools: Vec<ToolCardView>,
    /// The task attempt this run belongs to — the detail view groups runs by attempt.
    pub attempt: i64,
}

/// A task's detail view: its spec, status, and the full run chain (the audit trail).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetail {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub spec: String,
    pub status: String,
    /// Whether the task is parked at a human gate (the actions show).
    pub awaiting: bool,
    /// Whether the task is archived (hidden from the board) — its control bar offers Unarchive.
    pub archived: bool,
    pub runs: Vec<RunView>,
}

/// One pipeline step in the Workflow editor (the configured row). `kind` is `stage`/`loop`;
/// the validator fields apply to loops. Agent ids (not names) so the form's selects pre-fill.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepView {
    pub id: String,
    pub position: i64,
    pub kind: String,
    pub name: String,
    pub agent: Option<String>,
    pub prompt: Option<String>,
    pub validator_kind: Option<String>,
    pub validator_agent: Option<String>,
    pub validator_prompt: Option<String>,
    pub max_iterations: i64,
    pub on_exhaust: String,
}

/// The Workflow editor's step-form payload (owned). Empty agent/prompt strings mean "none";
/// the bridge normalizes by kind before persisting.
#[derive(Debug, Clone, Default)]
pub struct StepForm {
    pub kind: String,
    pub name: String,
    pub agent: String,
    pub prompt: String,
    pub validator_kind: String,
    pub validator_agent: String,
    pub validator_prompt: String,
    pub max_iterations: i64,
    pub on_exhaust: String,
}

/// One selectable tool in the agent drawer's tool picker: its source `group`
/// (`"web"` / `"coder"`) and `name`. Projected once at boot from the connected MCP
/// server + the built-in coder tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCatalogEntry {
    pub group: String,
    pub name: String,
}

/// What a Files-view entry is. Mirrors `workspace::ops::EntryKind` as a UI-side value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    #[default]
    File,
    Symlink,
}

/// One row in the Files view's directory listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
}

/// One node in the Files view's tree (a flattened, depth-tagged listing — root plus the children
/// of every expanded folder, in display order). The bridge owns which folders are expanded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileNode {
    pub name: String,
    /// Workspace-relative path (the create/open/rename/delete target).
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    /// Nesting depth from the root (0 = top level) — the row's indent.
    pub depth: usize,
    /// For a directory: whether it's expanded (its children follow). Always false for files.
    pub expanded: bool,
}

/// The file open in the Files view's right pane. `editable` is false for binary/oversized
/// files, with `note` explaining why; the editor is then replaced by that note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenFile {
    pub path: String,
    pub content: String,
    pub editable: bool,
    pub note: Option<String>,
}

/// What the System Map's right drawer is editing, if open. Endpoints/Channel keep the
/// row *key* (the drawer reads current values from `App`); the agent form is
/// *controlled* — it carries a live draft so it can re-render (when discovered models
/// arrive) without resetting the operator's in-progress input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editing {
    Endpoint(Option<String>),
    Agent(AgentDraft),
    Channel,
}

/// In-progress agent edit. `id == None` is a new agent; the rest mirror the form
/// fields so a re-render restores them. `prompt` is the agent's identity prompt (its
/// persona, stored in `agent_prompts`) and `tools` its enabled tool names — both loaded
/// from the DB when an existing agent is opened. Workspace ownership + membership are
/// managed on the workspace side (the Workspaces tier), not here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentDraft {
    pub id: Option<String>,
    pub name: String,
    pub endpoint: Option<String>,
    pub model: String,
    pub prompt: String,
    pub tools: Vec<String>,
}

/// Process-global projection. Selection (`open_chat`) lives here rather than in
/// per-connection `Ui` so a single `App`-typed renderer can draw the thread; for a
/// single-operator assistant, tabs sharing the open chat is acceptable (and matches
/// the reference app's shared-selection model).
#[derive(Debug, Default, State)]
#[storage(shared)]
pub struct App {
    /// Set by the bridge once the pipeline thread has booted and primed state.
    pub pipeline_ready: bool,
    /// Chat threads for the sidebar.
    pub chats: Vec<ChatSummary>,
    /// Archived threads, shown on the dedicated `/archived` screen. Filled by the
    /// bridge (`Command::ListArchived`) when that route opens.
    pub archived: Vec<ChatSummary>,
    /// The archived chat awaiting a delete confirmation (its row shows a confirm/cancel
    /// prompt). `None` when no delete is pending. Delete is irreversible, so it's a
    /// two-step inline confirm rather than a one-click action.
    pub pending_delete: Option<String>,
    /// The currently open thread id (shared across this operator's tabs).
    pub open_chat: Option<String>,
    /// Transcript of the open thread — chat turns and tool cards, interleaved.
    pub messages: Vec<TranscriptItem>,
    /// The agent is producing a reply on the open thread.
    pub sending: bool,
    /// A transient error from the last turn (e.g. the endpoint failed). Shown as a
    /// banner; never persisted, cleared on the next send.
    pub error: Option<String>,
    /// System Map: registry read-models, projected by the bridge.
    pub endpoints: Vec<EndpointView>,
    pub agents: Vec<AgentView>,
    /// The agent id the web channel currently routes to.
    pub web_agent: Option<String>,
    /// The System Map drawer's current target, or `None` when it's closed.
    pub editing: Option<Editing>,
    /// Models discovered from the agent draft's selected endpoint (`/v1/models`),
    /// populating the agent form's model dropdown. Empty until an endpoint is chosen.
    pub endpoint_models: Vec<String>,
    /// Every selectable tool (web + coder), projected once at boot — the source for the
    /// agent drawer's tool picker.
    pub tool_catalog: Vec<ToolCatalogEntry>,
    /// Workspaces (shareable agent project dirs), projected with the registry — the source
    /// for the sidebar tree + the workspace dashboards/Team.
    pub workspaces: Vec<WorkspaceView>,
    /// The id of the workspace the current `/w/:wid…` route points at, or `None` off any
    /// workspace route. Set by `on_route`; the sidebar highlights it and the workspace
    /// header/dashboard/Team renderers look its `WorkspaceView` up in `workspaces`.
    pub active_workspace: Option<String>,
    /// The Team tab's delete confirm is armed (deletion removes the workspace directory, so
    /// it's a two-step action). Cleared on navigation.
    pub workspace_delete_armed: bool,
    /// The active workspace's `AGENT.md` brief, loaded when a workspace route opens — backs
    /// the Overview tab's inline brief editor.
    pub workspace_brief: String,
    /// The active workspace's task lane (projected when its Tasks tab opens).
    pub tasks: Vec<TaskView>,
    /// The active workspace's pipeline step names — the kanban column headers (Phase F).
    pub pipeline_columns: Vec<String>,
    /// Tasks tab view: `true` = the flat list (mobile-friendly), `false` = the kanban board.
    pub tasks_list_view: bool,
    /// Whether the "New task" side panel is open. Pure UI state, toggled by the New-task button.
    pub new_task_panel_open: bool,
    /// The id of the task whose inline edit drawer is open (retitle / reassign / schedule /
    /// blockers), or `None`. Pure UI state, toggled by the card's edit button.
    pub editing_task: Option<String>,
    /// Per-workspace lane summary for the global dashboard's all-lanes view (running task +
    /// queued count), refreshed on each scheduler tick and when the dashboard opens.
    pub dashboard_lanes: Vec<LaneSummary>,
    /// The Prompts inspector's current selection (agent + run-kind) and composed preview.
    pub prompt_agent: Option<String>,
    pub prompt_run: RunKind,
    pub prompt_preview: Option<PromptPreview>,
    /// The editable global role preambles (effective values), for the Prompts defaults section.
    pub role_defaults: RoleDefaults,
    /// Tasks parked at a human gate (the Review inbox) + the count (for the nav badge).
    pub review_inbox: Vec<ReviewItem>,
    pub review_count: usize,
    /// The task whose detail view (run chain) is open, or `None`.
    pub task_detail: Option<TaskDetail>,
    /// The task whose **Peek** modal is open over the board, or `None` (closed).
    pub selected_task: Option<String>,
    /// The Peek modal's active tab: 0 = Overview, 1 = Activity, 2 = Review.
    pub detail_tab: usize,
    /// Run-chain tool rows the user toggled away from their default (collapsed, except a run's
    /// latest call). A tool is open iff `is_last XOR tool_toggled.contains(key)`.
    pub tool_toggled: Vec<String>,
    /// The board's active quick-filter: `all` | `awaiting` | `running` | `blocked` | `failed`.
    pub board_filter: String,
    /// The active workspace's configured pipeline steps (the Workflow editor).
    pub workflow_steps: Vec<StepView>,
    /// The pipeline editor is locked because the workspace has in-flight tasks.
    pub workflow_locked: bool,
    /// The step whose inline edit form is open (the Workflow editor), or `None`.
    pub editing_step: Option<String>,
    /// Files view: the id of the workspace being browsed.
    pub files_workspace: Option<String>,
    /// Files view: the current directory, relative to the workspace root (`""` = root).
    pub files_path: String,
    /// Files view: entries of `files_path` (dirs first). Legacy flat listing (kept for callers
    /// that still read it); the browser renders [`Self::files_tree`].
    pub files_entries: Vec<FileEntry>,
    /// Files view: the workspace tree — root plus the children of every expanded folder, flattened
    /// with depth for indented rendering.
    pub files_tree: Vec<FileNode>,
    /// Files view: the file open in the editor pane, if any.
    pub files_open: Option<OpenFile>,
    /// Files view: editor mode — `true` = rendered View (Markdown / highlighted code), `false` =
    /// raw Edit (the textarea). Reset to View each time a file opens. Pure UI state.
    pub files_view_mode: bool,
    /// Files view: which inline create prompt is open (`"file"` / `"folder"`), if any.
    pub files_creating: Option<String>,
    /// Files view: the controlled name input in the inline create prompt.
    pub files_new_name: String,
    /// Files view: the entry path currently being renamed (its row shows an input).
    pub files_rename: Option<String>,
    /// Files view: the entry path awaiting a delete confirm.
    pub files_pending_delete: Option<String>,
    /// Files view: a transient error/result banner (jailed path, write failure, …).
    pub files_error: Option<String>,
    /// Chat ids with an in-flight agent run (drives the sidebar busy dots).
    pub running: Vec<String>,
    /// Recent run-lifecycle events for the Activity board (newest first, capped).
    pub activity: Vec<ActivityEntry>,
    /// Live agent actions (tool calls in flight / just finished), newest first, capped — the
    /// real-time feed for the Activity board + a running task's detail.
    pub live_activity: Vec<LiveEvent>,
    /// Streamed reply text per currently-running task (the typewriter), shown in the detail.
    pub live_text: Vec<LiveText>,
    /// Recent operational log lines for the Logs viewer (newest first, capped).
    pub logs: Vec<LogLine>,
    /// Whether the mobile slide-in nav drawer is open. The desktop sidebar is always
    /// visible (a breakpoint override), so this only governs mobile.
    pub nav_open: bool,
}
