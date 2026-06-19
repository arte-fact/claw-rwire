//! The chat surface. `surface()` is the main-area view (a scrolling transcript over
//! a pinned composer); `thread_list()` and `new_chat_button()` live in the left
//! drawer. Layout fills its panel via plain-div flex items — renderer outputs are
//! wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Spinner, SpinnerSize, Text};
use rwire_markdown::Markdown;

use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole};

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
    el(El::Div)
        .st([St::BgApp])
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
        None => el(El::Div).style(Style::new().display("none")),
    }
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
    el(El::Div)
        .id(&key)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg, St::PLg])
        .append(items)
}

/// A 2px accent strip — rust-claw's left-bar marker for agent turns / active rows.
fn accent_bar(active: bool) -> ElementBuilder {
    el(El::Span)
        .style(
            Style::new()
                .width("2px")
                .set("flex", "0 0 2px")
                .set("align-self", "stretch"),
        )
        .st([if active { St::BgAccent } else { St::BgApp }])
}

fn hint(text: &str) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("padding-top", "12vh"))
        .st([St::TextCenter, St::TextMuted])
        .append([Text::body(text.to_owned()).muted().build()])
}

/// Flat terminal style (rust-claw): a bold sender label over the body, no bubbles.
/// Agent turns get a left accent bar; user text is plain, agent renders markdown.
fn message_row(message: &ChatMessage) -> ElementBuilder {
    let (sender, sender_color) = match message.role {
        MessageRole::User => ("you", St::TextHigh),
        MessageRole::Agent => ("claw", St::TextAccent),
    };
    let body = match message.role {
        MessageRole::User => el(El::Div)
            .st([St::TextDefault])
            .style(Style::new().set("white-space", "pre-wrap"))
            .append([el(El::Span).text(&message.text)]),
        MessageRole::Agent => el(El::Div).append([Markdown::new(message.text.clone()).build()]),
    };
    let header = el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span).st([St::FontBold, sender_color]).text(sender),
            el(El::Span)
                .st([St::TextLow, St::TextXs])
                .text(&message.time),
        ]);
    let block = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::Flex1])
        .append([header, body]);
    match message.role {
        MessageRole::User => el(El::Div)
            .st([
                St::DisplayFlex,
                St::GapMd,
                St::MlAuto,
                St::MaxW40rem,
                St::WFull,
            ])
            .append([accent_bar(true), block]),
        MessageRole::Agent => el(El::Div)
            .st([St::DisplayFlex, St::GapMd])
            .append([accent_bar(true), block]),
    }
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

fn sending_row() -> ElementBuilder {
    el(El::Div).st([St::DisplayFlex, St::GapMd]).append([
        accent_bar(true),
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                Spinner::new().size(SpinnerSize::Sm).build(),
                Text::body("thinking…").muted().build(),
            ]),
    ])
}

/// Keyed by transcript length so the textarea is a fresh (empty) element after each
/// turn — clears the input on send without a dedicated client opcode.
#[renderer]
fn composer(app: &App) -> ElementBuilder {
    let field_id = format!("composer-{}", app.messages.len());
    el(El::Form)
        .st([
            St::DisplayFlex,
            St::ItemsEnd,
            St::GapSm,
            St::PMd,
            St::BorderTSubtle,
        ])
        .on(Ev::Submit, send_message())
        .append([
            el(El::Textarea)
                .id(&field_id)
                .attr("name", "message")
                .attr("rows", "1")
                .at_str(rwire::At::Placeholder, "write a message…")
                .st([
                    St::Flex1,
                    St::BgSurface,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::PxMd,
                    St::PySm,
                    St::TextDefault,
                ])
                .style(Style::new().set("outline", "none").set("resize", "none"))
                .focus([St::BorderAccent]),
            el(El::Button)
                .attr("type", "submit")
                .data("act", "send")
                .st([
                    St::PxMd,
                    St::PySm,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::BgSurface,
                    St::TextDefault,
                    St::CursorPointer,
                ])
                .hover([St::BorderAccent, St::TextHigh])
                .append([el(El::Span).text("send")]),
        ])
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
    // `<a data-route href="/chat/:id">`: selecting a conversation is navigation —
    // the rwire client router pushes the URL and `on_route` opens it.
    let href = format!("/chat/{id}");
    let row = el(El::A)
        .attr("href", &href)
        .attr("data-route", "")
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PrMd,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
        ])
        .hover([St::TextHigh])
        .append([accent_bar(active), dot, label]);
    if active {
        row.st([St::TextHigh])
    } else {
        row.st([St::TextMuted])
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[handler]
fn new_chat(app: &mut App, ctx: &EventContext) {
    // Mint the id here so we can navigate to /chat/{id} right away — the bridge
    // persists it asynchronously. Optimistically show the empty conversation; the
    // route push (flushed after this update) puts the URL in sync.
    let Ok(id) = crate::id::new_id() else {
        return;
    };
    app.open_chat = Some(id.clone());
    app.messages.clear();
    app.sending = false;
    ctx.navigate(format!("/chat/{id}"));
    bridge::send(Command::NewChat { id });
}

/// Optimistically open a conversation (called by the router on a `/chat/:id`
/// route): show it at once, clear the previous transcript, and ask the bridge to
/// load its messages.
pub fn open(app: &mut App, id: &str) {
    app.open_chat = Some(id.to_owned());
    app.messages.clear();
    app.sending = false;
    bridge::send(Command::OpenChat(id.to_owned()));
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
        time: now_hm(),
    });
    app.sending = true;
    bridge::send(Command::Submit { chat, text });
}
