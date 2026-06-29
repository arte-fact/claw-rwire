//! Small UI building blocks shared across the app's surfaces — the page header and the
//! button family every view draws from, so buttons look the same everywhere.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, HandlerSpec, St, el, icons};
use rwire_components::{Button, ButtonSize, Text};
use rwire_markdown::{Markdown, ProseSize};

/// Render model output as **compact** markdown. The markdown component's heading scale
/// (`xl`/`2xl`) is far larger than the app's small UI text, so we shrink the size-scale CSS
/// vars (`--T*`) on the wrapper: everything renders a notch down, headings only slightly above
/// the body. One helper so chat, the task detail, and the review inbox stay consistent.
pub fn markdown(text: String) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol])
        .style(
            Style::new()
                .set("--T6", "1.05rem") // h1
                .set("--T5", "0.98rem") // h2
                .set("--T4", "0.92rem") // h3 / lg
                .set("--T3", "0.82rem") // base
                .set("--T2", "0.82rem"), // sm — body (via ProseSize::Sm), inline code, pre
        )
        .append([Markdown::new(text).size(ProseSize::Sm).full_width().build()])
}

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
    Button::secondary(label.to_string()).on_click(on_click)
}

/// A small secondary button (Clear, …) — the same look as [`action`].
pub fn clear_button(label: &str, on_click: HandlerSpec) -> ElementBuilder {
    action(label, on_click)
}

/// A bordered form-submit button (`type=submit`) with high-contrast text.
pub fn submit(label: &str) -> ElementBuilder {
    el(El::Button)
        .at(At::Type, Av::Submit)
        .st([
            St::PxMd,
            St::PySm,
            St::BorderDefault,
            St::Rounded0,
            St::BgApp,
            St::TextHigh,
            St::CursorPointer,
            St::FontInheritAll,
        ])
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
    if danger {
        return Button::destructive(label.to_string())
            .icon(icon)
            .on_click(on_click);
    }
    Button::secondary(label.to_string())
        .icon(icon)
        .on_click(on_click)
}

/// The primary filled-accent submit button (`type=submit`) — the composer-Send style, for
/// the one main action on a surface.
pub fn accent_submit(icon: icons::Icon, label: &str) -> ElementBuilder {
    Button::default()
        .size(ButtonSize::Sm)
        .build()
        .at(At::Type, Av::Submit)
        .append([icons::icon_sized(icon, 14), el(El::Span).text(label)])
}

/// A bare, subtle icon-only button (`type=button`) for inline row actions. `danger`
/// colours it as destructive.
pub fn icon_button(icon: icons::Icon, on_click: HandlerSpec, danger: bool) -> ElementBuilder {
    Button::ghost("")
        .size(ButtonSize::Sm)
        .on_click(on_click)
        .hover(if danger {
            [St::TextError]
        } else {
            [St::TextHigh]
        })
        .append([icons::icon_sized(icon, 14)])
}
