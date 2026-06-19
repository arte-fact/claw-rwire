//! Agents tier: a read-only column of agent rows (each opens the drawer to edit) plus
//! an add button. The drawer form links the agent to an endpoint (a `<select>`, the
//! Agent→Endpoint edge of the map).

use rwire::style::Style;
use rwire::{At, El, ElementBuilder, St, el, handler, renderer};

use super::{action, badge, card, drawer_form, field, labeled, row_head, select_st, submit};
use crate::bridge::{self, Command};
use crate::state::{AgentView, App, Editing, EndpointView};

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

/// The drawer form: `None` = create; `Some(id)` = edit that agent.
pub fn form(app: &App, key: Option<&str>) -> ElementBuilder {
    let existing = key.and_then(|id| app.agents.iter().find(|a| a.id == id));
    if key.is_some() && existing.is_none() {
        return el(El::Div).style(Style::new().display("none"));
    }
    let name = existing.map(|a| a.name.as_str()).unwrap_or("");
    let endpoint = existing.and_then(|a| a.endpoint.as_deref());
    let model = existing.and_then(|a| a.model.as_deref()).unwrap_or("");

    let fields = [
        field("Name", "name", name, "e.g. Coder", false),
        endpoint_select(&app.endpoints, endpoint),
        field("Model", "model", model, "model name", false),
    ];
    if existing.is_some() {
        drawer_form(
            save(),
            fields,
            [
                submit("Save"),
                action("Delete", delete()).data("act", "delete"),
                action("Cancel", super::close()).data("act", "cancel"),
            ],
        )
    } else {
        drawer_form(
            save(),
            fields,
            [
                submit("Add"),
                action("Cancel", super::close()).data("act", "cancel"),
            ],
        )
    }
}

fn endpoint_select(endpoints: &[EndpointView], selected: Option<&str>) -> ElementBuilder {
    let mut options: Vec<ElementBuilder> =
        vec![el(El::Option).attr("value", "").text("— no endpoint —")];
    for endpoint in endpoints {
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
            .st(select_st())
            .append(options),
    )
}

#[handler]
fn open_new(app: &mut App) {
    app.editing = Some(Editing::Agent(None));
}

#[handler]
fn open_edit(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("id") {
        app.editing = Some(Editing::Agent(Some(id.to_owned())));
    }
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    let id = match &app.editing {
        Some(Editing::Agent(id)) => id.clone(),
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
    if let Some(Editing::Agent(Some(id))) = &app.editing {
        let id = id.clone();
        app.error = None;
        app.editing = None;
        bridge::send(Command::DeleteAgent { id });
    }
}
