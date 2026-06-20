//! Small UI building blocks shared across the app's surfaces — the page header and the
//! button family every view draws from, so buttons look the same everywhere.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, HandlerSpec, St, el, icons};
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

// The app's button family. Every view draws from these so buttons look consistent: a
// bordered, `BgApp`, hover-accent-border base for secondary actions and form submits, a
// single filled-accent primary (the composer-Send style), and a bare icon button for
// inline row actions.

/// A bordered secondary button (`type=button`): muted text, hover lifts to accent border.
pub fn action(label: &str, on_click: HandlerSpec) -> ElementBuilder {
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

/// A small secondary button (Clear, …) — the same look as [`action`].
pub fn clear_button(label: &str, on_click: HandlerSpec) -> ElementBuilder {
    action(label, on_click)
}

/// A bordered form-submit button (`type=submit`) with high-contrast text.
pub fn submit(label: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "submit")
        .st([
            St::PxMd,
            St::PySm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgApp,
            St::TextHigh,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .hover([St::BorderAccent])
        .text(label)
}

/// A bordered icon+label action (`type=button`). `danger` colours it as destructive.
pub fn action_icon(
    icon: icons::Icon,
    label: &str,
    on_click: HandlerSpec,
    danger: bool,
) -> ElementBuilder {
    let color = if danger { St::TextError } else { St::TextMuted };
    el(El::Button)
        .attr("type", "button")
        .on(Ev::Click, on_click)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PySm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgApp,
            color,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .hover([St::TextHigh, St::BorderAccent])
        .append([icons::icon_sized(icon, 14), el(El::Span).text(label)])
}

/// The primary filled-accent submit button (`type=submit`) — the composer-Send style, for
/// the one main action on a surface.
pub fn accent_submit(icon: icons::Icon, label: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "submit")
        .style(Style::new().set("font", "inherit").set("border", "none"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PySm,
            St::RoundedMd,
            St::BgAccent,
            St::TextOnAccent,
            St::CursorPointer,
        ])
        .hover([St::BgAccentHover])
        .append([icons::icon_sized(icon, 14), el(El::Span).text(label)])
}

/// A bare, subtle icon-only button (`type=button`) for inline row actions. `danger`
/// colours it as destructive.
pub fn icon_button(icon: icons::Icon, on_click: HandlerSpec, danger: bool) -> ElementBuilder {
    let color = if danger { St::TextError } else { St::TextLow };
    el(El::Button)
        .attr("type", "button")
        .on(Ev::Click, on_click)
        .style(
            Style::new()
                .set("font", "inherit")
                .set("border", "none")
                .set("background", "transparent"),
        )
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::PxXs,
            St::FlexShrink0,
            St::CursorPointer,
            color,
        ])
        .hover([St::TextHigh])
        .append([icons::icon_sized(icon, 14)])
}
