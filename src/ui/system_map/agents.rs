//! Agents tier: a read-only column of agent rows (each opens the drawer to edit) plus
//! an add button. The drawer form is *controlled* (its fields live in `App.editing`'s
//! `AgentDraft`) so it can re-render when discovered models arrive without resetting
//! the operator's input. Picking an endpoint fetches its `/v1/models` into a dropdown.

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

    let mut actions = vec![submit(if draft.id.is_some() { "Save" } else { "Add" })];
    if draft.id.is_some() {
        actions.push(action("Delete", delete()).data("act", "delete"));
    }
    actions.push(action("Cancel", super::close()).data("act", "cancel"));

    drawer_form(save(), [name, endpoint, model], actions)
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
    };
    let endpoint = draft.endpoint.clone();
    app.editing = Some(Editing::Agent(draft));
    app.endpoint_models = Vec::new();
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
fn save(app: &mut App, ctx: &EventContext) {
    let id = match app.editing.as_ref() {
        Some(Editing::Agent(draft)) => draft.id.clone(),
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
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertAgent {
        id,
        name,
        endpoint,
        model,
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
