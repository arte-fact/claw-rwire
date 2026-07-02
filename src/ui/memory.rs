//! Shared memory-surface widgets (R3.5): the muted fold/event lines and the editable memory
//! card, used by both conversation pages — the agent thread (workspace scope) and the
//! concierge chat (global scope). The pages differ only in which scope row their handlers
//! target, so the handlers come in as [`rwire::HandlerSpec`]s.

use rwire::{El, ElementBuilder, Ev, HandlerSpec, St, el, icons};
use rwire_components::{Button, ButtonSize, Textarea};

use crate::ui::widgets::accent_submit;

/// A muted, centered one-liner — the fold line and the timeline's memory-event lines.
pub fn muted_line(text: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::TextXs, St::TextLow, St::TextCenter])
        .text(text)
}

/// A memory event in a timeline (R3.5 M2) — one muted line at the moment a fold/edit/clear
/// happened. The thread stays a conversation; memory is its quiet narrator.
pub fn event_line(time: &str, note: &str) -> ElementBuilder {
    muted_line(&format!("· {time} — {note} ·"))
}

/// Everything the editable memory card needs from its page: the scope's summary + pause
/// state, a card id suffix (stable per scope, so switching scopes swaps the textarea), the
/// meta line, and the page's scope-targeting handlers.
pub struct MemoryCard<'a> {
    pub id_suffix: String,
    pub meta: String,
    pub summary: &'a str,
    pub paused: bool,
    pub on_save: HandlerSpec,
    pub on_clear: HandlerSpec,
    pub on_pause: HandlerSpec,
}

/// The memory affordance (R3): a scope's working memory, inspectable and editable in place.
/// Save overwrites the summary, Clear resets it, Pause/Resume freezes the auto-fold.
pub fn memory_card(card: MemoryCard) -> ElementBuilder {
    el(El::Form)
        .on(Ev::Submit, card.on_save)
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
            St::PSm,
            St::BorderDefault,
            St::Rounded0,
            St::BgSurface,
        ])
        .append([
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::GapSm,
                    St::TextXs,
                    St::TextMuted,
                ])
                .append([
                    el(El::Span)
                        .st([St::TextUppercase, St::TrackingWide])
                        .text("Memory"),
                    el(El::Span).text(&format!("· {}", card.meta)),
                ]),
            Textarea::new()
                .id(format!("memory-{}", card.id_suffix))
                .name("summary")
                .rows(4)
                .value(card.summary.to_owned())
                .build(),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    accent_submit(icons::Icon::Check, "Save"),
                    small_button("Clear", card.on_clear, None),
                    small_button(
                        if card.paused { "Resume" } else { "Pause" },
                        card.on_pause,
                        Some(("paused", if card.paused { "false" } else { "true" })),
                    ),
                ]),
        ])
}

/// A small secondary memory action, optionally carrying one `data-(key)=value` attribute.
fn small_button(label: &str, on_click: HandlerSpec, data: Option<(&str, &str)>) -> ElementBuilder {
    let button = Button::secondary(label.to_owned())
        .size(ButtonSize::Sm)
        .on_click(on_click);
    match data {
        Some((key, value)) => button.data(key, value),
        None => button,
    }
}
