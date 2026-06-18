//! The app shell: a fixed left drawer (brand, nav, conversations) beside a fixed
//! main area that fills the viewport. Access is gated by rwire's form-login
//! (`Server::auth`), so an unauthenticated client never reaches this tree.
//!
//! Layout note: every `#[renderer]` is wrapped by rwire in an inline `__synced_`
//! span, which can't act as a flex/positioning container. So the structural
//! containers here are plain elements (drawer, main, panels), and renderer outputs
//! only ever sit *inside* them.

mod chat;
mod pages;

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::Text;

use crate::state::{App, Tab};

const DRAWER_W: &str = "16rem";

pub fn app() -> ElementBuilder {
    el(El::Div).st([St::BgApp]).append([drawer(), main_area()])
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
        .st([St::PxMd, St::PyXs, St::TextXs, St::TextMuted, St::FontMono])
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
            el(El::Span)
                .st([St::FontMono, St::FontSemibold, St::TextLg, St::TextAccent])
                .text("claw"),
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
fn nav(app: &App) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::Gap0])
        .append(Tab::ALL.map(|tab| nav_row(tab, app.tab == tab)))
}

fn nav_row(tab: Tab, active: bool) -> ElementBuilder {
    let row = el(El::Div)
        .data("tab", tab.slug())
        .on(Ev::Click, select_tab())
        .st([
            St::PxMd,
            St::PySm,
            St::RoundedMd,
            St::CursorPointer,
            St::FontMono,
            St::TextSm,
        ])
        .hover([St::BgHover])
        .append([Text::body(tab.label().to_owned()).build()]);
    if active {
        row.st([St::BgActive, St::TextHigh])
    } else {
        row.st([St::TextMuted])
    }
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
        .append([content()])
}

#[renderer]
fn content(app: &App) -> ElementBuilder {
    // All panels stay mounted (a renderer introduced after first paint renders
    // empty); the inactive ones are display:none. Each fills the positioned main.
    // The chat panel nests the `transcript` renderer inside this one — safe since
    // rwire's nested synced-region updates reconcile children correctly.
    el(El::Div).style(fill()).append([
        panel(app.tab == Tab::Chat, chat::surface()),
        panel(app.tab == Tab::System, pages::admin()),
        panel(
            app.tab == Tab::Activity,
            pages::placeholder("Activity", "Live run activity lands in Phase 4."),
        ),
        panel(
            app.tab == Tab::Logs,
            pages::placeholder("Logs", "Structured logs land in Phase 4."),
        ),
    ])
}

fn panel(active: bool, body: ElementBuilder) -> ElementBuilder {
    if active {
        el(El::Div)
            .style(fill())
            .st([St::DisplayFlex, St::FlexCol, St::OverflowHidden])
            .append([body])
    } else {
        // Only display:none — no competing display utility (else flex would win by
        // source order and every panel would show at once).
        el(El::Div)
            .style(Style::new().display("none"))
            .append([body])
    }
}

/// `position:absolute; inset:0` — fills the nearest positioned ancestor regardless
/// of the inline synced spans in between.
fn fill() -> Style {
    Style::new().position("absolute").set("inset", "0")
}

#[handler]
fn select_tab(app: &mut App, ctx: &EventContext) {
    if let Some(tab) = ctx.data("tab").and_then(Tab::from_slug) {
        app.tab = tab;
    }
}
