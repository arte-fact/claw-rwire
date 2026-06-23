//! The workspace **Tasks** section: the lane's create form + task cards. Split out of
//! `workspace.rs` (the shell/header/Team live there) so the board UI — cards, the create
//! form, and the per-card edit drawer — has room to grow. Renders from `App.tasks`, the
//! active workspace's projected lane.

use rwire::router::Link;
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
    // The assignable members (for the drawer's reassign select) and which card is open.
    let members: Vec<(String, String)> = app
        .agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
        .map(|a| (a.id.clone(), a.name.clone()))
        .collect();
    let board = if app.tasks_list_view {
        list_board(app, &members, &workspace.id)
    } else {
        kanban_board(app, &workspace.id)
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            card("New task", create_form(app, workspace)),
            view_toggle(app.tasks_list_view),
            board,
        ])
}

/// The flat list view (the original): cards with the inline edit drawer.
fn list_board(app: &App, members: &[(String, String)], wid: &str) -> ElementBuilder {
    let editing = app.editing_task.as_deref();
    let mut cards: Vec<ElementBuilder> = app
        .tasks
        .iter()
        .map(|task| task_card(task, members, editing == Some(task.id.as_str()), wid))
        .collect();
    if cards.is_empty() {
        cards.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted, St::PySm])
                .text("No tasks yet."),
        );
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

/// The kanban board: one column per pipeline step, then Done and Closed. A task sits in its
/// current step's column (or Done/Closed when terminal) and moves left→right as it advances.
fn kanban_board(app: &App, wid: &str) -> ElementBuilder {
    let step_count = app.pipeline_columns.len();
    let mut columns = app.pipeline_columns.clone();
    columns.push("Done".to_owned());
    columns.push("Closed".to_owned());
    let lanes: Vec<ElementBuilder> = columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let cards: Vec<&crate::state::TaskView> = app
                .tasks
                .iter()
                .filter(|task| task_column(task, step_count) == index)
                .collect();
            kanban_column(name, &cards, wid)
        })
        .collect();
    el(El::Div)
        .style(Style::new().set("overflow-x", "auto"))
        .st([St::DisplayFlex, St::GapMd])
        .append(lanes)
}

/// Which column index a task belongs to: its step (0..step_count), Done (step_count), or
/// Closed (step_count+1).
fn task_column(task: &crate::state::TaskView, step_count: usize) -> usize {
    match task.status.as_str() {
        "done" => step_count,
        "failed" | "rejected" | "cancelled" => step_count + 1,
        _ => (task.step_position.max(0) as usize).min(step_count.saturating_sub(1)),
    }
}

fn kanban_column(name: &str, tasks: &[&crate::state::TaskView], wid: &str) -> ElementBuilder {
    let mut items = vec![
        el(El::Div)
            .st([
                St::TextXs,
                St::TextMuted,
                St::TextUppercase,
                St::TrackingWide,
                St::PbXs,
            ])
            .text(&format!("{name} · {}", tasks.len())),
    ];
    for task in tasks {
        items.push(kanban_card(task, wid));
    }
    el(El::Div)
        .style(
            Style::new()
                .set("min-width", "12rem")
                .set("flex", "1 0 12rem"),
        )
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(items)
}

fn kanban_card(task: &TaskView, wid: &str) -> ElementBuilder {
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                Link::route(&format!("/w/{wid}/tasks/{}", task.id))
                    .style(Style::new().set("font", "inherit"))
                    .st([
                        St::Flex1,
                        St::TextXs,
                        St::TextHigh,
                        St::FontSemibold,
                        St::NoUnderline,
                    ])
                    .hover([St::TextAccent])
                    .text(&task.title),
                status_badge(&task.status),
            ]),
    ];
    let mut meta = task.agent.as_deref().unwrap_or("owner").to_owned();
    if task.iteration > 0 {
        meta.push_str(&format!(" · iter {}", task.iteration + 1));
    }
    if task.recurrent {
        meta.push_str(" · ⟳");
    }
    rows.push(el(El::Div).st([St::TextXs, St::TextLow]).text(&meta));
    // A one-click Approve right on the card for tasks parked at a human gate.
    if task.status == "awaiting" {
        rows.push(
            el(El::Button)
                .attr("type", "button")
                .data("task", &task.id)
                .on(Ev::Click, kanban_approve())
                .style(Style::new().set("font", "inherit"))
                .st([
                    St::TextXs,
                    St::PxSm,
                    St::PyXs,
                    St::RoundedSm,
                    St::BgAccent,
                    St::TextOnAccent,
                    St::BorderAccent,
                    St::CursorPointer,
                ])
                .text("Approve"),
        );
    }
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::RoundedSm,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

/// The Kanban ↔ List view switch (immediate-apply chips).
fn view_toggle(list: bool) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs])
        .append([
            toggle_chip("Board", "kanban", !list),
            toggle_chip("List", "list", list),
        ])
}

fn toggle_chip(label: &str, view: &str, active: bool) -> ElementBuilder {
    let chip = el(El::Button)
        .attr("type", "button")
        .data("view", view)
        .on(Ev::Click, toggle_view())
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::CursorPointer,
        ])
        .text(label);
    if active {
        chip.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        chip.st([St::BgApp, St::TextMuted]).hover([St::TextHigh])
    }
}

#[handler]
fn toggle_view(app: &mut App, ctx: &EventContext) {
    if let Some(view) = ctx.data("view") {
        app.tasks_list_view = view == "list";
    }
}

#[handler]
fn kanban_approve(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::ReviewVerdict {
            task_id: id.to_owned(),
            decision: "pass".to_owned(),
            feedback: String::new(),
        });
    }
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

/// The run-control buttons for a task card, by status: Stop while live, Start/Restart when
/// stopped, Restart when terminal, Edit only when not running (editing a live run is a no-op).
fn control_buttons(task: &TaskView) -> Vec<ElementBuilder> {
    let icon = |glyph, on_click| {
        crate::ui::widgets::icon_button(glyph, on_click, false).data("task", &task.id)
    };
    match task.status.as_str() {
        "running" => vec![
            icon(icons::Icon::Pause, stop_task()),
            icon(icons::Icon::Close, cancel_task()),
        ],
        "pending" => vec![
            icon(icons::Icon::Edit, edit_task()),
            icon(icons::Icon::Pause, stop_task()),
            icon(icons::Icon::Close, cancel_task()),
        ],
        "awaiting" => vec![
            icon(icons::Icon::Pause, stop_task()),
            icon(icons::Icon::Close, cancel_task()),
        ],
        "stopped" => vec![
            icon(icons::Icon::Play, start_task()),
            restart_button(&task.id),
            icon(icons::Icon::Edit, edit_task()),
            icon(icons::Icon::Close, cancel_task()),
        ],
        // Terminal (done | failed | rejected | cancelled): a clean-slate Restart.
        _ => vec![restart_button(&task.id)],
    }
}

/// A small text "Restart" button (there's no rotate glyph) carrying the task id.
fn restart_button(task_id: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("task", task_id)
        .on(Ev::Click, restart_task())
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .text("Restart")
}

#[handler]
fn stop_task(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::StopTask { id: id.to_owned() });
    }
}

#[handler]
fn start_task(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::StartTask { id: id.to_owned() });
    }
}

#[handler]
fn restart_task(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::RestartTask { id: id.to_owned() });
    }
}

fn task_card(
    task: &TaskView,
    members: &[(String, String)],
    open: bool,
    wid: &str,
) -> ElementBuilder {
    let mut head: Vec<ElementBuilder> = vec![
        // The title links to the task's detail view (its run chain).
        Link::route(&format!("/w/{wid}/tasks/{}", task.id))
            .style(Style::new().set("font", "inherit"))
            .st([
                St::Flex1,
                St::TextSm,
                St::TextHigh,
                St::FontSemibold,
                St::NoUnderline,
            ])
            .hover([St::TextAccent])
            .text(&task.title),
        status_badge(&task.status),
    ];
    for button in control_buttons(task) {
        head.push(button);
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(head),
    ];
    if open {
        rows.push(edit_form(task, members));
    } else {
        rows.push(el(El::Div).st([St::TextXs, St::TextMuted]).text(&task.spec));
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
        if task.attempt > 0 {
            meta.push_str(&format!(" · attempt {}", task.attempt + 1));
        }
        if let Some(next) = &task.next_retry {
            meta.push_str(&format!(" · retry {} → {next}", task.auto_retries + 1));
        }
        if !task.blockers.is_empty() {
            meta.push_str(&format!(" · blocked by {}", task.blockers.join(", ")));
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

/// The inline edit drawer for one task: retitle, rewrite the spec, reassign to a member,
/// change the schedule, and edit the blocker ids — saved as one form submit.
fn edit_form(task: &TaskView, members: &[(String, String)]) -> ElementBuilder {
    let mut assignee = vec![owner_option(task.agent_id.is_none())];
    for (id, name) in members {
        let option = el(El::Option).attr("value", id).text(name);
        assignee.push(if task.agent_id.as_deref() == Some(id.as_str()) {
            option.bool_attr(rwire::At::Selected)
        } else {
            option
        });
    }
    el(El::Form)
        .on(Ev::Submit, save_edit())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm, St::PtXs])
        .append([
            // The task id rides as a hidden field so the submit handler reads it from
            // FormData (a form's data-attribute isn't exposed on the Submit event).
            el(El::Input)
                .attr("type", "hidden")
                .attr("name", "id")
                .attr("value", &task.id),
            el(El::Input)
                .attr("name", "title")
                .attr("value", &task.title)
                .attr("autocomplete", "off")
                .style(Style::new().set("font", "inherit"))
                .st(input_st())
                .focus([St::BorderAccent]),
            el(El::Textarea)
                .attr("name", "spec")
                .attr("rows", "2")
                .style(
                    Style::new()
                        .set("font", "inherit")
                        .set("resize", "vertical"),
                )
                .st(input_st())
                .focus([St::BorderAccent])
                .text(&task.spec),
            el(El::Input)
                .attr("name", "schedule")
                .attr("value", task.schedule.as_deref().unwrap_or(""))
                .attr("placeholder", "Cron schedule — empty = one-shot")
                .attr("autocomplete", "off")
                .style(Style::new().set("font", "inherit"))
                .st(input_st())
                .focus([St::BorderAccent]),
            el(El::Input)
                .attr("name", "blockers")
                .attr("value", &task.blockers.join(", "))
                .attr("placeholder", "Blocker task ids, comma-separated")
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
                    accent_submit(icons::Icon::Check, "Save"),
                    crate::ui::widgets::icon_button(icons::Icon::Close, close_edit(), false),
                ]),
        ])
}

fn owner_option(selected: bool) -> ElementBuilder {
    let option = el(El::Option).attr("value", "").text("— owner —");
    if selected {
        option.bool_attr(rwire::At::Selected)
    } else {
        option
    }
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

/// Open a task's inline edit drawer (pure UI state — no bridge round-trip).
#[handler]
fn edit_task(app: &mut App, ctx: &EventContext) {
    app.editing_task = ctx.data("task").map(str::to_owned);
}

/// Close the edit drawer without saving.
#[handler]
fn close_edit(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    app.editing_task = None;
}

/// Save the edit drawer: post the changes and close it.
#[handler]
fn save_edit(app: &mut App, ctx: &EventContext) {
    let Some(id) = ctx.field("id").map(str::to_owned).filter(|v| !v.is_empty()) else {
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
    let blockers = ctx
        .field("blockers")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    bridge::send(Command::UpdateTask {
        id,
        title,
        spec,
        agent,
        schedule,
        blockers,
    });
    app.editing_task = None;
}
