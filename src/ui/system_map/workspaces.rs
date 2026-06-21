//! Workspaces tier: the project directories a team of agents works in. A read-only column of
//! workspace rows (name + team size), each opening the drawer to edit. The drawer is the
//! single place owner + membership are managed: an **owner** (the workspace's domain lead /
//! home agent) and **members** (the team). A new workspace mints its id on the bridge — the
//! drawer only edits the name (the directory id is immutable).

use rwire::style::Style;
use rwire::{At, El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use super::{action, badge, card, drawer_form, labeled, row_head, select_st, submit, text_input};
use crate::bridge::{self, Command};
use crate::state::{AgentView, App, Editing, WorkspaceDraft, WorkspaceView};
use crate::ui::widgets::action_icon;

#[renderer]
pub fn column(app: &App) -> ElementBuilder {
    let mut items: Vec<ElementBuilder> = app.workspaces.iter().map(row).collect();
    items.push(action("+ Add workspace", open_new()).data("act", "add-workspace"));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(items)
}

fn row(workspace: &WorkspaceView) -> ElementBuilder {
    let summary = match workspace.member_names.len() {
        0 => "unused".to_owned(),
        1 => "1 agent".to_owned(),
        n => format!("{n} agents"),
    };
    let detail = if workspace.member_names.is_empty() {
        "No members yet.".to_owned()
    } else {
        workspace.member_names.join(", ")
    };
    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
            .append([
                row_head(&workspace.name, badge(&summary)),
                action("Edit", open_edit()).data("id", &workspace.id),
            ]),
        el(El::Div).st([St::TextXs, St::TextMuted]).text(&detail),
    ])
}

pub fn form(app: &App, draft: &WorkspaceDraft) -> ElementBuilder {
    let name = labeled(
        "Name",
        text_input("name", &draft.name, "e.g. Website", name_changed()),
    );
    let owner = owner_field(app, draft);
    let members = members_field(app, draft);
    let actions = if draft.confirm_delete {
        // Armed confirm: deletion removes the directory + its files, so it's a deliberate
        // second click. (The bridge still refuses while members are assigned.)
        vec![
            el(El::Span)
                .st([St::TextXs, St::TextError])
                .text("Delete workspace and its files?"),
            action_icon(icons::Icon::Trash, "Delete", confirm_delete(), true),
            action("Cancel", cancel_delete()).data("act", "cancel-delete"),
        ]
    } else {
        let mut actions = vec![submit(if draft.id.is_some() { "Save" } else { "Add" })];
        if draft.id.is_some() {
            actions.push(action("Delete", arm_delete()).data("act", "delete"));
        }
        actions.push(action("Cancel", super::close()).data("act", "cancel"));
        actions
    };
    drawer_form(save(), [name, owner, members], actions)
}

/// The owner picker (the workspace's domain lead / home agent). An agent owns at most one
/// workspace, so picking an owner here releases any workspace it previously owned.
fn owner_field(app: &App, draft: &WorkspaceDraft) -> ElementBuilder {
    let selected = draft.owner.as_deref();
    let none = el(El::Option).attr("value", "").text("— no owner —");
    let mut options = vec![if selected.is_none() {
        none.bool_attr(At::Selected)
    } else {
        none
    }];
    for agent in &app.agents {
        let option = el(El::Option).attr("value", &agent.id).text(&agent.name);
        options.push(if selected == Some(agent.id.as_str()) {
            option.bool_attr(At::Selected)
        } else {
            option
        });
    }
    labeled(
        "Owner",
        el(El::Select)
            .attr("name", "owner")
            .on(Ev::Change, owner_changed())
            .st(select_st())
            .append(options),
    )
}

/// The members picker: a toggle-chip per agent, on iff it's on the team. The owner is always
/// a member (its chip reads as on and is left as-is), so the bridge re-adds it on save.
fn members_field(app: &App, draft: &WorkspaceDraft) -> ElementBuilder {
    let chips: Vec<ElementBuilder> = app
        .agents
        .iter()
        .map(|agent| {
            let on = draft.owner.as_deref() == Some(agent.id.as_str())
                || draft.members.iter().any(|m| m == &agent.id);
            member_chip(agent, on)
        })
        .collect();
    let body = if chips.is_empty() {
        el(El::Div)
            .st([St::TextXs, St::TextMuted])
            .text("No agents to add")
    } else {
        el(El::Div)
            .style(Style::new().set("flex-wrap", "wrap"))
            .st([St::DisplayFlex, St::GapXs])
            .append(chips)
    };
    labeled("Members", body)
}

fn member_chip(agent: &AgentView, on: bool) -> ElementBuilder {
    let (bg, fg, mark) = if on {
        (St::BgAccent, St::TextOnAccent, "✓")
    } else {
        (St::BgApp, St::TextMuted, "+")
    };
    el(El::Button)
        .attr("type", "button")
        .data("agent", &agent.id)
        .on(Ev::Click, member_toggled())
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
        .append([el(El::Span).text(mark), el(El::Span).text(&agent.name)])
}

#[handler]
fn open_new(app: &mut App) {
    app.editing = Some(Editing::Workspace(WorkspaceDraft::default()));
}

#[handler]
fn open_edit(app: &mut App, ctx: &EventContext) {
    let Some(id) = ctx.data("id") else {
        return;
    };
    let Some(workspace) = app.workspaces.iter().find(|w| w.id.as_str() == id) else {
        return;
    };
    app.editing = Some(Editing::Workspace(WorkspaceDraft {
        id: Some(workspace.id.clone()),
        name: workspace.name.clone(),
        owner: workspace.owner.clone(),
        members: workspace.members.clone(),
        confirm_delete: false,
    }));
}

#[handler]
fn name_changed(app: &mut App, ctx: &EventContext) {
    if let Some(Editing::Workspace(draft)) = app.editing.as_mut() {
        draft.name = ctx.text().unwrap_or_default().to_owned();
    }
}

#[handler]
fn owner_changed(app: &mut App, ctx: &EventContext) {
    let owner = ctx.text().map(str::to_owned).filter(|v| !v.is_empty());
    if let Some(Editing::Workspace(draft)) = app.editing.as_mut() {
        draft.owner = owner;
    }
}

#[handler]
fn member_toggled(app: &mut App, ctx: &EventContext) {
    let Some(agent) = ctx.data("agent").map(str::to_owned) else {
        return;
    };
    if let Some(Editing::Workspace(draft)) = app.editing.as_mut() {
        // The owner is always a member; toggling its chip off is a no-op (re-added on save).
        if draft.owner.as_deref() == Some(agent.as_str()) {
            return;
        }
        if let Some(pos) = draft.members.iter().position(|m| m == &agent) {
            draft.members.remove(pos);
        } else {
            draft.members.push(agent);
        }
    }
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    let (id, owner, members) = match app.editing.as_ref() {
        Some(Editing::Workspace(draft)) => {
            (draft.id.clone(), draft.owner.clone(), draft.members.clone())
        }
        _ => return,
    };
    let name = ctx.field("name").unwrap_or_default().trim().to_owned();
    if name.is_empty() {
        return;
    }
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertWorkspace {
        id,
        name,
        owner,
        members,
    });
}

/// Arm the inline delete confirm (the actions row swaps to a confirm/cancel prompt).
#[handler]
fn arm_delete(app: &mut App) {
    if let Some(Editing::Workspace(draft)) = app.editing.as_mut() {
        draft.confirm_delete = true;
    }
}

/// Disarm the delete confirm, restoring the normal Save/Delete/Cancel actions.
#[handler]
fn cancel_delete(app: &mut App) {
    if let Some(Editing::Workspace(draft)) = app.editing.as_mut() {
        draft.confirm_delete = false;
    }
}

/// Confirm: delete the workspace (the bridge refuses if agents are still assigned).
#[handler]
fn confirm_delete(app: &mut App) {
    let id = match app.editing.as_ref() {
        Some(Editing::Workspace(draft)) => draft.id.clone(),
        _ => return,
    };
    if let Some(id) = id {
        app.error = None;
        app.editing = None;
        bridge::send(Command::DeleteWorkspace { id });
    }
}
