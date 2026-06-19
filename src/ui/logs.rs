//! The Logs viewer: claw's recent operational log lines (`App.logs`), newest first —
//! the bridge routes its diagnostics through `bridge::log_line`, which both prints and
//! appends here.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use crate::state::{App, LogLine};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append([
            Text::heading1("Logs").build(),
            Text::body("Recent bridge diagnostics and errors.")
                .muted()
                .build(),
            list(),
        ])
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
