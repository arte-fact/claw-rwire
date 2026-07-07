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

use std::borrow::Cow;

use rwire::router::Link;
use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::{
    ChatAuthor, ChatItem, ChatItemCtx, ChatScroll, ChatTag, ChatTranscript, Composer, Text,
};

use crate::bridge::{self, Command};
use crate::state::{AgentThread, AgentThreadItem, AgentTurn, App, LiveItem};
use crate::ui::memory;
use crate::ui::run_chain::verdict_chip_of;
use crate::ui::task_detail::{live_stream_body, review_gate, stream_entry};

/// The page shell: a pinned header over the autoscrolling conversation — a static column whose
/// two renderer children (timeline, live tail) are siblings, never nested.
pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([
            header_bar(),
            ChatScroll::new(
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
                    // Two sibling synced regions: the windowed history (a ChatTranscript) and the
                    // live tail. Kept siblings — never the transcript's live_tail slot — because the
                    // timeline is its own keyed #[renderer]; nesting the live region inside it would
                    // put a renderer in a keyed subtree (a v1 failure mode). A token repaints only
                    // live_tail; a lifecycle change re-projects only the (tiny, windowed) timeline.
                    .append([timeline(), live_tail()]),
            )
            .build(),
            steer_bar(),
        ])
}

/// The steer composer (R4), pinned under the scroll like the chat's: leave the agent a note —
/// it queues now and delivers at the next pass boundary (or becomes a task if the agent is
/// idle here). A renderer keyed by the thread's item count so the field resets after a send.
#[renderer]
fn steer_bar(app: &App) -> ElementBuilder {
    let Some(thread) = app.agent_thread.as_ref() else {
        return el(El::Div).st([St::DisplayNone]);
    };
    let field_id = format!(
        "steer-{}-{}-{}",
        thread.agent_id,
        thread.workspace_id,
        thread.items.len()
    );
    el(El::Div)
        .style(Style::new().set("padding", "0.5rem 1rem 1rem"))
        .st([
            St::FlexShrink0,
            St::DisplayFlex,
            St::FlexCol,
            St::ItemsCenter,
        ])
        .append([Composer::new()
            .id(field_id)
            .placeholder(format!(
                "Message {} — delivered at its next pass…",
                thread.agent_name
            ))
            .compact(true)
            .on_submit(send_steer())
            .build()
            .st([St::MaxW60rem, St::WFull])])
}

#[handler]
fn send_steer(app: &mut App, ctx: &EventContext) {
    let Some(thread) = app.agent_thread.as_ref() else {
        return;
    };
    let body = ctx.field("message").unwrap_or_default().trim().to_owned();
    if body.is_empty() {
        return;
    }
    bridge::send(Command::SteerAgent {
        agent: thread.agent_id.clone(),
        workspace: thread.workspace_id.clone(),
        body,
    });
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
    let inner = match thread {
        None => Text::body("Loading…").muted().build(),
        Some(thread) => {
            let mut rows: Vec<Row> = Vec::new();
            if let Some(line) = fold_text(thread) {
                rows.push(Row::Fold(line));
            }
            // Stable per-item key: `truncated + index` is the turn's absolute position in the full
            // history, so growing the window on scroll-up doesn't re-key (and re-render) the turns
            // already on screen — the same discipline the concierge uses with its message rowid.
            for (index, item) in thread.items.iter().enumerate() {
                let key = format!("i{}", thread.truncated + index);
                rows.push(match item {
                    AgentThreadItem::Turn(turn) => Row::Turn { turn, key },
                    AgentThreadItem::ReviewGate { task_id } => Row::Gate { task_id, key },
                    AgentThreadItem::Memory { time, note } => Row::Memory { time, note, key },
                });
            }
            let mut transcript = ChatTranscript::new()
                .items_plain(rows.iter())
                .on_load_older(
                    expand_thread(),
                    thread.truncated as u32,
                    thread.truncated > 0,
                );
            // The empty state is the transcript's only when nothing streams either: the live tail is
            // a sibling region, so a bare "no activity" must not sit above a running turn.
            if thread.items.is_empty() && thread.live.is_empty() {
                transcript = transcript.empty_state(
                    Text::body("No activity yet — this agent hasn't worked on anything.")
                        .muted()
                        .build(),
                );
            }
            transcript.build()
        }
    };
    el(El::Div)
        .id(&format!("agent-thread-{key}"))
        .append([inner])
}

/// One agent-thread row adapted to rwire's `ChatItem`. A `Turn` carries the full entry chrome
/// (author · phase tag · time · verdict chip over its prose + feedback + meta line); the fold
/// marker, review gate, and memory event lines are chrome-free `row()` overrides — system lines,
/// not authored turns. `key` is the turn's absolute position, stable across window growth.
enum Row<'a> {
    Fold(String),
    Turn {
        turn: &'a AgentTurn,
        key: String,
    },
    Gate {
        task_id: &'a str,
        key: String,
    },
    Memory {
        time: &'a str,
        note: &'a str,
        key: String,
    },
}

impl ChatItem for Row<'_> {
    fn key(&self) -> Cow<'_, str> {
        match self {
            Row::Fold(_) => Cow::Borrowed("fold"),
            Row::Turn { key, .. } | Row::Gate { key, .. } | Row::Memory { key, .. } => {
                Cow::Borrowed(key)
            }
        }
    }

    fn author(&self) -> ChatAuthor {
        match self {
            Row::Turn { turn, .. } if turn.author == "you" => ChatAuthor::user("you"),
            Row::Turn { turn, .. } => ChatAuthor::agent(turn.author.clone()),
            // Fold/Gate/Memory render via `row()`; their author is never read.
            _ => ChatAuthor::agent("claw"),
        }
    }

    fn body(&self, _ctx: &ChatItemCtx<Self>) -> ElementBuilder {
        match self {
            Row::Turn { turn, .. } => turn_body(turn),
            _ => el(El::Span),
        }
    }

    fn time(&self) -> Option<Cow<'_, str>> {
        match self {
            Row::Turn { turn, .. } if !turn.time.is_empty() => Some(Cow::Borrowed(&turn.time)),
            _ => None,
        }
    }

    fn tag(&self) -> Option<ChatTag> {
        match self {
            Row::Turn { turn, .. } => Some(ChatTag::muted(turn.tag.clone())),
            _ => None,
        }
    }

    fn trailing(&self, _ctx: &ChatItemCtx<Self>) -> Option<ElementBuilder> {
        match self {
            Row::Turn { turn, .. } => Some(verdict_chip_of(turn.verdict.as_deref(), turn.ok)),
            _ => None,
        }
    }

    fn row(&self, _ctx: &ChatItemCtx<Self>) -> Option<ElementBuilder> {
        match self {
            Row::Fold(text) => Some(memory::muted_line(text)),
            Row::Gate { task_id, .. } => Some(review_prompt(task_id)),
            Row::Memory { time, note, .. } => Some(memory::event_line(time, note)),
            Row::Turn { .. } => None,
        }
    }
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
            stream_entry(&thread.agent_name, &live.tag, live_stream_body(items))
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

#[handler]
fn expand_thread(app: &mut App, ctx: &EventContext) {
    // Fired by the scroll sentinel; `ctx.item_index()` carries the truncated count at fire time. A
    // repeated/stale fire only grows the window past the total, which the projection caps — benign.
    let _ = (app, ctx);
    bridge::send(Command::ExpandAgentThread);
}

/// The fold line at the top of the thread — where history passes into memory (R3): what's
/// past it isn't gone, it's condensed into the working memory shown just below. Falls back to
/// the plain truncation note when this scope has no memory yet.
fn fold_text(thread: &AgentThread) -> Option<String> {
    let has_memory = !thread.memory.trim().is_empty();
    let text = match (has_memory, thread.truncated) {
        (true, 0) => "· older work condensed into memory ·".to_owned(),
        (true, n) => {
            format!("· older work condensed into memory — {n} earlier turns past the fold ·")
        }
        (false, 0) => return None,
        (false, n) => format!("· {n} earlier turns not shown ·"),
    };
    Some(text)
}

/// One conversational turn's body (the entry chrome — author/tag/time/verdict — is the `ChatItem`'s):
/// the prose, any reviewer feedback, and the muted tool/task meta line. Your turns render as plain
/// text at the reply size; agent turns render as Markdown.
fn turn_body(turn: &AgentTurn) -> ElementBuilder {
    let yours = turn.author == "you";
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
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::MinW0])
        .append(column)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>() -> ChatItemCtx<Row<'a>> {
        ChatItemCtx { item_ref: None }
    }

    /// The fold marker, review gate, and memory lines are system rows — they bypass the authored-
    /// entry chrome via `row()`. (The "no button" seamless-history contract now lives in rwire's
    /// `ChatTranscript::on_load_older`, tested there.)
    #[test]
    fn system_rows_render_chrome_free_via_row() {
        assert!(Row::Fold("· folded ·".to_owned()).row(&ctx()).is_some());
        assert!(
            Row::Gate {
                task_id: "t1",
                key: "i0".to_owned(),
            }
            .row(&ctx())
            .is_some()
        );
        assert!(
            Row::Memory {
                time: "12:00",
                note: "noted",
                key: "i1".to_owned(),
            }
            .row(&ctx())
            .is_some()
        );
    }

    /// Items key by absolute position (`i{truncated+index}`), so a turn keeps its key as older pages
    /// load on scroll-up — the transcript morph reuses it instead of re-rendering the whole thread.
    #[test]
    fn item_keys_are_stable_absolute_positions() {
        assert_eq!(Row::Fold("x".to_owned()).key(), "fold");
        assert_eq!(
            Row::Gate {
                task_id: "t",
                key: "i5".to_owned(),
            }
            .key(),
            "i5"
        );
    }
}
