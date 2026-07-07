//! The project page's Remote panel (P5b): the deploy origin URL + which stored SSH deploy key to
//! push under. Saved together via `SetProjectRemote`, which also points the repo's `origin` remote
//! at the URL so pushes and the sync badges work through one name. The key list is the System page's
//! stored keys (name + fingerprint) — never any private material.

use rwire::{El, ElementBuilder, Ev, St, el, handler};
use rwire_components::{Input, Select};

use crate::bridge::{self, Command};
use crate::state::{App, ProjectDetail};
use crate::ui::widgets::submit;

pub(super) fn remote_panel(app: &App, detail: &ProjectDetail) -> ElementBuilder {
    let mut key_select = Select::new()
        .name("deploy_key")
        .value(detail.deploy_key.clone())
        .option("", "None (ssh-agent / default)");
    for key in &app.ssh_keys {
        key_select = key_select.option(
            key.name.clone(),
            format!("{} · {}", key.name, key.fingerprint),
        );
    }
    let form = el(El::Form)
        .on(Ev::Submit, save())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            labeled(
                "Origin URL",
                Input::text()
                    .name("origin")
                    .value(detail.origin.clone())
                    .placeholder("git@github.com:you/repo.git")
                    .build(),
            ),
            labeled("Deploy key", key_select.build()),
            el(El::Div)
                .st([St::DisplayFlex, St::GapSm, St::ItemsCenter])
                .append([submit("Save remote")]),
        ]);
    super::panel("Remote", form)
}

/// A stacked label + control — the project page has no shared `labeled`, so a small local one.
fn labeled(label: &str, control: ElementBuilder) -> ElementBuilder {
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

#[handler]
fn save(app: &mut App, ctx: &EventContext) {
    let Some(detail) = app.project_detail.as_ref() else {
        return;
    };
    let workspace = detail.workspace_id.clone();
    let project = detail.name.clone();
    // A form submit serializes its named controls, so the select's value arrives via `field`.
    let origin = ctx.field("origin").unwrap_or_default().trim().to_owned();
    let deploy_key = ctx
        .field("deploy_key")
        .unwrap_or_default()
        .trim()
        .to_owned();
    bridge::send(Command::SetProjectRemote {
        workspace,
        project,
        origin,
        deploy_key,
    });
}
