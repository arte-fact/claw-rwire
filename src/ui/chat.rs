//! The concierge's one persistent conversation (roadmap R1). `surface()` is the main-area
//! view (a scrolling transcript over a pinned composer) rendered at `/` — there is no "new
//! chat"; the thread never resets. Layout fills its panel via plain-div flex items — renderer
//! outputs are wrapped in inline `__synced_` spans, so they only ever sit *inside* a plain
//! flex/scroll container, never act as one.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, Pc, St, el, handler, icons, renderer};
use rwire_components::{Spinner, SpinnerSize, Text};

use super::thread;
use crate::bridge::{self, Command};
use crate::state::{App, ChatMessage, MessageRole, TranscriptItem};
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

/// Always renders the same column→list structure; empty states are just a hint
/// item in the list. Keeping the structure constant (only the children change)
/// lets DOM morphing swap items positionally — a changing structure leaves the
/// previous subtree behind.
#[renderer]
fn transcript(app: &App) -> ElementBuilder {
    let items: Vec<ElementBuilder> = if app.open_chat.is_none() {
        vec![hint("Starting up…")]
    } else {
        // Concierge parity (R3.5 M3): the chat is the concierge's thread, so it opens on the
        // same fold line + memory card the agent pages have — the global scope row.
        let mut messages: Vec<ElementBuilder> = Vec::new();
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
            messages.push(memory::muted_line(&line));
        }
        messages.extend(
            app.messages
                .iter()
                .enumerate()
                .map(|(index, item)| transcript_row(item, index, &app.tool_toggled)),
        );
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
        // Centered and width-limited (tokens — see the reading-measure note at the top). The
        // composer reserves its own space below, so just breathing room above and below.
        .st([
            St::MaxW60rem,
            St::MxAuto,
            St::WFull,
            St::PyLg,
            St::PxMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapLg,
        ])
        .append(items)
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

/// One transcript entry: a chat turn, or an inline tool call rendered with the **shared**
/// collapsible tool row (so chat tool calls read exactly like the task thread's).
fn transcript_row(item: &TranscriptItem, index: usize, toggled: &[String]) -> ElementBuilder {
    match item {
        TranscriptItem::Message(message) => message_row(message),
        TranscriptItem::Tool(card) => {
            crate::ui::run_chain::tool_row(card, &format!("chat.{index}"), toggled)
        }
    }
}

/// Flat terminal style (rust-claw): a bold sender label over the body, no bubbles — built on the
/// shared [`thread::entry`] so a chat turn and a task-thread entry are the same widget. The left
/// accent bar marks *your* turns; the agent's reply runs full-width with no bar.
fn message_row(message: &ChatMessage) -> ElementBuilder {
    let (sender, sender_color) = match message.role {
        MessageRole::User => ("you", St::TextHigh),
        MessageRole::Agent => ("claw", St::TextAccent),
    };
    let body = match message.role {
        // Match the agent's compact Markdown body (see `widgets::markdown`, which shrinks the
        // prose to ~0.82rem) so your turn and claw's reply read at the same size.
        MessageRole::User => el(El::Div)
            // `font-size: 0.82rem` — matches the agent's compact markdown; no token for it.
            .style(Style::new().set("font-size", "0.82rem"))
            .st([St::TextDefault, St::WhitespacePreWrap])
            .append([el(El::Span).text(&message.text)]),
        MessageRole::Agent => crate::ui::widgets::markdown(message.text.clone()),
    };
    let is_user = matches!(message.role, MessageRole::User);
    let entry = thread::entry(
        sender,
        sender_color,
        None,
        &message.time,
        None,
        is_user,
        vec![body],
    );
    if is_user {
        // Right-aligned, capped at 40rem but never wider than the column, so on mobile it
        // wraps within the viewport instead of spilling past it.
        el(El::Div)
            .style(
                Style::new()
                    .set("align-self", "flex-end")
                    .set("max-width", "min(40rem, 100%)"),
            )
            .append([entry])
    } else {
        entry
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
    // A textarea so messages can be multiline. `field-sizing: content` auto-grows it to the
    // text (starting at one row), capped at `max-height` where it scrolls. `data-enter-submit`
    // opts into rwire's runtime behavior: Enter submits the form (→ the Submit handler), and
    // Shift+Enter inserts a newline — the standard chat convention.
    el(El::Textarea)
        .id(field_id)
        .at_str(At::Name, "message")
        .at_str(At::Rows, "1")
        .at(At::Autocomplete, Av::Off)
        .data("enter-submit", "1")
        .at_str(At::Placeholder, "Message claw…")
        .st([
            St::WFull,
            St::TextDefault,
            St::BgTransparent,
            St::BorderNone,
            St::OutlineNone,
            St::ResizeNone,
            St::OverflowYAuto,
            St::FieldSizingContent,
        ])
        // `padding`/`max-height: 40vh` stay inline (genuinely dynamic); field-sizing has a token.
        .style(
            Style::new()
                .set("padding", "0.25rem")
                .set("max-height", "40vh"),
        )
}

/// The composer's bottom row: a left tools slot and the Send button on the right.
fn actions_row() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
        .append([composer_tools(), send_button()])
}

/// Left-side slot for composer actions (attach, tools, model picker) — for now a muted hint
/// of the keyboard convention. New buttons drop in here without disturbing the layout.
fn composer_tools() -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::TextXs,
            St::TextLow,
        ])
        .append([el(El::Span).text("⏎ send · ⇧⏎ newline")])
}

fn send_button() -> ElementBuilder {
    el(El::Button)
        .at(At::Type, Av::Submit)
        .at_str(At::AriaLabel, "Send")
        .data("act", "send")
        .st([
            St::BorderNone,
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
    app.messages.push(TranscriptItem::Message(ChatMessage {
        role: MessageRole::User,
        text: text.clone(),
        time: now_hm(),
    }));
    app.sending = true;
    bridge::send(Command::Submit { chat, text });
}
