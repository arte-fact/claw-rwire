//! The app shell: a top nav bar over a swappable content area. The nav is driven
//! by per-connection `Ui::tab`; a liveness dot reads process-global `App`. Access
//! is gated by rwire's HTTP Basic auth (`Server::basic_auth`), so an unauthenticated
//! client never reaches this tree — no in-app gate needed.

mod chat;
mod tabs;

use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use crate::state::{App, Tab, Ui};

pub fn app() -> ElementBuilder {
    el(El::Div)
        .st([St::BgApp, St::MinHScreen, St::DisplayFlex, St::FlexCol])
        .append([header_bar(), content()])
}

fn header_bar() -> ElementBuilder {
    el(El::Header)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyBetween,
            St::PxLg,
            St::BgSurface,
            St::BorderBDefault,
        ])
        .append([brand_and_nav(), status()])
}

fn brand_and_nav() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapLg])
        .append([
            el(El::Span)
                .st([St::FontMono, St::FontSemibold, St::TextLg, St::TextAccent])
                .text("claw"),
            nav(),
        ])
}

#[renderer]
fn nav(ui: &Ui) -> ElementBuilder {
    el(El::Nav)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append(Tab::ALL.map(|tab| nav_item(tab, ui.tab == tab)))
}

fn nav_item(tab: Tab, active: bool) -> ElementBuilder {
    let base = el(El::A)
        .text(tab.label())
        .data("tab", tab.slug())
        .on(Ev::Click, select_tab())
        .st([
            St::PxMd,
            St::PyMd,
            St::CursorPointer,
            St::FontMono,
            St::TextSm,
        ]);
    if active {
        base.st([St::TextAccent, St::BorderBAccent])
    } else {
        base.st([St::TextMuted])
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
                .st([St::FontMono, St::TextSm, St::TextMuted])
                .text(label),
        ])
}

#[renderer]
fn content(ui: &Ui) -> ElementBuilder {
    // Every panel stays mounted; switching tabs only toggles visibility. rwire
    // won't populate a synced renderer that appears after first paint, so removing
    // and re-adding a panel (the chat surface in particular) would render it empty.
    el(El::Main)
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::OverflowHidden])
        .append([
            panel(ui.tab == Tab::Chat, chat::surface()),
            panel(
                ui.tab == Tab::System,
                tabs::placeholder("System Map", "The layered admin lands in Phase 3."),
            ),
            panel(
                ui.tab == Tab::Activity,
                tabs::placeholder("Activity", "Live run activity lands in Phase 4."),
            ),
            panel(
                ui.tab == Tab::Logs,
                tabs::placeholder("Logs", "Structured logs land in Phase 4."),
            ),
        ])
}

fn panel(active: bool, content: ElementBuilder) -> ElementBuilder {
    // Inactive panels must carry ONLY `display:none` — adding the flex display
    // utility too lets `display:flex` win by stylesheet source order, leaving every
    // panel visible (all four tabs stacked).
    if active {
        el(El::Div)
            .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::OverflowHidden])
            .append([content])
    } else {
        el(El::Div).st([St::DisplayNone]).append([content])
    }
}

#[handler]
fn select_tab(ui: &mut Ui, ctx: &EventContext) {
    if let Some(tab) = ctx.data("tab").and_then(Tab::from_slug) {
        ui.tab = tab;
    }
}
