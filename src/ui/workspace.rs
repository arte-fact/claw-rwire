//! A workspace view: a header (name · owner · team) + a tab row (Dashboard / Files / Tasks
//! / Team) + the active section's content. The route closure knows the workspace id and
//! which tab is active, so the shell + tabs are built statically; the header and the
//! Dashboard/Team sections are `App` renderers that look the live `WorkspaceView` up by
//! `App.active_workspace` (set by `on_route`).

use rwire::router::Link;
use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Avatar, AvatarSize, Card, Input, Select, Text, TextColor, Tooltip};

use crate::bridge::{self, Command};
use crate::state::{App, WorkspaceView};
use crate::ui::files;
use crate::ui::workspace_tasks::initials;

/// The sections of a workspace, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsTab {
    Dashboard,
    Files,
    Tasks,
    Workflow,
    Team,
}

impl WsTab {
    fn label(self) -> &'static str {
        match self {
            WsTab::Dashboard => "Overview",
            WsTab::Files => "Files",
            WsTab::Tasks => "Tasks",
            WsTab::Workflow => "Workflow",
            WsTab::Team => "Team",
        }
    }

    /// The route for this tab within workspace `wid`. The board (`Tasks`) is the bare `/w/{wid}`
    /// landing; Overview moved to its own path.
    fn path(self, wid: &str) -> String {
        match self {
            WsTab::Tasks => format!("/w/{wid}"),
            WsTab::Dashboard => format!("/w/{wid}/overview"),
            WsTab::Files => format!("/w/{wid}/files"),
            WsTab::Workflow => format!("/w/{wid}/workflow"),
            WsTab::Team => format!("/w/{wid}/team"),
        }
    }
}

// Board first — it's the landing view.
const TABS: [WsTab; 5] = [
    WsTab::Tasks,
    WsTab::Dashboard,
    WsTab::Files,
    WsTab::Workflow,
    WsTab::Team,
];

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
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            Text::heading1(workspace.name.clone()).build(),
            Text::body(format!("owner {owner} · {team}"))
                .muted()
                .build(),
            member_avatars(workspace),
        ])
}

/// A row of member avatars (initials, tooltip = name) — the team at a glance in the header.
fn member_avatars(workspace: &WorkspaceView) -> ElementBuilder {
    let avatars: Vec<ElementBuilder> = workspace
        .member_names
        .iter()
        .take(8)
        .map(|name| {
            Tooltip::new(name.clone())
                .child(
                    Avatar::new()
                        .fallback(initials(name))
                        .size(AvatarSize::Sm)
                        .build(),
                )
                .build()
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::FlexWrap])
        .append(avatars)
}

fn tab_row(wid: &str, active: WsTab) -> ElementBuilder {
    el(El::Div)
        .st([St::FlexWrap])
        .st([St::DisplayFlex, St::GapSm, St::BorderBDefault, St::PbSm])
        .append(TABS.map(|tab| tab_link(tab, &tab.path(wid), tab == active)))
}

fn tab_link(tab: WsTab, path: &str, active: bool) -> ElementBuilder {
    let color = if active { St::TextHigh } else { St::TextMuted };
    let mut link = Link::route(path)
        .st([
            St::PxSm,
            St::PySm,
            St::Rounded0,
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
        return Text::body("Select a workspace.").muted().build();
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
            Text::body(format!("{}{role}", a.name))
                .color(TextColor::High)
                .build()
        })
        .collect();
    if roster.is_empty() {
        roster.push(Text::body("No members yet.").muted().build());
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
            card("Delegation", auto_approve_toggle(workspace)),
            card("Brief", brief_editor(&workspace.id, &app.workspace_brief)),
            card(
                "Review standards",
                standards_editor(&workspace.id, &app.workspace_standards),
            ),
        ])
}

/// The auto-approve toggle (R3): off = delegated work waits at the human gates; on = the plan is
/// queued directly and per-task sign-off gates pass automatically, so the workspace runs hands-off
/// (an agent reviewer in a loop step still reviews — only the *human* gates are skipped). Clicking
/// flips it.
fn auto_approve_toggle(workspace: &WorkspaceView) -> ElementBuilder {
    let on = workspace.auto_approve;
    let (label, hint, chip) = if on {
        (
            "Auto-approve · ON",
            "Runs hands-off — the plan and per-task sign-offs pass automatically (agent review still runs).",
            St::BgAccent,
        )
    } else {
        (
            "Auto-approve · OFF",
            "Delegated work waits for your approval — at the plan gate and each task's sign-off.",
            St::BgApp,
        )
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Button)
                .at(At::Type, Av::Button)
                .data("workspace", &workspace.id)
                .data("on", if on { "1" } else { "0" })
                .on(Ev::Click, toggle_auto_approve())
                .st([St::FontInheritAll])
                .st([
                    St::SelfStart,
                    St::TextXs,
                    St::PxSm,
                    St::PyXs,
                    St::Rounded0,
                    St::BorderDefault,
                    chip,
                    if on { St::TextOnAccent } else { St::TextHigh },
                    St::CursorPointer,
                ])
                .text(label),
            Text::caption(hint).muted().build(),
        ])
}

#[handler]
fn toggle_auto_approve(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("workspace") {
        let currently_on = ctx.data("on") == Some("1");
        bridge::send(Command::SetWorkspaceAutoApprove {
            id: id.to_owned(),
            on: !currently_on,
        });
    }
}

/// The inline `AGENT.md` editor on the Overview — the workspace's shared project brief.
/// Keyed by workspace id so switching workspaces loads a fresh textarea.
fn brief_editor(wid: &str, content: &str) -> ElementBuilder {
    crate::ui::widgets::md_editor(
        format!("brief-{wid}"),
        "brief",
        content,
        "What this workspace is for — its goal, the project it owns…",
        "9rem",
        save_brief(),
        "Save brief",
    )
}

/// The validator standards memo (R5): curated review rules injected into every review run in
/// this workspace — validators stay memory-free otherwise. Rules, not a work log.
fn standards_editor(wid: &str, content: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div).st([St::TextXs, St::TextMuted]).text(
                "Rules the reviewer checks every deliverable against — kept short and curated.",
            ),
            crate::ui::widgets::md_editor(
                format!("standards-{wid}"),
                "standards",
                content,
                "e.g. tests must pass with no warnings; code lives under projects/<name>/ …",
                "6rem",
                save_standards(),
                "Save standards",
            ),
        ])
}

#[handler]
fn save_standards(app: &mut App, ctx: &EventContext) {
    let Some(id) = app.active_workspace.clone() else {
        return;
    };
    let content = ctx.field("standards").unwrap_or_default().to_owned();
    bridge::send(Command::SaveStandards {
        workspace: id,
        content,
    });
}

#[handler]
fn save_brief(app: &mut App, ctx: &EventContext) {
    let Some(id) = app.active_workspace.clone() else {
        return;
    };
    let content = ctx.field("brief").unwrap_or_default().to_owned();
    bridge::send(Command::SaveBrief {
        workspace: id,
        content,
    });
}

/// The Tasks section — the workspace's lane: a create form + the queued/running/finished
/// task cards. (The scheduler that *runs* them lands in the next increment; for now they
/// sit `pending`.)
/// The Team section — the workspace's name, **owner**, and **members**, all editable inline
/// (immediate-apply: each change fires a command and re-projects). Plus a guarded delete.
#[renderer]
pub fn team(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return Text::body("Select a workspace.").muted().build();
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card("Name", name_field(&workspace.name)),
            card("Owner", owner_field(app, workspace.owner.as_deref())),
            card("Members", members_field(app, workspace)),
            card("Danger zone", delete_block(app.workspace_delete_armed)),
        ])
}

fn name_field(name: &str) -> ElementBuilder {
    Input::text()
        .name("wsname")
        .value(name.to_owned())
        .build()
        .at(At::Autocomplete, Av::Off)
        .on(Ev::Change, rename())
}

fn owner_field(app: &App, owner: Option<&str>) -> ElementBuilder {
    let mut field = Select::new()
        .name("owner")
        .value(owner.unwrap_or("").to_owned())
        .option("", "— no owner —");
    for agent in &app.agents {
        field = field.option(agent.id.clone(), agent.name.clone());
    }
    field.build().on(Ev::Change, set_owner())
}

/// One toggle-chip per agent: on = a member (the owner's chip is on + can't be toggled off).
fn members_field(app: &App, workspace: &WorkspaceView) -> ElementBuilder {
    if app.agents.is_empty() {
        return Text::body("No agents to add.").muted().build();
    }
    let chips: Vec<ElementBuilder> = app
        .agents
        .iter()
        .map(|agent| {
            let is_owner = workspace.owner.as_deref() == Some(agent.id.as_str());
            let on = is_owner || workspace.members.iter().any(|m| m == &agent.id);
            member_chip(&agent.id, &agent.name, on, is_owner)
        })
        .collect();
    el(El::Div)
        .st([St::FlexWrap])
        .st([St::DisplayFlex, St::GapXs])
        .append(chips)
}

fn member_chip(id: &str, name: &str, on: bool, is_owner: bool) -> ElementBuilder {
    let (bg, fg, mark) = if on {
        (St::BgAccent, St::TextOnAccent, "✓")
    } else {
        (St::BgApp, St::TextMuted, "+")
    };
    let label = if is_owner {
        format!("{name} (owner)")
    } else {
        name.to_owned()
    };
    el(El::Button)
        .at(At::Type, Av::Button)
        .data("agent", id)
        .data("on", if on { "1" } else { "0" })
        .on(Ev::Click, toggle_member())
        .st([St::FontInheritAll])
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::PxSm,
            St::PyXs,
            St::BorderDefault,
            St::Rounded0,
            St::TextXs,
            St::CursorPointer,
            bg,
            fg,
        ])
        .append([el(El::Span).text(mark), el(El::Span).text(&label)])
}

fn delete_block(armed: bool) -> ElementBuilder {
    if armed {
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                Text::caption("Delete this workspace and its files?")
                    .error()
                    .build(),
                crate::ui::widgets::action_icon(
                    rwire::icons::Icon::Trash,
                    "Delete",
                    confirm_delete(),
                    true,
                ),
                crate::ui::widgets::action("Cancel", cancel_delete()),
            ])
    } else {
        crate::ui::widgets::action_icon(
            rwire::icons::Icon::Trash,
            "Delete workspace",
            arm_delete(),
            true,
        )
    }
}

// ── Team handlers (immediate-apply over the active workspace) ───────────────────

#[handler]
fn rename(app: &mut App, ctx: &EventContext) {
    let (Some(id), Some(name)) = (app.active_workspace.clone(), ctx.text()) else {
        return;
    };
    let name = name.trim().to_owned();
    if !name.is_empty() {
        bridge::send(Command::RenameWorkspace { id, name });
    }
}

#[handler]
fn set_owner(app: &mut App, ctx: &EventContext) {
    let Some(id) = app.active_workspace.clone() else {
        return;
    };
    let owner = ctx.text().map(str::to_owned).filter(|v| !v.is_empty());
    bridge::send(Command::SetWorkspaceOwner { id, owner });
}

#[handler]
fn toggle_member(app: &mut App, ctx: &EventContext) {
    let (Some(id), Some(agent)) = (app.active_workspace.clone(), ctx.data("agent")) else {
        return;
    };
    let agent = agent.to_owned();
    if ctx.data("on") == Some("1") {
        bridge::send(Command::RemoveMember { id, agent });
    } else {
        bridge::send(Command::AddMember { id, agent });
    }
}

#[handler]
fn arm_delete(app: &mut App) {
    app.workspace_delete_armed = true;
}

#[handler]
fn cancel_delete(app: &mut App) {
    app.workspace_delete_armed = false;
}

#[handler]
fn confirm_delete(app: &mut App) {
    app.workspace_delete_armed = false;
    if let Some(id) = app.active_workspace.clone() {
        bridge::send(Command::DeleteWorkspace { id });
    }
}

/// A titled card block (label + body), reusing the dashboard surface styling.
pub(crate) fn card(label: &str, body: ElementBuilder) -> ElementBuilder {
    Card::new()
        .children([
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
        .build()
}
