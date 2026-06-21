//! A workspace view: a header (name · owner · team) + a tab row (Dashboard / Files / Tasks
//! / Team) + the active section's content. The route closure knows the workspace id and
//! which tab is active, so the shell + tabs are built statically; the header and the
//! Dashboard/Team sections are `App` renderers that look the live `WorkspaceView` up by
//! `App.active_workspace` (set by `on_route`).

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use crate::state::App;
use crate::ui::files;

/// The sections of a workspace, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsTab {
    Dashboard,
    Files,
    Tasks,
    Team,
}

impl WsTab {
    fn label(self) -> &'static str {
        match self {
            WsTab::Dashboard => "Overview",
            WsTab::Files => "Files",
            WsTab::Tasks => "Tasks",
            WsTab::Team => "Team",
        }
    }

    /// The route for this tab within workspace `wid`.
    fn path(self, wid: &str) -> String {
        match self {
            WsTab::Dashboard => format!("/w/{wid}"),
            WsTab::Files => format!("/w/{wid}/files"),
            WsTab::Tasks => format!("/w/{wid}/tasks"),
            WsTab::Team => format!("/w/{wid}/team"),
        }
    }
}

const TABS: [WsTab; 4] = [WsTab::Dashboard, WsTab::Files, WsTab::Tasks, WsTab::Team];

/// The framed workspace view for route `wid`, with `active` tab highlighted and `content`
/// below the tab row.
pub fn shell(wid: &str, active: WsTab, content: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([header(), tab_row(wid, active), content])
}

/// The workspace masthead: its name + an owner/team line, from the live `WorkspaceView`.
#[renderer]
fn header(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return Text::heading1("Workspace").build();
    };
    let owner = workspace
        .owner
        .as_ref()
        .and_then(|id| app.agents.iter().find(|a| &a.id == id))
        .map(|a| a.name.as_str())
        .unwrap_or("no owner");
    let team = match workspace.member_names.len() {
        0 => "no members".to_owned(),
        n => format!("{n} member{}", if n == 1 { "" } else { "s" }),
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            Text::heading1(workspace.name.clone()).build(),
            Text::body(format!("owner {owner} · {team}"))
                .muted()
                .build(),
        ])
}

fn tab_row(wid: &str, active: WsTab) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("flex-wrap", "wrap"))
        .st([St::DisplayFlex, St::GapSm, St::BorderBDefault, St::PbSm])
        .append(TABS.map(|tab| tab_link(tab, &tab.path(wid), tab == active)))
}

fn tab_link(tab: WsTab, path: &str, active: bool) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    let mut link = Link::route(path)
        .st([
            St::PxSm,
            St::PySm,
            St::RoundedSm,
            St::TextSm,
            St::NoUnderline,
            St::CursorPointer,
            color,
        ])
        .hover([St::TextHigh])
        .append([el(El::Span).text(tab.label())]);
    if active {
        link = link.st([St::BgSubtle]);
    }
    link
}

// ── Sections ────────────────────────────────────────────────────────────────────

/// The Files section — the workspace's two-pane browser/editor (scoped via the route).
pub fn files_section() -> ElementBuilder {
    files::section()
}

/// The Overview (default) section: owner, the team roster, and a pointer to the brief.
#[renderer]
pub fn dashboard(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("Select a workspace.");
    };
    let owner = workspace.owner.clone();
    let mut roster: Vec<ElementBuilder> = app
        .agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
        .map(|a| {
            let role = if Some(&a.id) == owner.as_ref() {
                " · owner"
            } else {
                ""
            };
            el(El::Div)
                .st([St::TextSm, St::TextHigh])
                .text(&format!("{}{role}", a.name))
        })
        .collect();
    if roster.is_empty() {
        roster.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted])
                .text("No members yet."),
        );
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card(
                "Team",
                el(El::Div)
                    .st([St::DisplayFlex, St::FlexCol, St::GapXs])
                    .append(roster),
            ),
            card(
                "Brief",
                el(El::Div).st([St::TextSm, St::TextMuted]).text(
                    "The workspace's AGENT.md is its shared project brief — edit it in the \
                     Files tab.",
                ),
            ),
        ])
}

/// The Tasks section — a placeholder until the task board (Phase C) lands.
pub fn tasks() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            Text::body("Tasks")
                .build()
                .st([St::TextHigh, St::FontSemibold]),
            el(El::Div).st([St::TextSm, St::TextMuted]).text(
                "This workspace's task queue will live here — sequential within the workspace, \
             parallel across workspaces. Coming with the task board.",
            ),
        ])
}

/// The Team section — owner + members. For now read-only; managed in the System Map's
/// Workspaces tier (a later increment moves management here).
#[renderer]
pub fn team(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("Select a workspace.");
    };
    let owner_name = workspace
        .owner
        .as_ref()
        .and_then(|id| app.agents.iter().find(|a| &a.id == id))
        .map(|a| a.name.as_str())
        .unwrap_or("—");
    let members = if workspace.member_names.is_empty() {
        "No members yet.".to_owned()
    } else {
        workspace.member_names.join(", ")
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card(
                "Owner",
                el(El::Div).st([St::TextSm, St::TextHigh]).text(owner_name),
            ),
            card(
                "Members",
                el(El::Div).st([St::TextSm, St::TextHigh]).text(&members),
            ),
            el(El::Div).st([St::TextXs, St::TextMuted]).append([
                el(El::Span).text("Manage the owner + members in "),
                Link::route("/system")
                    .st([St::TextAccent, St::NoUnderline, St::CursorPointer])
                    .append([el(El::Span).text("System → Workspaces")]),
                el(El::Span).text("."),
            ]),
        ])
}

/// A titled card block (label + body), reusing the dashboard surface styling.
fn card(label: &str, body: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::RoundedSm,
            St::PMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append([
            el(El::Div)
                .st([
                    St::TextXs,
                    St::TextMuted,
                    St::TextUppercase,
                    St::TrackingWide,
                ])
                .text(label),
            body,
        ])
}
