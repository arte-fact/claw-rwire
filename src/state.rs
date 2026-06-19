//! Application state, split by rwire storage tier.
//!
//! `App` is process-global (`#[storage(shared)]`) — the projection the pipeline
//! writes into and every connection reads. `Ui` is per-connection
//! (`#[storage(memory)]`) — view-local selection that must not leak between
//! browser tabs. rwire renderers/handlers take exactly one state type, so the two
//! are never mixed in a single function; they compose at the element level.

use rwire::State;

/// The top-level navigation surfaces (ARCHITECTURE: chat + the System Map admin +
/// activity/logs). Drives `Ui::tab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Chat,
    System,
    Activity,
    Logs,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Chat, Tab::System, Tab::Activity, Tab::Logs];

    pub fn slug(self) -> &'static str {
        match self {
            Tab::Chat => "chat",
            Tab::System => "system",
            Tab::Activity => "activity",
            Tab::Logs => "logs",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::System => "System",
            Tab::Activity => "Activity",
            Tab::Logs => "Logs",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|tab| tab.slug() == slug)
    }

    /// The URL path this tab lives at. Chat is the app root.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Chat => "/",
            Tab::System => "/system",
            Tab::Activity => "/activity",
            Tab::Logs => "/logs",
        }
    }

    /// Resolve a URL path (from the rwire client router) to a tab. `/` is Chat.
    pub fn from_path(path: &str) -> Option<Tab> {
        match path.trim_end_matches('/') {
            "" => Some(Tab::Chat),
            rest => Tab::from_slug(rest.trim_start_matches('/')),
        }
    }
}

/// One row in the chat sidebar (projected from `messaging_groups` / web threads).
#[derive(Debug, Clone, Default)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub busy: bool,
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

/// Process-global projection. Selection (`open_chat`) lives here rather than in
/// per-connection `Ui` so a single `App`-typed renderer can draw the thread; for a
/// single-operator assistant, tabs sharing the open chat is acceptable (and matches
/// the reference app's shared-selection model).
#[derive(Debug, Default, State)]
#[storage(shared)]
pub struct App {
    /// The active main-area surface. Shared (not per-connection) so an `App`
    /// handler — selecting a conversation — can also switch to the chat view.
    pub tab: Tab,
    /// Set by the bridge once the pipeline thread has booted and primed state.
    pub pipeline_ready: bool,
    /// Chat threads for the sidebar.
    pub chats: Vec<ChatSummary>,
    /// The currently open thread id (shared across this operator's tabs).
    pub open_chat: Option<String>,
    /// Transcript of the open thread.
    pub messages: Vec<ChatMessage>,
    /// The agent is producing a reply on the open thread.
    pub sending: bool,
}
