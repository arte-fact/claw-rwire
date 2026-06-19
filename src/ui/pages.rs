//! Non-chat placeholder surfaces (Activity, Logs) shown in the main area. The
//! System tab has its own module (`system_map`). Each returns a scrollable, padded
//! page that fills its (flex) panel.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el};
use rwire_components::Text;

pub fn placeholder(title: &str, detail: &str) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append([
            Text::heading1(title.to_owned()).build(),
            Text::body(detail.to_owned()).muted().build(),
        ])
}
