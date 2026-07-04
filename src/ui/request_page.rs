//! The request page (G2): `/w/:wid/requests/:rid` — one user ask's whole story in a scroll:
//! status + branch, the ask verbatim, the task timeline with per-task diff activity, the
//! repos it touched (branch point → merge), and the global diff against the branch point.

use rwire::{El, ElementBuilder, Link, St, el, handler, renderer};
use rwire_components::{
    Badge, BadgeFill, BadgeIntent, BadgeShape, Breadcrumb, Button, ButtonSize, Text,
};

use crate::bridge::{self, Command};
use crate::diff::DiffKind;
use crate::state::{App, RequestDetail};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::OverflowYAuto, St::PMd])
        .append([body()])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(detail) = &app.request_detail else {
        return Text::body("Loading the request…").muted().build();
    };
    let mut sections = vec![header(app, detail), ask_block(detail)];
    if !detail.validation.is_empty() {
        sections.push(section(
            "Validation",
            el(El::Div)
                .st([
                    St::TextSm,
                    St::TextDefault,
                    St::WhitespacePreWrap,
                    St::BreakWords,
                ])
                .text(&detail.validation),
        ));
    }
    if !detail.tasks.is_empty() {
        sections.push(section("Tasks", timeline(detail)));
    }
    if !detail.repos.is_empty() {
        sections.push(section("Repos", repos(detail)));
    }
    if !detail.diff_stat.is_empty() {
        sections.push(section("Diff", global_diff(detail)));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg, St::MaxW2xl])
        .append(sections)
}

/// The page's first line, per the navigation redesign (N0): breadcrumb trail
/// (workspace › request), the status badge riding the crumb tail, the branch, and the
/// status-appropriate controls right-aligned — place, state, and verbs on one line.
fn header(app: &App, detail: &RequestDetail) -> ElementBuilder {
    let workspace = app
        .workspaces
        .iter()
        .find(|w| w.id == detail.workspace_id)
        .map(|w| w.name.clone())
        .unwrap_or_else(|| detail.workspace_id.clone());
    let mut row = vec![
        Breadcrumb::new()
            .item(workspace, Some(format!("/w/{}", detail.workspace_id)))
            .item(detail.title.clone(), None::<String>)
            .build(),
        status_badge(&detail.status),
    ];
    if !detail.branch.is_empty() {
        row.push(
            el(El::Span)
                .st([St::TextXs, St::TextMuted])
                .text(&detail.branch),
        );
    }
    row.push(controls(detail));
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append(row)
}

/// The user's words, verbatim — what the request is ultimately judged against.
fn ask_block(detail: &RequestDetail) -> ElementBuilder {
    section(
        "The ask",
        el(El::Div)
            .st([
                St::TextSm,
                St::TextDefault,
                St::WhitespacePreWrap,
                St::BreakWords,
                St::PSm,
                St::BgApp,
            ])
            .text(&detail.ask),
    )
}

fn timeline(detail: &RequestDetail) -> ElementBuilder {
    let rows: Vec<ElementBuilder> = detail
        .tasks
        .iter()
        .map(|task| {
            let label = if task.is_plan { "plan" } else { "task" };
            let mut parts = vec![
                el(El::Span).st([St::TextLow, St::TextXs]).text(label),
                Link::route(&format!("/w/{}/tasks/{}", detail.workspace_id, task.id))
                    .st([St::Flex1, St::MinW0, St::Truncate, St::TextSm])
                    .hover([St::TextAccent])
                    .text(&task.title),
            ];
            if !task.stat.is_empty() {
                parts.push(
                    el(El::Span)
                        .st([St::TextXs, St::TextMuted])
                        .text(&task.stat),
                );
            }
            parts.push(status_badge(&task.status));
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append(parts)
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(rows)
}

fn repos(detail: &RequestDetail) -> ElementBuilder {
    let rows: Vec<ElementBuilder> = detail
        .repos
        .iter()
        .map(|repo| {
            let merge = if repo.merge.is_empty() {
                "unmerged".to_owned()
            } else {
                format!("merged @ {}", repo.merge)
            };
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
                .append([
                    el(El::Span).st([St::TextDefault]).text(&repo.repo),
                    el(El::Span)
                        .st([St::TextMuted])
                        .text(&format!("from {}", repo.base)),
                    el(El::Span).st([St::TextMuted]).text(&merge),
                ])
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append(rows)
}

/// The whole request against its branch point: the stat block, then the capped patch in the
/// shared DiffLine colours.
fn global_diff(detail: &RequestDetail) -> ElementBuilder {
    let mut children = vec![
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::WhitespacePreWrap])
            .text(&detail.diff_stat),
    ];
    if !detail.diff_patch.is_empty() {
        let rows: Vec<ElementBuilder> = crate::diff::patch_lines(&detail.diff_patch)
            .into_iter()
            .map(|line| {
                let color = match line.kind {
                    DiffKind::Added => St::TextSuccess,
                    DiffKind::Removed => St::TextError,
                    DiffKind::Equal => St::TextMuted,
                };
                el(El::Div)
                    .st([St::WhitespacePreWrap, St::BreakWords, St::TextXs, color])
                    .text(&line.text)
            })
            .collect();
        children.push(
            el(El::Div)
                .st([St::PSm, St::BgApp, St::OverflowXAuto])
                .append(rows),
        );
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(children)
}

/// The status-appropriate explicit acts (G3). Delivery/rollback stay reversible-by-design
/// (a merge can be reverted; an abandon drops only an unmerged branch); Push is the one
/// outward act and never happens un-clicked.
fn controls(detail: &RequestDetail) -> ElementBuilder {
    let id = detail.id.as_str();
    let mut buttons: Vec<ElementBuilder> = Vec::new();
    match detail.status.as_str() {
        "validating" => {
            buttons.push(control(Button::primary("Deliver"), deliver(), id));
            buttons.push(control(Button::secondary("Abandon"), abandon(), id));
        }
        "planning" | "executing" => {
            buttons.push(control(Button::secondary("Abandon"), abandon(), id));
        }
        "delivered" => {
            buttons.push(control(Button::secondary("Roll back"), rollback(), id));
            if detail.has_remote {
                buttons.push(control(Button::primary("Push"), push(), id));
            }
        }
        "rolled-back" if detail.has_remote => {
            buttons.push(control(Button::primary("Push"), push(), id));
        }
        _ => {}
    }
    if buttons.is_empty() {
        return el(El::Div).st([St::DisplayNone]);
    }
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::MlAuto])
        .append(buttons)
}

fn control(button: Button, on_click: rwire::HandlerSpec, id: &str) -> ElementBuilder {
    button
        .size(ButtonSize::Sm)
        .on_click(on_click)
        .data("request", id)
}

#[handler]
fn deliver(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send(Command::RequestDeliver { id: id.to_owned() });
    }
}

#[handler]
fn abandon(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send(Command::RequestAbandon { id: id.to_owned() });
    }
}

#[handler]
fn rollback(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send(Command::RequestRollback { id: id.to_owned() });
    }
}

#[handler]
fn push(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send(Command::RequestPush { id: id.to_owned() });
    }
}

fn section(title: &str, body: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div)
                .st([St::TextXs, St::TextLow, St::TextUppercase])
                .text(title),
            body,
        ])
}

/// The request lifecycle as a badge — same square/outline vocabulary as task cards.
fn status_badge(status: &str) -> ElementBuilder {
    let intent = match status {
        "executing" | "running" => BadgeIntent::Primary,
        "delivered" | "done" => BadgeIntent::Success,
        "rejected" | "failed" | "cancelled" => BadgeIntent::Error,
        "awaiting" | "planning" => BadgeIntent::Warning,
        _ => BadgeIntent::Default,
    };
    Badge::new()
        .intent(intent)
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .text(status.to_owned())
        .build()
}
