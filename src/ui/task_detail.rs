//! The task detail view (`/w/:wid/tasks/:tid`, Phase F-4): a task's spec and its full run
//! chain as a transcript — each producer/validator run with its output and verdict, the audit
//! trail of how the work flowed through the pipeline. Parked tasks carry the gate actions.
//! Renders from `App.task_detail`.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, RunView, TaskDetail};
use crate::ui::workspace::card;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapMd, St::PMd])
        .append([body()])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(detail) = &app.task_detail else {
        return el(El::Div).st([St::TextSm, St::TextMuted]).text("Loading…");
    };
    let mut sections = vec![
        back_link(&detail.workspace_id),
        header(detail),
        // The spec.
        card(
            "Spec",
            el(El::Div)
                .style(Style::new().set("white-space", "pre-wrap"))
                .st([St::TextSm, St::TextHigh])
                .text(&detail.spec),
        ),
    ];
    // The run chain (the transcript).
    let mut runs: Vec<ElementBuilder> = detail.runs.iter().map(run_card).collect();
    if runs.is_empty() {
        runs.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted])
                .text("No runs yet — the task hasn't started."),
        );
    }
    sections.push(card(
        "Run chain",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(runs),
    ));
    // Gate actions when parked.
    if detail.awaiting {
        sections.push(actions(&detail.id));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append(sections)
}

fn back_link(workspace_id: &str) -> ElementBuilder {
    Link::route(&format!("/w/{workspace_id}/tasks"))
        .style(Style::new().set("font", "inherit"))
        .st([St::TextXs, St::TextMuted, St::NoUnderline])
        .hover([St::TextHigh])
        .text("← Back to the board")
}

fn header(detail: &TaskDetail) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span)
                .st([St::Flex1, St::TextLg, St::TextHigh, St::FontSemibold])
                .text(&detail.title),
            el(El::Span)
                .st([St::TextXs, St::TextMuted, St::TextUppercase])
                .text(&detail.status),
        ])
}

fn run_card(run: &RunView) -> ElementBuilder {
    // Label: the role, plus the iteration for a loop run and the verdict for a validator.
    let mut label = match run.role.as_str() {
        "validator" => "Review".to_owned(),
        "producer" => "Produce".to_owned(),
        _ => "Stage".to_owned(),
    };
    if run.iteration > 0 {
        label.push_str(&format!(" · iter {}", run.iteration + 1));
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                el(El::Span)
                    .st([St::Flex1, St::TextXs, St::TextHigh, St::FontSemibold])
                    .text(&label),
                el(El::Span).st([St::TextXs, St::TextLow]).text(&run.agent),
                verdict_chip(run),
            ]),
    ];
    let shown = if run.output.trim().is_empty() {
        "(no output)"
    } else {
        &run.output
    };
    rows.push(
        el(El::Div)
            .style(
                Style::new()
                    .set("white-space", "pre-wrap")
                    .set("word-break", "break-word"),
            )
            .st([St::TextXs, St::TextMuted])
            .text(shown),
    );
    if let Some(feedback) = run.feedback.as_deref().filter(|f| !f.trim().is_empty()) {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextAccent])
                .text(&format!("feedback: {feedback}")),
        );
    }
    el(El::Div)
        .st([
            St::BgApp,
            St::BorderDefault,
            St::RoundedSm,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

fn verdict_chip(run: &RunView) -> ElementBuilder {
    let (text, color) = match run.verdict.as_deref() {
        Some("pass") => ("pass", St::TextAccent),
        Some("revise") => ("revise", St::TextMuted),
        Some("reject") => ("reject", St::TextLow),
        _ if !run.ok => ("failed", St::TextLow),
        _ => ("", St::TextLow),
    };
    if text.is_empty() {
        return el(El::Span);
    }
    el(El::Span)
        .st([St::TextXs, St::TextUppercase, St::TrackingWide, color])
        .text(text)
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
                        .attr("type", "hidden")
                        .attr("name", "task")
                        .attr("value", task_id),
                    el(El::Input)
                        .attr("name", "feedback")
                        .attr("placeholder", "What to change…")
                        .attr("autocomplete", "off")
                        .style(Style::new().set("font", "inherit").set("flex", "1"))
                        .st([
                            St::PxSm,
                            St::PySm,
                            St::BorderDefault,
                            St::RoundedSm,
                            St::BgApp,
                            St::TextHigh,
                            St::OutlineNone,
                        ])
                        .focus([St::BorderAccent]),
                    el(El::Button)
                        .attr("type", "submit")
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
    let btn = el(El::Button)
        .attr("type", "button")
        .data("task", task)
        .on(Ev::Click, on_click)
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::CursorPointer,
        ])
        .text(label);
    if accent {
        btn.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        btn.st([St::BorderDefault, St::BgApp, St::TextMuted])
            .hover([St::TextHigh])
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
