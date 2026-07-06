//! The concierge's one persistent conversation (roadmap R1). `surface()` is the main-area
//! view (a scrolling transcript over a pinned composer) rendered at `/` — there is no "new
//! chat"; the thread never resets. Layout fills its panel via plain-div flex items — renderer
//! outputs are wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use std::borrow::Cow;

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::{ChatAuthor, ChatItem, ChatItemCtx, ChatTranscript, Composer, Text};

use super::thread;
use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole, ToolCardView, TranscriptItem};
use crate::ui::memory;

// The shared reading measure for conversation surfaces is `St::MaxW60rem` + `St::MxAuto` —
// tokens, not inline styles: the client morph reliably patches classes across route swaps,
// whereas a reused node's inline `style` can go stale (the width-loss bug this replaced).

// ── Main area ───────────────────────────────────────────────────────────────

pub fn surface() -> ElementBuilder {
    // A column: a scrolling transcript above a composer that reserves its own height
    // (a non-shrinking flex child), so the composer can never cover the last turn.
    el(El::Div)
        .st([
            St::MinH0,
            St::Flex1,
            St::DisplayFlex,
            St::FlexCol,
            St::OverflowHidden,
        ])
        .append([thread::scroll(transcript()), composer_bar()])
}

/// The composer row: an elevated, centered, width-limited card sitting below the
/// transcript. `flex-shrink: 0` so it always keeps its height; the surrounding padding
/// gives it the inset, floating look without overlaying the conversation.
fn composer_bar() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("padding", "0.5rem 1rem 1rem"))
        .st([
            St::FlexShrink0,
            St::DisplayFlex,
            St::FlexCol,
            St::ItemsCenter,
        ])
        .append([composer_column()])
}

/// Width-limited column holding the error banner above the composer pill.
fn composer_column() -> ElementBuilder {
    el(El::Div)
        .st([
            St::MaxW60rem,
            St::WFull,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append([error_banner(), composer()])
}

/// A transient error from the last turn (endpoint failure). Hidden when clear.
#[renderer]
fn error_banner(app: &App) -> ElementBuilder {
    match &app.error {
        Some(message) => el(El::Div)
            .st([
                St::PxMd,
                St::PySm,
                St::TextXs,
                St::TextError,
                St::BorderTSubtle,
            ])
            .append([el(El::Span).text(&format!("⚠ {message}"))]),
        None => el(El::Div).st([St::DisplayNone]),
    }
}

/// The transcript: the concierge's turns as authored `ChatItem` entries rendered through rwire's
/// `ChatTranscript`. The memory-fold line rides a chrome-free `row()`; the writing cue and empty
/// state are the component's own. Keyed by chat id so a chat switch swaps the whole subtree
/// instead of positionally reusing the previous thread's nodes.
#[renderer]
fn transcript(app: &App) -> ElementBuilder {
    let starting = app.open_chat.is_none();
    let mut rows: Vec<Row> = Vec::new();
    if !starting {
        // Concierge parity (R3.5 M3): the chat is the concierge's thread, so it opens on the
        // same fold line + memory card the agent pages have — the global scope row.
        if let Some(mem) = app.chat_memory.as_ref()
            && !mem.summary.trim().is_empty()
        {
            let line = if mem.last_fold.is_empty() {
                "· older messages condensed into memory ·".to_owned()
            } else {
                format!(
                    "· older messages condensed into memory · last fold {} ·",
                    mem.last_fold
                )
            };
            rows.push(Row::Fold(line));
        }
        rows.extend(app.messages.iter().map(|item| match item {
            TranscriptItem::Message { id, message } => Row::Msg { id: *id, message },
            TranscriptItem::Tool { id, card } => Row::Tool { id: *id, card },
        }));
    }

    let empty = hint(if starting {
        "Starting up…"
    } else {
        "Say hello to start the conversation."
    });
    let transcript = ChatTranscript::new()
        .items_plain(rows.iter())
        // Seamless history (P4a): older turns arrive as the reader scrolls up — no button.
        // `messages_truncated` is the count past the window and the sentinel's `next`; zero hides it.
        .on_load_older(
            expand_chat(),
            app.messages_truncated as u32,
            app.messages_truncated > 0,
        )
        // Show the writing cue only while a turn is in flight (`writing(None)` hides it).
        .writing(app.sending.then(|| Cow::from("claw is typing…")))
        .empty_state(empty)
        .build();

    let key = format!("thread-{}", app.open_chat.as_deref().unwrap_or("none"));
    el(El::Div)
        .id(&key)
        // Centered and width-limited (tokens — see the reading-measure note at the top); the flex
        // column lets the transcript stretch to the measure. The composer reserves its own space
        // below, so just breathing room above and below.
        .st([
            St::MaxW60rem,
            St::MxAuto,
            St::WFull,
            St::PyLg,
            St::PxMd,
            St::DisplayFlex,
            St::FlexCol,
        ])
        .append([transcript])
}

/// One concierge transcript row, adapted to rwire's `ChatItem` so the surface renders through
/// `ChatTranscript`/`ChatEntry` — the same authored-entry chrome (intent rail + header) as the
/// agent thread. `id` (the `web_messages` rowid) keys the entry and the tool disclosure, so older
/// pages prepend on scroll-up without re-keying — and morphing — the rest of the transcript.
enum Row<'a> {
    /// The memory-fold marker above the oldest turn — a chrome-free muted line via `row()`.
    Fold(String),
    Msg {
        id: i64,
        message: &'a ChatMessage,
    },
    Tool {
        id: i64,
        card: &'a ToolCardView,
    },
}

impl ChatItem for Row<'_> {
    fn key(&self) -> Cow<'_, str> {
        match self {
            Row::Fold(_) => Cow::Borrowed("fold"),
            Row::Msg { id, .. } => Cow::Owned(format!("m{id}")),
            Row::Tool { id, .. } => Cow::Owned(format!("t{id}")),
        }
    }

    fn author(&self) -> ChatAuthor {
        match self {
            Row::Msg { message, .. } => match message.role {
                MessageRole::User => ChatAuthor::user("you"),
                MessageRole::Agent => ChatAuthor::agent("claw"),
            },
            // Tool chatter is claw's, de-emphasised.
            Row::Tool { .. } => ChatAuthor::agent("claw").muted(),
            // Fold renders via `row()`; its author is never read.
            Row::Fold(_) => ChatAuthor::agent("claw"),
        }
    }

    fn body(&self, _ctx: &ChatItemCtx<Self>) -> ElementBuilder {
        match self {
            Row::Msg { message, .. } => match message.role {
                // Match claw's compact Markdown size so your turn and claw's reply read alike.
                MessageRole::User => el(El::Div)
                    // `font-size: 0.82rem` — matches the agent's compact markdown; no token for it.
                    .style(Style::new().set("font-size", "0.82rem"))
                    .st([St::TextDefault, St::WhitespacePreWrap])
                    .append([el(El::Span).text(&message.text)]),
                MessageRole::Agent => crate::ui::widgets::markdown(message.text.clone()),
            },
            // The tool call as the shared native-details disclosure (identical to the task thread).
            Row::Tool { id, card } => crate::ui::run_chain::tool_row(card, &format!("chat.{id}")),
            Row::Fold(_) => el(El::Span),
        }
    }

    fn time(&self) -> Option<Cow<'_, str>> {
        match self {
            Row::Msg { message, .. } => Some(Cow::Borrowed(message.time.as_str())),
            Row::Tool { card, .. } => Some(Cow::Borrowed(card.time.as_str())),
            Row::Fold(_) => None,
        }
    }

    fn row(&self, _ctx: &ChatItemCtx<Self>) -> Option<ElementBuilder> {
        match self {
            Row::Fold(line) => Some(memory::muted_line(line)),
            _ => None,
        }
    }

    fn groups_with(&self, prev: &Self) -> bool {
        // Suppress the repeated header for a same-author run (same role / consecutive tools).
        match (self, prev) {
            (Row::Msg { message: a, .. }, Row::Msg { message: b, .. }) => a.role == b.role,
            (Row::Tool { .. }, Row::Tool { .. }) => true,
            _ => false,
        }
    }
}

/// A 2px accent strip — rust-claw's left-bar marker for agent turns / active rows.
/// Inactive in the sidebar is `BgSidebar` (invisible) rather than `BgApp`: the two
/// shades differ, so `BgApp` would paint a faint vertical line down every idle row.
/// In the transcript (always `active`) the accent is what shows.
pub(super) fn accent_bar(active: bool) -> ElementBuilder {
    el(El::Span)
        // `width: 2px` / `flex: 0 0 2px` — a hairline strip, no token for the 2px basis.
        .style(Style::new().width("2px").set("flex", "0 0 2px"))
        .st([
            St::SelfStretch,
            if active { St::BgAccent } else { St::BgSidebar },
        ])
}

/// A fixed-width slot for a drawer row's leading glyph (status dot, nav icon, action
/// icon, or nothing). Centering every glyph in the same 1rem box means each row's
/// label starts at the same x — one text column down the whole drawer, whatever the
/// glyph's own size. Pair it with [`accent_bar`] for the full leading gutter.
pub(super) fn icon_slot(glyph: ElementBuilder) -> ElementBuilder {
    el(El::Span)
        .style(Style::new().width("1rem").set("flex", "0 0 1rem"))
        .st([St::DisplayFlex, St::ItemsCenter, St::JustifyCenter])
        .append([glyph])
}

fn hint(text: &str) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("padding-top", "12vh"))
        .st([St::TextCenter, St::TextMuted])
        .append([Text::body(text.to_owned()).muted().build()])
}

/// `HH:MM` in UTC (matching the DB's UTC `created_at` stamps), for the optimistic
/// user turn shown before the bridge reloads the persisted thread.
fn now_hm() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60)
}

/// The composer pill: a rounded, shadowed card holding the message input over an
/// actions row (a left slot for future tools, the Send button on the right). It's a
/// `<form>`, so the single-line `<input>` submits on Enter natively — no keydown
/// opcode (rwire `preventDefault`s bound keydowns, which would block typing). Keyed by
/// transcript length so the input is a fresh (empty) element after each turn, which
/// clears it without a dedicated client opcode.
#[renderer]
fn composer(app: &App) -> ElementBuilder {
    Composer::new()
        .id(format!("composer-{}", app.messages.len()))
        .placeholder("Message claw…")
        .hint("⏎ send · ⇧⏎ newline")
        .on_submit(send_message())
        .build()
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[handler]
fn send_message(app: &mut App, ctx: &EventContext) {
    let text = ctx.field("message").unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return;
    }
    let Some(chat) = app.open_chat.clone() else {
        return;
    };
    // Optimistic echo before the DB round-trip: id `-1` (not yet persisted). The next projection
    // replaces it with the real windowed load, so this transient key never collides.
    app.messages.push(TranscriptItem::Message {
        id: -1,
        message: ChatMessage {
            role: MessageRole::User,
            text: text.clone(),
            time: now_hm(),
        },
    });
    app.sending = true;
    bridge::send(Command::Submit { chat, text });
}

/// The seamless-history sentinel scrolled into view — ask the bridge to grow the open thread's
/// window and re-project it with the next page of older turns prepended (P4a).
#[handler]
fn expand_chat(app: &mut App, ctx: &EventContext) {
    let _ = (app, ctx);
    bridge::send(Command::ExpandChat);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rwire_components::ChatItem;

    /// Entries key by the `web_messages` rowid, not list position — so paging older turns in on
    /// scroll-up doesn't re-key (and re-render) the turns already on screen (P4a slice 4b refinement).
    #[test]
    fn rows_key_by_stable_id_not_position() {
        let msg = ChatMessage {
            role: MessageRole::User,
            text: "hi".to_owned(),
            time: "12:00".to_owned(),
        };
        let card = ToolCardView {
            name: "search".to_owned(),
            arguments: String::new(),
            result: String::new(),
            ok: true,
            time: "12:00".to_owned(),
        };
        assert_eq!(
            Row::Msg {
                id: 42,
                message: &msg
            }
            .key()
            .as_ref(),
            "m42"
        );
        assert_eq!(Row::Tool { id: 7, card: &card }.key().as_ref(), "t7");
        // The optimistic just-sent turn keys distinctly (id -1); older pages loading in front of it
        // leave every id-keyed entry above untouched.
        assert_eq!(
            Row::Msg {
                id: -1,
                message: &msg
            }
            .key()
            .as_ref(),
            "m-1"
        );
    }
}
