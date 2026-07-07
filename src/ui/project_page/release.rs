//! The project page's Release control (P5a): merge the project's `develop` into `main` — a
//! working-tree-less, CI-gated merge — and push `main` iff an origin exists. Project-level: the
//! whole develop line is released, not one request.

use rwire::{El, ElementBuilder, St, el, handler};
use rwire_components::{Button, ButtonSize, Text};

use crate::bridge::{self, Command};
use crate::state::{App, ProjectDetail};

/// How far develop leads main + a Release button, enabled only when a Release can run (develop
/// leads main and the merge is conflict-free).
pub(super) fn release_panel(detail: &ProjectDetail) -> ElementBuilder {
    let status = if detail.develop_ahead == 0 {
        "main is up to date with develop — nothing to release.".to_owned()
    } else {
        let plural = if detail.develop_ahead == 1 {
            "commit"
        } else {
            "commits"
        };
        format!("develop leads main by {} {plural}.", detail.develop_ahead)
    };
    let button = Button::primary("Release develop → main").size(ButtonSize::Sm);
    let button = if detail.releasable {
        button
    } else {
        button.disabled(true)
    };
    let mut content = vec![
        Text::body(status).muted().build(),
        button
            .on_click(release())
            .data("workspace", &detail.workspace_id)
            .data("project", &detail.name),
    ];
    // Blocked while develop is ahead ⇒ the merge would conflict — say so plainly.
    if detail.develop_ahead > 0 && !detail.releasable {
        content.push(
            Text::body("Blocked — develop and main conflict; reconcile on develop first.")
                .muted()
                .build(),
        );
    }
    super::panel(
        "Release",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(content),
    )
}

#[handler]
fn release(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if let (Some(workspace), Some(project)) = (ctx.data("workspace"), ctx.data("project")) {
        bridge::send(Command::ReleaseProject {
            workspace: workspace.to_owned(),
            project: project.to_owned(),
        });
    }
}
