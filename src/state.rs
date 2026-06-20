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
    Archived,
    System,
    Files,
    Activity,
    Logs,
}

impl Tab {
    /// The tabs shown in the nav footer, below the live conversation list. Chat is
    /// intentionally absent — the conversation list above the nav *is* the chat
    /// surface, so a separate link would be redundant. (The `Chat` variant still backs
    /// the `/` and `/chat/:id` routes; it's just not a nav row.) Archived leads the
    /// footer as it's the closest relative of the conversations above.
    pub const NAV: [Tab; 5] = [
        Tab::Archived,
        Tab::System,
        Tab::Files,
        Tab::Activity,
        Tab::Logs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Archived => "Archived",
            Tab::System => "System",
            Tab::Files => "Files",
            Tab::Activity => "Activity",
            Tab::Logs => "Logs",
        }
    }

    /// The URL path this tab lives at. Chat is the app root.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Chat => "/",
            Tab::Archived => "/archived",
            Tab::System => "/system",
            Tab::Files => "/files",
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
            Tab::Archived => Icon::Archive,
            Tab::System => Icon::Cpu,
            Tab::Files => Icon::Folder,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Started,
    Finished,
    Failed,
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
#[derive(Debug, Clone)]
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
/// fields so a re-render restores them. `prompt` is the agent's `AGENT.md` (its system
/// prompt) and `tools` its enabled tool names — both loaded from disk/DB when an
/// existing agent is opened.
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
    /// Files view: the agent whose workspace is being browsed.
    pub files_agent: Option<String>,
    /// Files view: the current directory, relative to the workspace root (`""` = root).
    pub files_path: String,
    /// Files view: entries of `files_path` (dirs first).
    pub files_entries: Vec<FileEntry>,
    /// Files view: the file open in the editor pane, if any.
    pub files_open: Option<OpenFile>,
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
    /// Recent operational log lines for the Logs viewer (newest first, capped).
    pub logs: Vec<LogLine>,
    /// Whether the mobile slide-in nav drawer is open. The desktop sidebar is always
    /// visible (a breakpoint override), so this only governs mobile.
    pub nav_open: bool,
}
