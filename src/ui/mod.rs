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

use rwire::router::Link;
use rwire::router::RoutePattern;
use rwire::style::Style;
use rwire::{CurrentRoute, El, ElementBuilder, Router, St, el, handler, icons, outlet, renderer};
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
    // Mono everywhere, small text — the terminal feel of rust-claw. font-family and
    // font-size inherit through the fixed drawer/main, so set them once at the root.
    el(El::Div)
        .st([St::BgApp, St::FontMono, St::TextSm])
        .append([drawer(), main_area()])
}

// ── Drawer ───────────────────────────────────────────────────────────────────

fn drawer() -> ElementBuilder {
    el(El::Aside)
        .style(
            Style::new()
                .position("fixed")
                .set("top", "0")
                .set("left", "0")
                .set("bottom", "0")
                .width(DRAWER_W),
        )
        .st([
            St::BgSidebar,
            St::BorderRDefault,
            St::DisplayFlex,
            St::FlexCol,
        ])
        .append([drawer_head(), conversations_label(), drawer_threads()])
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
    // click, pushes history, and re-renders the matched view into the outlet.
    Link::route(tab.path())
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
    // The `outlet()` is where the rwire router renders the matched route view; on
    // navigation it swaps the view (and registers its renderers) with no reload.
    el(El::Div)
        .style(
            Style::new()
                .position("fixed")
                .set("top", "0")
                .set("right", "0")
                .set("bottom", "0")
                .set("left", DRAWER_W),
        )
        .st([St::OverflowHidden])
        .append([outlet()])
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
    if let Some(id) = RoutePattern::new("/chat/:id")
        .matches(path)
        .and_then(|params| params.get("id").map(str::to_owned))
    {
        bridge::send(Command::OpenChat(id));
    }
}
