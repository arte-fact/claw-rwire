//! The System Map: a **read-only** Channels → Agents → Endpoints schema of the
//! registry. Create / update / delete happen in a right-side drawer; `App.editing`
//! holds which row (or new-row slot) the drawer targets, and a single `drawer()`
//! renderer paints the matching form.
//!
//! Structural columns are plain elements; each tier's renderer output sits *inside* a
//! column (renderers are wrapped in inline synced spans and can't be flex tracks).

mod agents;
mod endpoints;
mod workspaces;

use rwire::style::Style;
use rwire::{At, El, ElementBuilder, Ev, HandlerSpec, St, el, handler, renderer};
use rwire_components::{Drawer, DrawerPosition, Text};

use crate::bridge::{self, Command};
use crate::state::{App, Editing};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            Text::heading1("System").build(),
            Text::body("Channels route to agents; agents call endpoints.")
                .muted()
                .build(),
            error_banner(),
            board(),
            drawer(),
        ])
}

/// A transient admin error (e.g. an FK violation when deleting a referenced row).
/// Hidden when clear.
#[renderer]
fn error_banner(app: &App) -> ElementBuilder {
    match &app.error {
        Some(message) => el(El::Div)
            .st([
                St::PxMd,
                St::PySm,
                St::TextXs,
                St::TextError,
                St::BorderDefault,
                St::RoundedSm,
                St::BgSurface,
            ])
            .append([el(El::Span).text(&format!("⚠ {message}"))]),
        None => el(El::Div).style(Style::new().display("none")),
    }
}

fn board() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::GapLg, St::ItemsStart])
        .style(Style::new().set("flex-wrap", "wrap"))
        .append([
            column("Channels", channels()),
            column("Agents", agents::column()),
            column("Workspaces", workspaces::column()),
            column("Endpoints", endpoints::column()),
        ])
}

fn column(label: &str, body: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().set("flex", "1 1 19rem"))
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Div)
                .st([
                    St::TextXs,
                    St::TextMuted,
                    St::TextUppercase,
                    St::TrackingWide,
                ])
                .text(label),
            body,
        ])
}

// ── Channels (read-only; the Web→agent link is edited in the drawer) ────────────

#[renderer]
fn channels(app: &App) -> ElementBuilder {
    let routes_to = app
        .web_agent
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .map_or_else(|| "no agent".to_owned(), |a| a.name.clone());
    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
            .append([
                row_head("Web", badge("built-in")),
                action("Edit", open_channel()).data("act", "edit-channel"),
            ]),
        el(El::Div)
            .st([St::TextXs, St::TextMuted])
            .text(&format!("Operator chat → {routes_to}")),
    ])
}

#[handler]
fn open_channel(app: &mut App) {
    app.editing = Some(Editing::Channel);
}

/// Close the drawer without changes (backdrop / Cancel).
#[handler]
pub(super) fn close(app: &mut App) {
    app.editing = None;
}

// ── Drawer ──────────────────────────────────────────────────────────────────

#[renderer]
fn drawer(app: &App) -> ElementBuilder {
    let (title, content) = match &app.editing {
        Some(Editing::Endpoint(key)) => (
            if key.is_some() {
                "Edit endpoint"
            } else {
                "New endpoint"
            },
            endpoints::form(app, key.as_deref()),
        ),
        Some(Editing::Agent(draft)) => (
            if draft.id.is_some() {
                "Edit agent"
            } else {
                "New agent"
            },
            agents::form(app, draft),
        ),
        Some(Editing::Workspace(draft)) => (
            if draft.id.is_some() {
                "Edit workspace"
            } else {
                "New workspace"
            },
            workspaces::form(draft),
        ),
        Some(Editing::Channel) => ("Web channel", channel_form(app)),
        None => ("", el(El::Div)),
    };
    Drawer::new()
        .position(DrawerPosition::Right)
        .open(app.editing.is_some())
        .title(title)
        .on_close(close())
        .content(content)
        .build()
}

fn channel_form(app: &App) -> ElementBuilder {
    let options = app.agents.iter().map(|agent| {
        let option = el(El::Option).attr("value", &agent.id).text(&agent.name);
        if app.web_agent.as_deref() == Some(agent.id.as_str()) {
            option.bool_attr(At::Selected)
        } else {
            option
        }
    });
    drawer_form(
        route_web(),
        [labeled(
            "Agent",
            el(El::Select)
                .attr("name", "agent")
                .st(select_st())
                .append(options),
        )],
        [
            submit("Save"),
            action("Cancel", close()).data("act", "cancel"),
        ],
    )
}

#[handler]
fn route_web(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.field("agent").filter(|value| !value.is_empty()) {
        let id = id.to_owned();
        app.error = None;
        app.editing = None;
        bridge::send(Command::SetWebAgent { id });
    }
}

// ── Shared building blocks (used by the endpoints/agents submodules) ────────────

pub(super) fn card(children: impl IntoIterator<Item = ElementBuilder>) -> ElementBuilder {
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
        .append(children)
}

pub(super) fn badge(text: &str) -> ElementBuilder {
    el(El::Span)
        .st([
            St::TextXs,
            St::TextMuted,
            St::BorderDefault,
            St::RoundedSm,
            St::PxXs,
        ])
        .text(text)
}

/// A row's title: a bold name beside a status badge.
pub(super) fn row_head(name: &str, badge: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span).st([St::TextHigh, St::FontSemibold]).text(name),
            badge,
        ])
}

// Edit/Delete/Cancel/Add-row buttons and the drawer-form Save/Add submit use the shared
// button family, so the System Map matches every other surface. Re-exported so existing
// `super::{action, submit}` imports in this module keep resolving.
pub(super) use crate::ui::widgets::{action, submit};

pub(super) fn select_st() -> [St; 8] {
    [
        St::WFull,
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
}

/// A controlled text input that reports its value on `change` (blur). The agent form
/// re-renders when discovered models arrive, so its inputs can't be uncontrolled.
pub(super) fn text_input(
    name: &str,
    value: &str,
    placeholder: &str,
    on_change: HandlerSpec,
) -> ElementBuilder {
    el(El::Input)
        .attr("name", name)
        .attr("value", value)
        .attr("placeholder", placeholder)
        .attr("autocomplete", "off")
        .st(select_st())
        .style(Style::new().set("font", "inherit"))
        .focus([St::BorderAccent])
        .on(Ev::Change, on_change)
}

/// A labeled control (its child input/select), stacked.
pub(super) fn labeled(label: &str, control: ElementBuilder) -> ElementBuilder {
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
                .text(label),
            control,
        ])
}

/// A labeled text input. `password` masks it (used for the write-only api_key).
pub(super) fn field(
    label: &str,
    name: &str,
    value: &str,
    placeholder: &str,
    password: bool,
) -> ElementBuilder {
    let input = el(El::Input)
        .attr("name", name)
        .attr("value", value)
        .attr("placeholder", placeholder)
        .attr("autocomplete", "off")
        .st(select_st())
        .style(Style::new().set("font", "inherit"))
        .focus([St::BorderAccent]);
    let input = if password {
        input.attr("type", "password")
    } else {
        input
    };
    labeled(label, input)
}

/// A `<form>` that posts on submit: stacked `fields`, then an actions row.
pub(super) fn drawer_form(
    submit: HandlerSpec,
    fields: impl IntoIterator<Item = ElementBuilder>,
    actions: impl IntoIterator<Item = ElementBuilder>,
) -> ElementBuilder {
    let mut children: Vec<ElementBuilder> = fields.into_iter().collect();
    children.push(
        el(El::Div)
            .st([St::DisplayFlex, St::GapSm, St::ItemsCenter])
            .append(actions),
    );
    el(El::Form)
        .on(Ev::Submit, submit)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append(children)
}
