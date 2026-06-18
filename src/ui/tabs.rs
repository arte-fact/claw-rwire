//! Placeholder panels for the not-yet-built tabs. The Chat tab is a real surface
//! (see `chat.rs`); System/Activity/Logs land in later phases.

use rwire::{El, ElementBuilder, St, el};
use rwire_components::Text;

pub fn placeholder(title: &str, detail: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::PLg, St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            Text::heading2(title.to_owned()).build(),
            Text::body(detail.to_owned()).muted().build(),
        ])
}
