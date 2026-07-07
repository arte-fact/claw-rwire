//! The project page (P1 → P5c): `/w/:wid/projects/:pid` — a project's control center. A
//! read-model over the directory's branches' file trees + CI pipelines (P5c), its release/remote
//! panels, and the requests that touched it (projects are derived, no table). Split into submodules
//! as the surface grew: [`tree`] owns the branch file browser.

mod pipeline;
mod release;
mod remote;
mod tree;

use rwire::{El, ElementBuilder, Link, St, el, renderer};
use rwire_components::{Badge, BadgeFill, BadgeIntent, BadgeShape, Text};

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
        tree::files_panel(detail),
        requests_panel(detail),
        crate::ui::log_drawer::drawer(app),
    ];
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg, St::MaxW80rem])
        .append(sections)
}

/// Breadcrumb (workspace › project).
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
        .append([Breadcrumb::new()
            .item(workspace, Some(format!("/w/{}", detail.workspace_id)))
            .item(detail.name.clone(), None::<String>)
            .build()])
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
