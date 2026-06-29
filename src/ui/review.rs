//! The Review inbox (`/review`, Phase F-3): the cross-workspace surface for tasks parked at a
//! human gate. Each card shows the task + the work produced, with Approve / Request changes /
//! Reject — the verdict drives the validation loop. Renders from `App.review_inbox`.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, ReviewItem};
use crate::ui::widgets::{accent_submit, page_header};
use crate::ui::workspace::card;
use rwire_components::{Button, ButtonSize};

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
    let mut children: Vec<ElementBuilder> = if app.review_inbox.is_empty() {
        vec![
            el(El::Div)
                .st([St::TextSm, St::TextMuted, St::PySm])
                .text("Nothing needs your review right now."),
        ]
    } else {
        app.review_inbox
            .iter()
            .map(|item| review_card(item, &app.tool_toggled))
            .collect()
    };
    // The same Peek modal as the board — a review row opens the task in place, over the inbox.
    children.push(crate::ui::task_detail::peek_modal(app));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append(children)
}

fn review_card(item: &ReviewItem, toggled: &[String]) -> ElementBuilder {
    let body = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            // Provenance: the workspace (→ its board) and the spec.
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::GapSm,
                    St::TextXs,
                    St::TextMuted,
                ])
                .append([
                    Link::route(&format!("/w/{}/tasks", item.workspace_id))
                        .style(Style::new().set("font", "inherit"))
                        .st([St::TextMuted, St::NoUnderline])
                        .hover([St::TextAccent])
                        .text(&item.workspace),
                    el(El::Span).text(&format!("· {}", item.spec)),
                ]),
            // The work to review: a plan shows its proposed tasks; an ordinary task its full run.
            work_to_review(item, toggled),
            // Approve / Reject (clicks) + open the task in the Peek modal, then a Request-changes form.
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append(action_row(item)),
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

/// The reviewable work: a plan's proposed task list (R3), else the producer's full run.
fn work_to_review(item: &ReviewItem, toggled: &[String]) -> ElementBuilder {
    if item.is_plan {
        return proposed_view(&item.proposed);
    }
    match &item.run {
        Some(run) => crate::ui::run_chain::run_card(run, "r", true, toggled),
        None => el(El::Div)
            .st([St::TextXs, St::TextMuted, St::PSm])
            .text("(no output produced)"),
    }
}

/// The proposed tasks of a plan, as a small list of cards (title · assignee · schedule · spec).
fn proposed_view(proposed: &[crate::state::ProposedTask]) -> ElementBuilder {
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

/// The review row's action buttons: Approve / Reject, plus — for an ordinary task — an "Open task"
/// that opens it in the shared Peek modal (a plan shows its proposed tasks inline instead).
fn action_row(item: &ReviewItem) -> Vec<ElementBuilder> {
    let mut row = vec![
        approve_button(&item.task_id),
        plain("Reject", reject(), &item.task_id),
    ];
    if !item.is_plan {
        row.push(open_button(&item.task_id));
    }
    row
}

/// A borderless "Open task →" that opens the Peek modal over the inbox (reuses the board's modal).
fn open_button(task: &str) -> ElementBuilder {
    Button::ghost("Open task →")
        .size(ButtonSize::Sm)
        .on_click(crate::ui::task_detail::open_peek())
        .data("task", task)
}

/// The accented Approve button, carrying the task id for the click handler.
fn approve_button(task: &str) -> ElementBuilder {
    Button::primary("Approve")
        .size(ButtonSize::Sm)
        .on_click(approve())
        .data("task", task)
}

fn plain(label: &str, on_click: rwire::HandlerSpec, task: &str) -> ElementBuilder {
    Button::secondary(label.to_owned())
        .size(ButtonSize::Sm)
        .on_click(on_click)
        .data("task", task)
}

fn submit_chip(label: &str) -> ElementBuilder {
    accent_submit(icons::Icon::ArrowUp, label)
}

fn field_st() -> [St; 7] {
    [
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::Rounded0,
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
