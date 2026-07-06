//! The task **page** (navigation redesign N1): `/w/:wid/tasks/:tid` — the canonical home of
//! one task, replacing the Peek modal. A breadcrumb (workspace › request › task) over the
//! task's **thread**: a chat log built on the shared [`thread`] widgets — the originating
//! spec, each run as an authored, phase-tagged entry with collapsible tool/diff rows, the
//! live run streaming in — above a pinned gate bar (review composer when parked, else run
//! controls). Board cards and deep links route here; renders from `App.task_detail`.

use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::{
    Avatar, AvatarSize, Breadcrumb, Button, ButtonSize, Input, StatusDot, StatusDotIntent, Text,
};

use super::thread;
use crate::bridge::{self, Command};
use crate::state::{App, LiveItem, ProposedTask, TaskDetail};

/// The task page, filling the workspace shell's content area. The thread supplies its own
/// scroll; the page never scrolls as a whole, so the breadcrumb header and the gate bar stay
/// in view (the same pinned-chrome shape the modal had).
pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([body()])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(detail) = &app.task_detail else {
        return Text::body("Loading the task…").muted().build();
    };
    el(El::Div)
        .st([
            St::Flex1,
            St::MinH0,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append([header(app, detail), page_thread(app, detail)])
}

/// The page's first line: breadcrumb (workspace › request › task) with the status badge on
/// the tail, then the assignee + step meta — place and state before the thread.
fn header(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let workspace = app
        .workspaces
        .iter()
        .find(|w| w.id == detail.workspace_id)
        .map(|w| w.name.clone())
        .unwrap_or_else(|| detail.workspace_id.clone());
    let mut crumbs = Breadcrumb::new().item(workspace, Some(format!("/w/{}", detail.workspace_id)));
    if !detail.request_id.is_empty() {
        crumbs = crumbs.item(
            crate::text::first_line_capped(&detail.request_title, 40),
            Some(format!(
                "/w/{}/requests/{}",
                detail.workspace_id, detail.request_id
            )),
        );
    }
    let crumbs = crumbs.item(detail.title.clone(), None::<String>).build();
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append([crumbs, status_badge(&detail.status), meta(app, detail)])
}

/// Assignee avatar + the mockup's context chips: step, assignee, attempt · iteration —
/// facts, not filters. The header's quiet tail.
fn meta(app: &App, detail: &TaskDetail) -> ElementBuilder {
    use rwire_components::Chip;
    let task = app.tasks.iter().find(|t| t.id == detail.id);
    let assignee = task.and_then(|t| t.agent.as_deref()).unwrap_or("owner");
    let mut row = vec![Avatar::new().name(assignee).size(AvatarSize::Sm).build()];
    if let Some(task) = task {
        if let Some(step) = app.pipeline_columns.get(task.step_position.max(0) as usize) {
            row.push(Chip::new(step.clone()).build());
        }
        row.push(
            Chip::new(format!(
                "attempt {} · iter {}",
                task.attempt + 1,
                task.iteration + 1
            ))
            .build(),
        );
    }
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::MlAuto,
            St::FlexWrap,
        ])
        .append(row)
}

/// The task lifecycle as a badge — the same square/outline vocabulary as the request page.
fn status_badge(status: &str) -> ElementBuilder {
    use rwire_components::{Badge, BadgeFill, BadgeIntent, BadgeShape};
    let intent = match status {
        "running" => BadgeIntent::Primary,
        "done" => BadgeIntent::Success,
        "failed" | "rejected" | "cancelled" => BadgeIntent::Error,
        "awaiting" => BadgeIntent::Warning,
        _ => BadgeIntent::Default,
    };
    Badge::new()
        .intent(intent)
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .text(status.to_owned())
        .build()
}

/// The task as a chat log. An autoscrolling column of authored, phase-tagged entries —
/// the originating spec, a plan's proposal, each persisted run, and (while running) the live run
/// streaming in — above a pinned gate bar (review composer when parked, else run controls).
fn page_thread(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let column = el(El::Div)
        .st([St::WFull, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PLg])
        .append(thread_entries(detail));
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::DisplayFlex, St::FlexCol])
        .append([thread::scroll(column), gate_bar(app, detail)])
}

/// The ordered thread entries: the spec (as the originating request), a plan's proposed tasks,
/// each run as an authored entry, then the live run when one is in flight.
fn thread_entries(detail: &TaskDetail) -> Vec<ElementBuilder> {
    // The spec lives in its own panel above (the mockup's SPEC box) — the thread starts
    // with the work: a plan's proposal, then the runs.
    let mut entries: Vec<ElementBuilder> = Vec::new();
    if detail.is_plan {
        entries.push(thread::entry(
            "claw",
            St::TextAccent,
            Some("Plan"),
            "",
            None,
            false,
            vec![proposed_view(&detail.proposed, detail.awaiting)],
        ));
    }
    for (index, run) in detail.runs.iter().enumerate() {
        entries.push(run_entry(run, &format!("a{index}")));
    }
    // The live run is its own `#[renderer]`, so a token flush repaints only this entry — not the
    // whole board/modal. It reads the live stream itself and self-hides when nothing is streaming,
    // so it's included unconditionally (the board never subscribes to `live_turns`).
    entries.push(live_entry());
    entries
}

/// One persisted run as an authored, phase-tagged thread entry: author (agent) + role tag +
/// timestamp + verdict chip, over its collapsible tool rows, Markdown reply and any feedback.
/// Shared by the Peek modal's Thread and the agent thread (`ui::agent`), so a run reads the same.
pub(crate) fn run_entry(run: &crate::state::RunView, key: &str) -> ElementBuilder {
    let mut body: Vec<ElementBuilder> = Vec::new();
    if !run.memory.trim().is_empty() {
        // What the run remembered at launch (R3.5 M4) — collapsed, above the tool forensics.
        body.push(crate::ui::run_chain::memory_row(
            &run.memory,
            &format!("{key}.mem"),
        ));
    }
    if !run.diff.trim().is_empty() {
        // What the pass actually changed on disk (G2) — collapsed, next to the forensics.
        body.push(crate::ui::run_chain::diff_row(
            &run.diff,
            &run.diff_files,
            &format!("{key}.diff"),
        ));
    }
    if !run.checks.is_empty() {
        // The machine verdict on this pass (P1b) — a badge strip, red tails behind a disclosure.
        body.push(crate::ui::run_chain::checks_row(
            &run.checks,
            &format!("{key}.checks"),
        ));
    }
    body.extend(
        run.tools
            .iter()
            .enumerate()
            .map(|(index, tool)| crate::ui::run_chain::tool_row(tool, &format!("{key}.{index}"))),
    );
    if run.output.trim().is_empty() {
        body.push(Text::caption("(no output)").muted().build());
    } else {
        body.push(crate::ui::widgets::markdown(run.output.clone()));
    }
    if let Some(feedback) = run.feedback.as_deref().filter(|f| !f.trim().is_empty()) {
        body.push(
            Text::caption(format!("feedback: {feedback}"))
                .accent()
                .build(),
        );
    }
    let author_color = if run.agent == "you" {
        St::TextHigh
    } else {
        St::TextAccent
    };
    thread::entry(
        &run.agent,
        author_color,
        Some(&crate::ui::run_chain::role_tag(run)),
        &run.time,
        Some(crate::ui::run_chain::verdict_chip(run)),
        false,
        body,
    )
}

/// The live run as the newest thread entry: the agent + current step, a `● running` marker, its
/// in-progress tool events, and the reply as it streams in (the typewriter `▌`).
#[renderer]
fn live_entry(app: &App) -> ElementBuilder {
    // Self-contained synced region: only this reads `live_turns`, so a token flush repaints just
    // the live line. Show nothing unless the page's task is running.
    let hidden = || el(El::Div).st([St::DisplayNone]);
    let Some(detail) = app.task_detail.as_ref() else {
        return hidden();
    };
    if detail.status != "running" {
        return hidden();
    }
    let items: &[LiveItem] = app
        .live_turns
        .iter()
        .find(|turn| turn.task_id == detail.id)
        .map(|turn| turn.items.as_slice())
        .unwrap_or(&[]);
    let task = app.tasks.iter().find(|t| t.id == detail.id);
    let author = task.and_then(|t| t.agent.as_deref()).unwrap_or("owner");
    let mut tag = task
        .and_then(|t| app.pipeline_columns.get(t.step_position.max(0) as usize))
        .cloned()
        .unwrap_or_else(|| "Running".to_owned());
    if let Some(task) = task.filter(|t| t.iteration > 0) {
        tag.push_str(&format!(" · iter {}", task.iteration + 1));
    }
    thread::entry(
        author,
        St::TextAccent,
        Some(&tag),
        "",
        Some(running_dot()),
        false,
        live_stream_body(items),
    )
}

/// The live stream's body elements in arrival order — a "working…" placeholder while empty, then
/// each text segment (the typewriter cursor riding only the final one) and tool row. Shared by the
/// Peek live entry and the agent thread's live entry (`ui::agent`).
pub(crate) fn live_stream_body(items: &[LiveItem]) -> Vec<ElementBuilder> {
    let mut body: Vec<ElementBuilder> = Vec::new();
    if items.is_empty() {
        body.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text("The agent is working…"),
        );
    }
    let last = items.len().saturating_sub(1);
    for (index, item) in items.iter().enumerate() {
        body.push(match item {
            LiveItem::Text(text) => streaming_text(text, index == last),
            LiveItem::Tool {
                time,
                action,
                in_progress,
                ok,
            } => live_event_row(time, action, *in_progress, *ok),
        });
    }
    body
}

/// A small `● running` marker for a live entry's header. Shared with the agent thread.
pub(crate) fn running_dot() -> ElementBuilder {
    StatusDot::new()
        .intent(StatusDotIntent::Primary)
        .label("running")
        .build()
}

/// One live tool event: an in-progress / ok / failed mark, its time, and the humanized action.
fn live_event_row(time: &str, action: &str, in_progress: bool, ok: bool) -> ElementBuilder {
    let (mark, color) = if in_progress {
        ("▷", St::TextMuted)
    } else if ok {
        ("✓", St::TextAccent)
    } else {
        ("✗", St::TextLow)
    };
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
        .append([
            el(El::Span).st([color]).text(mark),
            el(El::Span).st([St::TextMuted]).text(time),
            el(El::Span).st([St::TextHigh]).text(action),
        ])
}

/// A streaming reply segment. The trailing typewriter cursor is drawn only on the segment that is
/// currently growing (the last item in the turn), so earlier segments read as settled text.
fn streaming_text(text: &str, cursor: bool) -> ElementBuilder {
    let body = if cursor {
        format!("{text}▌")
    } else {
        text.to_owned()
    };
    el(El::Div)
        .st([
            St::WhitespacePreWrap,
            St::BreakWords,
            St::TextXs,
            St::TextHigh,
            St::PtXs,
        ])
        .text(&body)
}

/// A plan's proposed tasks as a small list of cards (title · assignee · schedule · spec) — the
/// work the user approves when they approve the plan gate. An empty list means the owner is
/// (re-)drafting — after "Request changes" the old proposal is discarded and a planning turn
/// runs for a while — so say that instead of a bare "(no tasks proposed)" beside live buttons.
fn proposed_view(proposed: &[ProposedTask], awaiting: bool) -> ElementBuilder {
    if proposed.is_empty() {
        // Mid-replan the list is genuinely empty (say so); on a finished plan the proposal was
        // consumed by materialisation — the created tasks are on the board.
        let note = if awaiting {
            "Planning in progress — the owner is drafting the proposal; it will appear here."
        } else {
            "The proposal was approved — its tasks are on the board."
        };
        return el(El::Div)
            .st([St::TextXs, St::TextMuted, St::Italic, St::PSm])
            .text(note);
    }
    let cards: Vec<ElementBuilder> = proposed
        .iter()
        .map(|p| {
            let mut meta = p.agent.clone().unwrap_or_else(|| "owner".to_owned());
            if let Some(schedule) = &p.schedule {
                meta.push_str(&format!(" · ⟳ {schedule}"));
            }
            el(El::Div)
                .st([
                    St::BgApp,
                    St::BorderDefault,
                    St::PSm,
                    St::DisplayFlex,
                    St::FlexCol,
                    St::GapXs,
                ])
                .append([
                    el(El::Div)
                        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                        .append([
                            el(El::Span)
                                .st([St::Flex1, St::TextSm, St::TextHigh, St::FontSemibold])
                                .text(&p.title),
                            el(El::Span).st([St::TextXs, St::TextLow]).text(&meta),
                        ]),
                    el(El::Div).st([St::TextXs, St::TextMuted]).text(&p.spec),
                ])
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

/// The status-appropriate run-control buttons: Stop while live, Start/Restart when stopped,
/// Archive/Unarchive when terminal. Shared by the Overview control bar and the Thread's gate bar.
fn control_buttons(detail: &TaskDetail) -> Vec<ElementBuilder> {
    let id = detail.id.as_str();
    match detail.status.as_str() {
        "running" | "pending" => vec![
            action_button("Stop", stop(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        "stopped" => vec![
            action_button("Start", start(), id, true),
            action_button("Restart", restart(), id, false),
            action_button("Cancel", cancel(), id, false),
        ],
        // Closed-but-reopenable (failed | rejected | cancelled): a transient failure (a timed-out
        // review, a flaky endpoint) must not dead-end the task — Restart begins a fresh attempt.
        // `done` stays final: archive to declutter the board (and restore it if archived).
        _ if detail.archived => vec![action_button("Unarchive", unarchive(), id, false)],
        "failed" | "rejected" | "cancelled" => vec![
            action_button("Restart", restart(), id, true),
            action_button("Archive", archive(), id, false),
        ],
        _ => vec![action_button("Archive", archive(), id, true)],
    }
}

/// The Thread's pinned bottom bar — the review composer when the task is parked at a human gate,
/// else the run controls. A `FlexShrink0` sibling below the scroll, like the chat composer.
fn gate_bar(app: &App, detail: &TaskDetail) -> ElementBuilder {
    let inner = if detail.awaiting && detail.is_plan && detail.proposed.is_empty() {
        // A plan parked before its proposal exists (or mid-replan): an Approve here would
        // be a silent no-op — say what's happening instead of offering a dead button.
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::Italic])
            .text("Still planning — the proposal will appear here for your review.")
    } else if detail.awaiting {
        review_gate(&detail.id)
    } else {
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(control_buttons(detail))
    };
    let mut children = vec![inner];
    // The docked steer composer (the mockup's bottom bar): while the task is live, a note
    // here queues for the assignee and rides its next pass boundary (R4) — on a phone the
    // task page reads as a chat with the agent, which is what it is.
    if matches!(detail.status.as_str(), "running" | "pending") {
        use rwire_components::Composer;
        let agent = app
            .tasks
            .iter()
            .find(|t| t.id == detail.id)
            .and_then(|t| t.agent.as_deref())
            .unwrap_or("the agent");
        children.push(
            Composer::new()
                .id(format!("task-steer-{}", detail.id))
                .placeholder(format!("Steer {agent} — delivered at its next pass…"))
                .compact(true)
                .on_submit(send_task_steer())
                .build()
                .st([St::WFull]),
        );
    }
    el(El::Div)
        .st([
            St::FlexShrink0,
            St::BorderT,
            St::PMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .md([St::PxLg])
        .append(children)
}

/// Queue a steer note for the task's assignee (falls back to the workspace owner at run
/// time, matching the scheduler's effective-assignee rule).
#[handler]
fn send_task_steer(app: &mut App, ctx: &EventContext) {
    let Some(detail) = app.task_detail.as_ref() else {
        return;
    };
    let body = ctx.field("message").unwrap_or_default().trim().to_owned();
    if body.is_empty() {
        return;
    }
    let agent = app
        .tasks
        .iter()
        .find(|t| t.id == detail.id)
        .and_then(|t| t.agent_id.clone())
        .or_else(|| {
            app.workspaces
                .iter()
                .find(|w| w.id == detail.workspace_id)
                .and_then(|w| w.owner.clone())
        });
    let Some(agent) = agent else {
        return;
    };
    bridge::send(Command::SteerAgent {
        agent,
        workspace: detail.workspace_id.clone(),
        body,
    });
}

/// The inline review composer for a parked task: Approve / Reject, then a "what to change…" field
/// that submits a change request in flux — the re-run then streams into the thread above it. Shared
/// with the agent thread (`ui::agent`), where a worker's parked task is reviewed in its own thread.
pub(crate) fn review_gate(task_id: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    action_button("Approve", approve(), task_id, true),
                    action_button("Reject", reject(), task_id, false),
                ]),
            el(El::Form)
                .on(Ev::Submit, request_changes())
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    crate::ui::widgets::hidden("task", task_id),
                    Input::text()
                        .name("feedback")
                        .placeholder("What to change…")
                        .autocomplete("off")
                        .build()
                        .st([St::Flex1]),
                    Button::outline("Request changes".to_owned())
                        .size(ButtonSize::Xs)
                        .build()
                        .at(At::Type, Av::Submit),
                ]),
        ])
}

fn action_button(
    label: &str,
    on_click: rwire::HandlerSpec,
    task: &str,
    accent: bool,
) -> ElementBuilder {
    let btn = if accent {
        Button::primary(label.to_owned())
    } else {
        Button::secondary(label.to_owned())
    };
    btn.size(ButtonSize::Sm)
        .on_click(on_click)
        .data("task", task)
}

#[handler]
fn stop(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::StopTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn start(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::StartTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn restart(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::RestartTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn archive(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ArchiveTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn unarchive(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::UnarchiveTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn cancel(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::CancelTask {
            id: task.to_owned(),
        });
    }
}

#[handler]
fn approve(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ReviewVerdict {
            task_id: task.to_owned(),
            decision: "pass".to_owned(),
            feedback: String::new(),
        });
    }
}

#[handler]
fn reject(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(task) = ctx.data("task") {
        bridge::send(Command::ReviewVerdict {
            task_id: task.to_owned(),
            decision: "reject".to_owned(),
            feedback: String::new(),
        });
    }
}

#[handler]
fn request_changes(app: &mut App, ctx: &EventContext) {
    let _ = app;
    let Some(task) = ctx
        .field("task")
        .map(str::to_owned)
        .filter(|t| !t.is_empty())
    else {
        return;
    };
    let feedback = ctx.field("feedback").unwrap_or_default().trim().to_owned();
    bridge::send(Command::ReviewVerdict {
        task_id: task,
        decision: "revise".to_owned(),
        feedback,
    });
}
