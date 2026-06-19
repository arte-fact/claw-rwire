//! The Logs viewer: claw's recent operational log lines (`App.logs`), newest first —
//! the bridge routes its diagnostics through `bridge::log_line`, which both prints and
//! appends here.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Text;

use crate::state::{App, LogLine};
use crate::ui::widgets::{clear_button, page_header};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append([
            page_header(
                "Logs",
                "Recent bridge diagnostics and errors.",
                clear_control(),
            ),
            list(),
        ])
}

#[renderer]
fn clear_control(app: &App) -> ElementBuilder {
    if app.logs.is_empty() {
        return el(El::Div).style(Style::new().display("none"));
    }
    clear_button("Clear", clear())
}

#[handler]
fn clear(app: &mut App) {
    app.logs.clear();
}

#[renderer]
fn list(app: &App) -> ElementBuilder {
    if app.logs.is_empty() {
        return el(El::Div)
            .st([St::PySm])
            .append([Text::body("No logs yet.").muted().build()]);
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::FontMono, St::TextXs])
        .append(app.logs.iter().map(line))
}

fn line(entry: &LogLine) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::GapMd, St::PyXs])
        .append([
            el(El::Span).st([St::TextLow]).text(&entry.time),
            el(El::Span)
                .st([St::TextMuted])
                .style(Style::new().set("white-space", "pre-wrap"))
                .text(&entry.message),
        ])
}
