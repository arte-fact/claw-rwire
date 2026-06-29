//! The task **Peek** modal (Phase F-4 / UX overhaul U2): a task's one-page view — spec, the
//! live tool feed, the full run chain (each producer/validator run with its transcript, diffs
//! and verdict), and the run controls / review gate — rendered as a custom overlay over the board
//! so you never leave the kanban. Opened by a card click (`open_peek`) or a deep link to
//! `/w/:wid/tasks/:tid` (the bridge sets `selected_task`). Renders from `App.task_detail`.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, icons};
use rwire_components::{Avatar, AvatarSize, Button, ButtonSize, Input, Text};

use crate::bridge::{self, Command};
use crate::state::{App, LiveEvent, TaskDetail};
use crate::ui::workspace::card;
use crate::ui::workspace_tasks::initials;

/// The Peek modal: a self-contained overlay opened over the board when a task is selected. A plain
/// builder (not its own `#[renderer]`) called inside the board renderer — so reading
/// `selected_task` / `task_detail` / live state subscribes the board, which re-renders it live (the
/// same pattern as the New-task drawer). Built by hand rather than the `Modal` component so the
/// panel paints **above** the backdrop (the component layers the backdrop on top of the panel).
pub fn peek_modal(app: &App) -> ElementBuilder {
    let Some(selected) = app.selected_task.as_deref() else {
        return el(El::Div).st([St::DisplayNone]);
    };
    let content = match &app.task_detail {
        Some(detail) if detail.id == selected => peek_content(app, detail),
        _ => Text::body("Loading…").muted().build(),
    };
    // Container (flex-centred) holding two siblings: the backdrop (fills it, click = close) and the
    // panel (later in DOM, so it paints above the backdrop — no z-index juggling needed).
    el(El::Div)
        .st([
            St::PositionFixed,
            St::Inset0,
            St::Z1400,
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyCenter,
            St::PSm,
        ])
        .md([St::PMd])
        .append([
            el(El::Div)
                .st([
                    St::PositionAbsolute,
                    St::Inset0,
                    St::BgOverlay50,
                    St::CursorPointer,
                ])
                .on(Ev::Click, close_peek()),
            el(El::Div)
                // A wide, **fixed-height** panel (60rem × 85vh, capped to the viewport) so the modal
                // is the same size whatever the task — the head is pinned and the body scrolls.
                .style(
                    Style::new()
                        .set("width", "60rem")
                        .set("max-width", "100%")
                        .set("height", "85vh"),
                )
                .st([
                    St::PositionRelative,
                    St::DisplayFlex,
                    St::FlexCol,
                    St::BgSurface,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::OverflowHidden,
                ])
                .append([content]),
        ])
}

/// The modal body: a pinned head (close row, header, tab strip) over a scrolling body (the active
/// tab) — so the fixed-height panel keeps its chrome in view while the content scrolls.
fn peek_content(app: &App, detail: &TaskDetail) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::Flex1, St::MinH0])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapMd, St::PMd])
                .md([St::PLg])
                .append([
                    close_row(),
                    peek_header(app, detail),
                    peek_tabs(app.detail_tab),
                ]),
            el(El::Div)
                .st([St::Flex1, St::MinH0, St::OverflowYAuto, St::PxMd, St::PbMd])
                .md([St::PxLg, St::PbLg])
                .append([peek_tab_body(app, detail)]),
        ])
}

/// A right-aligned close (✕) button for the panel.
fn close_row() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::JustifyEnd])
        .append([el(El::Button)
            .at(At::Type, Av::Button)
            .at_str(At::AriaLabel, "Close")
            .on(Ev::Click, close_peek())
            .st([
                St::DisplayFlex,
                St::ItemsCenter,
                St::BgTransparent,
                St::BorderNone,
                St::P0,
                St::TextMuted,
                St::CursorPointer,
            ])
            .hover([St::TextHigh])
            .append([icons::icon_sized(icons::Icon::Close, 18)])])
}

/// Title + assignee avatar + status + (when live) the current step / iteration.
fn peek_header(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let task = app.tasks.iter().find(|t| t.id == detail.id);
    let assignee = task.and_then(|t| t.agent.as_deref()).unwrap_or("owner");
    let mut row = vec![
        Avatar::new()
            .fallback(initials(assignee))
            .size(AvatarSize::Sm)
            .build(),
        el(El::Span)
            .st([St::Flex1, St::TextLg, St::TextHigh, St::FontSemibold])
            .text(&detail.title),
    ];
    let terminal = matches!(
        detail.status.as_str(),
        "done" | "failed" | "rejected" | "cancelled"
    );
    if let Some(task) = task.filter(|_| !terminal)
        && let Some(step) = app.pipeline_columns.get(task.step_position.max(0) as usize)
    {
        let mut label = step.clone();
        if task.iteration > 0 {
            label.push_str(&format!(" · iter {}", task.iteration + 1));
        }
        row.push(el(El::Span).st([St::TextXs, St::TextMuted]).text(&label));
    }
    row.push(
        el(El::Span)
            .st([St::TextXs, St::TextMuted, St::TextUppercase])
            .text(&detail.status),
    );
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append(row)
}

/// The Overview / Activity / Review tab strip.
fn peek_tabs(active: usize) -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::BorderBDefault,
            St::PbSm,
        ])
        .append([
            tab_chip("Overview", 0, active),
            tab_chip("Activity", 1, active),
            tab_chip("Review", 2, active),
        ])
}

fn tab_chip(label: &str, index: usize, active: usize) -> ElementBuilder {
    let chip = el(El::Button)
        .at(At::Type, Av::Button)
        .data("tab", &index.to_string())
        .on(Ev::Click, set_detail_tab())
        .st([
            St::FontInheritAll,
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::CursorPointer,
        ])
        .text(label);
    if index == active {
        chip.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        chip.st([St::BgApp, St::TextMuted]).hover([St::TextHigh])
    }
}

/// The active tab's content.
fn peek_tab_body(app: &App, detail: &TaskDetail) -> ElementBuilder {
    match app.detail_tab {
        1 => peek_activity(app, detail),
        2 => peek_review(detail),
        _ => peek_overview(detail),
    }
}

/// Overview: the spec (markdown) and the run controls.
fn peek_overview(detail: &TaskDetail) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append([
            card("Spec", crate::ui::widgets::markdown(detail.spec.clone())),
            control_bar(detail),
        ])
}

/// Activity: the live tool feed (while running) on top, then the persisted run chain in a
/// bounded scroll area so a long chain stays inside the modal.
fn peek_activity(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let mut sections = Vec::new();
    if detail.status == "running" {
        let mut live: Vec<&LiveEvent> = app
            .live_activity
            .iter()
            .filter(|event| event.task_id == detail.id)
            .collect();
        live.reverse(); // ring is newest-first; show chronological
        let streaming = app
            .live_text
            .iter()
            .find(|text| text.task_id == detail.id)
            .map(|text| text.text.as_str())
            .unwrap_or("");
        sections.push(live_card(&live, streaming));
    }
    let last_run = detail.runs.len().saturating_sub(1);
    let runs: Vec<ElementBuilder> = detail
        .runs
        .iter()
        .enumerate()
        .map(|(index, run)| {
            crate::ui::run_chain::run_card(
                run,
                &format!("a{index}"),
                index == last_run,
                &app.tool_toggled,
            )
        })
        .collect();
    let chain = if runs.is_empty() {
        Text::body("No runs recorded yet.").muted().build()
    } else {
        // No own scroll — the modal body scrolls (the fixed-height panel owns the single scroll).
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(runs)
    };
    sections.push(card("Run chain", chain));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append(sections)
}

/// Review: the gate actions when parked, else a note.
fn peek_review(detail: &TaskDetail) -> ElementBuilder {
    if detail.awaiting {
        actions(&detail.id)
    } else {
        card(
            "Review",
            Text::body("This task isn't waiting on your review right now.")
                .muted()
                .build(),
        )
    }
}

/// The live tool-activity feed for a running task — what the agent is doing now, plus its reply
/// as it streams in (the typewriter).
fn live_card(events: &[&LiveEvent], streaming: &str) -> ElementBuilder {
    let mut rows = vec![
        el(El::Div)
            .st([St::TextXs, St::TextAccent, St::FontSemibold])
            .text("▶ Running — live activity"),
    ];
    if events.is_empty() && streaming.is_empty() {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text("The agent is working… (activity will appear here)"),
        );
    }
    for event in events {
        let (mark, color) = if event.in_progress {
            ("▷", St::TextMuted)
        } else if event.ok {
            ("✓", St::TextAccent)
        } else {
            ("✗", St::TextLow)
        };
        rows.push(
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
                .append([
                    el(El::Span).st([color]).text(mark),
                    el(El::Span).st([St::TextMuted]).text(&event.time),
                    el(El::Span).st([St::TextHigh]).text(&event.action),
                ]),
        );
    }
    if !streaming.is_empty() {
        rows.push(
            el(El::Div)
                .style(
                    Style::new()
                        .set("white-space", "pre-wrap")
                        .set("word-break", "break-word"),
                )
                .st([St::TextXs, St::TextHigh, St::PtXs])
                .text(&format!("{streaming}▌")),
        );
    }
    card(
        "Live",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapXs])
            .append(rows),
    )
}

/// The run-control bar: Stop while live, Start/Restart when stopped, Restart when terminal.
fn control_bar(detail: &TaskDetail) -> ElementBuilder {
    let id = detail.id.as_str();
    let buttons = match detail.status.as_str() {
        "running" | "pending" => vec![
            action_button("Stop", stop(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        "stopped" => vec![
            action_button("Start", start(), id, true),
            action_button("Restart", restart(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        // Terminal (done | failed | rejected | cancelled): a clean-slate Restart.
        _ => vec![action_button("Restart", restart(), id, true)],
    };
    card(
        "Controls",
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(buttons),
    )
}

/// The gate actions for a parked task: Approve / Request-changes(notes) / Reject.
fn actions(task_id: &str) -> ElementBuilder {
    let buttons = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    action_button("Approve", approve(), task_id, true),
                    action_button("Reject", reject(), task_id, false),
                ]),
            el(El::Form)
                .on(Ev::Submit, request_changes())
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    el(El::Input)
                        .at(At::Type, Av::Hidden)
                        .at_str(At::Name, "task")
                        .at_str(At::Value, task_id),
                    Input::text()
                        .name("feedback")
                        .placeholder("What to change…")
                        .autocomplete("off")
                        .build()
                        .st([St::Flex1]),
                    el(El::Button)
                        .at(At::Type, Av::Submit)
                        .st([
                            St::FontInheritAll,
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
                        .text("Request changes"),
                ]),
        ]);
    card("Your review", buttons)
}

fn action_button(
    label: &str,
    on_click: rwire::HandlerSpec,
    task: &str,
    accent: bool,
) -> ElementBuilder {
    let btn = if accent {
        Button::primary(label.to_owned())
    } else {
        Button::secondary(label.to_owned())
    };
    btn.size(ButtonSize::Sm)
        .on_click(on_click)
        .data("task", task)
}

/// Open a task's Peek modal (card click) — set the selection + load its detail.
#[handler]
pub fn open_peek(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("task") {
        app.selected_task = Some(id.to_owned());
        app.detail_tab = 0;
        bridge::send(Command::LoadTaskDetail {
            task_id: id.to_owned(),
        });
    }
}

/// Close the Peek modal (backdrop / the X).
#[handler]
fn close_peek(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    app.selected_task = None;
}

/// Switch the modal's active tab.
#[handler]
fn set_detail_tab(app: &mut App, ctx: &EventContext) {
    if let Some(tab) = ctx.data("tab") {
        app.detail_tab = tab.parse().unwrap_or(0);
    }
}

#[handler]
fn stop(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::StopTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn start(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::StartTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn restart(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::RestartTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn cancel(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::CancelTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn approve(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ReviewVerdict {
            task_id: task.to_owned(),
            decision: "pass".to_owned(),
            feedback: String::new(),
        });
    }
}

#[handler]
fn reject(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ReviewVerdict {
            task_id: task.to_owned(),
            decision: "reject".to_owned(),
            feedback: String::new(),
        });
    }
}

#[handler]
fn request_changes(app: &mut App, ctx: &EventContext) {
    let _ = app;
    let Some(task) = ctx
        .field("task")
        .map(str::to_owned)
        .filter(|t| !t.is_empty())
    else {
        return;
    };
    let feedback = ctx.field("feedback").unwrap_or_default().trim().to_owned();
    bridge::send(Command::ReviewVerdict {
        task_id: task,
        decision: "revise".to_owned(),
        feedback,
    });
}
