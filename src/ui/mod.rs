//! The app shell: a fixed left drawer (brand, nav, conversations) beside a fixed
//! main area that fills the viewport. Access is gated by rwire's form-login
//! (`Server::auth`), so an unauthenticated client never reaches this tree.
//!
//! Layout note: every `#[renderer]` is wrapped by rwire in an inline `__synced_`
//! span, which can't act as a flex/positioning container. So the structural
//! containers here are plain elements (drawer, main, the per-view wrapper), and
//! renderer outputs — including the router `outlet()` — only ever sit *inside* them.

mod activity;
mod chat;
mod logs;
mod system_map;
mod widgets;

use rwire::router::Link;
use rwire::router::RoutePattern;
use rwire::style::Style;
use rwire::{
    CurrentRoute, El, ElementBuilder, Ev, Router, St, el, handler, icons, outlet, renderer,
};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::{App, Tab};

/// The app's routes: each path renders its view into the shell's [`outlet`]. `/` and
/// `/chat/:id` share the chat surface (the `:id` is opened by [`on_route`]).
pub fn routes() -> Router {
    Router::new()
        .page("/", |_| view(chat::surface()))
        .page("/chat/:id", |_| view(chat::surface()))
        .page("/system", |_| view(system_map::page()))
        .page("/activity", |_| view(activity::page()))
        .page("/logs", |_| view(logs::page()))
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
        .style(Style::new().set("height", "100vh"))
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
                .set("height", "100vh")
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
        .append([drawer_head(), conversations_label(), drawer_threads()])
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

fn conversations_label() -> ElementBuilder {
    el(El::Div)
        .style(
            Style::new()
                .text_transform("uppercase")
                .set("letter-spacing", "0.06em"),
        )
        .st([St::PyMd, St::PxXs, St::TextXs, St::TextMuted, St::FontMono])
        .append([el(El::Span).text("Conversations")])
}

fn drawer_head() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm, St::PMd])
        .append([brand_row(), nav(), chat::new_chat_button()])
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
            // Glyph + wordmark; the icon inherits currentColor from the accent text.
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
                    icons::icon_sized(icons::Icon::Terminal, 18),
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

#[renderer]
fn nav(route: &CurrentRoute) -> ElementBuilder {
    let path = route.path();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::Gap0])
        .append(Tab::ALL.map(|tab| nav_row(tab, tab_active(path, tab))))
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
    // Flat: a left accent bar + accent text marks the active item (no filled pill).
    let bar = el(El::Span)
        .style(
            Style::new()
                .width("2px")
                .set("flex", "0 0 2px")
                .set("align-self", "stretch"),
        )
        .st([if active { St::BgAccent } else { St::BgApp }]);
    let label = if active {
        Text::body(tab.label().to_owned())
            .build()
            .st([St::TextAccent])
    } else {
        Text::body(tab.label().to_owned())
            .build()
            .st([St::TextMuted])
    };
    // `Link::route` is rwire's client-routed `<a>`: the client router intercepts the
    // click, pushes history, and re-renders the matched view into the outlet. The
    // extra `close_nav` collapses the mobile drawer once you've picked a destination.
    Link::route(tab.path())
        .on(Ev::Click, close_nav())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PySm,
            St::PxXs,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
        ])
        .hover([St::TextHigh])
        .append([bar, label])
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
    let _ = app; // side-effect only: no `App` writes, so this emits no re-render
    let Some(path) = ctx.text() else {
        return;
    };
    // Leaving any view closes the System Map drawer (its `editing` is shared state, so
    // it would otherwise re-open when you return to `/system`).
    bridge::send(Command::CloseDrawer);
    if let Some(id) = RoutePattern::new("/chat/:id")
        .matches(path)
        .and_then(|params| params.get("id").map(str::to_owned))
    {
        bridge::send(Command::OpenChat(id));
    }
}
