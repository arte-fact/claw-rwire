//! The chat surface. `surface()` is the main-area view (a scrolling transcript over
//! a pinned composer); `thread_list()` and `new_chat_button()` live in the left
//! drawer. Layout fills its panel via plain-div flex items — renderer outputs are
//! wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, HandlerSpec, Pc, St, el, handler, icons, renderer};
use rwire_components::{Spinner, SpinnerSize, Text};

use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole, ToolCardView, TranscriptItem};

/// The reading width of the whole chat — transcript and composer share it, centered,
/// so lines don't sprawl on wide screens.
const CHAT_MAX_W: &str = "720px";

// ── Main area ───────────────────────────────────────────────────────────────

pub fn surface() -> ElementBuilder {
    // A column: a scrolling transcript above a composer that reserves its own height
    // (a non-shrinking flex child), so the composer can never cover the last turn.
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::OverflowHidden])
        .append([transcript_scroll(), composer_bar()])
}

/// The scrolling transcript area. `column-reverse` is the no-JS autoscroll: it makes the
/// bottom the scroll origin, so the view stays pinned to the newest turn as content
/// grows — yet a reader who scrolls up keeps their place (new turns land below, off
/// screen). The single inner [`transcript`] keeps normal top-to-bottom message order.
fn transcript_scroll() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexColReverse])
        .append([transcript()])
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
        .style(
            Style::new()
                .set("width", "100%")
                .set("max-width", CHAT_MAX_W),
        )
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
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
        let mut messages: Vec<ElementBuilder> = app.messages.iter().map(transcript_row).collect();
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
        // Centered and width-limited. The composer reserves its own space below, so no
        // outsized bottom padding is needed — just breathing room above and below.
        .style(
            Style::new()
                .set("width", "100%")
                .set("max-width", CHAT_MAX_W)
                .set("margin", "0 auto")
                .set("padding", "1.5rem 1.25rem"),
        )
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append(items)
}

/// A 2px accent strip — rust-claw's left-bar marker for agent turns / active rows.
/// Inactive in the sidebar is `BgSidebar` (invisible) rather than `BgApp`: the two
/// shades differ, so `BgApp` would paint a faint vertical line down every idle row.
/// In the transcript (always `active`) the accent is what shows.
pub(super) fn accent_bar(active: bool) -> ElementBuilder {
    el(El::Span)
        .style(
            Style::new()
                .width("2px")
                .set("flex", "0 0 2px")
                .set("align-self", "stretch"),
        )
        .st([if active { St::BgAccent } else { St::BgSidebar }])
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

/// One transcript entry: a chat turn or an inline tool card.
fn transcript_row(item: &TranscriptItem) -> ElementBuilder {
    match item {
        TranscriptItem::Message(message) => message_row(message),
        TranscriptItem::Tool(card) => tool_card(card),
    }
}

/// A tool invocation rendered inline: a `🔧 name · args · status` header over a capped,
/// scrollable result box. Subtle (it's secondary to the conversation). Reused by the task
/// detail's run transcript.
pub(crate) fn tool_card(card: &ToolCardView) -> ElementBuilder {
    let (status, status_color) = if card.ok {
        ("ok", St::TextAccent)
    } else {
        ("failed", St::TextError)
    };
    let header = el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::TextXs,
            St::FontMono,
        ])
        .append([
            el(El::Span)
                .st([St::TextMuted])
                .text(&format!("🔧 {}", card.name)),
            el(El::Span)
                .style(Style::new().min_width("0"))
                .st([St::TextLow, St::Flex1, St::Truncate])
                .text(&preview(&card.arguments, 80)),
            el(El::Span).st([status_color]).text(status),
            el(El::Span).st([St::TextLow]).text(&card.time),
        ]);
    let body = el(El::Pre)
        .st([St::TextXs, St::TextLow, St::FontMono])
        .style(
            Style::new()
                .set("white-space", "pre-wrap")
                .set("margin", "0")
                .set("max-height", "8rem")
                .overflow("auto"),
        )
        .text(&clip(&card.result, 800));
    el(El::Div)
        .style(
            Style::new()
                .set("align-self", "flex-start")
                .set("max-width", "100%"),
        )
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
            St::PSm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgSurface,
            St::WFull,
        ])
        .append([header, body])
}

/// One-line preview for the card header: collapses whitespace runs, then truncates.
fn preview(text: &str, max_chars: usize) -> String {
    clip(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
    )
}

/// Char-aware truncation with a trailing ellipsis, preserving newlines (used for the
/// multi-line result body).
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

/// Flat terminal style (rust-claw): a bold sender label over the body, no bubbles.
/// The left accent bar marks *your* turns; the agent's reply runs full-width with no
/// bar, so the two are easy to tell apart at a glance.
fn message_row(message: &ChatMessage) -> ElementBuilder {
    let (sender, sender_color) = match message.role {
        MessageRole::User => ("you", St::TextHigh),
        MessageRole::Agent => ("claw", St::TextAccent),
    };
    let body = match message.role {
        // `TextBase` to match the agent's Markdown prose (its wrapper is hardcoded to
        // `TextBase`), so your turn and claw's reply read at the same size — the app's
        // `TextSm` default would otherwise render your text a notch smaller.
        MessageRole::User => el(El::Div)
            .st([St::TextDefault, St::TextBase])
            .style(Style::new().set("white-space", "pre-wrap"))
            .append([el(El::Span).text(&message.text)]),
        MessageRole::Agent => crate::ui::widgets::markdown(message.text.clone()),
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
        // `MinW0` lets the column shrink below its content's intrinsic width and
        // `BreakWords` breaks long unbroken tokens (URLs, inline code) — together they
        // stop a reply from forcing a horizontal scroll on a narrow viewport. Code blocks
        // keep their own `white-space: pre` + inner horizontal scroll.
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
            St::MinW0,
            St::BreakWords,
        ])
        .append([header, body]);
    match message.role {
        // Right-aligned, capped at 40rem but never wider than the column, so on mobile it
        // wraps within the viewport instead of spilling past it.
        MessageRole::User => el(El::Div)
            .style(
                Style::new()
                    .set("align-self", "flex-end")
                    .set("max-width", "min(40rem, 100%)"),
            )
            .st([St::DisplayFlex, St::GapMd])
            .append([accent_bar(true), block]),
        // Full-width; `Flex1` over the block's `MinW0` lets the prose wrap at the column
        // edge instead of pushing it wider.
        MessageRole::Agent => el(El::Div)
            .st([St::DisplayFlex, St::MinW0])
            .append([block.st([St::Flex1])]),
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
    // The agent's pending turn — no accent bar, matching its finished replies.
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            Spinner::new().size(SpinnerSize::Sm).build(),
            Text::body("thinking…").muted().build(),
        ])
}

/// The composer pill: a rounded, shadowed card holding the message input over an
/// actions row (a left slot for future tools, the Send button on the right). It's a
/// `<form>`, so the single-line `<input>` submits on Enter natively — no keydown
/// opcode (rwire `preventDefault`s bound keydowns, which would block typing). Keyed by
/// transcript length so the input is a fresh (empty) element after each turn, which
/// clears it without a dedicated client opcode.
#[renderer]
fn composer(app: &App) -> ElementBuilder {
    let field_id = format!("composer-{}", app.messages.len());
    el(El::Form)
        .on(Ev::Submit, send_message())
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
            St::BgSurface,
            St::BorderDefault,
            St::RoundedLg,
            St::PMd,
            St::ShadowLg,
        ])
        .pseudo(Pc::FocusWithin, [St::BorderAccent])
        .append([message_input(&field_id), actions_row()])
}

fn message_input(field_id: &str) -> ElementBuilder {
    el(El::Input)
        .id(field_id)
        .attr("name", "message")
        .attr("type", "text")
        .attr("autocomplete", "off")
        .at_str(rwire::At::Placeholder, "Message claw…")
        .st([St::WFull, St::TextDefault])
        .style(
            Style::new()
                .set("background", "transparent")
                .set("border", "none")
                .set("outline", "none")
                .set("font", "inherit")
                .set("padding", "0.25rem"),
        )
}

/// The composer's bottom row: a left tools slot and the Send button on the right.
fn actions_row() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
        .append([composer_tools(), send_button()])
}

/// Left-side slot for future composer actions (attach, tools, model picker). Empty for
/// now — new buttons drop in here without disturbing the layout.
fn composer_tools() -> ElementBuilder {
    el(El::Div).st([St::DisplayFlex, St::ItemsCenter, St::GapXs])
}

fn send_button() -> ElementBuilder {
    el(El::Button)
        .attr("type", "submit")
        .attr("aria-label", "Send")
        .data("act", "send")
        .style(Style::new().set("font", "inherit").set("border", "none"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PxMd,
            St::PySm,
            St::RoundedMd,
            St::BgAccent,
            St::TextOnAccent,
            St::CursorPointer,
        ])
        .hover([St::BgAccentHover])
        .append([
            el(El::Span).text("Send"),
            icons::icon_sized(icons::Icon::ArrowUp, 16),
        ])
}

// ── Drawer ───────────────────────────────────────────────────────────────────

pub fn new_chat_button() -> ElementBuilder {
    // The primary action, styled as an aligned accent row (not a boxed pill) so its
    // label sits in the same column as the conversation rows it creates into.
    el(El::Button)
        .attr("type", "button")
        .data("act", "new-chat")
        .on(Ev::Click, new_chat())
        .style(
            Style::new()
                .set("font", "inherit")
                .set("border", "none")
                .set("background", "transparent")
                .set("width", "100%")
                .set("text-align", "left"),
        )
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::CursorPointer,
            St::TextSm,
            St::TextAccent,
        ])
        .hover([St::TextHigh])
        .append([
            accent_bar(false),
            icon_slot(icons::icon_sized(icons::Icon::Plus, 16)),
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
    let rows = app.chats.iter().map(|chat| {
        let busy = app.running.iter().any(|id| id == &chat.id);
        chat_row(&chat.id, &chat.title, open == Some(&chat.id), busy)
    });
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::PxMd])
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
    // `Link::route` (client-routed `<a>`): selecting a conversation is navigation —
    // the client router pushes /chat/:id and the bridge loads the thread.
    let href = format!("/chat/{id}");
    let link = Link::route(&href)
        .style(Style::new().min_width("0"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::Flex1,
            St::PyXs,
            St::CursorPointer,
            St::TextSm,
            St::NoUnderline,
            if active { St::TextHigh } else { St::TextMuted },
        ])
        .hover([St::TextHigh])
        .append([accent_bar(active), icon_slot(dot), label]);
    // The archive control is a *sibling* of the link, not nested inside the anchor:
    // clicking it archives the thread without also triggering navigation.
    el(El::Div).st([St::DisplayFlex, St::ItemsCenter]).append([
        link,
        row_icon_button(icons::Icon::Archive, "Archive", archive(), id),
    ])
}

/// A subtle, inline icon-button for a chat row (archive in the active list, restore in
/// the archived list). Always visible but low-contrast until hovered — rwire has no
/// group-hover, so reveal-on-row-hover isn't available. The target chat id rides on a
/// `data-chat` attribute the handler reads back.
pub(super) fn row_icon_button(
    icon: icons::Icon,
    aria: &str,
    on_click: HandlerSpec,
    chat_id: &str,
) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .attr("aria-label", aria)
        .data("chat", chat_id)
        .on(Ev::Click, on_click)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::PxXs,
            St::PyXs,
            St::RoundedSm,
            St::TextLow,
            St::CursorPointer,
            St::FlexShrink0,
        ])
        .style(
            Style::new()
                .set("font", "inherit")
                .set("border", "none")
                .set("background", "transparent"),
        )
        .hover([St::TextHigh])
        .append([icons::icon_sized(icon, 14)])
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

/// Archive the chat named by the clicked row's `data-chat`. No `App` mutation here —
/// the bridge re-projects the active + archived lists after the DB write.
#[handler]
fn archive(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("chat") {
        bridge::send(Command::ArchiveChat(id.to_owned()));
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
    app.messages.push(TranscriptItem::Message(ChatMessage {
        role: MessageRole::User,
        text: text.clone(),
        time: now_hm(),
    }));
    app.sending = true;
    bridge::send(Command::Submit { chat, text });
}
