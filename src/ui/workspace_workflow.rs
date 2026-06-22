//! The workspace **Workflow** tab (Phase F-5): the pipeline editor. Add / remove / reorder
//! steps and configure each — a stage (one agent run) or a bounded validation loop (producer +
//! validator + iteration cap + on-exhaust). Editing is **locked while the workspace has
//! in-flight tasks**, so a running task never sees its pipeline change. Renders from
//! `App.workflow_steps` / `workflow_locked` / `editing_step`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, StepForm, StepView, WorkspaceView};
use crate::ui::widgets::page_header;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([
            page_header(
                "Workflow",
                "The pipeline every task in this workspace flows through — stages run once, loops \
                 produce and review until they pass. Empty = a single Execute stage.",
                el(El::Div),
            ),
            body(),
        ])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(workspace) = app
        .active_workspace
        .as_deref()
        .and_then(|id| app.workspaces.iter().find(|w| w.id == id))
    else {
        return el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("Select a workspace.");
    };
    let wid = workspace.id.clone();
    let members = members_of(app, workspace);
    let editing = app.editing_step.clone();

    let mut sections: Vec<ElementBuilder> = Vec::new();
    if app.workflow_locked {
        sections.push(
            el(El::Div)
                .st([
                    St::TextXs,
                    St::TextAccent,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::PSm,
                    St::BgSurface,
                ])
                .text(
                    "Editing is locked while this workspace has tasks in flight. Let them finish \
                     (or clear them) to change the pipeline.",
                ),
        );
    }
    if app.workflow_steps.is_empty() {
        sections.push(
            el(El::Div)
                .st([St::TextSm, St::TextMuted])
                .text("No custom pipeline — tasks run a single Execute stage. Add steps below."),
        );
    } else {
        let count = app.workflow_steps.len();
        for (index, step) in app.workflow_steps.iter().enumerate() {
            sections.push(step_card(
                step,
                index,
                count,
                &members,
                editing.as_deref() == Some(step.id.as_str()),
                app.workflow_locked,
                &wid,
            ));
        }
    }
    if !app.workflow_locked {
        sections.push(add_bar());
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(sections)
}

fn step_card(
    step: &StepView,
    index: usize,
    count: usize,
    members: &[(String, String)],
    editing: bool,
    locked: bool,
    wid: &str,
) -> ElementBuilder {
    let is_loop = step.kind == "loop";
    let summary = if is_loop {
        let reviewer = match step.validator_kind.as_deref() {
            Some("agent") => agent_label(&step.validator_agent, members),
            _ => "you".to_owned(),
        };
        format!(
            "loop · {} → review: {reviewer} · ≤{} iters · on exhaust: {}",
            agent_label(&step.agent, members),
            step.max_iterations,
            step.on_exhaust,
        )
    } else {
        format!("stage · {}", agent_label(&step.agent, members))
    };
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                el(El::Span)
                    .st([St::TextXs, St::TextLow])
                    .text(&format!("{}.", index + 1)),
                el(El::Span)
                    .st([St::Flex1, St::TextSm, St::TextHigh, St::FontSemibold])
                    .text(&step.name),
                controls(step, index, count, locked),
            ]),
        el(El::Div).st([St::TextXs, St::TextMuted]).text(&summary),
    ];
    if editing && !locked {
        rows.push(edit_form(step, members, wid));
    }
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::RoundedSm,
            St::PMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append(rows)
}

/// Edit / move / delete controls (hidden while locked).
fn controls(step: &StepView, index: usize, count: usize, locked: bool) -> ElementBuilder {
    if locked {
        return el(El::Span);
    }
    let mut buttons = vec![icon_btn("edit", toggle_edit(), &step.id)];
    if index > 0 {
        buttons.push(icon_btn("↑", move_up(), &step.id));
    }
    if index + 1 < count {
        buttons.push(icon_btn("↓", move_down(), &step.id));
    }
    buttons.push(icon_btn("delete", delete_step(), &step.id));
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs])
        .append(buttons)
}

fn icon_btn(label: &str, on_click: rwire::HandlerSpec, step: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("step", step)
        .on(Ev::Click, on_click)
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .text(label)
}

/// The inline edit form for a step. Validator fields apply to loops (ignored for stages on save).
fn edit_form(step: &StepView, members: &[(String, String)], wid: &str) -> ElementBuilder {
    let kind_opts = [
        ("stage", "Stage (run once)"),
        ("loop", "Loop (produce ⇄ review)"),
    ];
    let vkind_opts = [("human", "Human (you review)"), ("agent", "Agent reviewer")];
    let exhaust_opts = [("human", "Ask you (park)"), ("fail", "Fail the task")];
    el(El::Form)
        .on(Ev::Submit, save_step())
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
            St::PtSm,
            St::BorderTSubtle,
        ])
        .append([
            hidden("id", &step.id),
            hidden("workspace", wid),
            labeled("Name", text_field("name", &step.name, "Step name")),
            labeled("Kind", select("kind", &kind_opts, &step.kind)),
            labeled(
                "Agent",
                agent_select("agent", step.agent.as_deref(), members),
            ),
            labeled(
                "Role preamble (optional)",
                textarea("prompt", step.prompt.as_deref().unwrap_or("")),
            ),
            // Loop-only fields.
            el(El::Div)
                .st([St::TextXs, St::TextLow, St::TextUppercase, St::TrackingWide])
                .text("Loop only"),
            labeled(
                "Reviewer",
                select(
                    "validator_kind",
                    &vkind_opts,
                    step.validator_kind.as_deref().unwrap_or("human"),
                ),
            ),
            labeled(
                "Reviewer agent",
                agent_select("validator_agent", step.validator_agent.as_deref(), members),
            ),
            labeled(
                "Reviewer preamble (optional)",
                textarea(
                    "validator_prompt",
                    step.validator_prompt.as_deref().unwrap_or(""),
                ),
            ),
            labeled(
                "Max iterations",
                text_field("max_iterations", &step.max_iterations.to_string(), "2"),
            ),
            labeled(
                "On exhaust",
                select("on_exhaust", &exhaust_opts, &step.on_exhaust),
            ),
            el(El::Button)
                .attr("type", "submit")
                .style(Style::new().set("font", "inherit"))
                .st([
                    St::TextXs,
                    St::PxSm,
                    St::PyXs,
                    St::RoundedSm,
                    St::BgAccent,
                    St::TextOnAccent,
                    St::BorderAccent,
                    St::CursorPointer,
                    St::SelfStart,
                ])
                .text("Save step"),
        ])
}

fn add_bar() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::PtSm])
        .append([
            add_btn("+ Stage", add_stage()),
            add_btn("+ Loop", add_loop()),
        ])
}

fn add_btn(label: &str, on_click: rwire::HandlerSpec) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .on(Ev::Click, on_click)
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxMd,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .text(label)
}

// ── small form helpers ─────────────────────────────────────────────────────────

fn labeled(label: &str, field: ElementBuilder) -> ElementBuilder {
    el(El::Label)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            el(El::Span).st([St::TextXs, St::TextMuted]).text(label),
            field,
        ])
}

fn hidden(name: &str, value: &str) -> ElementBuilder {
    el(El::Input)
        .attr("type", "hidden")
        .attr("name", name)
        .attr("value", value)
}

fn field_st() -> [St; 7] {
    [
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
}

fn text_field(name: &str, value: &str, placeholder: &str) -> ElementBuilder {
    el(El::Input)
        .attr("name", name)
        .attr("value", value)
        .attr("placeholder", placeholder)
        .attr("autocomplete", "off")
        .style(Style::new().set("font", "inherit"))
        .st(field_st())
        .focus([St::BorderAccent])
}

fn textarea(name: &str, value: &str) -> ElementBuilder {
    el(El::Textarea)
        .attr("name", name)
        .attr("rows", "2")
        .style(
            Style::new()
                .set("font", "inherit")
                .set("resize", "vertical"),
        )
        .st(field_st())
        .focus([St::BorderAccent])
        .text(value)
}

fn select(name: &str, options: &[(&str, &str)], selected: &str) -> ElementBuilder {
    let opts: Vec<ElementBuilder> = options
        .iter()
        .map(|(value, label)| {
            let mut option = el(El::Option).attr("value", value).text(label);
            if *value == selected {
                option = option.attr("selected", "selected");
            }
            option
        })
        .collect();
    el(El::Select)
        .attr("name", name)
        .style(Style::new().set("font", "inherit"))
        .st(field_st())
        .append(opts)
}

fn agent_select(
    name: &str,
    selected: Option<&str>,
    members: &[(String, String)],
) -> ElementBuilder {
    let mut opts = vec![{
        let mut owner = el(El::Option).attr("value", "").text("— owner —");
        if selected.is_none() {
            owner = owner.attr("selected", "selected");
        }
        owner
    }];
    for (id, name) in members {
        let mut option = el(El::Option).attr("value", id).text(name);
        if selected == Some(id.as_str()) {
            option = option.attr("selected", "selected");
        }
        opts.push(option);
    }
    el(El::Select)
        .attr("name", name)
        .style(Style::new().set("font", "inherit"))
        .st(field_st())
        .append(opts)
}

fn members_of(app: &App, workspace: &WorkspaceView) -> Vec<(String, String)> {
    app.agents
        .iter()
        .filter(|a| workspace.members.iter().any(|m| m == &a.id))
        .map(|a| (a.id.clone(), a.name.clone()))
        .collect()
}

fn agent_label(agent: &Option<String>, members: &[(String, String)]) -> String {
    match agent {
        None => "owner".to_owned(),
        Some(id) => members
            .iter()
            .find(|(i, _)| i == id)
            .map_or_else(|| id.clone(), |(_, n)| n.clone()),
    }
}

// ── handlers ────────────────────────────────────────────────────────────────────

#[handler]
fn add_stage(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    if let Some(workspace) = app.active_workspace.clone() {
        bridge::send(Command::AddStep {
            workspace,
            kind: "stage".to_owned(),
        });
    }
}

#[handler]
fn add_loop(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    if let Some(workspace) = app.active_workspace.clone() {
        bridge::send(Command::AddStep {
            workspace,
            kind: "loop".to_owned(),
        });
    }
}

#[handler]
fn toggle_edit(app: &mut App, ctx: &EventContext) {
    if let Some(step) = ctx.data("step") {
        let step = step.to_owned();
        app.editing_step = if app.editing_step.as_deref() == Some(step.as_str()) {
            None
        } else {
            Some(step)
        };
    }
}

#[handler]
fn delete_step(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(id)) = (app.active_workspace.clone(), ctx.data("step")) {
        app.editing_step = None;
        bridge::send(Command::DeleteStep {
            workspace,
            id: id.to_owned(),
        });
    }
}

#[handler]
fn move_up(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(id)) = (app.active_workspace.clone(), ctx.data("step")) {
        bridge::send(Command::MoveStep {
            workspace,
            id: id.to_owned(),
            up: true,
        });
    }
}

#[handler]
fn move_down(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(id)) = (app.active_workspace.clone(), ctx.data("step")) {
        bridge::send(Command::MoveStep {
            workspace,
            id: id.to_owned(),
            up: false,
        });
    }
}

#[handler]
fn save_step(app: &mut App, ctx: &EventContext) {
    let Some(workspace) = app.active_workspace.clone() else {
        return;
    };
    let Some(id) = ctx.field("id").map(str::to_owned).filter(|s| !s.is_empty()) else {
        return;
    };
    let form = StepForm {
        kind: ctx.field("kind").unwrap_or("stage").to_owned(),
        name: ctx.field("name").unwrap_or_default().to_owned(),
        agent: ctx.field("agent").unwrap_or_default().to_owned(),
        prompt: ctx.field("prompt").unwrap_or_default().to_owned(),
        validator_kind: ctx.field("validator_kind").unwrap_or("human").to_owned(),
        validator_agent: ctx.field("validator_agent").unwrap_or_default().to_owned(),
        validator_prompt: ctx.field("validator_prompt").unwrap_or_default().to_owned(),
        max_iterations: ctx
            .field("max_iterations")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(2),
        on_exhaust: ctx.field("on_exhaust").unwrap_or("human").to_owned(),
    };
    app.editing_step = None;
    bridge::send(Command::UpdateStep {
        workspace,
        id,
        form,
    });
}
