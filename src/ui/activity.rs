//! The Activity board: a **live** feed of agent tool-activity (`App.live_activity`) on top of
//! the newest-first run-lifecycle log (`App.activity`). Every row deep-links to its task (and
//! workspace) where the context exists. Running chats also show as busy dots in the sidebar.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Text;

use crate::state::{ActivityEntry, ActivityKind, App, LiveEvent};
use crate::ui::widgets::{clear_button, page_header};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Activity",
                "What the agents are doing right now, and the runs as they start and finish. \
                 Click any row to open its task.",
                clear_control(),
            ),
            live(),
            board(),
        ])
}

#[renderer]
fn clear_control(app: &App) -> ElementBuilder {
    if app.activity.is_empty() {
        return el(El::Div).style(Style::new().display("none"));
    }
    clear_button("Clear", clear())
}

#[handler]
fn clear(app: &mut App) {
    app.activity.clear();
}

/// The live tool-activity feed (in-flight + recently finished tool calls), newest first.
#[renderer]
fn live(app: &App) -> ElementBuilder {
    if app.live_activity.is_empty() {
        return el(El::Div).style(Style::new().display("none"));
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::TextXs, St::TextAccent, St::FontSemibold, St::PbXs])
            .text("▶ Live"),
    ];
    rows.extend(app.live_activity.iter().map(live_row));
    el(El::Div).st([St::DisplayFlex, St::FlexCol]).append(rows)
}

fn live_row(event: &LiveEvent) -> ElementBuilder {
    let (mark, color) = if event.in_progress {
        ("▷", St::TextMuted)
    } else if event.ok {
        ("✓", St::TextAccent)
    } else {
        ("✗", St::TextError)
    };
    Link::route(&format!(
        "/w/{}/tasks/{}",
        event.workspace_id, event.task_id
    ))
    .st([St::FontInheritAll])
    .st([
        St::DisplayFlex,
        St::ItemsCenter,
        St::GapMd,
        St::PyXs,
        St::NoUnderline,
        St::BorderBSubtle,
    ])
    .hover([St::BgSurface])
    .append([
        el(El::Span)
            .st([St::TextLow, St::TextXs, St::FontMono])
            .text(&event.time),
        el(El::Span).st([color, St::TextXs]).text(mark),
        el(El::Span)
            .st([St::TextHigh, St::TextXs, St::FontSemibold])
            .text(&event.agent),
        el(El::Span)
            .st([St::TextMuted, St::TextXs, St::Flex1, St::Truncate])
            .text(&event.action),
        el(El::Span)
            .st([St::TextLow, St::TextXs])
            .text(&event.workspace),
    ])
}

#[renderer]
fn board(app: &App) -> ElementBuilder {
    if app.activity.is_empty() {
        return el(El::Div)
            .st([St::PySm])
            .append([Text::body("No activity yet.").muted().build()]);
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol])
        .append(app.activity.iter().map(row))
}

fn row(entry: &ActivityEntry) -> ElementBuilder {
    let (label, color) = match entry.kind {
        ActivityKind::Started => ("started", St::TextMuted),
        ActivityKind::Finished => ("replied", St::TextAccent),
        ActivityKind::Failed => ("failed", St::TextError),
    };
    let mut children = vec![
        el(El::Span)
            .st([St::TextLow, St::TextXs, St::FontMono])
            .text(&entry.time),
        el(El::Span)
            .st([color, St::TextXs, St::FontMono])
            .text(label),
        el(El::Span)
            .st([St::TextHigh, St::Flex1, St::Truncate])
            .text(&entry.title),
    ];
    if !entry.detail.is_empty() {
        children.push(
            el(El::Span)
                .st([St::TextMuted, St::TextXs, St::Truncate])
                .text(&entry.detail),
        );
    }
    let row_st = [
        St::DisplayFlex,
        St::ItemsCenter,
        St::GapMd,
        St::PySm,
        St::BorderBDefault,
    ];
    // Task lifecycle rows deep-link to the task detail; chat rows are plain.
    match (&entry.task_id, &entry.workspace_id) {
        (Some(task), Some(workspace)) => Link::route(&format!("/w/{workspace}/tasks/{task}"))
            .st([St::FontInheritAll])
            .st(row_st)
            .st([St::NoUnderline])
            .hover([St::BgSurface])
            .append(children),
        _ => el(El::Div).st(row_st).append(children),
    }
}
