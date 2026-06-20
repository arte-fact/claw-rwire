//! Workspaces tier: the shareable project directories agents work in. A read-only column
//! of workspace rows (name + who shares it), each opening the drawer to rename; plus an add
//! button. A new workspace mints its id on the bridge — the drawer only edits the display
//! name (the directory id is immutable). Agents are assigned to a workspace in the Agents
//! tier's drawer, so several can share one project.

use rwire::{El, ElementBuilder, St, el, handler, icons, renderer};

use super::{action, badge, card, drawer_form, labeled, row_head, submit, text_input};
use crate::bridge::{self, Command};
use crate::state::{App, Editing, WorkspaceDraft, WorkspaceView};
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
    let summary = match workspace.agents.len() {
        0 => "unused".to_owned(),
        1 => "1 agent".to_owned(),
        n => format!("{n} agents"),
    };
    let detail = if workspace.agents.is_empty() {
        "No agents assigned yet.".to_owned()
    } else {
        workspace.agents.join(", ")
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

pub fn form(draft: &WorkspaceDraft) -> ElementBuilder {
    let name = labeled(
        "Name",
        text_input("name", &draft.name, "e.g. Website", name_changed()),
    );
    let actions = if draft.confirm_delete {
        // Armed confirm: deletion removes the directory + its files, so it's a deliberate
        // second click. (The bridge still refuses while agents are assigned.)
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
    drawer_form(save(), [name], actions)
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
fn save(app: &mut App, ctx: &EventContext) {
    let id = match app.editing.as_ref() {
        Some(Editing::Workspace(draft)) => draft.id.clone(),
        _ => return,
    };
    let name = ctx.field("name").unwrap_or_default().trim().to_owned();
    if name.is_empty() {
        return;
    }
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertWorkspace { id, name });
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
