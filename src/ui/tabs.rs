//! Per-tab content panels. Phase 1 ships placeholders; the chat panel already
//! reads the shared projection so the live-update path is exercised end to end.

use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::{EmptyState, Stack, Text};

use crate::state::App;

#[renderer]
pub fn chat(app: &App) -> ElementBuilder {
    if app.chats.is_empty() {
        return EmptyState::new()
            .title("No chats yet")
            .description("Conversations will appear here once the chat surface lands.")
            .build();
    }
    let rows = app.chats.iter().map(|chat| {
        let dot = if chat.busy { St::BgAccent } else { St::BgMuted };
        el(El::Div)
            .id(&chat.id)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::PySm])
            .append([
                el(El::Span).st([St::WSp2, St::HSp2, St::RoundedFull, dot]),
                Text::body(chat.title.clone()).build(),
            ])
    });
    Stack::column().children(rows).build()
}

pub fn placeholder(title: &str, detail: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            Text::heading2(title.to_owned()).build(),
            Text::body(detail.to_owned()).muted().build(),
        ])
}
