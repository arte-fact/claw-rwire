//! The Activity board: a live, newest-first log of agent-run events (`App.activity`).
//! The currently-running chats also show as busy dots in the sidebar (`App.running`).

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Text;

use crate::state::{ActivityEntry, ActivityKind, App};
use crate::ui::widgets::{clear_button, page_header};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append([
            page_header(
                "Activity",
                "Agent runs as they start and finish.",
                clear_control(),
            ),
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
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapMd,
            St::PySm,
            St::BorderBDefault,
        ])
        .append(children)
}
