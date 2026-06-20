//! Agents tier: a read-only column of agent rows (each opens the drawer to edit) plus
//! an add button. The drawer form is *controlled* (its fields live in `App.editing`'s
//! `AgentDraft`) so it can re-render when discovered models arrive without resetting
//! the operator's input. Picking an endpoint fetches its `/v1/models` into a dropdown.

use rwire::style::Style;
use rwire::{At, El, ElementBuilder, Ev, St, el, handler, renderer};

use super::{action, badge, card, drawer_form, labeled, row_head, select_st, submit, text_input};
use crate::bridge::{self, Command};
use crate::state::{AgentDraft, AgentView, App, Editing};

#[renderer]
pub fn column(app: &App) -> ElementBuilder {
    let mut items: Vec<ElementBuilder> = app.agents.iter().map(row).collect();
    items.push(action("+ Add agent", open_new()).data("act", "add-agent"));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(items)
}

fn row(agent: &AgentView) -> ElementBuilder {
    let link = badge(agent.endpoint.as_deref().unwrap_or("unlinked"));
    let model = agent.model.as_deref().unwrap_or("no model");
    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
            .append([
                row_head(&agent.name, link),
                action("Edit", open_edit()).data("id", &agent.id),
            ]),
        el(El::Div).st([St::TextXs, St::TextMuted]).text(model),
    ])
}

/// The controlled drawer form, rendered from `draft`. The Model control is a dropdown
/// of `app.endpoint_models` (discovered for the draft's endpoint), or a free-text
/// fallback when none were discovered.
pub fn form(app: &App, draft: &AgentDraft) -> ElementBuilder {
    let name = labeled(
        "Name",
        text_input("name", &draft.name, "e.g. Coder", name_changed()),
    );
    let endpoint = endpoint_select(app, draft.endpoint.as_deref());
    let model = model_field(app, draft);
    let tools = tools_field(app, draft);
    let prompt = prompt_field(draft);

    let mut actions = vec![submit(if draft.id.is_some() { "Save" } else { "Add" })];
    if draft.id.is_some() {
        actions.push(action("Delete", delete()).data("act", "delete"));
    }
    actions.push(action("Cancel", super::close()).data("act", "cancel"));

    drawer_form(save(), [name, endpoint, model, tools, prompt], actions)
}

/// The per-tool picker: toggle-chips grouped by source (Web / Coder), built from the
/// projected `app.tool_catalog`. Each chip is on iff its name is in `draft.tools`.
fn tools_field(app: &App, draft: &AgentDraft) -> ElementBuilder {
    let mut sections: Vec<ElementBuilder> = Vec::new();
    for (key, label) in [("web", "Web"), ("coder", "Coder")] {
        let chips: Vec<ElementBuilder> = app
            .tool_catalog
            .iter()
            .filter(|entry| entry.group == key)
            .map(|entry| tool_chip(&entry.name, draft.tools.iter().any(|t| t == &entry.name)))
            .collect();
        if chips.is_empty() {
            continue;
        }
        sections.push(
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapXs])
                .append([
                    el(El::Span)
                        .st([
                            St::TextXs,
                            St::TextMuted,
                            St::TextUppercase,
                            St::TrackingWide,
                        ])
                        .text(label),
                    el(El::Div)
                        .style(Style::new().set("flex-wrap", "wrap"))
                        .st([St::DisplayFlex, St::GapXs])
                        .append(chips),
                ]),
        );
    }
    if sections.is_empty() {
        sections.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text("No tools available"),
        );
    }
    labeled(
        "Tools",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(sections),
    )
}

/// A clickable on/off chip for one tool. The `data-tool` rides the name to the handler.
fn tool_chip(name: &str, on: bool) -> ElementBuilder {
    let (bg, fg, mark) = if on {
        (St::BgAccent, St::TextOnAccent, "✓")
    } else {
        (St::BgApp, St::TextMuted, "+")
    };
    el(El::Button)
        .attr("type", "button")
        .data("tool", name)
        .on(Ev::Click, tool_toggled())
        .style(Style::new().set("font", "inherit"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::PxSm,
            St::PyXs,
            St::BorderDefault,
            St::RoundedSm,
            St::TextXs,
            St::CursorPointer,
            bg,
            fg,
        ])
        .append([el(El::Span).text(mark), el(El::Span).text(name)])
}

/// The agent's system prompt, persisted as `AGENT.md` in its workspace.
fn prompt_field(draft: &AgentDraft) -> ElementBuilder {
    labeled(
        "System prompt (AGENT.md)",
        el(El::Textarea)
            .attr("name", "prompt")
            .attr("rows", "6")
            .at_str(
                At::Placeholder,
                "Persona + instructions — saved as AGENT.md in the agent's workspace.",
            )
            .on(Ev::Change, prompt_changed())
            .st(select_st())
            .style(
                Style::new()
                    .set("font", "inherit")
                    .set("resize", "vertical")
                    .set("min-height", "8rem"),
            )
            .focus([St::BorderAccent])
            .text(&draft.prompt),
    )
}

fn endpoint_select(app: &App, selected: Option<&str>) -> ElementBuilder {
    let mut options: Vec<ElementBuilder> =
        vec![el(El::Option).attr("value", "").text("— no endpoint —")];
    for endpoint in &app.endpoints {
        let option = el(El::Option)
            .attr("value", &endpoint.name)
            .text(&endpoint.name);
        options.push(if selected == Some(endpoint.name.as_str()) {
            option.bool_attr(At::Selected)
        } else {
            option
        });
    }
    labeled(
        "Endpoint",
        el(El::Select)
            .attr("name", "endpoint")
            .on(Ev::Change, endpoint_changed())
            .st(select_st())
            .append(options),
    )
}

fn model_field(app: &App, draft: &AgentDraft) -> ElementBuilder {
    if app.endpoint_models.is_empty() {
        // No endpoint chosen, or discovery returned nothing — keep it free text.
        return labeled(
            "Model",
            text_input("model", &draft.model, "model name", model_changed()),
        );
    }
    // The draft's current model may not be advertised (a hand-set value); keep it.
    let mut models = app.endpoint_models.clone();
    if !draft.model.is_empty() && !models.iter().any(|m| m == &draft.model) {
        models.insert(0, draft.model.clone());
    }
    let mut options = vec![el(El::Option).attr("value", "").text("— select model —")];
    for model in &models {
        let option = el(El::Option).attr("value", model).text(model);
        options.push(if draft.model == *model {
            option.bool_attr(At::Selected)
        } else {
            option
        });
    }
    labeled(
        "Model",
        el(El::Select)
            .attr("name", "model")
            .on(Ev::Change, model_changed())
            .st(select_st())
            .append(options),
    )
}

#[handler]
fn open_new(app: &mut App) {
    app.editing = Some(Editing::Agent(AgentDraft::default()));
    app.endpoint_models = Vec::new();
}

#[handler]
fn open_edit(app: &mut App, ctx: &EventContext) {
    let Some(id) = ctx.data("id") else {
        return;
    };
    let Some(agent) = app.agents.iter().find(|a| a.id.as_str() == id) else {
        return;
    };
    let draft = AgentDraft {
        id: Some(agent.id.clone()),
        name: agent.name.clone(),
        endpoint: agent.endpoint.clone(),
        model: agent.model.clone().unwrap_or_default(),
        prompt: String::new(),
        tools: Vec::new(),
    };
    let id = agent.id.clone();
    let endpoint = draft.endpoint.clone();
    app.editing = Some(Editing::Agent(draft));
    app.endpoint_models = Vec::new();
    // The AGENT.md and tool selection live on disk/DB; the bridge reads them back into
    // the draft (like discovered models do).
    bridge::send(Command::LoadAgentPrompt { id: id.clone() });
    bridge::send(Command::LoadAgentTools { id });
    if let Some(endpoint) = endpoint {
        bridge::send(Command::DiscoverModels { endpoint });
    }
}

#[handler]
fn name_changed(app: &mut App, ctx: &EventContext) {
    if let Some(Editing::Agent(draft)) = app.editing.as_mut() {
        draft.name = ctx.text().unwrap_or_default().to_owned();
    }
}

#[handler]
fn endpoint_changed(app: &mut App, ctx: &EventContext) {
    let endpoint = ctx.text().map(str::to_owned).filter(|v| !v.is_empty());
    if let Some(Editing::Agent(draft)) = app.editing.as_mut() {
        draft.endpoint = endpoint.clone();
        draft.model.clear();
    }
    app.endpoint_models = Vec::new();
    if let Some(endpoint) = endpoint {
        bridge::send(Command::DiscoverModels { endpoint });
    }
}

#[handler]
fn model_changed(app: &mut App, ctx: &EventContext) {
    if let Some(Editing::Agent(draft)) = app.editing.as_mut() {
        draft.model = ctx.text().unwrap_or_default().to_owned();
    }
}

#[handler]
fn tool_toggled(app: &mut App, ctx: &EventContext) {
    let Some(tool) = ctx.data("tool").map(str::to_owned) else {
        return;
    };
    if let Some(Editing::Agent(draft)) = app.editing.as_mut() {
        if let Some(pos) = draft.tools.iter().position(|t| t == &tool) {
            draft.tools.remove(pos);
        } else {
            draft.tools.push(tool);
        }
    }
}

#[handler]
fn prompt_changed(app: &mut App, ctx: &EventContext) {
    if let Some(Editing::Agent(draft)) = app.editing.as_mut() {
        draft.prompt = ctx.text().unwrap_or_default().to_owned();
    }
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    // Tools live in the draft (the chips are controlled), not the form fields.
    let (id, tools) = match app.editing.as_ref() {
        Some(Editing::Agent(draft)) => (draft.id.clone(), draft.tools.clone()),
        _ => return,
    };
    let name = ctx.field("name").unwrap_or_default().trim().to_owned();
    if name.is_empty() {
        return;
    }
    let endpoint = ctx
        .field("endpoint")
        .map(str::to_owned)
        .filter(|value| !value.is_empty());
    let model = ctx
        .field("model")
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty());
    let prompt = ctx.field("prompt").unwrap_or_default().to_owned();
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertAgent {
        id,
        name,
        endpoint,
        model,
        prompt,
        tools,
    });
}

#[handler]
fn delete(app: &mut App) {
    let id = match app.editing.as_ref() {
        Some(Editing::Agent(draft)) => draft.id.clone(),
        _ => return,
    };
    if let Some(id) = id {
        app.error = None;
        app.editing = None;
        bridge::send(Command::DeleteAgent { id });
    }
}
