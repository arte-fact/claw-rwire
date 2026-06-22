//! The app shell: a fixed left drawer (brand, nav, conversations) beside a fixed
//! main area that fills the viewport. Access is gated by rwire's form-login
//! (`Server::auth`), so an unauthenticated client never reaches this tree.
//!
//! Layout note: every `#[renderer]` is wrapped by rwire in an inline `__synced_`
//! span, which can't act as a flex/positioning container. So the structural
//! containers here are plain elements (drawer, main, the per-view wrapper), and
//! renderer outputs — including the router `outlet()` — only ever sit *inside* them.

mod activity;
mod archived;
mod chat;
mod dashboard;
mod files;
mod logs;
mod prompts;
mod review;
mod system_map;
mod task_detail;
mod widgets;
mod workspace;
mod workspace_tasks;

use rwire::router::Link;
use rwire::router::RoutePattern;
use rwire::style::Style;
use rwire::{
    CurrentRoute, El, ElementBuilder, Ev, Router, St, el, handler, icons, outlet, renderer,
};

use crate::bridge::{self, Command};
use crate::state::{App, Tab};
use workspace::WsTab;

/// The app's routes: each path renders its view into the shell's [`outlet`]. `/` and
/// `/chat/:id` are the global concierge chat; `/w/:wid…` are the per-workspace views
/// (the `:wid` + active section are opened by [`on_route`]).
pub fn routes() -> Router {
    Router::new()
        .page("/", |_| view(chat::surface()))
        .page("/chat/:id", |_| view(chat::surface()))
        .page("/dashboard", |_| view(dashboard::page()))
        .page("/w/:wid", |p| {
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
            view(workspace::shell(wid(p), WsTab::Tasks, task_detail::page()))
        })
        .page("/w/:wid/team", |p| {
            view(workspace::shell(wid(p), WsTab::Team, workspace::team()))
        })
        .page("/archived", |_| view(archived::page()))
        .page("/review", |_| view(review::page()))
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
        .style(Style::new().set("height", "100dvh"))
        .st([
            St::BgApp,
            St::FontMono,
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
        .style(
            Style::new()
                .width(DRAWER_W)
                .set("height", "100dvh")
                .set("top", "0")
                .set("left", "0")
                .set("transition", "transform 0.2s ease"),
        )
        .st([
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
        // Conversation-first: brand, the primary New-chat action, the thread list, then
        // Archived at the tail of the conversation group; the secondary surfaces
        // (System/Activity/Logs) and Log out sit below it.
        .append([
            drawer_head(),
            section_label("Chat"),
            new_chat_bar(),
            drawer_threads(),
            workspaces_nav(),
            nav_footer(),
            drawer_foot(),
        ])
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

/// The Workspaces group: a labelled list of workspace links (each → its dashboard),
/// highlighting the one the current route is in, plus a create affordance. Sits between
/// the conversation list and the app-level nav footer.
#[renderer]
fn workspaces_nav(app: &App) -> ElementBuilder {
    let active = app.active_workspace.as_deref();
    let mut rows: Vec<ElementBuilder> = vec![
        el(El::Div)
            .st([
                St::PxMd,
                St::PbXs,
                St::TextXs,
                St::TextMuted,
                St::TextUppercase,
                St::TrackingWide,
            ])
            .text("Workspaces"),
    ];
    for workspace in &app.workspaces {
        rows.push(workspace_row(
            &workspace.id,
            &workspace.name,
            active == Some(workspace.id.as_str()),
        ));
    }
    // Create lives on the dashboard; per-workspace settings on its Team tab.
    rows.push(
        Link::route("/dashboard")
            .on(Ev::Click, close_nav())
            .st([
                St::DisplayFlex,
                St::ItemsCenter,
                St::GapSm,
                St::PyXs,
                St::PxMd,
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
            ]),
    );
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

fn workspace_row(id: &str, name: &str, active: bool) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    Link::route(&format!("/w/{id}"))
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
            chat::icon_slot(icons::icon_sized(icons::Icon::Folder, 16)),
            el(El::Span).text(name),
        ])
}

/// The primary action, sitting directly above the conversation list it creates into.
fn new_chat_bar() -> ElementBuilder {
    el(El::Div)
        .st([St::PxMd, St::PbSm])
        .append([chat::new_chat_button()])
}

/// Dims the page behind the mobile drawer; tap to close. Never shown on `md+`.
#[renderer]
fn backdrop(app: &App) -> ElementBuilder {
    if !app.nav_open {
        return el(El::Div).style(Style::new().display("none"));
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

/// The scrollable conversation list fills the drawer below the head. It's a plain
/// flex child (so it actually grows) wrapping the `thread_list` renderer.
fn drawer_threads() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::PySm])
        .append([chat::thread_list()])
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
        .attr("href", "/logout")
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
    let mut row = vec![
        chat::accent_bar(active),
        chat::icon_slot(icons::icon_sized(tab.icon(), 16)),
        el(El::Span).st([St::Flex1]).text(tab.label()),
    ];
    if tab == Tab::Review {
        row.push(review_badge());
    }
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

/// The Review nav row's count badge — how many tasks are parked at a human gate. Its own
/// `App` renderer so it stays live; empty (a zero-width span) when nothing's waiting.
#[renderer]
fn review_badge(app: &App) -> ElementBuilder {
    if app.review_count == 0 {
        return el(El::Span);
    }
    el(El::Span)
        .st([
            St::TextXs,
            St::PxXs,
            St::RoundedSm,
            St::BgAccent,
            St::TextOnAccent,
        ])
        .text(&app.review_count.to_string())
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
            el(El::Span)
                .st([St::FontMono, St::TextXs, St::TextMuted])
                .text(label),
        ])
}

// ── Main area ─────────────────────────────────────────────────────────────────

fn main_area() -> ElementBuilder {
    // A flex child that fills the space beside the sidebar (or the whole width on
    // mobile). `min-width:0` lets it shrink so chat content can't force a horizontal
    // scroll. The `outlet()` view fills the positioned content wrapper.
    el(El::Div)
        .style(Style::new().min_width("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::OverflowHidden])
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
    el(El::Button)
        .attr("type", "button")
        .attr("aria-label", "Toggle navigation")
        .on(Ev::Click, toggle_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::PxSm,
            St::PySm,
            St::BgApp,
            St::BorderDefault,
            St::RoundedSm,
            St::TextHigh,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .append([icons::icon_sized(icons::Icon::Menu, 18)])
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
    // Opening the review inbox loads the tasks parked at a human gate.
    if path == "/review" {
        bridge::send(Command::LoadReview);
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
    // Entering a workspace's Tasks tab projects its lane.
    if let Some(wid) = RoutePattern::new("/w/:wid/tasks")
        .matches(path)
        .and_then(|params| params.get("wid").map(str::to_owned))
    {
        bridge::send(Command::LoadTasks { workspace: wid });
    }
    // A task's detail view loads its run chain.
    if let Some(tid) = RoutePattern::new("/w/:wid/tasks/:tid")
        .matches(path)
        .and_then(|params| params.get("tid").map(str::to_owned))
    {
        bridge::send(Command::LoadTaskDetail { task_id: tid });
    }
    if let Some(id) = RoutePattern::new("/chat/:id")
        .matches(path)
        .and_then(|params| params.get("id").map(str::to_owned))
    {
        bridge::send(Command::OpenChat(id));
    }
}

/// The workspace id a path is within (any `/w/:wid…` route), else `None`.
fn workspace_of(path: &str) -> Option<String> {
    for pattern in [
        "/w/:wid",
        "/w/:wid/files",
        "/w/:wid/tasks",
        "/w/:wid/tasks/:tid",
        "/w/:wid/team",
    ] {
        if let Some(params) = RoutePattern::new(pattern).matches(path)
            && let Some(wid) = params.get("wid")
        {
            return Some(wid.to_owned());
        }
    }
    None
}
