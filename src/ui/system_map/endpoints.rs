//! Endpoints tier of the System Map: edit/delete existing endpoints + add new.

use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use super::{badge, delete_button, field, form_card, submit_button};
use crate::bridge::{self, Command};
use crate::state::{App, EndpointView};

#[renderer]
pub fn panel(app: &App) -> ElementBuilder {
    let mut cards: Vec<ElementBuilder> = app.endpoints.iter().map(edit_card).collect();
    cards.push(add_card());
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

fn edit_card(endpoint: &EndpointView) -> ElementBuilder {
    let key = if endpoint.has_key {
        badge("key set")
    } else {
        badge("no key")
    };
    form_card(
        save(),
        [
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    el(El::Span)
                        .st([St::TextHigh, St::FontSemibold])
                        .text(&endpoint.name),
                    key,
                ]),
            hidden("name", &endpoint.name),
            field(
                "Base URL",
                "base_url",
                &endpoint.base_url,
                "https://host/v1",
                false,
            ),
            field("API key", "api_key", "", "leave blank to keep", true),
        ],
        [
            submit_button("Save"),
            delete_button("name", &endpoint.name).on(Ev::Click, delete()),
        ],
    )
}

fn add_card() -> ElementBuilder {
    form_card(
        save(),
        [
            el(El::Div)
                .st([St::TextMuted, St::TextSm])
                .text("+ Add endpoint"),
            field("Name", "name", "", "e.g. openrouter", false),
            field("Base URL", "base_url", "", "https://host/v1", false),
            field("API key", "api_key", "", "optional", true),
        ],
        [submit_button("Add")],
    )
}

/// A hidden field carrying the row's id through the form submit.
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
    let base_url = ctx.field("base_url").unwrap_or_default().trim().to_owned();
    let api_key = ctx
        .field("api_key")
        .map(str::to_owned)
        .filter(|key| !key.trim().is_empty());
    app.error = None;
    bridge::send(Command::UpsertEndpoint {
        name,
        base_url,
        api_key,
    });
}

#[handler]
fn delete(app: &mut App, ctx: &EventContext) {
    if let Some(name) = ctx.data("name") {
        app.error = None;
        bridge::send(Command::DeleteEndpoint {
            name: name.to_owned(),
        });
    }
}
