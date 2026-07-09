//! SSH deploy-keys tier (P5b): a read-only column of key rows (name + fingerprint, each opens the
//! drawer to replace or delete) plus an add button. The private key is **write-only** — the form
//! takes it, the read-model only ever holds the fingerprint.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Textarea;

use super::{action_data, badge, card, drawer_form, field, labeled, row_head, submit};
use crate::bridge::{self, Command};
use crate::state::{App, Editing};

#[renderer]
pub fn column(app: &App) -> ElementBuilder {
    let mut items: Vec<ElementBuilder> = app.ssh_keys.iter().map(row).collect();
    items.push(action_data(
        "+ Add SSH key",
        open_new(),
        "act",
        "add-ssh-key",
    ));
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(items)
}

fn row(key: &crate::state::SshKeyView) -> ElementBuilder {
    card([
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::JustifyBetween])
            .append([
                row_head(&key.name, badge("deploy key")),
                action_data("Edit", open_edit(), "name", &key.name),
            ]),
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::Truncate])
            .text(&key.fingerprint),
    ])
}

/// The drawer form: `None` = add (name + key); `Some(name)` = replace/delete that key (name is the
/// immutable identity, so only the key field shows — blank keeps the stored key).
pub fn form(app: &App, key: Option<&str>) -> ElementBuilder {
    match key {
        None => drawer_form(
            save(),
            [
                field("Name", "name", "", "e.g. github-deploy", false),
                key_field("paste an OpenSSH private key"),
            ],
            [
                submit("Add", &save()),
                action_data("Cancel", super::close(), "act", "cancel"),
            ],
        ),
        Some(name) => match app.ssh_keys.iter().find(|k| k.name == name) {
            None => el(El::Div).style(Style::new().display("none")),
            Some(existing) => drawer_form(
                save(),
                [
                    labeled(
                        "Fingerprint",
                        el(El::Div)
                            .st([St::TextXs, St::TextMuted, St::Truncate])
                            .text(&existing.fingerprint),
                    ),
                    key_field("leave blank to keep the stored key"),
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

/// The multi-line private-key input — a whole OpenSSH key is pasted, so a textarea, not a field.
fn key_field(placeholder: &str) -> ElementBuilder {
    labeled(
        "Private key",
        Textarea::new()
            .name("private_key")
            .placeholder(placeholder.to_owned())
            .rows(6)
            .build(),
    )
}

#[handler]
fn open_new(app: &mut App) {
    app.editing = Some(Editing::SshKey(None));
}

#[handler]
fn open_edit(app: &mut App, ctx: &EventContext) {
    if let Some(name) = ctx.data("name") {
        app.editing = Some(Editing::SshKey(Some(name.to_owned())));
    }
}

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    let name = match &app.editing {
        Some(Editing::SshKey(Some(name))) => name.clone(),
        Some(Editing::SshKey(None)) => {
            let name = ctx.field("name").unwrap_or_default().trim().to_owned();
            if name.is_empty() {
                return;
            }
            name
        }
        _ => return,
    };
    let private_key = ctx
        .field("private_key")
        .map(str::to_owned)
        .filter(|key| !key.trim().is_empty());
    app.error = None;
    app.editing = None;
    bridge::send(Command::UpsertSshKey { name, private_key });
}

#[handler]
fn delete(app: &mut App) {
    if let Some(Editing::SshKey(Some(name))) = &app.editing {
        let name = name.clone();
        app.error = None;
        app.editing = None;
        bridge::send(Command::DeleteSshKey { name });
    }
}
