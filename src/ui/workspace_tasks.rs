//! The workspace **Tasks** section: the lane's create form + task cards. Split out of
//! `workspace.rs` (the shell/header/Team live there) so the board UI — cards, the create
//! form, and the per-card edit drawer — has room to grow. Renders from `App.tasks`, the
//! active workspace's projected lane.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, TaskView, WorkspaceView};
use crate::ui::widgets::accent_submit;
use crate::ui::workspace::card;

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

fn task_card(task: &TaskView) -> ElementBuilder {
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
