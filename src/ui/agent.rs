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
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::{AgentThread, AgentThreadItem, AgentTurn, App, LiveItem};
use crate::ui::memory;
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
                    .st([
                        St::MaxW60rem,
                        St::MxAuto,
                        St::WFull,
                        St::DisplayFlex,
                        St::FlexCol,
                        St::GapLg,
                        St::PMd,
                    ])
                    .md([St::PLg])
                    .append([timeline(), live_tail()]),
            ),
        ])
}

/// The pinned page header: the agent's name + its workspace, centered on the reading measure.
/// The border spans the page; the content aligns with the conversation column below.
fn header_bar() -> ElementBuilder {
    el(El::Div)
        .st([St::FlexShrink0, St::PMd, St::BorderBDefault])
        .md([St::PxLg])
        .append([el(El::Div)
            .st([
                St::MaxW60rem,
                St::MxAuto,
                St::WFull,
                St::DisplayFlex,
                St::ItemsCenter,
                St::GapSm,
            ])
            .append([agent_title()])])
}

/// The agent's name + workspace in the header — its own small region so the static scaffold
/// never re-renders. "Forge · Studio": the thread is (agent, workspace)-scoped.
#[renderer]
fn agent_title(app: &App) -> ElementBuilder {
    let (name, workspace) = app.agent_thread.as_ref().map_or(("…", ""), |thread| {
        (thread.agent_name.as_str(), thread.workspace_name.as_str())
    });
    let mut parts = vec![
        el(El::Span)
            .st([St::TextLg, St::TextHigh, St::FontSemibold])
            .text(name),
    ];
    if !workspace.is_empty() {
        parts.push(
            el(El::Span)
                .st([St::TextSm, St::TextMuted])
                .text(&format!("· {workspace}")),
        );
    }
    el(El::Span)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append(parts)
}

/// The conversation history: the windowed, chronological turns + any review gates at the tail.
/// Reads **only** `agent_thread` — a streamed token never repaints this region. Keyed by agent id
/// so switching agents replaces the subtree instead of morphing one thread into another.
#[renderer]
fn timeline(app: &App) -> ElementBuilder {
    let thread = app.agent_thread.as_ref();
    // The key carries BOTH ids: the same agent under two workspaces is two threads, and the
    // morph must replace the subtree when either changes.
    let key = thread.map_or_else(
        || "loading".to_owned(),
        |t| format!("{}-{}", t.agent_id, t.workspace_id),
    );
    let mut rows: Vec<ElementBuilder> = Vec::new();
    match thread {
        None => rows.push(Text::body("Loading…").muted().build()),
        Some(thread) => {
            if let Some(line) = fold_line(thread) {
                rows.push(line);
            }
            if !thread.memory.trim().is_empty() || thread.memory_paused {
                rows.push(memory_card(thread));
            }
            for item in &thread.items {
                rows.push(match item {
                    AgentThreadItem::Turn(turn) => turn_row(turn),
                    AgentThreadItem::ReviewGate { task_id } => review_prompt(task_id),
                    AgentThreadItem::Memory { time, note } => memory::event_line(time, note),
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
        .id(&format!(
            "agent-live-{}-{}",
            thread.agent_id, thread.workspace_id
        ))
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append(entries)
}

/// The fold line at the top of the thread — where history passes into memory (R3): what's
/// past it isn't gone, it's condensed into the working memory shown just below. Falls back to
/// the plain truncation note when this scope has no memory yet.
fn fold_line(thread: &AgentThread) -> Option<ElementBuilder> {
    let has_memory = !thread.memory.trim().is_empty();
    let text = match (has_memory, thread.truncated) {
        (true, 0) => "· older work condensed into memory ·".to_owned(),
        (true, n) => {
            format!("· older work condensed into memory — {n} earlier turns past the fold ·")
        }
        (false, 0) => return None,
        (false, n) => format!("· {n} earlier turns not shown ·"),
    };
    Some(memory::muted_line(&text))
}

/// The page's memory card (shared widget), targeting the thread's (agent, workspace) row.
fn memory_card(thread: &AgentThread) -> ElementBuilder {
    let mut meta = format!("what {} remembers here", thread.agent_name);
    if thread.memory_paused {
        meta.push_str(" · ⏸ auto-fold paused");
    }
    memory::memory_card(memory::MemoryCard {
        id_suffix: format!("{}-{}", thread.agent_id, thread.workspace_id),
        meta,
        summary: &thread.memory,
        paused: thread.memory_paused,
        on_save: save_memory(),
        on_clear: clear_memory(),
        on_pause: toggle_memory_pause(),
    })
}

/// The open thread's (agent, workspace) pair — every memory mutation targets that scope row.
fn open_scope(app: &App) -> Option<(String, String)> {
    app.agent_thread
        .as_ref()
        .map(|thread| (thread.agent_id.clone(), thread.workspace_id.clone()))
}

#[handler]
fn save_memory(app: &mut App, ctx: &EventContext) {
    let Some((agent_id, workspace)) = open_scope(app) else {
        return;
    };
    let summary = ctx.field("summary").unwrap_or_default().to_owned();
    bridge::send(Command::SetMemory {
        agent_id,
        workspace: Some(workspace),
        summary,
    });
}

#[handler]
fn clear_memory(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    let Some((agent_id, workspace)) = open_scope(app) else {
        return;
    };
    bridge::send(Command::ClearMemory {
        agent_id,
        workspace: Some(workspace),
    });
}

#[handler]
fn toggle_memory_pause(app: &mut App, ctx: &EventContext) {
    let Some((agent_id, workspace)) = open_scope(app) else {
        return;
    };
    let paused = ctx.data("paused") == Some("true");
    bridge::send(Command::SetMemoryPaused {
        agent_id,
        workspace: Some(workspace),
        paused,
    });
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
