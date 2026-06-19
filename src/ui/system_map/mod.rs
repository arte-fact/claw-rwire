//! The System Map: a layered **Channels → Agents → Endpoints** view of the registry
//! with full CRUD. Renderers read the shared read-models (`App.{endpoints,agents,
//! web_agent}`); edits post a bridge `Command` that writes `central.db` and re-projects.
//!
//! Structural columns are plain elements; each tier's renderer output sits *inside* a
//! column (renderers are wrapped in inline synced spans and can't be flex tracks).

mod agents;
mod endpoints;

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler, renderer};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::App;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PXl])
        .append([
            Text::heading1("System").build(),
            Text::body("Channels route to agents; agents call endpoints. Edits save immediately.")
                .muted()
                .build(),
            board(),
        ])
}

fn board() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::GapLg, St::ItemsStart])
        .style(Style::new().set("flex-wrap", "wrap"))
        .append([
            column("Channels", channels()),
            column("Agents", agents::panel()),
            column("Endpoints", endpoints::panel()),
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

/// The built-in Web channel, routing to a selectable agent (the Channels→Agent link).
#[renderer]
fn channels(app: &App) -> ElementBuilder {
    let options = app.agents.iter().map(|agent| {
        let option = el(El::Option).attr("value", &agent.id).text(&agent.name);
        if app.web_agent.as_deref() == Some(agent.id.as_str()) {
            option.bool_attr(At::Selected)
        } else {
            option
        }
    });
    let routes_to = app
        .web_agent
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .map_or_else(|| "no agent".to_owned(), |a| a.name.clone());

    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                el(El::Span)
                    .st([St::TextHigh, St::FontSemibold])
                    .text("Web"),
                badge("built-in"),
            ]),
        el(El::Div)
            .st([St::TextXs, St::TextMuted])
            .text(&format!("Operator chat → {routes_to}")),
        el(El::Select)
            .attr("name", "agent")
            .on(Ev::Change, route_web())
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

#[handler]
fn route_web(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.text().filter(|value| !value.is_empty()) {
        app.error = None;
        bridge::send(Command::SetWebAgent { id: id.to_owned() });
    }
}

// ── Shared card/form helpers (used by the endpoints/agents submodules) ──────────

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
        .focus([St::BorderAccent]);
    let input = if password {
        input.at(At::Type, Av::Password)
    } else {
        input
    };
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
            input,
        ])
}

pub(super) fn submit_button(label: &str) -> ElementBuilder {
    el(El::Button)
        .at(At::Type, Av::Submit)
        .st([
            St::PxMd,
            St::PySm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgApp,
            St::TextHigh,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .hover([St::BorderAccent])
        .text(label)
}

/// A non-submit delete button carrying its target id in a data attribute. The caller
/// attaches the right handler (delete differs per tier).
pub(super) fn delete_button(data_key: &str, data_value: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data(data_key, data_value)
        .st([
            St::PxSm,
            St::PySm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .style(Style::new().set("font", "inherit"))
        .hover([St::TextHigh])
        .text("Delete")
}

/// A `<form>` row that posts on submit, with Save (+ optional trailing button).
pub(super) fn form_card(
    submit: rwire::HandlerSpec,
    fields: impl IntoIterator<Item = ElementBuilder>,
    actions: impl IntoIterator<Item = ElementBuilder>,
) -> ElementBuilder {
    let mut children: Vec<ElementBuilder> = fields.into_iter().collect();
    children.push(
        el(El::Div)
            .st([St::DisplayFlex, St::GapSm, St::ItemsCenter])
            .append(actions),
    );
    card([el(El::Form)
        .on(Ev::Submit, submit)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(children)])
}
