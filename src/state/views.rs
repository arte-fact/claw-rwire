//! The view models the bridge projects into `App` — plain data the renderers draw. Split
//! from `state/mod.rs` (which keeps the `App` struct itself); re-exported flat, so callers
//! keep addressing everything as `crate::state::X`.

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
    /// The Request the task belongs to (G1) — the card wears a request tag when set.
    pub request_id: Option<String>,
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
    /// The pass's git diff (stat + capped patch across its commits, G2) — empty when the
    /// pass committed nothing. The Peek renders it behind a Diff disclosure.
    pub diff: String,
    /// The files this pass touched, each restorable to its pre-pass state (G3).
    pub diff_files: Vec<RestorableFile>,
    /// The memory layer the run launched with (R3.5 M4 provenance), or empty — the Peek shows
    /// it so "why did it do that?" has an answer.
    pub memory: String,
}

/// A task's detail view: its spec, status, and the full run chain (the audit trail).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetail {
    pub id: String,
    pub workspace_id: String,
    /// The owning request (id, title) when the task is request-bound — the middle crumb.
    pub request_id: String,
    pub request_title: String,
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

/// One row of the System page's memory overview (R3.5 M5): an (agent, workspace) scope with
/// its size, freshness, and pause flag, plus the route to the page where it's edited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryOverviewRow {
    pub agent_name: String,
    /// The scope's display label: the workspace name, or "global" for the concierge row.
    pub scope: String,
    /// Where to inspect/edit: the agent's thread page, or `/` for the global row.
    pub route: String,
    pub chars: usize,
    /// `YYYY-MM-DD HH:MM` of the last mutation.
    pub updated: String,
    pub paused: bool,
}

/// The concierge's global memory, projected for the chat page's card (R3.5 M3): who owns it
/// (the web agent — memory mutations target its global row), the summary, the pause state,
/// and the last fold's human line for the card's meta.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatMemory {
    pub agent_id: String,
    pub agent_name: String,
    pub summary: String,
    pub paused: bool,
    /// "08:45 — condensed 14 older messages into memory", or empty when never folded.
    pub last_fold: String,
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

// ── Requests (G2) ──────────────────────────────────────────────────────────────────

/// One open request on the board's Requests rail: progress + diffstat at a glance.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestCardView {
    pub id: String,
    pub title: String,
    /// `planning` / `executing` / `queued` / `validating` — the card's status chip.
    pub status: String,
    /// "2/5 task(s) done" (empty while planning).
    pub progress: String,
    /// "+A/−D across F file(s)" so far (empty until a pass commits).
    pub stat: String,
    /// Tasks done / total — the card's hairline meter (0/0 while planning).
    pub done: i64,
    pub total: i64,
    /// `req/<short>-<slug>` once minted (empty while planning).
    pub branch: String,
}

/// The request page (`/w/:wid/requests/:rid`): the whole story in one scroll.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestDetail {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    /// The user's words, verbatim — what delivery is ultimately judged against.
    pub ask: String,
    pub status: String,
    pub branch: String,
    /// The plan + work tasks, oldest first, each with its activity diffstat.
    pub tasks: Vec<RequestTaskEntry>,
    /// The repos the request touched (branch point → merge).
    pub repos: Vec<RequestRepoEntry>,
    /// The global diff's stat block (base..tip per repo, joined).
    pub diff_stat: String,
    /// The global diff's patch, capped — rendered with the DiffLine vocabulary.
    pub diff_patch: String,
    /// The macro-validation verdict + feedback (G3), empty before validation ran.
    pub validation: String,
    /// Whether any touched repo has a remote — shows the (explicit, gated) Push control.
    pub has_remote: bool,
    /// `MM-DD HH:MM` stamps for the history panel: asked at / last status change.
    pub asked_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestTaskEntry {
    pub id: String,
    pub title: String,
    pub status: String,
    pub is_plan: bool,
    /// Sum of the task's pass-commit numstats ("+A/−D"), empty when it committed nothing.
    pub stat: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestRepoEntry {
    pub repo: String,
    pub base: String,
    pub merge: String,
}

/// One file a pass changed, addressable for restore (G3): `checkout <sha>^ -- path`. The
/// task routes the restore to the right tree (its request's worktree while open, G4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestorableFile {
    pub workspace: String,
    pub task: String,
    pub repo: String,
    pub sha: String,
    pub path: String,
}
