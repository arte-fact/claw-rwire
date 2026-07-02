//! Shared memory-surface widgets (R3.5): the muted fold/event lines and the editable memory
//! card, used by both conversation pages — the agent thread (workspace scope) and the
//! concierge chat (global scope). The pages differ only in which scope row their handlers
//! target, so the handlers come in as [`rwire::HandlerSpec`]s.

use rwire::{El, ElementBuilder, St, el};

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
