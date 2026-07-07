//! The project page's Release control (P5a): merge the project's `develop` into `main` — a
//! working-tree-less, CI-gated merge — and push `main` iff an origin exists. Project-level: the
//! whole develop line is released, not one request.

use rwire::{At, El, ElementBuilder, St, el, handler};
use rwire_components::{
    Badge, BadgeFill, BadgeIntent, BadgeShape, Button, ButtonSize, StatusDot, StatusDotIntent, Text,
};

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
    let mut content = vec![Text::body(status).muted().build()];
    if let Some(badges) = sync_badges(detail) {
        content.push(badges);
    }
    if let Some(link) = main_preview_link(detail) {
        content.push(link);
    }
    content.push(
        button
            .on_click(release())
            .data("workspace", &detail.workspace_id)
            .data("project", &detail.name),
    );
    // A definite conflict blocks the Release — say so plainly (distinct from "nothing to release").
    if detail.release_conflict {
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

/// `origin/main` sync as outline badges (P5a): unpushed local commits (primary) and behind-origin
/// commits (warning). `None` when in sync or there's no remote — no news is shown.
fn sync_badges(detail: &ProjectDetail) -> Option<ElementBuilder> {
    if detail.origin_ahead == 0 && detail.origin_behind == 0 {
        return None;
    }
    let mut badges = Vec::new();
    if detail.origin_ahead > 0 {
        badges.push(sync_badge(
            BadgeIntent::Primary,
            format!("↑ {} unpushed", detail.origin_ahead),
        ));
    }
    if detail.origin_behind > 0 {
        badges.push(sync_badge(
            BadgeIntent::Warning,
            format!("↓ {} behind origin", detail.origin_behind),
        ));
    }
    Some(
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(badges),
    )
}

/// One sync badge — the §6 inventory shape (Square·Outline), tinted by intent.
fn sync_badge(intent: BadgeIntent, text: String) -> ElementBuilder {
    Badge::new()
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .intent(intent)
        .text(text)
        .build()
}

/// The main-staging preview as a pulsing external link (P5a) — the released line's live URL, same
/// `target=_blank` same-origin shape as the develop-staging link. `None` until a Release has started
/// a live server.
fn main_preview_link(detail: &ProjectDetail) -> Option<ElementBuilder> {
    if detail.main_preview_uri.is_empty() {
        return None;
    }
    Some(
        el(El::A)
            .at_str(At::Href, &detail.main_preview_uri)
            .at_str(At::Target, "_blank")
            .at_str(At::Rel, "noopener")
            .st([
                St::TextXs,
                St::TextAccent,
                St::NoUnderline,
                St::CursorPointer,
                St::DisplayFlex,
                St::ItemsCenter,
                St::GapXs,
            ])
            .hover([St::Underline])
            .append([
                StatusDot::new()
                    .intent(StatusDotIntent::Success)
                    .pulse(true)
                    .build(),
                el(El::Span).text("Preview main-staging ↗"),
            ]),
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
