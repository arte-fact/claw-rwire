//! Endpoints tier: a read-only column of endpoint rows (each opens the drawer to
//! edit) plus an add button. The drawer form (create or edit) lives in `form`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};

use super::{action_data, badge, card, drawer_form, field, row_head, submit};
use crate::bridge::{self, Command};
use crate::state::{App, Editing};

#[renderer]
pub fn column(app: &App) -> ElementBuilder {
    let mut items: Vec<ElementBuilder> = app.endpoints.iter().map(row).collect();
    items.push(action_data(
        "+ Add endpoint",
        open_new(),
        "act",
        "add-endpoint",
    ));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(items)
}

fn row(endpoint: &crate::state::EndpointView) -> ElementBuilder {
    let key = if endpoint.has_key {
        badge("key set")
    } else {
        badge("no key")
    };
    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
            .append([
                row_head(&endpoint.name, key),
                action_data("Edit", open_edit(), "name", &endpoint.name),
            ]),
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::Truncate])
            .text(&endpoint.base_url),
    ])
}

/// The drawer form: `None` = create; `Some(name)` = edit that endpoint (name is the
/// immutable key, so the edit form omits it).
pub fn form(app: &App, key: Option<&str>) -> ElementBuilder {
    match key {
        None => drawer_form(
            save(),
            [
                field("Name", "name", "", "e.g. openrouter", false),
                field("Base URL", "base_url", "", "https://host/v1", false),
                field("API key", "api_key", "", "optional", true),
            ],
            [
                submit("Add", &save()),
                action_data("Cancel", super::close(), "act", "cancel"),
            ],
        ),
        Some(name) => match app.endpoints.iter().find(|e| e.name == name) {
            None => el(El::Div).style(Style::new().display("none")),
            Some(endpoint) => drawer_form(
                save(),
                [
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
                    submit("Save", &save()),
                    action_data("Delete", delete(), "act", "delete"),
                    action_data("Cancel", super::close(), "act", "cancel"),
                ],
            ),
        },
    }
}

#[handler]
fn open_new(app: &mut App) {
    app.editing = Some(Editing::Endpoint(None));
}

#[handler]
fn open_edit(app: &mut App, ctx: &EventContext) {
    if let Some(name) = ctx.data("name") {
        app.editing = Some(Editing::Endpoint(Some(name.to_owned())));
    }
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    // Identity comes from the drawer's target; values from the form.
    let name = match &app.editing {
        Some(Editing::Endpoint(Some(name))) => name.clone(),
        Some(Editing::Endpoint(None)) => {
            let name = ctx.field("name").unwrap_or_default().trim().to_owned();
            if name.is_empty() {
                return;
            }
            name
        }
        _ => return,
    };
    let base_url = ctx.field("base_url").unwrap_or_default().trim().to_owned();
    let api_key = ctx
        .field("api_key")
        .map(str::to_owned)
        .filter(|key| !key.trim().is_empty());
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertEndpoint {
        name,
        base_url,
        api_key,
    });
}

#[handler]
fn delete(app: &mut App) {
    if let Some(Editing::Endpoint(Some(name))) = &app.editing {
        let name = name.clone();
        app.error = None;
        app.editing = None;
        bridge::send(Command::DeleteEndpoint { name });
    }
}
