//! The task **Peek** modal (Phase F-4 / UX overhaul U2): a task's one-page view over the board.
//! Two tabs — **Overview** (spec + run controls) and **Thread**, a chat log of the task built on
//! the shared [`thread`] widgets: each run is an authored, phase-tagged entry with collapsible
//! tool rows; the live run streams into the newest entry; and review happens inline in a pinned
//! composer at the bottom. Opened by a card click (`open_peek`) or a deep link to
//! `/w/:wid/tasks/:tid` (the bridge sets `selected_task`). Renders from `App.task_detail`.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, icons, renderer};
use rwire_components::{Avatar, AvatarSize, Button, ButtonSize, Input, Text};

use super::thread;
use crate::bridge::{self, Command};
use crate::state::{App, LiveItem, ProposedTask, TaskDetail};
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
                    St::Rounded0,
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
            // The body fills the remaining height; each tab owns its own scroll (Overview scrolls
            // as one pane, the Thread autoscrolls above a pinned gate bar).
            el(El::Div)
                .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
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

/// The Overview / Thread tab strip. Activity + Review are unified into the chat-log **Thread**.
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
            tab_chip("Thread", 1, active),
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
            St::Rounded0,
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

/// The active tab's content, filling the modal body.
fn peek_tab_body(app: &App, detail: &TaskDetail) -> ElementBuilder {
    match app.detail_tab {
        1 => peek_thread(app, detail),
        _ => scroll_pane(peek_overview(detail)),
    }
}

/// A simple scrolling pane for the Overview tab (the Thread supplies its own scroll).
fn scroll_pane(inner: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::OverflowYAuto, St::PxMd, St::PbMd])
        .md([St::PxLg, St::PbLg])
        .append([inner])
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

/// Thread: the task as a chat log. An autoscrolling column of authored, phase-tagged entries —
/// the originating spec, a plan's proposal, each persisted run, and (while running) the live run
/// streaming in — above a pinned gate bar (review composer when parked, else run controls).
fn peek_thread(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let column = el(El::Div)
        .st([St::WFull, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PLg])
        .append(thread_entries(app, detail));
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([thread::scroll(column), gate_bar(detail)])
}

/// The ordered thread entries: the spec (as the originating request), a plan's proposed tasks,
/// each run as an authored entry, then the live run when one is in flight.
fn thread_entries(app: &App, detail: &TaskDetail) -> Vec<ElementBuilder> {
    let mut entries = vec![thread::entry(
        "you",
        St::TextHigh,
        Some("request"),
        "",
        None,
        true,
        vec![crate::ui::widgets::markdown(detail.spec.clone())],
    )];
    if detail.is_plan {
        entries.push(thread::entry(
            "claw",
            St::TextAccent,
            Some("Plan"),
            "",
            None,
            false,
            vec![proposed_view(&detail.proposed)],
        ));
    }
    for (index, run) in detail.runs.iter().enumerate() {
        entries.push(run_entry(run, &format!("a{index}"), &app.tool_toggled));
    }
    // The live run is its own `#[renderer]`, so a token flush repaints only this entry — not the
    // whole board/modal. It reads the live stream itself and self-hides when nothing is streaming,
    // so it's included unconditionally (the board never subscribes to `live_turns`).
    entries.push(live_entry());
    entries
}

/// One persisted run as an authored, phase-tagged thread entry: author (agent) + role tag +
/// timestamp + verdict chip, over its collapsible tool rows, Markdown reply and any feedback.
fn run_entry(run: &crate::state::RunView, key: &str, toggled: &[String]) -> ElementBuilder {
    let mut body: Vec<ElementBuilder> = run
        .tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            crate::ui::run_chain::tool_row(tool, &format!("{key}.{index}"), toggled)
        })
        .collect();
    if run.output.trim().is_empty() {
        body.push(Text::caption("(no output)").muted().build());
    } else {
        body.push(crate::ui::widgets::markdown(run.output.clone()));
    }
    if let Some(feedback) = run.feedback.as_deref().filter(|f| !f.trim().is_empty()) {
        body.push(
            Text::caption(format!("feedback: {feedback}"))
                .accent()
                .build(),
        );
    }
    let author_color = if run.agent == "you" {
        St::TextHigh
    } else {
        St::TextAccent
    };
    thread::entry(
        &run.agent,
        author_color,
        Some(&crate::ui::run_chain::role_tag(run)),
        &run.time,
        Some(crate::ui::run_chain::verdict_chip(run)),
        false,
        body,
    )
}

/// The live run as the newest thread entry: the agent + current step, a `● running` marker, its
/// in-progress tool events, and the reply as it streams in (the typewriter `▌`).
#[renderer]
fn live_entry(app: &App) -> ElementBuilder {
    // Self-contained synced region: only this reads `live_turns`, so a token flush repaints just
    // the live line. Show nothing unless the *selected* task is running.
    let hidden = || el(El::Div).st([St::DisplayNone]);
    let Some(detail) = app
        .task_detail
        .as_ref()
        .filter(|d| app.selected_task.as_deref() == Some(d.id.as_str()))
    else {
        return hidden();
    };
    if detail.status != "running" {
        return hidden();
    }
    let items: &[LiveItem] = app
        .live_turns
        .iter()
        .find(|turn| turn.task_id == detail.id)
        .map(|turn| turn.items.as_slice())
        .unwrap_or(&[]);
    let task = app.tasks.iter().find(|t| t.id == detail.id);
    let author = task.and_then(|t| t.agent.as_deref()).unwrap_or("owner");
    let mut tag = task
        .and_then(|t| app.pipeline_columns.get(t.step_position.max(0) as usize))
        .cloned()
        .unwrap_or_else(|| "Running".to_owned());
    if let Some(task) = task.filter(|t| t.iteration > 0) {
        tag.push_str(&format!(" · iter {}", task.iteration + 1));
    }
    let mut body: Vec<ElementBuilder> = Vec::new();
    if items.is_empty() {
        body.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text("The agent is working…"),
        );
    }
    // Render the stream in arrival order; the typewriter cursor rides only the final text segment.
    let last = items.len().saturating_sub(1);
    for (index, item) in items.iter().enumerate() {
        body.push(match item {
            LiveItem::Text(text) => streaming_text(text, index == last),
            LiveItem::Tool {
                time,
                action,
                in_progress,
                ok,
            } => live_event_row(time, action, *in_progress, *ok),
        });
    }
    thread::entry(
        author,
        St::TextAccent,
        Some(&tag),
        "",
        Some(running_dot()),
        false,
        body,
    )
}

/// A small `● running` marker for the live entry's header.
fn running_dot() -> ElementBuilder {
    el(El::Span)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::TextXs,
            St::TextAccent,
        ])
        .append([
            el(El::Span).st([St::WSp2, St::HSp2, St::RoundedFull, St::BgAccent]),
            el(El::Span).text("running"),
        ])
}

/// One live tool event: an in-progress / ok / failed mark, its time, and the humanized action.
fn live_event_row(time: &str, action: &str, in_progress: bool, ok: bool) -> ElementBuilder {
    let (mark, color) = if in_progress {
        ("▷", St::TextMuted)
    } else if ok {
        ("✓", St::TextAccent)
    } else {
        ("✗", St::TextLow)
    };
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
        .append([
            el(El::Span).st([color]).text(mark),
            el(El::Span).st([St::TextMuted]).text(time),
            el(El::Span).st([St::TextHigh]).text(action),
        ])
}

/// A streaming reply segment. The trailing typewriter cursor is drawn only on the segment that is
/// currently growing (the last item in the turn), so earlier segments read as settled text.
fn streaming_text(text: &str, cursor: bool) -> ElementBuilder {
    let body = if cursor {
        format!("{text}▌")
    } else {
        text.to_owned()
    };
    el(El::Div)
        .style(
            Style::new()
                .set("white-space", "pre-wrap")
                .set("word-break", "break-word"),
        )
        .st([St::TextXs, St::TextHigh, St::PtXs])
        .text(&body)
}

/// A plan's proposed tasks as a small list of cards (title · assignee · schedule · spec) — the
/// work the user approves when they approve the plan gate.
fn proposed_view(proposed: &[ProposedTask]) -> ElementBuilder {
    if proposed.is_empty() {
        return el(El::Div)
            .st([St::TextXs, St::TextMuted, St::PSm])
            .text("(no tasks proposed)");
    }
    let cards: Vec<ElementBuilder> = proposed
        .iter()
        .map(|p| {
            let mut meta = p.agent.clone().unwrap_or_else(|| "owner".to_owned());
            if let Some(schedule) = &p.schedule {
                meta.push_str(&format!(" · ⟳ {schedule}"));
            }
            el(El::Div)
                .st([
                    St::BgApp,
                    St::BorderDefault,
                    St::Rounded0,
                    St::PSm,
                    St::DisplayFlex,
                    St::FlexCol,
                    St::GapXs,
                ])
                .append([
                    el(El::Div)
                        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                        .append([
                            el(El::Span)
                                .st([St::Flex1, St::TextSm, St::TextHigh, St::FontSemibold])
                                .text(&p.title),
                            el(El::Span).st([St::TextXs, St::TextLow]).text(&meta),
                        ]),
                    el(El::Div).st([St::TextXs, St::TextMuted]).text(&p.spec),
                ])
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

/// The status-appropriate run-control buttons: Stop while live, Start/Restart when stopped,
/// Archive/Unarchive when terminal. Shared by the Overview control bar and the Thread's gate bar.
fn control_buttons(detail: &TaskDetail) -> Vec<ElementBuilder> {
    let id = detail.id.as_str();
    match detail.status.as_str() {
        "running" | "pending" => vec![
            action_button("Stop", stop(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        "stopped" => vec![
            action_button("Start", start(), id, true),
            action_button("Restart", restart(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        // Terminal (done | failed | rejected | cancelled): done is done — archive to declutter the
        // board (and restore it if archived). No restart: a finished task stays finished.
        _ if detail.archived => vec![action_button("Unarchive", unarchive(), id, false)],
        _ => vec![action_button("Archive", archive(), id, true)],
    }
}

/// The Overview tab's run-control bar (the same buttons, in a titled card).
fn control_bar(detail: &TaskDetail) -> ElementBuilder {
    card(
        "Controls",
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(control_buttons(detail)),
    )
}

/// The Thread's pinned bottom bar — the review composer when the task is parked at a human gate,
/// else the run controls. A `FlexShrink0` sibling below the scroll, like the chat composer.
fn gate_bar(detail: &TaskDetail) -> ElementBuilder {
    let inner = if detail.awaiting {
        review_gate(&detail.id)
    } else {
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(control_buttons(detail))
    };
    el(El::Div)
        .st([St::FlexShrink0, St::BorderT, St::PMd])
        .md([St::PxLg])
        .append([inner])
}

/// The inline review composer for a parked task: Approve / Reject, then a "what to change…" field
/// that submits a change request in flux — the re-run then streams into the thread above it.
fn review_gate(task_id: &str) -> ElementBuilder {
    el(El::Div)
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
                            St::Rounded0,
                            St::BorderDefault,
                            St::BgApp,
                            St::TextMuted,
                            St::CursorPointer,
                        ])
                        .hover([St::TextHigh])
                        .text("Request changes"),
                ]),
        ])
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
fn archive(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ArchiveTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn unarchive(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::UnarchiveTask {
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
