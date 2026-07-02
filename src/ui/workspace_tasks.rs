//! The workspace **Tasks** section: the lane's create form + task cards. Split out of
//! `workspace.rs` (the shell/header/Team live there) so the board UI — cards, the create
//! form, and the per-card edit drawer — has room to grow. Renders from `App.tasks`, the
//! active workspace's projected lane.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, TaskView, WorkspaceView};
use crate::ui::widgets::accent_submit;
use rwire_components::{
    Avatar, AvatarSize, Drawer, DrawerPosition, Input, Select, Tag, TagIntent, Text, Textarea,
    Tooltip,
};

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
    let board = if app.tasks_list_view {
        list_board(app, &members, &workspace.id)
    } else {
        kanban_board(app, &workspace.id)
    };
    // The board (full width), the toolbar above it, and the create drawer (a right-side overlay
    // that's hidden until opened — matching the System Map's entity-creation drawer).
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            toolbar(app.tasks_list_view),
            filter_bar(&app.board_filter),
            locked_banner(app),
            board,
            new_task_drawer(app, workspace),
            // The in-situ pipeline step editor (opens from a step column header).
            crate::ui::workspace_workflow::step_drawer(app),
            // The Peek modal (opens over the board when a card is clicked or a task is deep-linked).
            crate::ui::task_detail::peek_modal(app),
        ])
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
            St::Rounded0,
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
    let chip = el(El::Button)
        .at(At::Type, Av::Button)
        .data("filter", value)
        .on(Ev::Click, set_board_filter())
        .st([
            St::FontInheritAll,
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::Rounded0,
            St::BorderDefault,
            St::CursorPointer,
        ])
        .text(label);
    if value == active {
        chip.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        chip.st([St::BgApp, St::TextMuted]).hover([St::TextHigh])
    }
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
    el(El::Button)
        .at(At::Type, Av::Button)
        .on(Ev::Click, open_new_task())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxSm,
            St::PyXs,
            St::Rounded0,
            St::BgAccent,
            St::TextOnAccent,
            St::BorderAccent,
            St::CursorPointer,
        ])
        .append([
            icons::icon(icons::Icon::Plus),
            el(El::Span).text("New task"),
        ])
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

/// The flat list view (the original): cards with the inline edit drawer.
fn list_board(app: &App, members: &[(String, String)], wid: &str) -> ElementBuilder {
    let editing = app.editing_task.as_deref();
    let mut cards: Vec<ElementBuilder> = app
        .tasks
        .iter()
        .filter(|task| task_matches_filter(task, &app.board_filter))
        .map(|task| task_card(task, members, editing == Some(task.id.as_str()), wid))
        .collect();
    if cards.is_empty() {
        cards.push(Text::body("No tasks yet.").muted().build().st([St::PySm]));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

/// The kanban board: one column per pipeline step, then Done and Closed. A task sits in its
/// current step's column (or Done/Closed when terminal) and moves left→right as it advances.
fn kanban_board(app: &App, wid: &str) -> ElementBuilder {
    let step_count = app.pipeline_columns.len();
    let locked = app
        .tasks
        .iter()
        .any(|task| matches!(task.status.as_str(), "running" | "awaiting"));
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
                .filter(|task| task_matches_filter(task, &app.board_filter))
                .collect();
            // The configured step row for this column (the implicit default Execute has no row).
            let step_id = if index < step_count {
                app.workflow_steps.get(index).map(|s| s.id.as_str())
            } else {
                None
            };
            kanban_column(
                name,
                &cards,
                &app.pipeline_columns,
                index < step_count,
                step_id,
                locked,
                wid,
            )
        })
        .collect();
    // Mobile: stack the columns vertically (scroll down). Desktop (`md+`): a row that scrolls
    // horizontally — the classic kanban.
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .md([St::FlexRow, St::OverflowXAuto])
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

fn kanban_column(
    name: &str,
    tasks: &[&crate::state::TaskView],
    steps: &[String],
    is_step: bool,
    step_id: Option<&str>,
    locked: bool,
    wid: &str,
) -> ElementBuilder {
    let mut items = vec![column_header(
        name,
        tasks.len(),
        is_step,
        step_id,
        locked,
        wid,
    )];
    for task in tasks {
        items.push(kanban_card(task, steps, wid));
    }
    // Full-width when stacked (mobile); a `≥12rem` grow-to-fill column in the desktop row.
    el(El::Div)
        .style(Style::new().set("min-width", "12rem"))
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .md([St::Flex1])
        .append(items)
}

/// A column header `NAME · count`. A pipeline-step column with a configured row opens that step's
/// editor in a drawer (the columns *are* the pipeline); a step with no row (the implicit default) or
/// a locked board links to the Workflow tab; Done/Closed are plain text.
fn column_header(
    name: &str,
    count: usize,
    is_step: bool,
    step_id: Option<&str>,
    locked: bool,
    wid: &str,
) -> ElementBuilder {
    let label = format!("{name} · {count}");
    let header_st = [
        St::FontInheritAll,
        St::TextXs,
        St::TextMuted,
        St::TextUppercase,
        St::TrackingWide,
        St::PbXs,
    ];
    if !is_step {
        return el(El::Div).st(header_st).text(&label);
    }
    match step_id.filter(|_| !locked) {
        // A configured, editable step → open its editor drawer in place.
        Some(id) => el(El::Button)
            .at(At::Type, Av::Button)
            .data("step", id)
            .on(Ev::Click, crate::ui::workspace_workflow::toggle_edit())
            .st(header_st)
            .st([
                St::TextLeft,
                St::BgTransparent,
                St::BorderNone,
                St::P0,
                St::CursorPointer,
            ])
            .hover([St::TextAccent])
            .append([el(El::Span).text(&label)]),
        // The implicit default (no row) or a locked board → jump to the Workflow tab.
        None => Link::route(&format!("/w/{wid}/workflow"))
            .st(header_st)
            .st([St::NoUnderline, St::CursorPointer])
            .hover([St::TextAccent])
            .append([el(El::Span).text(&label)]),
    }
}

/// A kanban card: title + status, the pipeline mini-stepper, a 2-line spec preview, an
/// assignee avatar + signal tags (blocked / recurrent / retry / attempt), and an inline action
/// row (Approve when parked, run controls otherwise) — the at-a-glance view of one task.
fn kanban_card(task: &TaskView, steps: &[String], wid: &str) -> ElementBuilder {
    let _ = wid;
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                // The title opens the task's Peek modal over the board (no navigation away).
                el(El::Button)
                    .at(At::Type, Av::Button)
                    .data("task", &task.id)
                    .on(Ev::Click, crate::ui::task_detail::open_peek())
                    .st([
                        St::FontInheritAll,
                        St::Flex1,
                        St::TextXs,
                        St::TextHigh,
                        St::FontSemibold,
                        St::TextLeft,
                        St::BgTransparent,
                        St::BorderNone,
                        St::P0,
                        St::CursorPointer,
                    ])
                    .hover([St::TextAccent])
                    .text(&task.title),
                status_badge(&task.status),
            ]),
    ];
    // The pipeline mini-stepper (only when there's a real multi-step pipeline to show).
    if steps.len() > 1 {
        rows.push(step_trail(steps, task));
    }
    // A 2-line spec preview so the card says what the task is, not just its title.
    if !task.spec.trim().is_empty() {
        rows.push(spec_preview(&task.spec));
    }
    // Assignee avatar + signal tags.
    rows.push(card_meta_row(task));
    // Inline actions: Approve right on a parked card, then the status-appropriate run controls.
    rows.push(card_actions(task));
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::Rounded0,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

/// The pipeline position as a compact `Execute › Refine › Approve` trail: the current step is
/// accented (with its loop iteration), past steps muted, future steps faint; a terminal task
/// shows the whole trail faint.
fn step_trail(steps: &[String], task: &TaskView) -> ElementBuilder {
    let terminal = matches!(
        task.status.as_str(),
        "done" | "failed" | "rejected" | "cancelled"
    );
    let current = (task.step_position.max(0) as usize).min(steps.len().saturating_sub(1));
    let mut parts: Vec<ElementBuilder> = Vec::new();
    for (index, name) in steps.iter().enumerate() {
        if index > 0 {
            parts.push(el(El::Span).st([St::TextLow]).text("›"));
        }
        let mut label = name.clone();
        if !terminal && index == current && task.iteration > 0 {
            label.push_str(&format!(" {}", task.iteration + 1));
        }
        let color = if terminal || index > current {
            St::TextLow
        } else if index == current {
            St::TextAccent
        } else {
            St::TextMuted
        };
        let span = el(El::Span).st([St::TextXs, color]).text(&label);
        parts.push(if !terminal && index == current {
            span.st([St::FontSemibold])
        } else {
            span
        });
    }
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::FlexWrap])
        .append(parts)
}

/// A 2-line clamped spec preview (the clamp is a genuinely dynamic CSS escape hatch).
fn spec_preview(spec: &str) -> ElementBuilder {
    el(El::Div)
        .style(
            Style::new()
                .set("display", "-webkit-box")
                .set("-webkit-line-clamp", "2")
                .set("-webkit-box-orient", "vertical")
                .set("overflow", "hidden"),
        )
        .st([St::TextXs, St::TextMuted])
        .text(spec)
}

/// The assignee avatar (initials, tooltip = full name) followed by the signal tags.
fn card_meta_row(task: &TaskView) -> ElementBuilder {
    let assignee = task.agent.as_deref().unwrap_or("owner");
    let avatar = Tooltip::new(assignee.to_owned())
        .child(
            Avatar::new()
                .fallback(initials(assignee))
                .size(AvatarSize::Sm)
                .build(),
        )
        .build();
    let mut row = vec![avatar];
    row.extend(signal_tags(task));
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::FlexWrap])
        .append(row)
}

/// Up to two uppercase initials for an avatar fallback (`Forge` → `F`, `John Doe` → `JD`).
pub(crate) fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if letters.is_empty() {
        "?".to_owned()
    } else {
        letters
    }
}

/// The card's signal tags — only the ones that apply: a human gate, blockers, recurrence, a
/// pending retry, and the manual-restart attempt count. A terminal card carries no live signals
/// (a done/cancelled task is no longer blocked or retrying).
fn signal_tags(task: &TaskView) -> Vec<ElementBuilder> {
    let mut tags = Vec::new();
    if matches!(
        task.status.as_str(),
        "done" | "failed" | "rejected" | "cancelled"
    ) {
        return tags;
    }
    if task.status == "awaiting" {
        tags.push(Tag::new("needs you").intent(TagIntent::Primary).build());
    }
    if !task.blockers.is_empty() {
        tags.push(
            Tag::new(format!("⛔ {}", task.blockers.len()))
                .intent(TagIntent::Warning)
                .build(),
        );
    }
    if task.recurrent {
        let label = match &task.schedule {
            Some(schedule) => format!("⟳ {schedule}"),
            None => "⟳".to_owned(),
        };
        tags.push(Tag::new(label).build());
    }
    if let Some(next) = &task.next_retry {
        tags.push(
            Tag::new(format!("retry → {next}"))
                .intent(TagIntent::Warning)
                .build(),
        );
    }
    if task.attempt > 0 {
        tags.push(Tag::new(format!("att {}", task.attempt + 1)).build());
    }
    tags
}

/// The inline action row: a one-click Approve on a parked card, then the status-appropriate run
/// controls (Stop / Start / Restart / Edit / Cancel), wrapping on a narrow column.
fn card_actions(task: &TaskView) -> ElementBuilder {
    let mut actions: Vec<ElementBuilder> = Vec::new();
    if task.status == "awaiting" {
        use rwire_components::{Button, ButtonSize};
        actions.push(
            Button::primary("Approve")
                .size(ButtonSize::Sm)
                .on_click(kanban_approve())
                .data("task", &task.id),
        );
    }
    actions.extend(control_buttons(task));
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::FlexWrap,
            St::PtXs,
        ])
        .append(actions)
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
        .at(At::Type, Av::Button)
        .data("view", view)
        .on(Ev::Click, toggle_view())
        .st([
            St::FontInheritAll,
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::Rounded0,
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

#[handler]
fn archive_task(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("task") {
        bridge::send(Command::ArchiveTask { id: id.to_owned() });
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
            .st([
                St::FontInheritAll,
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
            St::Rounded0,
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
    let mut assignee = Select::new()
        .name("agent")
        .value(task.agent_id.clone().unwrap_or_default())
        .option("", "— owner —");
    for (id, name) in members {
        assignee = assignee.option(id.clone(), name.clone());
    }
    el(El::Form)
        .on(Ev::Submit, save_edit())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm, St::PtXs])
        .append([
            // The task id rides as a hidden field so the submit handler reads it from
            // FormData (a form's data-attribute isn't exposed on the Submit event).
            el(El::Input)
                .at(At::Type, Av::Hidden)
                .at_str(At::Name, "id")
                .at_str(At::Value, &task.id),
            Input::text()
                .name("title")
                .value(task.title.clone())
                .autocomplete("off")
                .build(),
            Textarea::new()
                .name("spec")
                .rows(2)
                .value(task.spec.clone())
                .build(),
            Input::text()
                .name("schedule")
                .value(task.schedule.clone().unwrap_or_default())
                .placeholder("Cron schedule — empty = one-shot")
                .autocomplete("off")
                .build(),
            Input::text()
                .name("blockers")
                .value(task.blockers.join(", "))
                .placeholder("Blocker task ids, comma-separated")
                .autocomplete("off")
                .build(),
            retries_input(task.max_retries),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    assignee.build().st([St::Flex1]),
                    accent_submit(icons::Icon::Check, "Save"),
                    crate::ui::widgets::icon_button(icons::Icon::Close, close_edit(), false),
                ]),
        ])
}

fn status_badge(status: &str) -> ElementBuilder {
    // A sharp, outlined status chip (the Badge component is a fixed `RoundedFull` pill, which can't
    // go square — and the outlined chip matches rust-claw's original status look anyway).
    let color = match status {
        "running" => St::TextAccent,
        "done" => St::TextHigh,
        "failed" => St::TextError,
        _ => St::TextMuted,
    };
    el(El::Span)
        .st([St::TextXs, color, St::BorderDefault, St::Rounded0, St::PxXs])
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
