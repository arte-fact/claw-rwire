//! The project page (P1 → P5c): `/w/:wid/projects/:pid` — a project's control center. A
//! read-model over the directory's kind, its branches' file trees + CI pipelines (P5c), and the
//! requests that touched it (projects are derived, no table). Kind is the paved-road bundle; the
//! operator can override detection here. Split into submodules as the surface grew:
//! [`tree`] owns the branch file browser.

mod pipeline;
mod release;
mod remote;
mod tree;

use rwire::{El, ElementBuilder, EventContext, Link, St, el, handler, renderer};
use rwire_components::{Badge, BadgeFill, BadgeIntent, BadgeShape, Select, Text};

use crate::bridge::{self, Command};
use crate::state::{App, ProjectDetail};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .st([St::Flex1, St::MinH0, St::OverflowYAuto, St::PMd])
        .append([body()])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    let Some(detail) = &app.project_detail else {
        return Text::body("Loading the project…").muted().build();
    };
    let sections = vec![
        header(app, detail),
        tree::switcher(detail),
        pipeline::pipeline_panel(detail),
        release::release_panel(detail),
        remote::remote_panel(app, detail),
        settings_panel(detail),
        tree::files_panel(detail),
        requests_panel(detail),
        crate::ui::log_drawer::drawer(app),
    ];
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg, St::MaxW80rem])
        .append(sections)
}

/// Breadcrumb (workspace › project) with the kind badge riding the crumb tail (N0/§3).
fn header(app: &App, detail: &ProjectDetail) -> ElementBuilder {
    use rwire_components::Breadcrumb;
    let workspace = app
        .workspaces
        .iter()
        .find(|w| w.id == detail.workspace_id)
        .map(|w| w.name.clone())
        .unwrap_or_else(|| detail.workspace_id.clone());
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append([
            Breadcrumb::new()
                .item(workspace, Some(format!("/w/{}", detail.workspace_id)))
                .item(detail.name.clone(), None::<String>)
                .build(),
            kind_badge(&detail.kind_slug),
        ])
}

/// The kind as a square outline badge (§6 inventory: Badge Square·Outline).
fn kind_badge(slug: &str) -> ElementBuilder {
    Badge::new()
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .intent(BadgeIntent::Primary)
        .text(slug.to_owned())
        .build()
}

/// The one interactive P1 cell: the kind override. Detection is the default; the operator
/// picks a kind to pin it (or "auto" to clear the override and re-detect).
fn settings_panel(detail: &ProjectDetail) -> ElementBuilder {
    let source = if detail.kind_overridden {
        "operator override"
    } else {
        "detected"
    };
    let select = Select::new()
        .name("kind")
        .option("auto", "Auto-detect")
        .option("rwire-app", "rwire-app")
        .option("rust-bin", "rust-bin")
        .option("node-app", "node-app")
        .option("docs", "docs")
        .option("other", "other")
        .value(if detail.kind_overridden {
            detail.kind_slug.clone()
        } else {
            "auto".to_owned()
        })
        .on_change(set_kind());
    panel(
        "Kind",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append([
                el(El::Div)
                    .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                    .append([
                        kind_badge(&detail.kind_slug),
                        el(El::Span).st([St::TextXs, St::TextMuted]).text(source),
                    ]),
                select,
                el(El::Div).st([St::TextXs, St::TextMuted]).text(
                    "Kind selects the review checks, preview command, and the \
                           conventions injected into every task in this project.",
                ),
            ]),
    )
}

/// The project's changelog in ask-language — each request a client-routed link (§3).
fn requests_panel(detail: &ProjectDetail) -> ElementBuilder {
    if detail.requests.is_empty() {
        return panel(
            "Requests",
            Text::body("No requests have touched this project yet.")
                .muted()
                .build(),
        );
    }
    let rows: Vec<ElementBuilder> = detail
        .requests
        .iter()
        .map(|req| {
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::GapSm,
                    St::PySm,
                    St::BorderTSubtle,
                ])
                .append([
                    Link::route(&format!("/w/{}/requests/{}", detail.workspace_id, req.id))
                        .st([
                            St::TextSm,
                            St::TextAccent,
                            St::Flex1,
                            St::MinW0,
                            St::Truncate,
                        ])
                        .text(&req.title),
                    status_badge(&req.status),
                ])
        })
        .collect();
    panel(
        "Requests that touched this project",
        el(El::Div).st([St::DisplayFlex, St::FlexCol]).append(rows),
    )
}

/// A request status as a small outline badge, tinted by lifecycle.
fn status_badge(status: &str) -> ElementBuilder {
    let intent = match status {
        "delivered" => BadgeIntent::Success,
        "rejected" | "abandoned" | "rolled-back" => BadgeIntent::Error,
        "validating" => BadgeIntent::Warning,
        _ => BadgeIntent::Primary,
    };
    Badge::new()
        .shape(BadgeShape::Square)
        .fill(BadgeFill::Outline)
        .intent(intent)
        .text(status.to_owned())
        .build()
}

/// A bordered panel — label over content in a hairline box (the request page's `section`).
/// Shared with the page's submodules.
pub(super) fn panel(title: &str, content: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .st([
            St::BorderDefault,
            St::BgSurface,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append([
            el(El::Div)
                .st([
                    St::TextXs,
                    St::TextMuted,
                    St::TrackingWide,
                    St::TextUppercase,
                ])
                .text(title),
            content,
        ])
}

#[handler]
fn set_kind(app: &mut App, ctx: &EventContext) {
    if let Some(command) = kind_command(app.project_detail.as_ref(), ctx) {
        bridge::send(command);
    }
}

/// The pure core: a `<select>` change delivers its value as a `Text` payload (`ctx.text()`).
/// "auto" clears the override (empty slug); any real slug pins it.
fn kind_command(detail: Option<&ProjectDetail>, ctx: &EventContext) -> Option<Command> {
    let detail = detail?;
    let value = ctx.text().unwrap_or_default();
    let slug = if value == "auto" {
        String::new()
    } else {
        value.to_owned()
    };
    Some(Command::SetProjectKind {
        workspace: detail.workspace_id.clone(),
        project: detail.name.clone(),
        slug,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> ProjectDetail {
        ProjectDetail {
            workspace_id: "ws-1".to_owned(),
            name: "site".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn picking_a_kind_pins_that_slug() {
        match kind_command(Some(&detail()), &EventContext::from_text("rust-bin")) {
            Some(Command::SetProjectKind {
                workspace,
                project,
                slug,
            }) => {
                assert_eq!((workspace.as_str(), project.as_str()), ("ws-1", "site"));
                assert_eq!(slug, "rust-bin");
            }
            _ => panic!("expected SetProjectKind pinning the slug"),
        }
    }

    #[test]
    fn auto_clears_the_override() {
        match kind_command(Some(&detail()), &EventContext::from_text("auto")) {
            Some(Command::SetProjectKind { slug, .. }) => assert_eq!(slug, ""),
            _ => panic!("expected SetProjectKind clearing the override"),
        }
    }

    #[test]
    fn a_kind_change_without_a_loaded_project_is_ignored() {
        assert!(kind_command(None, &EventContext::from_text("rust-bin")).is_none());
    }
}
