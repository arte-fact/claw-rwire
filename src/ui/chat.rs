//! The chat surface. `surface()` is the main-area view (a scrolling transcript over
//! a pinned composer); `thread_list()` and `new_chat_button()` live in the left
//! drawer. Layout fills its panel via plain-div flex items — renderer outputs are
//! wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{Button, EmptyState, Spinner, SpinnerSize, Text};
use rwire_markdown::Markdown;

use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole, Tab};

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
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
        .append([transcript()])
}

fn composer_bar() -> ElementBuilder {
    el(El::Div).st([St::BorderTSubtle]).append([composer()])
}

#[renderer]
fn transcript(app: &App) -> ElementBuilder {
    if app.open_chat.is_none() {
        return centered(
            EmptyState::new()
                .title("No chat selected")
                .description("Pick a conversation or start a new one.")
                .build(),
        );
    }

    let mut items: Vec<ElementBuilder> = app.messages.iter().map(message_row).collect();
    if app.sending {
        items.push(sending_row());
    }
    if items.is_empty() {
        return centered(
            Text::body("Say hello to start the conversation.")
                .muted()
                .build(),
        );
    }

    column(
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapMd, St::PXl])
            .append(items),
    )
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
            .style(Style::new().max_width("80%"))
            .st([bubble_bg, St::PxMd, St::PyMd, St::RoundedLg])
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

fn centered(inner: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyCenter,
            St::PXl,
        ])
        .append([inner])
}

// ── Drawer ───────────────────────────────────────────────────────────────────

pub fn new_chat_button() -> ElementBuilder {
    Button::secondary("+  New chat")
        .full_width(true)
        .build()
        .data("act", "new-chat")
        .on(Ev::Click, new_chat())
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
        .append([dot, label]);
    if active {
        row.st([St::BgActive, St::TextHigh])
    } else {
        row.st([St::TextMuted])
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[handler]
fn new_chat(app: &mut App) {
    app.tab = Tab::Chat;
    bridge::send(Command::NewChat);
}

#[handler]
fn select_chat(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("chat") {
        app.tab = Tab::Chat;
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
