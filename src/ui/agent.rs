//! The agent thread page (`/agent/:id`, R2): the agent's working life as a conversation. Turns
//! read like chat — you (the task's spec) → the agent (its reply prose, phase tag, verdict chip)
//! → you / a reviewer (feedback) — with tool work as one muted "n tool calls · task ↗" line
//! linking to the board's Peek, where the forensics live. A parked task's review composer sits
//! at the tail, and a running task streams live below the history.
//!
//! Rendering structure: **two sibling synced regions** under static scaffolding — the keyed
//! [`timeline`] (reads `agent_thread` only) and the [`live_tail`] (reads `live_turns` too). A
//! streamed token repaints only the tail; a lifecycle change re-projects the (windowed, tiny)
//! timeline. No renderer nests inside a keyed subtree — the two failure modes of the v1 page.

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use super::chat::CHAT_MAX_W;
use crate::state::{AgentThreadItem, AgentTurn, App, LiveItem};
use crate::ui::run_chain::verdict_chip_of;
use crate::ui::task_detail::{live_stream_body, review_gate, running_dot};
use crate::ui::thread;

/// The page shell: a pinned header over the autoscrolling conversation — a static column whose
/// two renderer children (timeline, live tail) are siblings, never nested.
pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([
            header_bar(),
            thread::scroll(
                el(El::Div)
                    .style(
                        Style::new()
                            .set("max-width", CHAT_MAX_W)
                            .set("margin", "0 auto"),
                    )
                    .st([St::WFull, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
                    .md([St::PLg])
                    .append([timeline(), live_tail()]),
            ),
        ])
}

/// The pinned page header: the agent's name, centered on the reading measure. The border spans
/// the page; the content aligns with the conversation column below.
fn header_bar() -> ElementBuilder {
    el(El::Div)
        .st([St::FlexShrink0, St::PMd, St::BorderBDefault])
        .md([St::PxLg])
        .append([el(El::Div)
            .style(
                Style::new()
                    .set("max-width", CHAT_MAX_W)
                    .set("margin", "0 auto"),
            )
            .st([St::WFull, St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([agent_title()])])
}

/// The agent's name in the header — its own small region so the static scaffold never re-renders.
#[renderer]
fn agent_title(app: &App) -> ElementBuilder {
    let name = app
        .agent_thread
        .as_ref()
        .map_or("…", |thread| thread.agent_name.as_str());
    el(El::Span)
        .st([St::TextLg, St::TextHigh, St::FontSemibold])
        .text(name)
}

/// The conversation history: the windowed, chronological turns + any review gates at the tail.
/// Reads **only** `agent_thread` — a streamed token never repaints this region. Keyed by agent id
/// so switching agents replaces the subtree instead of morphing one thread into another.
#[renderer]
fn timeline(app: &App) -> ElementBuilder {
    let thread = app.agent_thread.as_ref();
    let key = thread.map_or("loading", |t| t.agent_id.as_str());
    let mut rows: Vec<ElementBuilder> = Vec::new();
    match thread {
        None => rows.push(Text::body("Loading…").muted().build()),
        Some(thread) => {
            if thread.truncated > 0 {
                rows.push(
                    el(El::Div)
                        .st([St::TextXs, St::TextLow, St::TextCenter])
                        .text(&format!("· {} earlier turns not shown ·", thread.truncated)),
                );
            }
            for item in &thread.items {
                rows.push(match item {
                    AgentThreadItem::Turn(turn) => turn_row(turn),
                    AgentThreadItem::ReviewGate { task_id } => review_prompt(task_id),
                });
            }
            if thread.items.is_empty() && thread.live.is_empty() {
                rows.push(
                    el(El::Div)
                        .st([St::TextSm, St::TextMuted])
                        .text("No activity yet — this agent hasn't worked on anything."),
                );
            }
        }
    }
    el(El::Div)
        .id(&format!("agent-thread-{key}"))
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append(rows)
}

/// The live run(s) streaming below the history — the only region that reads `live_turns`, so a
/// token flush repaints just this tail. Self-hides when the agent has nothing running.
#[renderer]
fn live_tail(app: &App) -> ElementBuilder {
    let Some(thread) = app.agent_thread.as_ref().filter(|t| !t.live.is_empty()) else {
        return el(El::Div).st([St::DisplayNone]);
    };
    let entries: Vec<ElementBuilder> = thread
        .live
        .iter()
        .map(|live| {
            let items: &[LiveItem] = app
                .live_turns
                .iter()
                .find(|turn| turn.task_id == live.task_id)
                .map(|turn| turn.items.as_slice())
                .unwrap_or(&[]);
            thread::entry(
                &thread.agent_name,
                St::TextAccent,
                Some(&live.tag),
                "",
                Some(running_dot()),
                false,
                live_stream_body(items),
            )
        })
        .collect();
    el(El::Div)
        .id(&format!("agent-live-{}", thread.agent_id))
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append(entries)
}

/// One conversational turn, on the shared [`thread::entry`] vocabulary: author + phase tag +
/// time + verdict chip over the prose, any reviewer feedback, and the muted tool/task meta line.
/// Your turns carry the accent rail (like the chat); agent turns render as Markdown replies.
fn turn_row(turn: &AgentTurn) -> ElementBuilder {
    let yours = turn.author == "you";
    let author_color = if yours { St::TextHigh } else { St::TextAccent };
    let body = if yours {
        // Plain text at the agent-reply size (matching the chat's user turns).
        el(El::Div)
            .style(Style::new().set("font-size", "0.82rem"))
            .st([St::TextDefault, St::WhitespacePreWrap])
            .append([el(El::Span).text(&turn.body)])
    } else {
        crate::ui::widgets::markdown(turn.body.clone())
    };
    let mut column = vec![body];
    if let Some(feedback) = turn.feedback.as_deref() {
        column.push(
            Text::caption(format!("feedback: {feedback}"))
                .accent()
                .build(),
        );
    }
    if turn.tool_count > 0 || turn.link_task {
        column.push(meta_line(turn));
    }
    thread::entry(
        &turn.author,
        author_color,
        Some(&turn.tag),
        &turn.time,
        Some(verdict_chip_of(turn.verdict.as_deref(), turn.ok)),
        yours,
        column,
    )
}

/// The muted meta line under a turn: the tool-work count and the task link into the board's
/// Peek — the one place forensics live, so the conversation stays prose.
fn meta_line(turn: &AgentTurn) -> ElementBuilder {
    let mut parts: Vec<ElementBuilder> = Vec::new();
    if turn.tool_count > 0 {
        let plural = if turn.tool_count == 1 { "" } else { "s" };
        parts.push(el(El::Span).text(&format!("{} tool call{plural} ·", turn.tool_count)));
    }
    parts.push(
        Link::route(&format!(
            "/w/{}/tasks/{}",
            turn.task.workspace_id, turn.task.id
        ))
        .st([
            St::MinW0,
            St::Truncate,
            St::TextMuted,
            St::NoUnderline,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .text(&format!("{} ↗", turn.task.title)),
    );
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::TextXs,
            St::TextMuted,
        ])
        .append(parts)
}

/// A parked task's inline review composer, set apart in a labelled surface so it reads as an
/// action the operator needs to take — approval home is the worker's own thread.
fn review_prompt(task_id: &str) -> ElementBuilder {
    el(El::Div)
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
                    St::TextXs,
                    St::TextUppercase,
                    St::TrackingWide,
                    St::TextAccent,
                ])
                .text("Needs your review"),
            review_gate(task_id),
        ])
}
