//! Agents tier: a read-only column of agent rows (each opens the drawer to edit) plus
//! an add button. The drawer form is *controlled* (its fields live in `App.editing`'s
//! `AgentDraft`) so it can re-render when discovered models arrive without resetting
//! the operator's input. Picking an endpoint fetches its `/v1/models` into a dropdown.

use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use super::{action_data, badge, card, drawer_form, labeled, row_head, submit, text_input};
use crate::bridge::{self, Command};
use crate::state::{AgentDraft, AgentView, App, Editing};
use rwire_components::{Select, Textarea};

#[renderer]
pub fn column(app: &App) -> ElementBuilder {
    let mut items: Vec<ElementBuilder> = app.agents.iter().map(row).collect();
    items.push(action_data("+ Add agent", open_new(), "act", "add-agent"));
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
                action_data("Edit", open_edit(), "id", &agent.id),
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
    let web_tools = tools_field(app, draft, "web", "Web tools", "No web tools available");
    let workspace_tools = tools_field(
        app,
        draft,
        "workspace",
        "Workspace tools",
        "No workspace tools available",
    );
    let prompt = prompt_field(draft);

    let mut actions = vec![submit(
        if draft.id.is_some() { "Save" } else { "Add" },
        &save(),
    )];
    if draft.id.is_some() {
        actions.push(action_data("Delete", delete(), "act", "delete"));
    }
    actions.push(action_data("Cancel", super::close(), "act", "cancel"));

    drawer_form(
        save(),
        [name, endpoint, model, web_tools, workspace_tools, prompt],
        actions,
    )
}

/// A tool picker for one `group`: toggle-chips, each on iff its name is in `draft.tools`.
/// Workspace tools are a per-agent capability — they fire only when the agent operates in a
/// workspace (owned, or a task's), which is gated at run time; membership is managed on the
/// Workspaces tier, so the chips show here unconditionally.
fn tools_field(
    app: &App,
    draft: &AgentDraft,
    group: &str,
    label: &str,
    empty: &str,
) -> ElementBuilder {
    let chips = tool_chips(app, draft, group);
    let body = if chips.is_empty() {
        el(El::Div).st([St::TextXs, St::TextMuted]).text(empty)
    } else {
        chip_wrap(chips)
    };
    labeled(label, body)
}

/// The chips for one tool `group`, each on iff selected in `draft.tools`.
fn tool_chips(app: &App, draft: &AgentDraft, group: &str) -> Vec<ElementBuilder> {
    app.tool_catalog
        .iter()
        .filter(|entry| entry.group == group)
        .map(|entry| tool_chip(&entry.name, draft.tools.iter().any(|t| t == &entry.name)))
        .collect()
}

/// A wrapping flex row of tool chips.
fn chip_wrap(chips: Vec<ElementBuilder>) -> ElementBuilder {
    el(El::Div)
        .st([St::FlexWrap])
        .st([St::DisplayFlex, St::GapXs])
        .append(chips)
}

/// A clickable on/off chip for one tool. The `data-tool` rides the name to the handler.
fn tool_chip(name: &str, on: bool) -> ElementBuilder {
    crate::ui::widgets::toggle_chip(name, on, tool_toggled()).data("tool", name)
}

/// The agent's identity prompt (its persona), stored in `agent_prompts` — operator-owned
/// and distinct per agent. The shared project context lives in the workspace's `AGENT.md`,
/// edited in the Files view.
fn prompt_field(draft: &AgentDraft) -> ElementBuilder {
    labeled(
        "Identity / persona",
        Textarea::new()
            .name("prompt")
            .rows(6)
            .placeholder(
                "Who this agent is — persona + instructions. Layer 2 of 4 (role · identity · \
                 brief · memory); see the Prompts view for the assembled result. The shared \
                 project brief lives in the workspace's AGENT.md.",
            )
            .value(draft.prompt.clone())
            .build()
            .on(Ev::Change, prompt_changed()),
    )
}

fn endpoint_select(app: &App, selected: Option<&str>) -> ElementBuilder {
    let mut field = Select::new()
        .name("endpoint")
        .value(selected.unwrap_or("").to_owned())
        .option("", "— no endpoint —");
    for endpoint in &app.endpoints {
        field = field.option(endpoint.name.clone(), endpoint.name.clone());
    }
    labeled("Endpoint", field.build().on(Ev::Change, endpoint_changed()))
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
    let mut field = Select::new()
        .name("model")
        .value(draft.model.clone())
        .option("", "— select model —");
    for model in &models {
        field = field.option(model.clone(), model.clone());
    }
    labeled("Model", field.build().on(Ev::Change, model_changed()))
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
    // Tools live in the draft (the chips are controlled), not the plain form fields.
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
