//! The global dashboard: a cross-workspace overview — one card per workspace with its
//! owner, team size, and (later) task-lane status. The only place every workspace is
//! visible at once. Renders from the projected `App.workspaces`.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use crate::state::{App, WorkspaceView};
use crate::ui::widgets::page_header;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Dashboard",
                "Every workspace at a glance — owners, teams, and what's running.",
                el(El::Div).style(Style::new().display("none")),
            ),
            grid(),
        ])
}

#[renderer]
fn grid(app: &App) -> ElementBuilder {
    if app.workspaces.is_empty() {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("No workspaces yet. Create one in System → Workspaces.");
    }
    let owner_name = |w: &WorkspaceView| {
        w.owner
            .as_ref()
            .and_then(|id| app.agents.iter().find(|a| &a.id == id))
            .map_or_else(|| "no owner".to_owned(), |a| a.name.clone())
    };
    let cards: Vec<ElementBuilder> = app
        .workspaces
        .iter()
        .map(|w| card(w, &owner_name(w)))
        .collect();
    el(El::Div)
        .style(
            Style::new()
                .set("display", "grid")
                .set(
                    "grid-template-columns",
                    "repeat(auto-fill, minmax(16rem, 1fr))",
                )
                .set("gap", "0.75rem"),
        )
        .append(cards)
}

fn card(workspace: &WorkspaceView, owner: &str) -> ElementBuilder {
    let team = match workspace.member_names.len() {
        0 => "no members".to_owned(),
        n => format!("{n} member{}", if n == 1 { "" } else { "s" }),
    };
    Link::route(&format!("/w/{}", workspace.id))
        .style(Style::new().set("font", "inherit"))
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
            St::BgSurface,
            St::BorderDefault,
            St::RoundedSm,
            St::PMd,
            St::NoUnderline,
            St::CursorPointer,
        ])
        .hover([St::BorderAccent])
        .append([
            Text::body(workspace.name.clone())
                .build()
                .st([St::TextHigh, St::FontSemibold]),
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text(&format!("owner {owner} · {team}")),
            // Task-lane status arrives with the task board (Phase C).
            el(El::Div).st([St::TextXs, St::TextLow]).text("idle"),
        ])
}
