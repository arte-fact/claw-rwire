//! The chat surface: a thread sidebar, a transcript, and a read-on-send composer.
//! Handlers mutate `App` optimistically and post a `Command`; the bridge persists
//! and projects the authoritative thread back.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Button, EmptyState, Spinner, SpinnerSize, Text};
use rwire_markdown::Markdown;

use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole};

pub fn surface() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::DisplayFlex, St::OverflowHidden])
        .append([sidebar_panel(), thread_panel()])
}

fn sidebar_panel() -> ElementBuilder {
    el(El::Aside)
        .style(Style::new().width("16rem").set("flex", "0 0 16rem"))
        .st([
            St::BgSidebar,
            St::BorderRDefault,
            St::DisplayFlex,
            St::FlexCol,
            St::OverflowHidden,
        ])
        .append([new_chat_bar(), sidebar_list()])
}

fn new_chat_bar() -> ElementBuilder {
    el(El::Div)
        .st([St::PSm, St::BorderBSubtle])
        .append([Button::primary("+ New chat")
            .full_width(true)
            .build()
            .data("act", "new-chat")
            .on(Ev::Click, new_chat())])
}

#[renderer]
fn sidebar_list(app: &App) -> ElementBuilder {
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
        .style(Style::new().set("overflow-y", "auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
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
            St::CursorPointer,
            St::TextSm,
        ])
        .append([dot, label]);
    if active {
        row.st([St::BgActive, St::TextHigh])
    } else {
        row.st([St::TextMuted])
    }
}

fn thread_panel() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("min-height", "0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
        .append([transcript(), composer()])
}

#[renderer]
fn transcript(app: &App) -> ElementBuilder {
    if app.open_chat.is_none() {
        return el(El::Div)
            .st([
                St::Flex1,
                St::DisplayFlex,
                St::ItemsCenter,
                St::JustifyCenter,
            ])
            .append([EmptyState::new()
                .title("No chat selected")
                .description("Start a new chat to begin.")
                .build()]);
    }

    let mut items: Vec<ElementBuilder> = app.messages.iter().map(message_row).collect();
    if app.sending {
        items.push(sending_row());
    }
    if items.is_empty() {
        items.push(
            el(El::Div)
                .st([St::PMd])
                .append([Text::body("Say hello to start the conversation.")
                    .muted()
                    .build()]),
        );
    }

    el(El::Div)
        .style(
            Style::new()
                .set("min-height", "0")
                .set("overflow-y", "auto"),
        )
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapMd, St::PLg])
        .append(items)
}

fn message_row(message: &ChatMessage) -> ElementBuilder {
    let (justify, bubble_bg, body) = match message.role {
        MessageRole::User => (
            St::JustifyEnd,
            St::BgAccent,
            el(El::Span).st([St::TextOnAccent]).text(&message.text),
        ),
        MessageRole::Agent => (
            St::JustifyStart,
            St::BgSurface,
            Markdown::new(message.text.clone()).build(),
        ),
    };
    el(El::Div)
        .st([St::DisplayFlex, justify])
        .append([el(El::Div)
            .style(Style::new().set("max-width", "70%"))
            .st([bubble_bg, St::PxMd, St::PyMd, St::RoundedMd])
            .append([body])])
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
                St::RoundedMd,
            ])
            .append([
                Spinner::new().size(SpinnerSize::Sm).build(),
                Text::body("thinking…").muted().build(),
            ])])
}

/// Keyed by transcript length: each sent/received turn changes the textarea's id,
/// so DOM morphing replaces it with a fresh (empty) element — clearing the input
/// on send without a dedicated client opcode. Typing doesn't touch `messages`, so
/// the field is stable between sends.
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
                .at_str(rwire::At::Placeholder, "Message…")
                .st([St::Flex1])
                .style(Style::new().set("resize", "none")),
            Button::primary("Send")
                .build()
                .attr("type", "submit")
                .data("act", "send"),
        ])
}

#[handler]
fn new_chat(_app: &mut App) {
    bridge::send(Command::NewChat);
}

#[handler]
fn select_chat(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("chat") {
        app.open_chat = Some(id.to_owned());
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
