//! The app shell: a top nav bar over a swappable content area. The nav is driven
//! by per-connection `Ui::tab`; a liveness dot reads process-global `App`. Because
//! an rwire renderer takes exactly one state type, the bar is assembled by a plain
//! function that composes the `Ui`-synced nav and the `App`-synced status side by
//! side.

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
    let panel = match ui.tab {
        Tab::Chat => tabs::chat(),
        Tab::System => tabs::placeholder("System Map", "The layered admin lands in Phase 3."),
        Tab::Activity => tabs::placeholder("Activity", "Live run activity lands in Phase 4."),
        Tab::Logs => tabs::placeholder("Logs", "Structured logs land in Phase 4."),
    };
    el(El::Main)
        .st([St::Flex1, St::PLg, St::DisplayFlex, St::FlexCol])
        .append([panel])
}

#[handler]
fn select_tab(ui: &mut Ui, ctx: &EventContext) {
    if let Some(tab) = ctx.data("tab").and_then(Tab::from_slug) {
        ui.tab = tab;
    }
}
