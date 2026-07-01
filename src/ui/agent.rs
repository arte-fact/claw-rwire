//! The agent thread page (`/agent/:id`): an agent's whole working life as one autoscrolling,
//! chat-like timeline — its tasks (each a section marker deep-linking to its board) with their
//! runs, inline review composers for tasks parked at a gate, and the live run streaming at the
//! tail. Built on the shared [`thread`] widgets and the same `run_entry` the Peek modal uses, so a
//! run reads identically here. Renders from `App.agent_thread` + `App.live_turns`.
//!
//! One renderer, keyed by agent id — the same shape the chat thread uses (a keyed subtree of plain
//! rows), so switching agents replaces the timeline cleanly and streaming re-renders in place.

use rwire::router::Link;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use crate::state::{AgentThreadItem, App, LiveItem};
use crate::ui::task_detail::{live_stream_body, review_gate, run_entry, running_dot};
use crate::ui::thread;

/// The page shell: a pinned agent header over the autoscrolling timeline.
pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([agent_view()])
}

/// The agent header + timeline. A single synced region (like the chat transcript): it reads the
/// projected `agent_thread` and the live `live_turns`, so a lifecycle change or a streamed token
/// re-renders the keyed subtree in place. The key (agent id) makes switching agents swap cleanly.
#[renderer]
fn agent_view(app: &App) -> ElementBuilder {
    // Always the same shape — header over a keyed, scrolling column — so the morph never has to
    // reconcile two different structures (the loading/empty states are just a row in the column).
    let thread = app.agent_thread.as_ref();
    let name = thread.map_or("", |t| t.agent_name.as_str());
    let key = thread.map_or("loading", |t| t.agent_id.as_str());
    let mut entries: Vec<ElementBuilder> = Vec::new();
    match thread {
        None => entries.push(Text::body("Loading…").muted().build()),
        Some(thread) => {
            for (index, item) in thread.items.iter().enumerate() {
                entries.push(match item {
                    AgentThreadItem::Task {
                        id,
                        workspace_id,
                        title,
                        status,
                    } => task_marker(id, workspace_id, title, status),
                    AgentThreadItem::Run(run) => {
                        run_entry(run, &format!("a{index}"), &app.tool_toggled)
                    }
                    AgentThreadItem::ReviewGate { task_id } => review_prompt(task_id),
                });
            }
            // The live run(s) stream at the tail — plain entries (no nested renderer), so the keyed
            // subtree stays a flat list of rows the morph can swap positionally, like chat.
            for live in &thread.live {
                let items: &[LiveItem] = app
                    .live_turns
                    .iter()
                    .find(|turn| turn.task_id == live.task_id)
                    .map(|turn| turn.items.as_slice())
                    .unwrap_or(&[]);
                entries.push(live_entry_row(&thread.agent_name, &live.tag, items));
            }
            if entries.is_empty() {
                entries.push(
                    el(El::Div)
                        .st([St::TextSm, St::TextMuted])
                        .text("No activity yet — this agent hasn't run any tasks."),
                );
            }
        }
    }
    // Key the timeline by agent id: switching agents yields a new id, so the DOM morph replaces the
    // whole subtree instead of positionally reusing the previous agent's nodes. Same pattern the
    // chat thread uses for switching conversations.
    let column = el(El::Div)
        .id(&format!("agent-thread-{key}"))
        .st([St::WFull, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PLg])
        .append(entries);
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([agent_header(name), thread::scroll(column)])
}

/// The pinned page header: the agent's name.
fn agent_header(name: &str) -> ElementBuilder {
    el(El::Div)
        .st([
            St::FlexShrink0,
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PMd,
            St::BorderBDefault,
        ])
        .md([St::PxLg])
        .append([el(El::Span)
            .st([St::Flex1, St::TextLg, St::TextHigh, St::FontSemibold])
            .text(name)])
}

/// One task section marker: a linked title (→ its board Peek) with a status chip — the header the
/// task's runs hang under in the timeline.
fn task_marker(id: &str, workspace_id: &str, title: &str, status: &str) -> ElementBuilder {
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PbXs,
            St::BorderBSubtle,
        ])
        .append([
            Link::route(&format!("/w/{workspace_id}/tasks/{id}"))
                .st([
                    St::Flex1,
                    St::TextSm,
                    St::TextHigh,
                    St::FontSemibold,
                    St::NoUnderline,
                    St::CursorPointer,
                ])
                .hover([St::TextAccent])
                .text(title),
            status_chip(status),
        ])
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

/// A small uppercase status chip for a task marker, coloured by lifecycle.
fn status_chip(status: &str) -> ElementBuilder {
    let color = match status {
        "running" | "done" => St::TextAccent,
        "failed" | "rejected" => St::TextLow,
        "awaiting" => St::TextHigh,
        _ => St::TextMuted,
    };
    el(El::Span)
        .st([St::TextXs, St::TextUppercase, color])
        .text(status)
}

/// One running task's live entry: the agent + current step, a running dot, and its interleaved
/// text/tool stream (the typewriter). A plain builder — the enclosing `agent_view` re-renders it
/// as tokens arrive, exactly as the chat transcript re-renders while a reply streams.
fn live_entry_row(agent_name: &str, tag: &str, items: &[LiveItem]) -> ElementBuilder {
    thread::entry(
        agent_name,
        St::TextAccent,
        Some(tag),
        "",
        Some(running_dot()),
        false,
        live_stream_body(items),
    )
}
