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
    Archived,
    Prompts,
    System,
    Activity,
    Logs,
}

impl Tab {
    /// The app-level surfaces shown in the nav footer. Chat is intentionally absent — the
    /// conversation list above the nav *is* the chat surface. Dashboard leads (the
    /// cross-workspace overview); Files is gone (it's now a per-workspace tab), and review is
    /// managed per-workspace (board + Peek), so there's no global Review surface.
    pub const NAV: [Tab; 6] = [
        Tab::Dashboard,
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

/// One item in a running task's live turn — the interleaved stream as it arrives, so the thread
/// shows the real `text → tool → text` sequence instead of all tools then all text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveItem {
    /// A streamed reply segment (grows token-by-token until a tool call breaks it).
    Text(String),
    /// A tool call: what it's doing / did, and its state.
    Tool {
        /// `HH:MM:SS` (UTC).
        time: String,
        /// Humanized action — e.g. "reading src/main.rs" → "read src/main.rs".
        action: String,
        /// True while the tool is running; false once finished.
        in_progress: bool,
        /// The tool's success, once finished.
        ok: bool,
    },
}

/// A currently-running task's live turn: its items in arrival order. Cleared when the run is
/// recorded (the persisted transcript then takes over as the source of truth).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveTurn {
    pub task_id: String,
    pub items: Vec<LiveItem>,
}

impl LiveTurn {
    /// Extend the current text segment; a tool call in between means the next chunk starts a new
    /// segment, so the stream keeps its real `text → tool → text` order.
    pub fn push_text(&mut self, chunk: &str) {
        match self.items.last_mut() {
            Some(LiveItem::Text(buffer)) => buffer.push_str(chunk),
            _ => self.items.push(LiveItem::Text(chunk.to_owned())),
        }
    }

    /// Record a tool call starting (in-flight) at this point in the stream.
    pub fn start_tool(&mut self, time: String, action: String) {
        self.items.push(LiveItem::Tool {
            time,
            action,
            in_progress: true,
            ok: false,
        });
    }

    /// Flip the most recent still-running tool to finished with `ok`.
    pub fn finish_tool(&mut self, ok: bool) {
        if let Some(LiveItem::Tool {
            in_progress,
            ok: tool_ok,
            ..
        }) = self.items.iter_mut().rev().find(|item| {
            matches!(
                item,
                LiveItem::Tool {
                    in_progress: true,
                    ..
                }
            )
        }) {
            *in_progress = false;
            *tool_ok = ok;
        }
    }
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
    /// The memory scope this run kind reads (R3 injection matrix): `None` = the agent's global
    /// row (chat), `Some(workspace)` = that workspace's row (task/planning runs). The panel's
    /// Save / Clear / Pause act on this row.
    pub memory_scope: Option<String>,
    /// Whether this run kind gets a memory layer at all — false for validators, whose runs are
    /// deliberately memory-free (the panel explains instead of editing).
    pub memory_applies: bool,
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

/// One proposed task in a plan under review (R3) — shown in the Peek modal's Review tab when a
/// plan task is parked at its gate.
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
    /// `HH:MM` (UTC) the run was recorded — the thread entry's timestamp.
    pub time: String,
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
    /// Whether this is a reviewable **Plan** task (R3) — the Review tab lists its proposed tasks.
    pub is_plan: bool,
    /// For a plan: the proposed (held) child tasks the user is approving (empty otherwise).
    pub proposed: Vec<ProposedTask>,
    pub runs: Vec<RunView>,
}

/// A run's phase label: the role as a verb (`producer→Produce`, `validator→Review`, else
/// `Stage`), plus the loop iteration when past the first. Shared by the run-chain entries and
/// the agent thread's conversational turns, so a phase reads identically on both.
#[must_use]
pub fn role_phase(role: &str, iteration: i64) -> String {
    let base = match role {
        "validator" => "Review",
        "producer" => "Produce",
        _ => "Stage",
    };
    if iteration > 0 {
        format!("{base} · iter {}", iteration + 1)
    } else {
        base.to_owned()
    }
}

/// The task a thread turn belongs to — enough for its deep link into the board's Peek, where
/// the full forensics (tool transcripts) live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskLink {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
}

/// One conversational turn in an agent's thread (R2): chat grain — prose, not forensics. A turn
/// is the task being opened (author "you", body = the spec), one of the agent's own runs (its
/// reply prose), or an incoming reviewer's verdict + feedback. Tool work is carried only as a
/// count — the muted "n tool calls · view task" line links to the Peek for the full story.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTurn {
    /// Who speaks: "you" for the operator (specs, human verdicts), else an agent's display name.
    pub author: String,
    /// The author is this thread's agent — its own reply (markdown body, accent author).
    pub own: bool,
    /// `HH:MM` stamp.
    pub time: String,
    /// Phase tag: "task" for an opened task, else the run's phase ("Produce · iter 2", "Review").
    pub tag: String,
    /// The turn's prose: the spec, the run's reply, or the reviewer's output.
    pub body: String,
    /// A validator's concrete asked changes, shown as an accent caption under the body.
    pub feedback: Option<String>,
    /// Verdict chip (validator turns): `pass` / `revise` / `reject`.
    pub verdict: Option<String>,
    /// How many tool calls the run made (0 = no meta line).
    pub tool_count: usize,
    /// The run succeeded (a failed run shows the error chip).
    pub ok: bool,
    /// The task this turn belongs to, for the deep link.
    pub task: TaskLink,
    /// Show the task link on the meta line (opened turns and out-of-lane reviews always do).
    pub link_task: bool,
}

/// One entry in an agent's thread: a conversational turn, a parked task's inline review
/// composer (kept at the tail — the pending question sits at the bottom, like a chat), or a
/// memory event (R3.5 M2) — a fold/edit/clear rendered as one muted line at the moment it
/// happened, so history becoming memory is watchable. The turn is boxed so the small variants
/// don't pay the large one's size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentThreadItem {
    Turn(Box<AgentTurn>),
    ReviewGate { task_id: String },
    Memory { time: String, note: String },
}

/// A currently-running task of the agent, for its live streaming entry on the agent page. The
/// live items themselves come from `App.live_turns` (keyed by `task_id`), so a token flush
/// repaints only the live renderer, not the whole timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentLive {
    pub task_id: String,
    pub title: String,
    /// The current step label (`Build` / `Build · iter 2` / `Running`) for the entry's tag.
    pub tag: String,
}

/// An agent's thread **in one workspace**: its identity plus the merged, time-ordered,
/// **windowed** conversation of its work there and any live runs — the projection the
/// `/w/:wid/agent/:aid` page renders. Workspace-scoped to mirror the context model: an agent in
/// two workspaces has two threads, as it has two memories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentThread {
    pub agent_id: String,
    pub agent_name: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub items: Vec<AgentThreadItem>,
    /// The agent's tasks here that are running right now — their live streams.
    pub live: Vec<AgentLive>,
    /// How many older turns fell outside the window (part of the fold line at the top).
    pub truncated: usize,
    /// The agent's (agent, workspace) rolling memory — what the fold line folds *into*. Shown
    /// and editable at the top of the thread (the R3 memory affordance).
    pub memory: String,
    /// Whether auto-folding is paused for this scope (operator control).
    pub memory_paused: bool,
}

/// An agent's live status in the nav, derived from its in-flight tasks. `Awaiting` (a task parked
/// at a human gate) takes precedence over `Working` — it's the state that needs the operator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentStatus {
    #[default]
    Idle,
    Working,
    Awaiting,
}

/// One agent row in the agent-list nav: identity, live status, and how many of its tasks are
/// parked awaiting the operator (the approval badge).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentNavRow {
    pub id: String,
    pub name: String,
    pub status: AgentStatus,
    pub awaiting: usize,
}

/// One workspace group in the nav: the workspace (its board reachable via the group header) and
/// its member agents — the concierge excluded (it's pinned on top). `awaiting` is the workspace's
/// total parked-task count, for the header badge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentNavGroup {
    pub workspace_id: String,
    pub workspace_name: String,
    pub awaiting: usize,
    pub agents: Vec<AgentNavRow>,
}

/// The agent-list nav model (Phase 2): the concierge pinned on top, then agents grouped by the
/// workspace they belong to. Projected by `bridge::agent_nav`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentNav {
    /// The concierge (the web agent you route through), pinned first. `None` until one is set.
    pub concierge: Option<AgentNavRow>,
    pub groups: Vec<AgentNavGroup>,
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
    /// The task whose detail view (run chain) is open, or `None`.
    pub task_detail: Option<TaskDetail>,
    /// The agent whose thread page (`/w/:wid/agent/:aid`) is open, or `None` — so background
    /// lifecycle events know whether to re-project [`Self::agent_thread`].
    pub open_agent: Option<String>,
    /// The workspace the open agent thread is scoped to (an agent in two workspaces has two
    /// threads); pairs with [`Self::open_agent`] for the nav row highlight.
    pub open_agent_workspace: Option<String>,
    /// The open agent's merged, time-ordered thread (its tasks + runs + live runs), or `None`.
    pub agent_thread: Option<AgentThread>,
    /// The agent-list nav (Phase 2): the concierge pinned on top, then agents grouped by
    /// workspace, each with a live status + approval badge. Projected by `bridge::agent_nav`.
    pub agent_nav: AgentNav,
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
    /// Per currently-running task, the interleaved live stream (text segments + tool events in
    /// arrival order) shown in the task detail's Thread — the typewriter, in order.
    pub live_turns: Vec<LiveTurn>,
    /// Recent operational log lines for the Logs viewer (newest first, capped).
    pub logs: Vec<LogLine>,
    /// Whether the mobile slide-in nav drawer is open. The desktop sidebar is always
    /// visible (a breakpoint override), so this only governs mobile.
    pub nav_open: bool,
}

#[cfg(test)]
mod tests {
    use super::{LiveItem, LiveTurn};

    #[test]
    fn live_turn_interleaves_text_and_tools_in_arrival_order() {
        let mut turn = LiveTurn::default();
        turn.push_text("let me read the file");
        turn.start_tool("00:00:01".to_owned(), "read src/lib.rs".to_owned());
        turn.finish_tool(true);
        turn.push_text("now editing");

        // Order preserved; the second text is a *new* segment (not glued to the first), and the
        // tool between them is marked finished — the real stream, not "all tools then all text".
        assert_eq!(
            turn.items,
            vec![
                LiveItem::Text("let me read the file".to_owned()),
                LiveItem::Tool {
                    time: "00:00:01".to_owned(),
                    action: "read src/lib.rs".to_owned(),
                    in_progress: false,
                    ok: true,
                },
                LiveItem::Text("now editing".to_owned()),
            ]
        );
    }

    #[test]
    fn live_turn_appends_consecutive_text_into_one_segment() {
        let mut turn = LiveTurn::default();
        turn.push_text("hel");
        turn.push_text("lo");
        assert_eq!(turn.items, vec![LiveItem::Text("hello".to_owned())]);
    }

    #[test]
    fn live_turn_finish_flips_only_the_last_running_tool() {
        let mut turn = LiveTurn::default();
        turn.start_tool("t1".to_owned(), "a".to_owned());
        turn.finish_tool(true);
        turn.start_tool("t2".to_owned(), "b".to_owned());
        turn.finish_tool(false);
        assert_eq!(
            turn.items,
            vec![
                LiveItem::Tool {
                    time: "t1".to_owned(),
                    action: "a".to_owned(),
                    in_progress: false,
                    ok: true
                },
                LiveItem::Tool {
                    time: "t2".to_owned(),
                    action: "b".to_owned(),
                    in_progress: false,
                    ok: false
                },
            ]
        );
    }
}
