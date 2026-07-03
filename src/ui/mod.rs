//! The app shell: a fixed left drawer (brand, nav, conversations) beside a fixed
//! main area that fills the viewport. Access is gated by rwire's form-login
//! (`Server::auth`), so an unauthenticated client never reaches this tree.
//!
//! Layout note: every `#[renderer]` is wrapped by rwire in an inline `__synced_`
//! span, which can't act as a flex/positioning container. So the structural
//! containers here are plain elements (drawer, main, the per-view wrapper), and
//! renderer outputs — including the router `outlet()` — only ever sit *inside* them.

mod activity;
mod agent;
mod archived;
mod chat;
mod dashboard;
mod files;
mod logs;
mod memory;
mod prompts;
mod request_page;
mod run_chain;
mod system_map;
mod task_detail;
mod thread;
mod widgets;
mod workspace;
mod workspace_tasks;
mod workspace_workflow;

use rwire::router::Link;
use rwire::router::RoutePattern;
use rwire::style::Style;
use rwire::{
    At, CurrentRoute, El, ElementBuilder, Ev, Router, St, el, handler, icons, outlet, renderer,
};
use rwire_components::{Button, ButtonIntent, ButtonSize, StatusDot, StatusDotIntent};

use crate::bridge::{self, Command};
use crate::state::{App, Tab};
use workspace::WsTab;

/// The app's routes: each path renders its view into the shell's [`outlet`]. `/` is the
/// concierge's one persistent conversation (`/chat/:id` is kept only so stale bookmarks
/// resolve to it); `/w/:wid…` are the per-workspace views (the `:wid` + active section are
/// opened by [`on_route`]).
pub fn routes() -> Router {
    Router::new()
        .page("/", |_| view(chat::surface()))
        .page("/chat/:id", |_| view(chat::surface()))
        .page("/dashboard", |_| view(dashboard::page()))
        // The board is the workspace's landing view (the "board is the workspace" overhaul).
        .page("/w/:wid", |p| {
            view(workspace::shell(
                wid(p),
                WsTab::Tasks,
                workspace_tasks::tasks(),
            ))
        })
        .page("/w/:wid/overview", |p| {
            view(workspace::shell(
                wid(p),
                WsTab::Dashboard,
                workspace::dashboard(),
            ))
        })
        .page("/w/:wid/files", |p| {
            view(workspace::shell(
                wid(p),
                WsTab::Files,
                workspace::files_section(),
            ))
        })
        .page("/w/:wid/tasks", |p| {
            view(workspace::shell(
                wid(p),
                WsTab::Tasks,
                workspace_tasks::tasks(),
            ))
        })
        .page("/w/:wid/tasks/:tid", |p| {
            // A deep link to a task renders the board; the bridge opens its Peek modal over it.
            view(workspace::shell(
                wid(p),
                WsTab::Tasks,
                workspace_tasks::tasks(),
            ))
        })
        .page("/w/:wid/requests/:rid", |p| {
            // The request page (G2): the ask's whole story — plan, tasks, diffs, delivery.
            view(workspace::shell(wid(p), WsTab::Tasks, request_page::page()))
        })
        .page("/w/:wid/workflow", |p| {
            view(workspace::shell(
                wid(p),
                WsTab::Workflow,
                workspace_workflow::page(),
            ))
        })
        .page("/w/:wid/team", |p| {
            view(workspace::shell(wid(p), WsTab::Team, workspace::team()))
        })
        // An agent's thread is workspace-scoped (an agent in two workspaces has two threads,
        // mirroring its two memories) — so the URL carries both ids.
        .page("/w/:wid/agent/:aid", |_| view(agent::page()))
        .page("/archived", |_| view(archived::page()))
        .page("/prompts", |_| view(prompts::page()))
        .page("/system", |_| view(system_map::page()))
        .page("/activity", |_| view(activity::page()))
        .page("/logs", |_| view(logs::page()))
}

/// The `:wid` route param (empty if absent — the workspace renderers then show a fallback).
fn wid(params: &rwire::router::RouteParams) -> &str {
    params.get("wid").unwrap_or("")
}

/// Wrap a route view so it fills the positioned main area (the outlet between them is
/// an inline `__synced_` span and can't be the flex container itself).
fn view(body: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .style(fill())
        .st([St::DisplayFlex, St::FlexCol, St::OverflowHidden])
        .append([body])
}

const DRAWER_W: &str = "16rem";

pub fn app() -> ElementBuilder {
    // Mono everywhere, small text — the terminal feel of rust-claw. A flex row so the
    // drawer is a static sidebar on desktop and a fixed slide-in overlay on mobile.
    el(El::Div)
        // `100dvh` (dynamic viewport height), not `100vh`: on mobile the URL bar shrinks
        // the visible viewport, and `100vh` (the *large* viewport) would push the bottom
        // — the composer — under the bar. `dvh` tracks the bar auto-hiding, so the
        // layout always fits the visible area.
        .st([
            St::HDvh,
            St::BgApp,
            St::TextSm,
            St::DisplayFlex,
            St::OverflowHidden,
        ])
        .append([nav_drawer(), backdrop(), main_area()])
}

// ── Drawer ───────────────────────────────────────────────────────────────────

/// The left navigation drawer. On `md+` it's a static, always-visible sidebar; below
/// that it's a fixed slide-in overlay toggled by `Ui::nav_open` (the hamburger). The
/// `md` breakpoint overrides force it visible and in-flow regardless of `nav_open`.
#[renderer]
fn nav_drawer(app: &App) -> ElementBuilder {
    let slide = if app.nav_open {
        St::TransformNone
    } else {
        St::TranslateXNegFull
    };
    el(El::Aside)
        // `width: 16rem` + slide `transition` — no tokens for the custom drawer width / easing.
        .style(
            Style::new()
                .width(DRAWER_W)
                .set("transition", "transform 0.2s ease"),
        )
        .st([
            St::HDvh,
            St::Top0,
            St::Left0,
            St::BgSidebar,
            St::BorderRDefault,
            St::DisplayFlex,
            St::FlexCol,
            St::FlexShrink0,
            St::PositionFixed,
            St::Z1400,
            slide,
        ])
        .md([St::PositionStatic, St::TransformNone])
        // Agent-first (R1): brand, then the agent list (Concierge pinned on top — its one
        // persistent conversation replaces the chat-thread list and "New chat"), then the
        // secondary surfaces (Dashboard/Activity/Logs/…) and Log out below it.
        .append([drawer_head(), drawer_agents(), nav_footer(), drawer_foot()])
}

/// The scrollable agent list fills the drawer below the head — a plain flex child (so it
/// actually grows) wrapping the `agents_nav` renderer.
fn drawer_agents() -> ElementBuilder {
    el(El::Div)
        .st([St::MinH0, St::OverflowAuto, St::Flex1])
        .append([agents_nav()])
}

/// A small uppercase group label in the sidebar tree (Chat / Workspaces).
fn section_label(text: &str) -> ElementBuilder {
    el(El::Div)
        .st([
            St::PxMd,
            St::PbXs,
            St::TextXs,
            St::TextMuted,
            St::TextUppercase,
            St::TrackingWide,
        ])
        .text(text)
}

/// The agent-list nav (Phase 2): the **Concierge** pinned on top, then agents grouped under the
/// workspace they belong to. Each group header links to that workspace's board (boards stay
/// reachable), and each agent row deep-links to its thread with a live status dot + approval badge.
/// Projected by `bridge::agent_nav` into `app.agent_nav`.
#[renderer]
fn agents_nav(app: &App) -> ElementBuilder {
    let open = app.open_agent.as_deref();
    let open_ws = app.open_agent_workspace.as_deref();
    let active_ws = app.active_workspace.as_deref();
    let nav = &app.agent_nav;
    let mut rows: Vec<ElementBuilder> = vec![section_label("Concierge")];
    if let Some(concierge) = &nav.concierge {
        // The concierge row opens its one persistent conversation at `/` (not an agent-thread
        // task log). Highlighted whenever no other agent's page is open — the sticky
        // selection the old thread list used.
        rows.push(concierge_row(concierge, open.is_none()));
    }
    for group in &nav.groups {
        rows.push(group_header(
            &group.workspace_id,
            &group.workspace_name,
            active_ws == Some(group.workspace_id.as_str()),
            group.awaiting,
        ));
        for agent in &group.agents {
            // Threads are (agent, workspace)-scoped, so the same agent under two workspace
            // groups is two distinct rows with two distinct URLs — only the open pair lights up.
            let active =
                open == Some(agent.id.as_str()) && open_ws == Some(group.workspace_id.as_str());
            rows.push(agent_row(agent, &group.workspace_id, active));
        }
    }
    rows.push(new_workspace_link());
    el(El::Div)
        .st([
            St::BorderTSubtle,
            St::PySm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

/// A workspace group header — a clickable section label linking to the workspace's board (so every
/// board stays reachable), with the workspace's "needs you" badge on the right.
fn group_header(id: &str, name: &str, active: bool, awaiting: usize) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    Link::route(&format!("/w/{id}"))
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PtSm,
            St::PbXs,
            St::TextXs,
            St::TextUppercase,
            St::TrackingWide,
            St::NoUnderline,
            St::CursorPointer,
            color,
        ])
        .hover([St::TextHigh])
        .append([
            el(El::Span).st([St::Flex1]).text(name),
            awaiting_badge(awaiting),
        ])
}

/// The pinned concierge row: same anatomy as an agent row, but it routes to `/` — the one
/// persistent conversation — and leads with the chat glyph rather than a task-status dot.
fn concierge_row(row: &crate::state::AgentNavRow, active: bool) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    Link::route("/")
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxMd,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
            color,
        ])
        .hover([St::TextHigh])
        .append([
            chat::accent_bar(active),
            chat::icon_slot(icons::icon_sized(icons::Icon::MessageSquare, 16)),
            el(El::Span).st([St::Flex1]).text(&row.name),
            awaiting_badge(row.awaiting),
        ])
}

/// One agent row: the leading gutter (accent rail + status dot), the name, and — when it has tasks
/// parked at a gate — the approval badge. Deep-links to the agent's thread **in this workspace**
/// (`/w/:wid/agent/:aid`), mirroring the (agent, workspace) context scoping.
fn agent_row(row: &crate::state::AgentNavRow, workspace_id: &str, active: bool) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    Link::route(&format!("/w/{workspace_id}/agent/{}", row.id))
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxMd,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
            color,
        ])
        .hover([St::TextHigh])
        .append([
            chat::accent_bar(active),
            chat::icon_slot(status_dot(row.status)),
            el(El::Span).st([St::Flex1]).text(&row.name),
            awaiting_badge(row.awaiting),
        ])
}

/// The leading status glyph: an accent **pulsing** dot while the agent is working, else a muted
/// dot (an awaiting agent is idle-but-parked — its approval badge carries that signal).
fn status_dot(status: crate::state::AgentStatus) -> ElementBuilder {
    let working = status == crate::state::AgentStatus::Working;
    StatusDot::new()
        .intent(if working {
            StatusDotIntent::Primary
        } else {
            StatusDotIntent::Muted
        })
        .pulse(working)
        .build()
}

/// The create affordance at the tail of the agent nav — workspaces are created on the dashboard.
fn new_workspace_link() -> ElementBuilder {
    Link::route("/dashboard")
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxMd,
            St::PtSm,
            St::TextSm,
            St::TextMuted,
            St::NoUnderline,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .append([
            chat::accent_bar(false),
            chat::icon_slot(icons::icon_sized(icons::Icon::Plus, 16)),
            el(El::Span).text("New workspace"),
        ])
}

/// Dims the page behind the mobile drawer; tap to close. Never shown on `md+`.
#[renderer]
fn backdrop(app: &App) -> ElementBuilder {
    if !app.nav_open {
        return el(El::Div).st([St::DisplayNone]);
    }
    el(El::Div)
        .st([St::FixedFill, St::BgOverlay50, St::Z1300])
        .md([St::DisplayNone])
        .on(Ev::Click, close_nav())
}

#[handler]
fn toggle_nav(app: &mut App) {
    app.nav_open = !app.nav_open;
}

#[handler]
pub fn close_nav(app: &mut App) {
    app.nav_open = false;
}

fn drawer_head() -> ElementBuilder {
    // The masthead: brand + live status. Nav moved to the footer (below the threads).
    el(El::Div)
        .st([St::PxMd, St::PtMd, St::PbSm])
        .append([brand_row()])
}

/// The secondary surfaces, below the conversation group they support. Chat is omitted
/// — the conversation list above *is* the chat surface, so a separate link is
/// redundant. A top border sets it apart from the conversations.
fn nav_footer() -> ElementBuilder {
    el(El::Div)
        .st([St::BorderTSubtle, St::PySm])
        .append([nav()])
}

fn brand_row() -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyBetween,
            St::PySm,
        ])
        .append([
            // Glyph + wordmark, on the shared gutter so "claw" heads the same text
            // column as every row below it. The icon inherits the accent currentColor.
            el(El::Span)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::GapSm,
                    St::TextAccent,
                    St::FontSemibold,
                    St::TextLg,
                ])
                .append([
                    chat::accent_bar(false),
                    chat::icon_slot(icons::icon_sized(icons::Icon::Terminal, 16)),
                    el(El::Span).text("claw"),
                ]),
            status(),
        ])
}

/// The drawer footer: a logout affordance pinned to the bottom.
fn drawer_foot() -> ElementBuilder {
    el(El::Div)
        .st([St::BorderTSubtle, St::PMd])
        .append([logout_link()])
}

/// A plain `<a href="/logout">` — deliberately *not* `Link::route`. `/logout` is a
/// server route on rwire's auth gate (`Server::auth`), so a full-page navigation lets
/// the gate clear the token cookie and redirect to the login page; a client-routed
/// link would try to resolve it as an in-app route and fail.
fn logout_link() -> ElementBuilder {
    el(El::A)
        .at_str(At::Href, "/logout")
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::TextSm,
            St::TextMuted,
            St::NoUnderline,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .append([
            chat::accent_bar(false),
            chat::icon_slot(icons::icon_sized(icons::Icon::LogOut, 16)),
            el(El::Span).text("Log out"),
        ])
}

#[renderer]
fn nav(route: &CurrentRoute) -> ElementBuilder {
    let path = route.path();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append(Tab::NAV.map(|tab| nav_row(tab, tab_active(path, tab))))
}

/// The active nav item is derived from the URL: Chat owns both `/` and any
/// `/chat/...` thread; the rest match their own path.
fn tab_active(path: &str, tab: Tab) -> bool {
    match tab {
        Tab::Chat => path == "/" || path.starts_with("/chat"),
        other => path == other.path(),
    }
}

fn nav_row(tab: Tab, active: bool) -> ElementBuilder {
    // The shared leading gutter (accent rail + icon slot) so the label lands in the
    // same column as the conversation rows. The icon + label inherit the row's
    // `currentColor`, so the single colour token drives both. Active is
    // `accent rail + TextHigh` — the same treatment the conversation rows use.
    let color = if active { St::TextHigh } else { St::TextMuted };
    // `Link::route` is rwire's client-routed `<a>`: the client router intercepts the
    // click, pushes history, and re-renders the matched view into the outlet. The
    // extra `close_nav` collapses the mobile drawer once you've picked a destination.
    let row = vec![
        chat::accent_bar(active),
        chat::icon_slot(icons::icon_sized(tab.icon(), 16)),
        el(El::Span).st([St::Flex1]).text(tab.label()),
    ];
    Link::route(tab.path())
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxMd,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
            color,
        ])
        .hover([St::TextHigh])
        .append(row)
}

/// A workspace row's "needs you" count badge — how many of its tasks are parked at a human gate.
/// Empty (a zero-width span) when nothing's waiting.
fn awaiting_badge(count: usize) -> ElementBuilder {
    if count == 0 {
        return el(El::Span);
    }
    el(El::Span)
        .st([St::TextXs, St::PxXs, St::BgAccent, St::TextOnAccent])
        .text(&count.to_string())
}

#[renderer]
fn status(app: &App) -> ElementBuilder {
    let (dot, label) = if app.pipeline_ready {
        (St::BgAccent, "ready")
    } else {
        (St::BgMuted, "starting…")
    };
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span).st([St::WSp2, St::HSp2, St::RoundedFull, dot]),
            el(El::Span).st([St::TextXs, St::TextMuted]).text(label),
        ])
}

// ── Main area ─────────────────────────────────────────────────────────────────

fn main_area() -> ElementBuilder {
    // A flex child that fills the space beside the sidebar (or the whole width on
    // mobile). `min-width:0` lets it shrink so chat content can't force a horizontal
    // scroll. The `outlet()` view fills the positioned content wrapper.
    el(El::Div)
        .st([
            St::MinW0,
            St::Flex1,
            St::DisplayFlex,
            St::FlexCol,
            St::OverflowHidden,
        ])
        .append([
            mobile_topbar(),
            el(El::Div)
                .st([St::Flex1, St::PositionRelative, St::OverflowHidden])
                .append([outlet()]),
        ])
}

/// A slim bar shown only below `md`: a hamburger to open the drawer plus the wordmark.
fn mobile_topbar() -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PSm,
            St::BorderBDefault,
            St::BgSidebar,
        ])
        .md([St::DisplayNone])
        .append([
            hamburger(),
            el(El::Span)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::GapSm,
                    St::TextAccent,
                    St::FontSemibold,
                ])
                .append([
                    icons::icon_sized(icons::Icon::Terminal, 16),
                    el(El::Span).text("claw"),
                ]),
        ])
}

fn hamburger() -> ElementBuilder {
    Button::icon_only(icons::Icon::Menu, "Toggle navigation")
        .intent(ButtonIntent::Outline)
        .size(ButtonSize::Sm)
        .on_click(toggle_nav())
}

/// `position:absolute; inset:0` — fills the nearest positioned ancestor regardless
/// of the inline synced spans in between.
fn fill() -> Style {
    Style::new().position("absolute").set("inset", "0")
}

/// Route side-effects, alongside the rwire router (which renders the view + nav).
/// The router handles *which* view shows; this only triggers domain work the URL
/// implies: asking the bridge to open the conversation a `/chat/:id` points at. It
/// must NOT mutate `App` — the bridge owns the projection and broadcasts it, and a
/// broad mutation here would re-render shared renderers and fight the router's swap.
#[handler]
pub fn on_route(app: &mut App, ctx: &EventContext) {
    let _ = app; // side-effect only: effects go through the bridge, not direct `App` writes
    let Some(path) = ctx.text() else {
        return;
    };
    // Track the workspace the route is in, so the sidebar highlights it and the workspace
    // header/dashboard/Team renderers can resolve their `WorkspaceView`. Projected via the
    // bridge (a direct `App` write here doesn't reliably broadcast from the route hook).
    let workspace = workspace_of(path);
    bridge::send(Command::SetActiveWorkspace {
        id: workspace.clone(),
    });
    // Load the workspace's brief for the Overview editor.
    if let Some(wid) = workspace {
        bridge::send(Command::LoadBrief { workspace: wid });
    }
    // Leaving any view closes the System Map drawer (its `editing` is shared state, so
    // it would otherwise re-open when you return to `/system`).
    bridge::send(Command::CloseDrawer);
    // Opening the Archived screen refreshes its list from the DB.
    if path == "/archived" {
        bridge::send(Command::ListArchived);
    }
    // Opening the global dashboard refreshes its all-lanes task view.
    if path == "/dashboard" {
        bridge::send(Command::LoadDashboard);
    }
    // Opening the Prompts inspector composes the current selection (picking up any edits) and
    // loads the editable global role defaults.
    if path == "/prompts" {
        bridge::send(Command::LoadPrompt {
            agent: app.prompt_agent.clone(),
            run: app.prompt_run,
        });
        bridge::send(Command::LoadSettings);
    }
    // Entering a workspace's Files tab lists that workspace's root.
    if let Some(wid) = RoutePattern::new("/w/:wid/files")
        .matches(path)
        .and_then(|params| params.get("wid").map(str::to_owned))
    {
        bridge::send(Command::ListDir {
            workspace: wid,
            path: String::new(),
        });
    }
    // Entering a workspace's Tasks tab — or its landing board (`/w/:wid`) — projects its lane and
    // the pipeline (so the board's column headers can open the in-situ step editor).
    if let Some(wid) = RoutePattern::new("/w/:wid/tasks")
        .matches(path)
        .or_else(|| RoutePattern::new("/w/:wid").matches(path))
        .and_then(|params| params.get("wid").map(str::to_owned))
    {
        bridge::send(Command::LoadTasks {
            workspace: wid.clone(),
        });
        bridge::send(Command::LoadWorkflow { workspace: wid });
    }
    // A deep link to a task: load the board's lane (so the modal opens over a populated board)
    // and the task's detail/run chain (which also opens its Peek modal).
    if let Some(params) = RoutePattern::new("/w/:wid/requests/:rid").matches(path)
        && let Some(rid) = params.get("rid")
    {
        bridge::send(Command::LoadRequestDetail {
            request_id: rid.to_owned(),
        });
    }
    if let Some(params) = RoutePattern::new("/w/:wid/tasks/:tid").matches(path) {
        if let Some(wid) = params.get("wid").map(str::to_owned) {
            bridge::send(Command::LoadTasks { workspace: wid });
        }
        if let Some(tid) = params.get("tid").map(str::to_owned) {
            bridge::send(Command::LoadTaskDetail { task_id: tid });
        }
    }
    // The Workflow tab loads the workspace's configured pipeline + edit lock.
    if let Some(wid) = RoutePattern::new("/w/:wid/workflow")
        .matches(path)
        .and_then(|params| params.get("wid").map(str::to_owned))
    {
        bridge::send(Command::LoadWorkflow { workspace: wid });
    }
    // `/chat/:id` needs no side-effect: there is one persistent concierge conversation, always
    // loaded — the route just shows it (stale bookmarks resolve to the one thread).
    // Opening an agent's workspace thread projects its merged tasks/runs/live timeline; leaving
    // the agent pages clears the open pair so the nav highlight follows the route.
    match RoutePattern::new("/w/:wid/agent/:aid")
        .matches(path)
        .map(|params| {
            (
                params.get("aid").unwrap_or("").to_owned(),
                params.get("wid").unwrap_or("").to_owned(),
            )
        }) {
        Some((agent, workspace)) => bridge::send(Command::OpenAgentThread { agent, workspace }),
        None => bridge::send(Command::CloseAgentThread),
    }
}

/// The workspace id a path is within (any `/w/:wid…` route), else `None`.
fn workspace_of(path: &str) -> Option<String> {
    for pattern in [
        "/w/:wid",
        "/w/:wid/overview",
        "/w/:wid/files",
        "/w/:wid/tasks",
        "/w/:wid/tasks/:tid",
        "/w/:wid/requests/:rid",
        "/w/:wid/workflow",
        "/w/:wid/team",
        "/w/:wid/agent/:aid",
    ] {
        if let Some(params) = RoutePattern::new(pattern).matches(path)
            && let Some(wid) = params.get("wid")
        {
            return Some(wid.to_owned());
        }
    }
    None
}
