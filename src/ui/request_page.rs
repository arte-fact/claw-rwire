//! The request page (G2): `/w/:wid/requests/:rid` — one user ask's whole story in a scroll:
//! status + branch, the ask verbatim, the task timeline with per-task diff activity, the
//! repos it touched (branch point → merge), and the global diff against the branch point.

use rwire::{At, Av, El, ElementBuilder, Ev, Link, St, el, handler, renderer};
use rwire_components::{
    Badge, BadgeFill, BadgeIntent, BadgeShape, Breadcrumb, Button, ButtonSize, Input, StatusDot,
    StatusDotIntent, Text,
};

use crate::bridge::{self, Command};
use crate::diff::DiffKind;
use crate::state::{App, RequestDetail};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::OverflowYAuto, St::PMd])
        .append([body()])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(detail) = &app.request_detail else {
        return Text::body("Loading the request…").muted().build();
    };
    // Two columns on desktop (N3): the story on the left (contract → verdict → timeline),
    // the git reality on the right (branch, repos, history); phones stack them. The full
    // diff keeps the page's bottom at full width, one disclosure per file.
    // The owner's planning turn (P4d) streams live at the top of the story while it's in flight,
    // then collapses — retiring the opaque "planning…" wait on the request that launched it.
    let mut story = vec![live_planning_entry(), ask_block(detail)];
    if !detail.checks.is_empty() {
        // The machine verdict on the request's latest passes (P1b) — green reassures, red
        // names the failing checks the macro gate blocks auto-delivery on.
        let color = if detail.checks_ok == Some(false) {
            St::TextError
        } else {
            St::TextSuccess
        };
        story.push(section(
            "Checks",
            el(El::Div)
                .st([St::TextSm, color, St::WhitespacePreWrap, St::BreakWords])
                .text(&detail.checks),
        ));
    }
    if !detail.validation.is_empty() {
        story.push(section(
            "Validation",
            el(El::Div)
                .st([
                    St::TextSm,
                    St::TextDefault,
                    St::WhitespacePreWrap,
                    St::BreakWords,
                ])
                .text(&detail.validation),
        ));
    }
    // A parked (`validating`) request can go back to the agents to revise instead of only
    // Deliver/Abandon — the third gate move, placed right under the verdict the owner just read.
    if detail.status == "validating" {
        story.push(section(
            "Request changes",
            request_changes_form(detail.id.as_str()),
        ));
    }
    if !detail.tasks.is_empty() {
        story.push(section("Tasks", timeline(detail)));
    }
    let mut side = vec![branch_panel(detail)];
    if !detail.repos.is_empty() {
        side.push(section("Repos", repos(detail)));
    }
    side.push(section("History", history(detail)));
    let columns = el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .md([St::FlexRow, St::ItemsStart])
        .append([
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::FlexCol,
                    St::GapLg,
                    St::MinW0,
                    St::Flex1,
                ])
                .append(story),
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::FlexCol,
                    St::GapLg,
                    St::MinW0,
                    St::Flex1,
                    St::MaxWMd,
                ])
                .append(side),
        ]);
    let mut sections = vec![header(app, detail), columns];
    if !detail.diff_stat.is_empty() {
        sections.push(section("Diff", global_diff(detail)));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg, St::MaxW80rem])
        .append(sections)
}

/// The owner's planning turn streaming live on the request page (P4d): while the plan launched for
/// this request is in flight, its tool calls + reply stream in its own synced region (matched by
/// `request_id`), so the operator watches the planner work instead of a static "planning…" badge.
/// Hidden when no planning turn targets this request.
#[renderer]
fn live_planning_entry(app: &App) -> ElementBuilder {
    let hidden = || el(El::Div).st([St::DisplayNone]);
    let Some(request_id) = app.request_detail.as_ref().map(|detail| detail.id.as_str()) else {
        return hidden();
    };
    let Some(plan) = app
        .planning_live
        .iter()
        .find(|p| p.request_id == request_id)
    else {
        return hidden();
    };
    let items: &[crate::state::LiveItem] = app
        .live_turns
        .iter()
        .find(|turn| turn.task_id == plan.plan_id)
        .map(|turn| turn.items.as_slice())
        .unwrap_or(&[]);
    section(
        "Planning",
        crate::ui::task_detail::stream_entry(
            &plan.agent,
            "Plan",
            crate::ui::task_detail::live_stream_body(items),
        ),
    )
}

/// The page's first line, per the navigation redesign (N0): breadcrumb trail
/// (workspace › request), the status badge riding the crumb tail, the branch, and the
/// status-appropriate controls right-aligned — place, state, and verbs on one line.
fn header(app: &App, detail: &RequestDetail) -> ElementBuilder {
    let workspace = app
        .workspaces
        .iter()
        .find(|w| w.id == detail.workspace_id)
        .map(|w| w.name.clone())
        .unwrap_or_else(|| detail.workspace_id.clone());
    let mut row = vec![
        Breadcrumb::new()
            .item(workspace, Some(format!("/w/{}", detail.workspace_id)))
            .item(detail.title.clone(), None::<String>)
            .build(),
        status_badge(&detail.status),
    ];
    if !detail.branch.is_empty() {
        row.push(
            el(El::Span)
                .st([St::TextXs, St::TextMuted])
                .text(&detail.branch),
        );
    }
    if let Some(link) = preview_link(detail) {
        row.push(link);
    }
    row.push(controls(detail));
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append(row)
}

/// A live-preview link, shown only when a preview is running for this branch (P2c). It's a full
/// navigation (new tab) to the same-origin `/preview/<key>/` — a separate app served through the
/// proxy, not a claw route, so it can't be a client-routed `Link`.
fn preview_link(detail: &RequestDetail) -> Option<ElementBuilder> {
    if detail.preview_uri.is_empty() {
        return None;
    }
    Some(
        el(El::A)
            .at_str(At::Href, &detail.preview_uri)
            .at_str(At::Target, "_blank")
            .at_str(At::Rel, "noopener")
            .st([
                St::TextXs,
                St::TextAccent,
                St::NoUnderline,
                St::CursorPointer,
                St::DisplayFlex,
                St::ItemsCenter,
                St::GapXs,
            ])
            .hover([St::Underline])
            .append([
                StatusDot::new()
                    .intent(StatusDotIntent::Success)
                    .pulse(true)
                    .build(),
                el(El::Span).text("Preview this branch ↗"),
            ]),
    )
}

/// The user's words, verbatim — what the request is ultimately judged against.
fn ask_block(detail: &RequestDetail) -> ElementBuilder {
    section(
        "The ask",
        el(El::Div)
            .st([
                St::TextSm,
                St::TextDefault,
                St::WhitespacePreWrap,
                St::BreakWords,
                St::PSm,
                St::BgApp,
            ])
            .text(&detail.ask),
    )
}

/// The branch at a glance: name, the tasks-done hairline meter, and the diff totals.
fn branch_panel(detail: &RequestDetail) -> ElementBuilder {
    use rwire_components::{Progress, ProgressSize};
    let done = detail
        .tasks
        .iter()
        .filter(|task| task.status == "done")
        .count();
    let total = detail.tasks.len();
    let mut rows: Vec<ElementBuilder> = Vec::new();
    if !detail.branch.is_empty() {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextAccent, St::Truncate])
                .text(&detail.branch),
        );
    }
    if total > 0 {
        rows.push(
            Progress::new()
                .value(done as u32)
                .max(total as u32)
                .size(ProgressSize::Sm)
                .label(format!("{done}/{total} tasks done"))
                .build(),
        );
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text(&format!("{done}/{total} task(s) done")),
        );
    }
    section(
        "Branch",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapXs])
            .append(rows),
    )
}

/// The lifecycle at a glance — asked / current status, from the row's own stamps.
fn history(detail: &RequestDetail) -> ElementBuilder {
    let row = |time: &str, text: &str| {
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
            .append([
                el(El::Span).st([St::TextLow]).text(time),
                el(El::Span).st([St::TextMuted]).text(text),
            ])
    };
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            row(&detail.asked_at, "asked"),
            row(
                &detail.updated_at,
                &format!("{} — last change", detail.status),
            ),
        ])
}

fn timeline(detail: &RequestDetail) -> ElementBuilder {
    let rows: Vec<ElementBuilder> = detail
        .tasks
        .iter()
        .map(|task| {
            let label = if task.is_plan { "plan" } else { "task" };
            let mut parts = vec![
                el(El::Span).st([St::TextLow, St::TextXs]).text(label),
                Link::route(&format!("/w/{}/tasks/{}", detail.workspace_id, task.id))
                    .st([St::Flex1, St::MinW0, St::Truncate, St::TextSm])
                    .hover([St::TextAccent])
                    .text(&task.title),
            ];
            if !task.stat.is_empty() {
                parts.push(
                    el(El::Span)
                        .st([St::TextXs, St::TextMuted])
                        .text(&task.stat),
                );
            }
            parts.push(status_badge(&task.status));
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append(parts)
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(rows)
}

fn repos(detail: &RequestDetail) -> ElementBuilder {
    let rows: Vec<ElementBuilder> = detail
        .repos
        .iter()
        .map(|repo| {
            let merge = if repo.merge.is_empty() {
                "unmerged".to_owned()
            } else {
                format!("merged @ {}", repo.merge)
            };
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
                .append([
                    el(El::Span).st([St::TextDefault]).text(&repo.repo),
                    el(El::Span)
                        .st([St::TextMuted])
                        .text(&format!("from {}", repo.base)),
                    el(El::Span).st([St::TextMuted]).text(&merge),
                ])
        })
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append(rows)
}

/// The whole request against its branch point: the stat block, then the patch split into
/// one disclosure per file (N3) — closed by default, so a big request scans as a file list.
fn global_diff(detail: &RequestDetail) -> ElementBuilder {
    let mut children = vec![
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::WhitespacePreWrap])
            .text(&detail.diff_stat),
    ];
    for (index, chunk) in split_per_file(&detail.diff_patch).into_iter().enumerate() {
        let summary = chunk
            .lines()
            .next()
            .unwrap_or("")
            .rsplit(" b/")
            .next()
            .unwrap_or("")
            .to_owned();
        let key = format!("reqdiff.{index}");
        let rows: Vec<ElementBuilder> = crate::diff::patch_lines(&chunk)
            .into_iter()
            .map(|line| {
                let color = match line.kind {
                    DiffKind::Added => St::TextSuccess,
                    DiffKind::Removed => St::TextError,
                    DiffKind::Equal => St::TextMuted,
                };
                el(El::Div)
                    .st([St::WhitespacePreWrap, St::BreakWords, St::TextXs, color])
                    .text(&line.text)
            })
            .collect();
        let body = el(El::Div)
            .st([St::PSm, St::BgApp, St::OverflowXAuto])
            .append(rows);
        children.push(crate::ui::run_chain::disclosure_row(
            &key, "file", &summary, None, body, false,
        ));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(children)
}

/// Split a multi-file patch on its `diff --git` boundaries (repo headers ride along with
/// the file that follows them).
fn split_per_file(patch: &str) -> Vec<String> {
    if patch.trim().is_empty() {
        return Vec::new();
    }
    let mut chunks: Vec<String> = Vec::new();
    for line in patch.lines() {
        if line.starts_with("diff --git ") || chunks.is_empty() {
            chunks.push(String::new());
        }
        let chunk = chunks.last_mut().expect("chunk");
        chunk.push_str(line);
        chunk.push('\n');
    }
    chunks
}

/// The status-appropriate explicit acts (G3). Delivery/rollback stay reversible-by-design
/// (a merge can be reverted; an abandon drops only an unmerged branch); Push is the one
/// outward act and never happens un-clicked.
fn controls(detail: &RequestDetail) -> ElementBuilder {
    let id = detail.id.as_str();
    let mut buttons: Vec<ElementBuilder> = Vec::new();
    match detail.status.as_str() {
        "validating" => {
            // Red machine checks (P1b): auto-delivery is held, and the button says so — a
            // Deliver here is an explicit human override of the red gate.
            let deliver_label = if detail.checks_ok == Some(false) {
                "Deliver anyway"
            } else {
                "Deliver"
            };
            buttons.push(control(Button::primary(deliver_label), deliver(), id));
            buttons.push(control(Button::secondary("Abandon"), abandon(), id));
        }
        "planning" | "executing" => {
            buttons.push(control(Button::secondary("Abandon"), abandon(), id));
        }
        "delivered" => {
            buttons.push(control(Button::secondary("Roll back"), rollback(), id));
            if detail.has_remote {
                buttons.push(control(Button::primary("Push"), push(), id));
            }
        }
        "rolled-back" if detail.has_remote => {
            buttons.push(control(Button::primary("Push"), push(), id));
        }
        _ => {}
    }
    if buttons.is_empty() {
        return el(El::Div).st([St::DisplayNone]);
    }
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::MlAuto])
        .append(buttons)
}

fn control(button: Button, on_click: rwire::HandlerSpec, id: &str) -> ElementBuilder {
    button
        .size(ButtonSize::Sm)
        .on_click(on_click)
        .data("request", id)
}

/// The revise affordance for a `validating` request: a feedback field + submit that re-delegates the
/// work to the agents (mirrors the task-level `review_gate`). Feedback is optional — a blank submit
/// still re-runs, falling back to the reviewer's recorded verdict.
fn request_changes_form(id: &str) -> ElementBuilder {
    el(El::Form)
        .on(Ev::Submit, request_changes())
        // WFull or the form collapses to ~0 in its flex-col section (the width-spanning rule).
        .st([St::WFull, St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            crate::ui::widgets::hidden("request", id),
            Input::text()
                .name("feedback")
                .placeholder("What to change…")
                .autocomplete("off")
                .build()
                .st([St::Flex1, St::MinW0]),
            Button::outline("Request changes".to_owned())
                .size(ButtonSize::Sm)
                .build()
                .at(At::Type, Av::Submit),
        ])
}

#[handler]
fn deliver(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send_action(Command::RequestDeliver { id: id.to_owned() }, ctx.defer());
    }
}

#[handler]
fn request_changes(app: &mut App, ctx: &EventContext) {
    let _ = app;
    let Some(id) = ctx
        .field("request")
        .map(str::to_owned)
        .filter(|id| !id.is_empty())
    else {
        return;
    };
    let feedback = ctx.field("feedback").unwrap_or_default().trim().to_owned();
    bridge::send(Command::RequestChanges { id, feedback });
}

#[handler]
fn abandon(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send_action(Command::RequestAbandon { id: id.to_owned() }, ctx.defer());
    }
}

#[handler]
fn rollback(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send_action(Command::RequestRollback { id: id.to_owned() }, ctx.defer());
    }
}

#[handler]
fn push(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let Some(id) = ctx.data("request") {
        bridge::send_action(Command::RequestPush { id: id.to_owned() }, ctx.defer());
    }
}

fn section(title: &str, body: ElementBuilder) -> ElementBuilder {
    // The mockup's bordered panel: label over content in a hairline box.
    el(El::Div)
        .st([
            St::BorderDefault,
            St::BgSurface,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append([
            el(El::Div)
                .st([St::TextXs, St::TextLow, St::TextUppercase])
                .text(title),
            body,
        ])
}

/// The request lifecycle as a badge — same square/outline vocabulary as task cards.
fn status_badge(status: &str) -> ElementBuilder {
    let intent = match status {
        "executing" | "running" => BadgeIntent::Primary,
        "delivered" | "done" => BadgeIntent::Success,
        "rejected" | "failed" | "cancelled" => BadgeIntent::Error,
        "awaiting" | "planning" => BadgeIntent::Warning,
        _ => BadgeIntent::Default,
    };
    Badge::new()
        .intent(intent)
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .text(status.to_owned())
        .build()
}
