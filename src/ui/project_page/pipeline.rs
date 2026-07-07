//! The project page's per-branch CI pipeline (P5c): the selected branch's producer runs, each a
//! badge strip via the shared `run_chain::checks_row` — so a badge opens that run's full log in the
//! shared logs drawer, exactly as on the task thread.

use rwire::{El, ElementBuilder, St, el};
use rwire_components::Text;

use crate::state::{BranchPipelineRun, ProjectDetail};

/// The selected branch's CI panel — one row per producer run, or an honest branch-aware note.
pub(super) fn pipeline_panel(detail: &ProjectDetail) -> ElementBuilder {
    if detail.pipeline.is_empty() {
        return super::panel("Pipeline", empty_note(detail));
    }
    let runs: Vec<ElementBuilder> = detail
        .pipeline
        .iter()
        .enumerate()
        .map(|(index, run)| run_row(detail, run, index))
        .collect();
    super::panel(
        &format!("Pipeline · {}", detail.selected_branch),
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append(runs),
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

fn empty_note(detail: &ProjectDetail) -> ElementBuilder {
    let text = if detail.selected_branch == crate::gitops::DEVELOP {
        "No free-lane CI recorded on develop yet — request work is checked on its req/* lane before \
         it merges here."
    } else {
        "No branch-scoped CI — passes are checked on req/* lanes before merge."
    };
    Text::body(text).muted().build()
}
