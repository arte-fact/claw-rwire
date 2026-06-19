//! Small UI building blocks shared by the dashboard surfaces (Activity, Logs).

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, HandlerSpec, St, el};
use rwire_components::Text;

/// A page header: heading + muted subtitle on the left, an action on the right.
pub fn page_header(
    title: &'static str,
    subtitle: &'static str,
    action: ElementBuilder,
) -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsStart,
            St::JustifyBetween,
            St::GapMd,
        ])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapXs])
                .append([
                    Text::heading1(title).build(),
                    Text::body(subtitle).muted().build(),
                ]),
            action,
        ])
}

/// A small secondary button (Clear, …).
pub fn clear_button(label: &str, on_click: HandlerSpec) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .on(Ev::Click, on_click)
        .st([
            St::PxSm,
            St::PySm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .hover([St::TextHigh, St::BorderAccent])
        .text(label)
}
