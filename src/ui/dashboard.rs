//! The global dashboard: a cross-workspace overview — one card per workspace with its
//! owner, team size, and (later) task-lane status. The only place every workspace is
//! visible at once. Renders from the projected `App.workspaces`.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Input, Text};

use crate::bridge::{self, Command};
use crate::state::{App, WorkspaceView};
use crate::ui::widgets::{page_header, submit};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Dashboard",
                "Every workspace at a glance — owners, teams, and what's running.",
                create_form(),
            ),
            grid(),
        ])
}

/// The inline create affordance in the header: a name input + Create, posting a new
/// workspace. It then appears in the grid to be opened + configured (owner/members) in its
/// Team tab.
fn create_form() -> ElementBuilder {
    el(El::Form)
        .on(Ev::Submit, create())
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            Input::text()
                .name("name")
                .placeholder("New workspace…")
                .build(),
            submit("Create"),
        ])
}

#[handler]
fn create(app: &mut App, ctx: &EventContext) {
    let _ = app;
    let name = ctx.field("name").unwrap_or_default().trim().to_owned();
    if !name.is_empty() {
        bridge::send(Command::CreateWorkspace { name });
    }
}

#[renderer]
fn grid(app: &App) -> ElementBuilder {
    if app.workspaces.is_empty() {
        return Text::body("No workspaces yet. Create one in System → Workspaces.")
            .muted()
            .build();
    }
    let owner_name = |w: &WorkspaceView| {
        w.owner
            .as_ref()
            .and_then(|id| app.agents.iter().find(|a| &a.id == id))
            .map_or_else(|| "no owner".to_owned(), |a| a.name.clone())
    };
    let lane = |id: &str| {
        app.dashboard_lanes
            .iter()
            .find(|l| l.workspace_id == id)
            .map_or((None, 0, 0), |l| (l.running.clone(), l.queued, l.awaiting))
    };
    let cards: Vec<ElementBuilder> = app
        .workspaces
        .iter()
        .map(|w| {
            let (running, queued, awaiting) = lane(&w.id);
            card(w, &owner_name(w), running.as_deref(), queued, awaiting)
        })
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

fn card(
    workspace: &WorkspaceView,
    owner: &str,
    running: Option<&str>,
    queued: usize,
    awaiting: usize,
) -> ElementBuilder {
    let team = match workspace.member_names.len() {
        0 => "no members".to_owned(),
        n => format!("{n} member{}", if n == 1 { "" } else { "s" }),
    };
    // The lane line: the running task (accented) with the queue depth, else idle.
    let (lane_text, lane_style) = match running {
        Some(title) => {
            let suffix = if queued > 0 {
                format!(" · {queued} queued")
            } else {
                String::new()
            };
            (format!("▶ {title}{suffix}"), St::TextAccent)
        }
        None if queued > 0 => (format!("{queued} queued"), St::TextMuted),
        None => ("idle".to_owned(), St::TextLow),
    };
    let mut rows = vec![
        Text::body(workspace.name.clone())
            .build()
            .st([St::TextHigh, St::FontSemibold]),
        Text::caption(format!("owner {owner} · {team}"))
            .muted()
            .build(),
        el(El::Div).st([St::TextXs, lane_style]).text(&lane_text),
    ];
    // The "needs you" line — tasks parked at a human gate in this workspace.
    if awaiting > 0 {
        rows.push(
            Text::caption(format!("● needs you: {awaiting}"))
                .accent()
                .build()
                .st([St::FontSemibold]),
        );
    }
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
        .append(rows)
}
