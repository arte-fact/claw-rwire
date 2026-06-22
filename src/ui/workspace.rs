//! A workspace view: a header (name · owner · team) + a tab row (Dashboard / Files / Tasks
//! / Team) + the active section's content. The route closure knows the workspace id and
//! which tab is active, so the shell + tabs are built statically; the header and the
//! Dashboard/Team sections are `App` renderers that look the live `WorkspaceView` up by
//! `App.active_workspace` (set by `on_route`).

use rwire::router::Link;
use rwire::style::Style;
use rwire::{At, El, ElementBuilder, Ev, St, el, handler, icons, renderer};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::{App, WorkspaceView};
use crate::ui::files;
use crate::ui::widgets::accent_submit;

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
            card("Brief", brief_editor(&workspace.id, &app.workspace_brief)),
        ])
}

/// The inline `AGENT.md` editor on the Overview — the workspace's shared project brief.
/// Keyed by workspace id so switching workspaces loads a fresh textarea.
fn brief_editor(wid: &str, content: &str) -> ElementBuilder {
    el(El::Form)
        .on(Ev::Submit, save_brief())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Textarea)
                .id(&format!("brief-{wid}"))
                .attr("name", "brief")
                .attr("spellcheck", "false")
                .style(
                    Style::new()
                        .set("font", "inherit")
                        .set("resize", "vertical")
                        .set("min-height", "9rem"),
                )
                .st([
                    St::WFull,
                    St::PSm,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::BgApp,
                    St::TextHigh,
                    St::FontMono,
                    St::TextXs,
                    St::OutlineNone,
                ])
                .focus([St::BorderAccent])
                .text(content),
            el(El::Div)
                .st([St::DisplayFlex])
                .append([accent_submit(icons::Icon::Check, "Save brief")]),
        ])
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
#[renderer]
pub fn tasks(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("Select a workspace.");
    };
    let mut cards: Vec<ElementBuilder> = app.tasks.iter().map(task_card).collect();
    if cards.is_empty() {
        cards.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted, St::PySm])
                .text("No tasks yet."),
        );
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card("New task", create_form(app, workspace)),
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapSm])
                .append(cards),
        ])
}

fn create_form(app: &App, workspace: &WorkspaceView) -> ElementBuilder {
    // Assignee options: the workspace's members (the owner is the default).
    let mut assignee = vec![el(El::Option).attr("value", "").text("— owner —")];
    for agent in app
        .agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
    {
        assignee.push(el(El::Option).attr("value", &agent.id).text(&agent.name));
    }
    el(El::Form)
        .on(Ev::Submit, submit_task())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Input)
                .attr("name", "title")
                .attr("placeholder", "Task title")
                .attr("autocomplete", "off")
                .style(Style::new().set("font", "inherit"))
                .st(input_st())
                .focus([St::BorderAccent]),
            el(El::Textarea)
                .attr("name", "spec")
                .attr("rows", "3")
                .at_str(rwire::At::Placeholder, "What should the agent do?")
                .style(
                    Style::new()
                        .set("font", "inherit")
                        .set("resize", "vertical"),
                )
                .st(input_st())
                .focus([St::BorderAccent]),
            el(El::Input)
                .attr("name", "schedule")
                .attr("placeholder", "Cron schedule — optional, e.g. 0 8 * * *")
                .attr("autocomplete", "off")
                .style(Style::new().set("font", "inherit"))
                .st(input_st())
                .focus([St::BorderAccent]),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    el(El::Select)
                        .attr("name", "agent")
                        .st(input_st())
                        .style(Style::new().set("flex", "1"))
                        .append(assignee),
                    accent_submit(icons::Icon::Plus, "Queue task"),
                ]),
        ])
}

fn input_st() -> [St; 8] {
    [
        St::WFull,
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
}

fn task_card(task: &crate::state::TaskView) -> ElementBuilder {
    let mut head: Vec<ElementBuilder> = vec![
        el(El::Span)
            .st([St::Flex1, St::TextSm, St::TextHigh, St::FontSemibold])
            .text(&task.title),
        status_badge(&task.status),
    ];
    if !matches!(task.status.as_str(), "done" | "failed" | "cancelled") {
        head.push(
            crate::ui::widgets::icon_button(icons::Icon::Close, cancel_task(), false)
                .data("task", &task.id),
        );
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(head),
        el(El::Div).st([St::TextXs, St::TextMuted]).text(&task.spec),
    ];
    let mut meta = task.agent.as_deref().unwrap_or("owner").to_owned();
    if task.recurrent {
        meta.push_str(" · ⟳");
        if let Some(schedule) = &task.schedule {
            meta.push_str(&format!(" {schedule}"));
        }
        if let Some(next) = &task.next_run {
            meta.push_str(&format!(" · next {next}"));
        }
    }
    rows.push(el(El::Div).st([St::TextXs, St::TextLow]).text(&meta));
    if let Some(result) = task
        .result
        .as_deref()
        .filter(|r| !r.is_empty() && matches!(task.status.as_str(), "done" | "failed"))
    {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted, St::Italic])
                .text(result),
        );
    }
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::RoundedSm,
            St::PMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

fn status_badge(status: &str) -> ElementBuilder {
    let color = match status {
        "running" => St::TextAccent,
        "done" => St::TextHigh,
        "failed" => St::TextError,
        _ => St::TextMuted,
    };
    el(El::Span)
        .st([
            St::TextXs,
            color,
            St::BorderDefault,
            St::RoundedSm,
            St::PxXs,
        ])
        .text(status)
}

#[handler]
fn submit_task(app: &mut App, ctx: &EventContext) {
    let Some(workspace) = app.active_workspace.clone() else {
        return;
    };
    let title = ctx.field("title").unwrap_or_default().trim().to_owned();
    let spec = ctx.field("spec").unwrap_or_default().trim().to_owned();
    if title.is_empty() || spec.is_empty() {
        return;
    }
    let agent = ctx
        .field("agent")
        .map(str::to_owned)
        .filter(|v| !v.is_empty());
    let schedule = ctx
        .field("schedule")
        .map(str::to_owned)
        .filter(|v| !v.trim().is_empty());
    bridge::send(Command::CreateTask {
        workspace,
        title,
        spec,
        agent,
        schedule,
    });
}

#[handler]
fn cancel_task(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::CancelTask { id: id.to_owned() });
    }
}

/// The Team section — the workspace's name, **owner**, and **members**, all editable inline
/// (immediate-apply: each change fires a command and re-projects). Plus a guarded delete.
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
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card("Name", name_field(&workspace.name)),
            card("Owner", owner_field(app, workspace.owner.as_deref())),
            card("Members", members_field(app, workspace)),
            card("Danger zone", delete_block(app.workspace_delete_armed)),
        ])
}

fn field_st() -> [St; 8] {
    [
        St::WFull,
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
}

fn name_field(name: &str) -> ElementBuilder {
    el(El::Input)
        .attr("name", "wsname")
        .attr("value", name)
        .attr("autocomplete", "off")
        .on(Ev::Change, rename())
        .style(Style::new().set("font", "inherit"))
        .st(field_st())
        .focus([St::BorderAccent])
}

fn owner_field(app: &App, owner: Option<&str>) -> ElementBuilder {
    let none = el(El::Option).attr("value", "").text("— no owner —");
    let mut options = vec![if owner.is_none() {
        none.bool_attr(At::Selected)
    } else {
        none
    }];
    for agent in &app.agents {
        let option = el(El::Option).attr("value", &agent.id).text(&agent.name);
        options.push(if owner == Some(agent.id.as_str()) {
            option.bool_attr(At::Selected)
        } else {
            option
        });
    }
    el(El::Select)
        .attr("name", "owner")
        .on(Ev::Change, set_owner())
        .st(field_st())
        .append(options)
}

/// One toggle-chip per agent: on = a member (the owner's chip is on + can't be toggled off).
fn members_field(app: &App, workspace: &WorkspaceView) -> ElementBuilder {
    if app.agents.is_empty() {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("No agents to add.");
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
        .style(Style::new().set("flex-wrap", "wrap"))
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
        .attr("type", "button")
        .data("agent", id)
        .data("on", if on { "1" } else { "0" })
        .on(Ev::Click, toggle_member())
        .style(Style::new().set("font", "inherit"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::PxSm,
            St::PyXs,
            St::BorderDefault,
            St::RoundedSm,
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
                el(El::Span)
                    .st([St::TextXs, St::TextError])
                    .text("Delete this workspace and its files?"),
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
