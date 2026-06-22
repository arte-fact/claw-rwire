//! The Review inbox (`/review`, Phase F-3): the cross-workspace surface for tasks parked at a
//! human gate. Each card shows the task + the work produced, with Approve / Request changes /
//! Reject — the verdict drives the validation loop. Renders from `App.review_inbox`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, ReviewItem};
use crate::ui::widgets::{accent_submit, page_header};
use crate::ui::workspace::card;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Review",
                "Work waiting on you, across every workspace. Approve to accept, request changes \
                 to send it back with feedback, or reject.",
                el(El::Div),
            ),
            body(),
        ])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    if app.review_inbox.is_empty() {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted, St::PySm])
            .text("Nothing needs your review right now.");
    }
    let cards: Vec<ElementBuilder> = app.review_inbox.iter().map(review_card).collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append(cards)
}

fn review_card(item: &ReviewItem) -> ElementBuilder {
    let body = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text(&format!("{} · {}", item.workspace, item.spec)),
            // The work to review.
            el(El::Div)
                .style(
                    Style::new()
                        .set("white-space", "pre-wrap")
                        .set("word-break", "break-word")
                        .set("font", "inherit"),
                )
                .st([
                    St::TextXs,
                    St::TextHigh,
                    St::BgApp,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::PSm,
                ])
                .text(if item.output.trim().is_empty() {
                    "(no output produced)"
                } else {
                    &item.output
                }),
            // Approve / Reject (clicks), then a Request-changes form with a notes box.
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    approve_button(&item.task_id),
                    plain("Reject", reject(), &item.task_id),
                ]),
            el(El::Form)
                .on(Ev::Submit, request_changes())
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    el(El::Input)
                        .attr("type", "hidden")
                        .attr("name", "task")
                        .attr("value", &item.task_id),
                    el(El::Input)
                        .attr("name", "feedback")
                        .attr("placeholder", "What to change…")
                        .attr("autocomplete", "off")
                        .style(Style::new().set("font", "inherit").set("flex", "1"))
                        .st(field_st())
                        .focus([St::BorderAccent]),
                    submit_chip("Request changes"),
                ]),
        ]);
    card(&item.title, body)
}

/// The accented Approve button, carrying the task id for the click handler.
fn approve_button(task: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("task", task)
        .on(Ev::Click, approve())
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
        .text("Approve")
}

fn plain(label: &str, on_click: rwire::HandlerSpec, task: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("task", task)
        .on(Ev::Click, on_click)
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
        .text(label)
}

fn submit_chip(label: &str) -> ElementBuilder {
    accent_submit(icons::Icon::ArrowUp, label)
}

fn field_st() -> [St; 7] {
    [
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
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
