//! Agents tier of the System Map: edit/delete existing agents + add new. Each agent
//! links to an endpoint (a `<select>`), which is the Agent→Endpoint edge of the map.

use rwire::style::Style;
use rwire::{At, El, ElementBuilder, Ev, St, el, handler, renderer};

use super::{badge, delete_button, field, form_card, submit_button};
use crate::bridge::{self, Command};
use crate::state::{AgentView, App, EndpointView};

#[renderer]
pub fn panel(app: &App) -> ElementBuilder {
    let mut cards: Vec<ElementBuilder> = app
        .agents
        .iter()
        .map(|agent| edit_card(agent, &app.endpoints))
        .collect();
    cards.push(add_card(&app.endpoints));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

fn edit_card(agent: &AgentView, endpoints: &[EndpointView]) -> ElementBuilder {
    let link = badge(agent.endpoint.as_deref().unwrap_or("unlinked"));
    form_card(
        save(),
        [
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    el(El::Span)
                        .st([St::TextHigh, St::FontSemibold])
                        .text(&agent.name),
                    link,
                ]),
            hidden("id", &agent.id),
            field("Name", "name", &agent.name, "Agent name", false),
            endpoint_select(endpoints, agent.endpoint.as_deref()),
            field(
                "Model",
                "model",
                agent.model.as_deref().unwrap_or(""),
                "model name",
                false,
            ),
        ],
        [
            submit_button("Save"),
            delete_button("id", &agent.id).on(Ev::Click, delete()),
        ],
    )
}

fn add_card(endpoints: &[EndpointView]) -> ElementBuilder {
    form_card(
        save(),
        [
            el(El::Div)
                .st([St::TextMuted, St::TextSm])
                .text("+ Add agent"),
            field("Name", "name", "", "e.g. Coder", false),
            endpoint_select(endpoints, None),
            field("Model", "model", "", "model name", false),
        ],
        [submit_button("Add")],
    )
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
    el(El::Label)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            el(El::Span)
                .st([
                    St::TextXs,
                    St::TextMuted,
                    St::TextUppercase,
                    St::TrackingWide,
                ])
                .text("Endpoint"),
            el(El::Select)
                .attr("name", "endpoint")
                .st([
                    St::WFull,
                    St::PxSm,
                    St::PySm,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::BgApp,
                    St::TextHigh,
                    St::OutlineNone,
                ])
                .style(Style::new().set("font", "inherit"))
                .append(options),
        ])
}

fn hidden(name: &str, value: &str) -> ElementBuilder {
    el(El::Input)
        .attr("type", "hidden")
        .attr("name", name)
        .attr("value", value)
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    let name = ctx.field("name").unwrap_or_default().trim().to_owned();
    if name.is_empty() {
        return;
    }
    let id = ctx
        .field("id")
        .map(str::to_owned)
        .filter(|id| !id.is_empty());
    let endpoint = ctx
        .field("endpoint")
        .map(str::to_owned)
        .filter(|value| !value.is_empty());
    let model = ctx
        .field("model")
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty());
    app.error = None;
    bridge::send(Command::UpsertAgent {
        id,
        name,
        endpoint,
        model,
    });
}

#[handler]
fn delete(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("id") {
        app.error = None;
        bridge::send(Command::DeleteAgent { id: id.to_owned() });
    }
}
