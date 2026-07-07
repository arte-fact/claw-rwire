//! Application state. Everything lives in the process-global `App`
//! (`#[storage(shared)]`) — the projection the pipeline writes into and every
//! connection reads. View-local flags (the mobile nav toggle, the System Map drawer
//! target) live here too rather than in a per-connection `#[storage(memory)]` state:
//! a *shared*-state renderer nested inside a *memory*-state renderer doesn't receive
//! the shared cache, and the sidebar's status + thread list are shared-state renderers
//! that sit inside the drawer. For a single operator, sharing these across tabs is
//! benign (the desktop sidebar ignores `nav_open` via a breakpoint override anyway).

use rwire::State;
mod views;
pub use views::*;

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
    /// Count of older turns past the window, not loaded into `messages` (P4a seamless history):
    /// the scroll sentinel's `next`, `> 0` while there's more to load. Zero when the whole thread
    /// fits the window.
    pub messages_truncated: usize,
    /// The agent is producing a reply on the open thread.
    pub sending: bool,
    /// A transient error from the last turn (e.g. the endpoint failed). Shown as a
    /// banner; never persisted, cleared on the next send.
    pub error: Option<String>,
    /// The failure-toast queue (L6): `surface()`d errors, newest last, capped — rendered
    /// as a dismissable stack by the shell on every page.
    pub toasts: Vec<ToastView>,
    /// System Map: registry read-models, projected by the bridge.
    pub endpoints: Vec<EndpointView>,
    /// SSH deploy keys (P5b): name + fingerprint only — the private material never reaches the model.
    pub ssh_keys: Vec<SshKeyView>,
    pub agents: Vec<AgentView>,
    /// The agent id the web channel currently routes to.
    pub web_agent: Option<String>,
    /// The System Map drawer's current target, or `None` when it's closed.
    pub editing: Option<Editing>,
    /// The CI logs drawer's contents (P4c), or `None` when it's closed — set when a check badge on
    /// the task thread is opened, cleared on close.
    pub log_drawer: Option<LogDrawerView>,
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
    /// The open workspace's `STANDARDS.md` — the validator standards memo (R5): curated review
    /// rules injected into review runs (validators stay memory-free otherwise).
    pub workspace_standards: String,
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
    /// The request page (`/w/:wid/requests/:rid`) currently open, or `None` (G2).
    pub request_detail: Option<RequestDetail>,
    /// The project page (`/w/:wid/projects/:pid`) currently open, or `None` (P1).
    pub project_detail: Option<ProjectDetail>,
    /// The active workspace's projects — the Work tab's Projects strip (P1).
    pub projects: Vec<ProjectCardView>,
    /// The active workspace's open requests — the board's Requests rail (G2).
    pub requests: Vec<RequestCardView>,
    /// The agent whose thread page (`/w/:wid/agent/:aid`) is open, or `None` — so background
    /// lifecycle events know whether to re-project [`Self::agent_thread`].
    pub open_agent: Option<String>,
    /// The workspace the open agent thread is scoped to (an agent in two workspaces has two
    /// threads); pairs with [`Self::open_agent`] for the nav row highlight.
    pub open_agent_workspace: Option<String>,
    /// The open agent's merged, time-ordered thread (its tasks + runs + live runs), or `None`.
    pub agent_thread: Option<AgentThread>,
    /// The concierge's **global** memory for the chat page's fold line + card (R3.5 M3) —
    /// parity with the agent page: the page shows what its agent remembers.
    pub chat_memory: Option<ChatMemory>,
    /// The System page's memory overview (R3.5 M5): every (agent, workspace) scope row.
    pub memory_overview: Vec<MemoryOverviewRow>,
    /// The agent-list nav (Phase 2): the concierge pinned on top, then agents grouped by
    /// workspace, each with a live status + approval badge. Projected by `bridge::agent_nav`.
    pub agent_nav: AgentNav,
    /// The board's active quick-filter: `all` | `awaiting` | `running` | `blocked` | `failed`.
    pub board_filter: String,
    /// The active workspace's machine-check command (L1: run after each producer pass,
    /// its output injected into the review) — the Settings editor's value.
    pub workspace_checks: String,
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
    /// Planning turns in flight (P4d): the chat/request → plan-task bridge that lets the concierge
    /// and the request page show the owner's live planning stream (keyed into `live_turns` by
    /// `plan_id`). Set when a plan turn launches, cleared when it lands.
    pub planning_live: Vec<PlanningLive>,
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
