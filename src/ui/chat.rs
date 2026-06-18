//! The chat surface. `surface()` is the main-area view (a scrolling transcript over
//! a pinned composer); `thread_list()` and `new_chat_button()` live in the left
//! drawer. Layout fills its panel via plain-div flex items — renderer outputs are
//! wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Button, Spinner, SpinnerSize, Text};
use rwire_markdown::Markdown;

use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole};

const COLUMN: &str = "48rem";

// ── Main area ───────────────────────────────────────────────────────────────

pub fn surface() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::OverflowHidden])
        .append([transcript_scroll(), composer_bar()])
}

fn transcript_scroll() -> ElementBuilder {
    el(El::Div)
        .style(
            Style::new()
                .min_height("0")
                .overflow("auto")
                .position("relative"),
        )
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
        .append([transcript()])
}

fn composer_bar() -> ElementBuilder {
    el(El::Div).st([St::BgApp]).append([composer()])
}

/// Always renders the same column→list structure; empty states are just a hint
/// item in the list. Keeping the structure constant (only the children change)
/// lets DOM morphing swap items positionally — a changing structure leaves the
/// previous subtree behind.
#[renderer]
fn transcript(app: &App) -> ElementBuilder {
    let items: Vec<ElementBuilder> = if app.open_chat.is_none() {
        vec![hint("Pick a conversation or start a new one.")]
    } else {
        let mut messages: Vec<ElementBuilder> = app.messages.iter().map(message_row).collect();
        if app.sending {
            messages.push(sending_row());
        }
        if messages.is_empty() {
            messages.push(hint("Say hello to start the conversation."));
        }
        messages
    };

    // Key the thread subtree by chat id: switching chats yields a new id, so the
    // morph replaces the whole subtree (old id becomes a removed leftover) instead
    // of positionally reusing the previous thread's nodes.
    let key = format!("thread-{}", app.open_chat.as_deref().unwrap_or("none"));
    column(
        el(El::Div)
            .id(&key)
            .st([St::DisplayFlex, St::FlexCol, St::GapXl, St::PXl])
            .append(items),
    )
}

fn hint(text: &str) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("padding-top", "12vh"))
        .st([St::TextCenter, St::TextMuted])
        .append([Text::body(text.to_owned()).muted().build()])
}

/// User turns are a right-aligned accent bubble; agent turns render as plain
/// left-aligned markdown across the column (Claude-style), so lists, code, and
/// headings read naturally without a box around them.
fn message_row(message: &ChatMessage) -> ElementBuilder {
    match message.role {
        MessageRole::User => el(El::Div)
            .st([St::DisplayFlex, St::JustifyEnd])
            .append([el(El::Div)
                .style(Style::new().max_width("75%"))
                .st([
                    St::BgAccent,
                    St::TextOnAccent,
                    St::PxLg,
                    St::PyMd,
                    St::RoundedXl,
                ])
                .append([el(El::Span).text(&message.text)])]),
        MessageRole::Agent => el(El::Div)
            .st([St::WFull])
            .append([Markdown::new(message.text.clone()).build()]),
    }
}

fn sending_row() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::JustifyStart])
        .append([el(El::Div)
            .st([
                St::DisplayFlex,
                St::ItemsCenter,
                St::GapSm,
                St::BgSurface,
                St::PxMd,
                St::PyMd,
                St::RoundedLg,
            ])
            .append([
                Spinner::new().size(SpinnerSize::Sm).build(),
                Text::body("thinking…").muted().build(),
            ])])
}

/// Keyed by transcript length so the textarea is a fresh (empty) element after each
/// turn — clears the input on send without a dedicated client opcode.
#[renderer]
fn composer(app: &App) -> ElementBuilder {
    let field_id = format!("composer-{}", app.messages.len());
    let pill = el(El::Div)
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::ItemsEnd,
            St::GapSm,
            St::BgSurface,
            St::RoundedXl,
            St::BorderSubtle,
            St::ShadowSm,
            St::PxMd,
            St::PySm,
        ])
        .append([
            el(El::Textarea)
                .id(&field_id)
                .attr("name", "message")
                .attr("rows", "1")
                .at_str(rwire::At::Placeholder, "Message claw…")
                .st([St::Flex1, St::TextDefault])
                .style(
                    Style::new()
                        .background("transparent")
                        .border("none")
                        .set("outline", "none")
                        .set("resize", "none")
                        .padding("0.25rem 0"),
                ),
            Button::primary("Send")
                .build()
                .attr("type", "submit")
                .data("act", "send"),
        ]);
    column(
        el(El::Form)
            .st([St::DisplayFlex, St::ItemsEnd, St::GapSm, St::PMd])
            .on(Ev::Submit, send_message())
            .append([pill]),
    )
}

/// A readable centered column (matches the transcript width).
fn column(inner: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .style(
            Style::new()
                .width("100%")
                .max_width(COLUMN)
                .margin("0 auto"),
        )
        .append([inner])
}

// ── Drawer ───────────────────────────────────────────────────────────────────

pub fn new_chat_button() -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("act", "new-chat")
        .on(Ev::Click, new_chat())
        .st([
            St::WFull,
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PySm,
            St::RoundedMd,
            St::BorderSubtle,
            St::BgSidebar,
            St::TextDefault,
            St::TextSm,
            St::CursorPointer,
        ])
        .hover([St::BgHover])
        .append([
            el(El::Span).st([St::TextMuted]).text("+"),
            el(El::Span).text("New chat"),
        ])
}

#[renderer]
pub fn thread_list(app: &App) -> ElementBuilder {
    if app.chats.is_empty() {
        return el(El::Div)
            .st([St::PMd])
            .append([Text::body("No chats yet").muted().build()]);
    }
    let open = app.open_chat.as_deref();
    let rows = app
        .chats
        .iter()
        .map(|chat| chat_row(&chat.id, &chat.title, open == Some(&chat.id), chat.busy));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::PxSm])
        .append(rows)
}

fn chat_row(id: &str, title: &str, active: bool, busy: bool) -> ElementBuilder {
    let dot = el(El::Span).st([
        St::WSp2,
        St::HSp2,
        St::RoundedFull,
        if busy { St::BgAccent } else { St::BgMuted },
    ]);
    let label = Text::body(title.to_owned())
        .build()
        .st([St::Flex1, St::Truncate]);
    let row = el(El::Div)
        .data("chat", id)
        .on(Ev::Click, select_chat())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PySm,
            St::RoundedMd,
            St::CursorPointer,
            St::TextSm,
        ])
        .hover([St::BgHover])
        .append([dot, label]);
    if active {
        row.st([St::BgActive, St::TextHigh])
    } else {
        row.st([St::TextMuted])
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[handler]
fn new_chat(_app: &mut App) {
    bridge::send(Command::NewChat);
}

#[handler]
fn select_chat(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("chat") {
        app.open_chat = Some(id.to_owned());
        app.messages.clear();
        app.sending = false;
        bridge::send(Command::OpenChat(id.to_owned()));
    }
}

#[handler]
fn send_message(app: &mut App, ctx: &EventContext) {
    let text = ctx.field("message").unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return;
    }
    let Some(chat) = app.open_chat.clone() else {
        return;
    };
    app.messages.push(ChatMessage {
        role: MessageRole::User,
        text: text.clone(),
    });
    app.sending = true;
    bridge::send(Command::Submit { chat, text });
}
