//! The agent thread page (`/agent/:id`, Phase 1 — read-only): an agent's whole working life as
//! one autoscrolling timeline. Built on the shared [`thread`] widgets and the same `run_entry` the
//! Peek modal uses, so a run reads identically here. Each of the agent's tasks is a section marker
//! (deep-linking to its board) followed by that task's runs; a running task streams live at the
//! tail via the isolated [`agent_live_entry`] renderer. Renders from the projected
//! `App.agent_thread` (see `bridge::agent_thread`).

use rwire::router::Link;
use rwire::{El, ElementBuilder, St, el, renderer};
use rwire_components::Text;

use crate::state::{AgentThreadItem, App, LiveItem};
use crate::ui::task_detail::{live_stream_body, run_entry, running_dot};
use crate::ui::thread;

/// The page shell: a pinned agent header over the autoscrolling timeline.
pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([agent_view()])
}

/// The agent header + timeline. Reads `agent_thread` (re-projected on task lifecycle events), not
/// `live_turns` — so a streaming token repaints only the nested [`agent_live_entry`], not the list.
#[renderer]
fn agent_view(app: &App) -> ElementBuilder {
    let Some(thread) = app.agent_thread.as_ref() else {
        return el(El::Div)
            .st([St::PMd, St::Flex1])
            .append([Text::body("Loading…").muted().build()]);
    };
    let mut entries: Vec<ElementBuilder> = Vec::new();
    for (index, item) in thread.items.iter().enumerate() {
        entries.push(match item {
            AgentThreadItem::Task {
                id,
                workspace_id,
                title,
                status,
            } => task_marker(id, workspace_id, title, status),
            AgentThreadItem::Run(run) => run_entry(run, &format!("a{index}"), &app.tool_toggled),
        });
    }
    if entries.is_empty() {
        entries.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted])
                .text("No activity yet — this agent hasn't run any tasks."),
        );
    }
    // The live run streams into its own renderer, appended unconditionally (it self-hides when the
    // agent has nothing running), so a token flush never repaints the timeline above.
    entries.push(agent_live_entry());
    let column = el(El::Div)
        .st([St::WFull, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PLg])
        .append(entries);
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([agent_header(&thread.agent_name), thread::scroll(column)])
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

/// The agent's running tasks streaming live, in their own synced region — so a token flush repaints
/// only this entry, not the whole timeline. Self-hides when the agent has nothing running.
#[renderer]
fn agent_live_entry(app: &App) -> ElementBuilder {
    let hidden = || el(El::Div).st([St::DisplayNone]);
    let Some(thread) = app.agent_thread.as_ref() else {
        return hidden();
    };
    if thread.live.is_empty() {
        return hidden();
    }
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
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append(entries)
}
