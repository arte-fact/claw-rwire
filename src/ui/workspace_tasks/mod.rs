//! The workspace **Tasks** section: the lane's create form + task cards. Split out of
//! `workspace.rs` (the shell/header/Team live there) so the board UI — cards, the create
//! form, and the per-card edit drawer — has room to grow. Renders from `App.tasks`, the
//! active workspace's projected lane.

use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, TaskView, WorkspaceView};
use crate::ui::widgets::accent_submit;
use rwire_components::{
    Badge, BadgeFill, BadgeIntent, BadgeShape, Button, ButtonSize, Drawer, DrawerPosition,
    EmptyState, Input, Progress, ProgressSize, Select, StatusDot, StatusDotIntent, Text, Textarea,
};

mod board;
mod list;

use board::kanban_board;
use list::list_board;

#[renderer]
pub fn tasks(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return Text::body("Select a workspace.").muted().build();
    };
    // The assignable members (for the drawer's reassign select) and which card is open.
    let members: Vec<(String, String)> = app
        .agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
        .map(|a| (a.id.clone(), a.name.clone()))
        .collect();
    // The board holds only free-standing tasks (N2): delegated work lives in its request's
    // timeline. A workspace with none gets a real empty state with the ways to start.
    let board = if !app.tasks.iter().any(|t| t.request_id.is_none()) {
        EmptyState::new()
            .title("No free-standing tasks")
            .description(
                "Delegated work lives in its requests above. Queue a one-off directly with                  New task.",
            )
            .build()
    } else if app.tasks_list_view {
        list_board(app, &members, &workspace.id)
    } else {
        kanban_board(app, &workspace.id)
    };
    // The board (full width), the toolbar above it, and the create drawer (a right-side overlay
    // that's hidden until opened — matching the System Map's entity-creation drawer).
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            needs_you(app, &workspace.id),
            requests_section(app, &workspace.id),
            projects_section(app, &workspace.id),
            el(El::Div)
                .st([St::TextXs, St::TextLow, St::TextUppercase])
                .text("Board · free-standing tasks"),
            toolbar(app.tasks_list_view),
            filter_bar(&app.board_filter),
            locked_banner(app),
            board,
            new_task_drawer(app, workspace),
            // The in-situ pipeline step editor (opens from a step column header).
            crate::ui::workspace_workflow::step_drawer(app),
        ])
}

/// The needs-you strip (N2): everything in the workspace waiting on the operator — tasks
/// parked at a human gate and requests parked in validation — each linking straight to the
/// page where the decision happens. Absent when nothing needs you.
fn needs_you(app: &App, workspace_id: &str) -> ElementBuilder {
    let mut rows: Vec<ElementBuilder> = app
        .tasks
        .iter()
        .filter(|task| task.status == "awaiting")
        .map(|task| {
            needs_row(
                rwire::Link::route(&format!("/w/{workspace_id}/tasks/{}", task.id))
                    .st([St::MinW0, St::Flex1, St::Truncate, St::TextSm, St::TextHigh])
                    .hover([St::TextAccent])
                    .text(&task.title),
                "awaiting your review",
            )
        })
        .collect();
    rows.extend(
        app.requests
            .iter()
            .filter(|request| request.status == "validating")
            .map(|request| {
                needs_row(
                    rwire::Link::route(&format!("/w/{workspace_id}/requests/{}", request.id))
                        .st([St::MinW0, St::Flex1, St::Truncate, St::TextSm, St::TextHigh])
                        .hover([St::TextAccent])
                        .text(&request.title),
                    "validated — awaiting your verdict",
                )
            }),
    );
    if rows.is_empty() {
        return el(El::Div).st([St::DisplayNone]);
    }
    el(El::Div)
        .st([
            St::BorderWarning,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

fn needs_row(link: ElementBuilder, note: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span)
                .st([St::TextXs, St::TextWarning])
                .text("needs you"),
            link,
            el(El::Span).st([St::TextXs, St::TextMuted]).text(note),
        ])
}

/// The Requests section (N2): every open request as a card — title, status, the hairline
/// done-meter, diffstat, branch, and a live "what is it doing right now" line. Hidden while
/// the workspace has no open requests, so boards without delegation stay as they were.
fn requests_section(app: &App, workspace_id: &str) -> ElementBuilder {
    if app.requests.is_empty() {
        return el(El::Div).st([St::DisplayNone]);
    }
    let mut children = vec![
        el(El::Div)
            .st([St::TextXs, St::TextLow, St::TextUppercase])
            .text(&format!("Requests · {} open", app.requests.len())),
    ];
    children.extend(app.requests.iter().map(|request| {
        let mut lines = vec![
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    rwire::Link::route(&format!("/w/{workspace_id}/requests/{}", request.id))
                        .st([
                            St::MinW0,
                            St::Flex1,
                            St::Truncate,
                            St::TextSm,
                            St::TextHigh,
                            St::FontSemibold,
                        ])
                        .hover([St::TextAccent])
                        .text(&request.title),
                    request_badge(&request.status),
                ]),
        ];
        let mut meta: Vec<ElementBuilder> = Vec::new();
        if request.total > 0 {
            meta.push(
                el(El::Div)
                    .style(rwire::style::Style::new().set("max-width", "8rem"))
                    .st([St::WFull])
                    .append([Progress::new()
                        .value(request.done.max(0) as u32)
                        .max(request.total.max(1) as u32)
                        .size(ProgressSize::Sm)
                        .label(request.progress.clone())
                        .build()]),
            );
            meta.push(
                el(El::Span)
                    .st([St::TextXs, St::TextMuted, St::WhitespaceNowrap])
                    .text(&request.progress),
            );
        }
        if !request.stat.is_empty() {
            meta.push(
                el(El::Span)
                    .st([St::TextXs, St::TextMuted, St::WhitespaceNowrap])
                    .text(&request.stat),
            );
        }
        if !request.branch.is_empty() {
            // Desktop-only detail: the branch name is noise on a phone (mobile-first, N2).
            meta.push(
                el(El::Span)
                    .st([St::TextXs, St::TextLow, St::Truncate, St::DisplayNone])
                    .md([St::DisplayBlock])
                    .text(&request.branch),
            );
        }
        if request.preview_live {
            // A live preview is up for this branch (P2c) — a green pulse invites a click-through
            // (the link itself lives on the request page header).
            meta.push(
                el(El::Div)
                    .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::MinW0])
                    .append([StatusDot::new()
                        .intent(StatusDotIntent::Success)
                        .pulse(true)
                        .label("preview")
                        .build()]),
            );
        }
        // The live line: the request's currently running task, straight off the lane.
        if let Some(task) = app
            .tasks
            .iter()
            .find(|t| t.request_id.as_deref() == Some(request.id.as_str()) && t.status == "running")
        {
            let agent = task.agent.clone().unwrap_or_else(|| "owner".to_owned());
            meta.push(
                el(El::Div)
                    .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::MinW0])
                    .append([
                        StatusDot::new().intent(StatusDotIntent::Primary).build(),
                        rwire::Link::route(&format!("/w/{workspace_id}/tasks/{}", task.id))
                            .st([St::TextXs, St::TextAccent, St::Truncate])
                            .hover([St::Underline])
                            .text(&task.title),
                        el(El::Span)
                            .st([St::TextXs, St::TextLow, St::WhitespaceNowrap])
                            .text(&agent),
                    ]),
            );
        }
        if !meta.is_empty() {
            lines.push(
                el(El::Div)
                    .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
                    .append(meta),
            );
        }
        el(El::Div)
            .st([
                St::BorderDefault,
                St::PSm,
                St::DisplayFlex,
                St::FlexCol,
                St::GapXs,
            ])
            .append(lines)
    }));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(children)
}

/// A request's lifecycle chip on a card.
fn request_badge(status: &str) -> ElementBuilder {
    let intent = match status {
        "executing" => BadgeIntent::Primary,
        "planning" | "validating" => BadgeIntent::Warning,
        _ => BadgeIntent::Default,
    };
    Badge::new()
        .intent(intent)
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .text(status.to_owned())
        .build()
}

/// The Projects strip (P1): the durable axis below the in-flight Requests rail — every
/// `projects/*` repo with its open-request count, each a link to its project page.
fn projects_section(app: &App, workspace_id: &str) -> ElementBuilder {
    if app.projects.is_empty() {
        return el(El::Div).st([St::DisplayNone]);
    }
    let mut children = vec![
        el(El::Div)
            .st([St::TextXs, St::TextLow, St::TextUppercase])
            .text(&format!("Projects · {}", app.projects.len())),
    ];
    children.extend(app.projects.iter().map(|project| {
        let open = if project.open_requests > 0 {
            format!("{} open", project.open_requests)
        } else {
            "quiet".to_owned()
        };
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::PySm])
            .append([
                rwire::Link::route(&format!("/w/{workspace_id}/projects/{}", project.name))
                    .st([
                        St::MinW0,
                        St::Flex1,
                        St::Truncate,
                        St::TextSm,
                        St::TextHigh,
                        St::FontSemibold,
                    ])
                    .hover([St::TextAccent])
                    .text(&project.name),
                el(El::Span).st([St::TextXs, St::TextLow]).text(&open),
            ])
    }));
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
            St::BorderDefault,
            St::BgSurface,
            St::PSm,
        ])
        .append(children)
}

/// A banner shown while the workspace has a task in flight (running/awaiting): the pipeline editor
/// is locked, so this explains why editing the workflow from a column header is unavailable.
fn locked_banner(app: &App) -> ElementBuilder {
    let locked = app
        .tasks
        .iter()
        .any(|task| matches!(task.status.as_str(), "running" | "awaiting"));
    if !locked {
        return el(El::Div).st([St::DisplayNone]);
    }
    el(El::Div)
        .st([
            St::SelfStart,
            St::TextXs,
            St::TextMuted,
            St::PxSm,
            St::PyXs,
            St::BgSubtle,
        ])
        .text("⏳ Pipeline locked while tasks are in flight — finish or stop them to edit the workflow.")
}

/// The board quick-filter chips — narrow the board to what needs attention without leaving it.
fn filter_bar(active: &str) -> ElementBuilder {
    let active = if active.is_empty() { "all" } else { active };
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::FlexWrap])
        .append([
            filter_chip("All", "all", active),
            filter_chip("Needs you", "awaiting", active),
            filter_chip("Running", "running", active),
            filter_chip("Blocked", "blocked", active),
            filter_chip("Failed", "failed", active),
        ])
}

fn filter_chip(label: &str, value: &str, active: &str) -> ElementBuilder {
    crate::ui::widgets::chip(label, value == active, set_board_filter()).data("filter", value)
}

#[handler]
fn set_board_filter(app: &mut App, ctx: &EventContext) {
    if let Some(filter) = ctx.data("filter") {
        app.board_filter = filter.to_owned();
    }
}

/// Whether a task passes the active board quick-filter.
fn task_matches_filter(task: &TaskView, filter: &str) -> bool {
    match filter {
        "awaiting" => task.status == "awaiting",
        "running" => task.status == "running",
        "blocked" => {
            !task.blockers.is_empty() && matches!(task.status.as_str(), "pending" | "stopped")
        }
        "failed" => matches!(task.status.as_str(), "failed" | "rejected" | "cancelled"),
        _ => true,
    }
}

/// The board toolbar: the Board/List view switch and the "New task" button.
fn toolbar(list: bool) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Div).st([St::Flex1]).append([view_toggle(list)]),
            new_task_button(),
        ])
}

/// The "New task" button — opens the create drawer.
fn new_task_button() -> ElementBuilder {
    Button::primary("New task".to_owned())
        .icon(icons::Icon::Plus)
        .size(ButtonSize::Sm)
        .on_click(open_new_task())
}

/// The create-task drawer: a right-side overlay (with backdrop) holding the create form, matching
/// the System Map's entity drawer. Hidden until `new_task_panel_open`; the backdrop / close (X)
/// dismiss it via `on_close`.
fn new_task_drawer(app: &App, workspace: &WorkspaceView) -> ElementBuilder {
    Drawer::new()
        .position(DrawerPosition::Right)
        .open(app.new_task_panel_open)
        .title("New task")
        .on_close(close_new_task())
        .content(create_form(app, workspace))
        .build()
}

/// Open the create drawer (pure UI state).
#[handler]
fn open_new_task(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    app.new_task_panel_open = true;
}

/// Close the create drawer (backdrop / Cancel).
#[handler]
fn close_new_task(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    app.new_task_panel_open = false;
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
    crate::ui::widgets::chip(label, active, toggle_view()).data("view", view)
}

#[handler]
fn toggle_view(app: &mut App, ctx: &EventContext) {
    if let Some(view) = ctx.data("view") {
        app.tasks_list_view = view == "list";
    }
}

fn create_form(app: &App, workspace: &WorkspaceView) -> ElementBuilder {
    // Assignee options: the workspace's members (the owner is the default).
    let mut assignee = Select::new().name("agent").option("", "— owner —");
    for agent in app
        .agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
    {
        assignee = assignee.option(agent.id.clone(), agent.name.clone());
    }
    el(El::Form)
        .on(Ev::Submit, submit_task())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            Input::text()
                .name("title")
                .placeholder("Task title")
                .required(true)
                .autocomplete("off")
                .build(),
            Textarea::new()
                .name("spec")
                .rows(3)
                .required(true)
                .placeholder("What should the agent do?")
                .build(),
            Input::text()
                .name("schedule")
                .placeholder("Cron schedule — optional, e.g. 0 8 * * *")
                .autocomplete("off")
                .build(),
            retries_input(None),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    assignee.build().st([St::Flex1]),
                    accent_submit(icons::Icon::Plus, "Queue task"),
                ]),
        ])
}

/// The optional per-task auto-retry-budget input, prefilled with `value` (the task's current
/// override) when editing. Blank = use the global default.
fn retries_input(value: Option<i64>) -> ElementBuilder {
    let field = Input::number()
        .name("max_retries")
        .min("0")
        .placeholder("Auto-retries on failure — blank = default")
        .autocomplete("off");
    match value {
        Some(n) => field.value(n.to_string()).build(),
        None => field.build(),
    }
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
        // Failed / rejected / cancelled tasks can be reopened — a transient failure must not
        // dead-end the card. `done` stays final (archive only).
        "failed" | "rejected" | "cancelled" => vec![
            restart_button(&task.id),
            icon(icons::Icon::Archive, archive_task()),
        ],
        _ => vec![icon(icons::Icon::Archive, archive_task())],
    }
}

/// A small text "Restart" button (there's no rotate glyph) carrying the task id.
fn restart_button(task_id: &str) -> ElementBuilder {
    use rwire_components::{Button, ButtonSize};
    Button::secondary("Restart")
        .size(ButtonSize::Sm)
        .on_click(restart_task())
        .data("task", task_id)
}

#[handler]
fn stop_task(ctx: &EventContext) {
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::StopTask { id: id.to_owned() });
    }
}

#[handler]
fn start_task(ctx: &EventContext) {
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::StartTask { id: id.to_owned() });
    }
}

#[handler]
fn restart_task(ctx: &EventContext) {
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::RestartTask { id: id.to_owned() });
    }
}

#[handler]
fn archive_task(ctx: &EventContext) {
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::ArchiveTask { id: id.to_owned() });
    }
}

fn status_badge(status: &str) -> ElementBuilder {
    // The rwire Badge, square + outline (axes this app pushed upstream), with semantic
    // intents: running rides the accent, done reads success, failed reads error, and a
    // parked `awaiting` warns.
    let intent = match status {
        "running" => BadgeIntent::Primary,
        "done" => BadgeIntent::Success,
        "failed" | "rejected" => BadgeIntent::Error,
        "awaiting" => BadgeIntent::Warning,
        _ => BadgeIntent::Default,
    };
    Badge::new()
        .intent(intent)
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .text(status.to_owned())
        .build()
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
        max_retries: parse_max_retries(ctx),
    });
    app.new_task_panel_open = false; // close the drawer once the task is queued
}

/// Parse the optional `max_retries` form field: a non-negative integer, or `None` (blank /
/// non-numeric) to fall back to the global default.
fn parse_max_retries(ctx: &rwire::EventContext) -> Option<i64> {
    ctx.field("max_retries")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n >= 0)
}

#[handler]
fn cancel_task(ctx: &EventContext) {
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
    let blockers = ctx
        .field("blockers")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let schedule = ctx
        .field("schedule")
        .map(str::to_owned)
        .filter(|v| !v.trim().is_empty());
    bridge::send(Command::UpdateTask {
        id,
        title,
        spec,
        agent,
        schedule,
        max_retries: parse_max_retries(ctx),
        blockers,
    });
    app.editing_task = None;
}
