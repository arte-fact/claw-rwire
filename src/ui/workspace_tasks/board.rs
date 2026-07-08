//! The kanban view: one column per pipeline step plus Done/Closed, each task as a card with
//! its step trail, meta row, signal tags, and per-status actions. Split from the module's
//! mod.rs (page scaffold, filters, shared controls, handlers).

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler};
use rwire_components::{Avatar, AvatarSize, Tag, TagIntent, Tooltip};

use crate::bridge::{self, Command};
use crate::state::{App, TaskView};

use super::{control_buttons, status_badge, task_matches_filter};

/// The kanban board: one column per pipeline step, then Done and Closed. A task sits in its
/// current step's column (or Done/Closed when terminal) and moves left→right as it advances.
pub(super) fn kanban_board(app: &App, wid: &str) -> ElementBuilder {
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
                .filter(|task| task.request_id.is_none())
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
        Some(id) => crate::ui::widgets::bare_button(
            crate::ui::workspace_workflow::toggle_edit().with_param_bytes(id.as_bytes().to_vec()),
        )
        .st(header_st)
        .st([St::TextLeft, St::P0])
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
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                // The title routes to the task's page (N1) — the canonical task surface.
                rwire::router::Link::route(&format!("/w/{wid}/tasks/{}", task.id))
                    .st([
                        St::Flex1,
                        St::TextXs,
                        St::TextHigh,
                        St::FontSemibold,
                        St::TextLeft,
                        St::P0,
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
        .child(Avatar::new().name(assignee).size(AvatarSize::Sm).build())
        .build();
    let mut row = vec![avatar];
    row.extend(signal_tags(task));
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs, St::FlexWrap])
        .append(row)
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
    if task.request_id.is_some() {
        // Part of a Request (G1) — the rail above the board tracks the whole ask.
        tags.push(Tag::new("request").build());
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
                .on_click(kanban_approve().with_param_bytes(task.id.as_bytes().to_vec())),
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

#[handler]
fn kanban_approve(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task_id) = super::task_param(ctx) {
        bridge::send_action(
            Command::ReviewVerdict {
                task_id,
                decision: "pass".to_owned(),
                feedback: String::new(),
            },
            ctx.defer(),
        );
    }
}
