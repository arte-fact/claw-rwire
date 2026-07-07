//! The project page's per-branch CI pipeline + preview link (P5c): the selected branch's producer
//! runs, each a badge strip via the shared `run_chain::checks_row` — so a badge opens that run's
//! full log in the shared logs drawer, exactly as on the task thread — plus, on develop, a live
//! link to the develop-staging preview.

use rwire::{At, El, ElementBuilder, St, el};
use rwire_components::{StatusDot, StatusDotIntent, Text};

use crate::state::{BranchPipelineRun, ProjectDetail};

/// The selected branch's CI panel: the develop-staging preview link (when live) over one row per
/// producer run, or an honest branch-aware note when the branch has no recorded CI.
pub(super) fn pipeline_panel(detail: &ProjectDetail) -> ElementBuilder {
    let mut content: Vec<ElementBuilder> = Vec::new();
    if let Some(link) = preview_link(detail) {
        content.push(link);
    }
    if detail.pipeline.is_empty() {
        content.push(empty_note(detail));
    } else {
        let runs: Vec<ElementBuilder> = detail
            .pipeline
            .iter()
            .enumerate()
            .map(|(index, run)| run_row(detail, run, index))
            .collect();
        content.push(
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapSm])
                .append(runs),
        );
    }
    super::panel(
        &format!("Pipeline · {}", detail.selected_branch),
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(content),
    )
}

/// One run: the task's title over its badge strip. The strip reuses `checks_row`, so each badge
/// opens that run's full log — the `run_id` is a real `task_runs.id` with `ci_logs` rows.
fn run_row(detail: &ProjectDetail, run: &BranchPipelineRun, index: usize) -> ElementBuilder {
    let key = format!("proj-ci-{}-{index}", detail.selected_branch);
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            el(El::Div)
                .st([St::TextXs, St::TextMuted, St::Truncate])
                .text(&run.task_title),
            crate::ui::run_chain::checks_row(&run.run_id, &run.checks, &key),
        ])
}

/// The develop-staging preview as a pulsing external link — same shape as the request page's, a
/// real `target=_blank` navigation to the same-origin `/preview/<key>/` (not a client route).
fn preview_link(detail: &ProjectDetail) -> Option<ElementBuilder> {
    if detail.preview_uri.is_empty() {
        return None;
    }
    Some(
        el(El::A)
            .at_str(At::Href, &detail.preview_uri)
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
                el(El::Span).text("Preview develop-staging ↗"),
            ]),
    )
}

fn empty_note(detail: &ProjectDetail) -> ElementBuilder {
    let text = if detail.selected_branch == crate::gitops::DEVELOP {
        "No free-lane CI recorded on develop yet — request work is checked on its req/* lane before \
         it merges here."
    } else {
        "No branch-scoped CI — passes are checked on req/* lanes before merge."
    };
    Text::body(text).muted().build()
}
