//! Non-chat surfaces shown in the main area. Each returns a scrollable, padded
//! page that fills its (flex) panel. System is the dedicated admin page; the
//! interactive System Map lands in Phase 3.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el};
use rwire_components::{Card, CardPadding, Gap, Stack, Stat, Text};

pub fn admin() -> ElementBuilder {
    page([
        Text::heading1("System").build(),
        Text::body(
            "Channels, agents, and endpoints at a glance. The interactive layered map lands next.",
        )
        .muted()
        .build(),
        Stack::row()
            .gap(Gap::Md)
            .wrap(true)
            .children([
                stat_card("Channels", "—"),
                stat_card("Agents", "—"),
                stat_card("Endpoints", "—"),
            ])
            .build(),
    ])
}

pub fn placeholder(title: &str, detail: &str) -> ElementBuilder {
    page([
        Text::heading1(title.to_owned()).build(),
        Text::body(detail.to_owned()).muted().build(),
    ])
}

fn stat_card(label: &str, value: &str) -> ElementBuilder {
    Card::new()
        .padding(CardPadding::Lg)
        .child(Stat::new(value.to_owned()).label(label.to_owned()).build())
        .build()
}

fn page(children: impl IntoIterator<Item = ElementBuilder>) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append(children)
}
