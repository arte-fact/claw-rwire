//! The project page's branch browser (P5c): a branch switcher + the selected branch's tree,
//! rendered with rwire's `FileTree`. The projected `ProjectTreeEntry` list maps to `FsEntry` here
//! — the rwire type derives no `Eq`, so it can't be a view field and is built at render.

use rwire::{El, ElementBuilder, St, el, handler};
use rwire_components::{FileTree, FsEntry, Select, Text};

use crate::bridge::{self, Command};
use crate::state::{App, ProjectDetail};

/// Expand every branch only for a small tree; a big one renders collapsed (native `<details>`).
const EXPAND_ALL_UNDER: usize = 100;

/// The branch switcher — pick a branch and the whole page re-projects for it. Hidden for a repo
/// with no branches (no commits yet).
pub(super) fn switcher(detail: &ProjectDetail) -> ElementBuilder {
    if detail.branches.is_empty() {
        return el(El::Div);
    }
    let mut select = Select::new();
    for branch in &detail.branches {
        select = select.option(branch.clone(), branch.clone());
    }
    let select = select
        .value(detail.selected_branch.clone())
        .on_change(switch_branch());
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append([
            el(El::Span)
                .st([
                    St::TextXs,
                    St::TextMuted,
                    St::TextUppercase,
                    St::TrackingWide,
                ])
                .text("Branch"),
            select,
        ])
}

/// The selected branch's files as a read-only tree panel; an empty tree shows a muted note.
pub(super) fn files_panel(detail: &ProjectDetail) -> ElementBuilder {
    if detail.tree.is_empty() {
        return super::panel(
            "Files",
            Text::body("No files on this branch.").muted().build(),
        );
    }
    let entries: Vec<FsEntry> = detail
        .tree
        .iter()
        .map(|entry| FsEntry {
            rel: entry.rel.clone(),
            name: entry.name.clone(),
            is_dir: entry.is_dir,
            depth: entry.depth,
        })
        .collect();
    let tree = FileTree::new(&entries);
    let tree = if entries.len() <= EXPAND_ALL_UNDER {
        tree.expand_all()
    } else {
        tree
    };
    super::panel("Files", tree.build())
}

/// Re-project the page for the chosen branch. A nameless `Select` delivers its value as text.
#[handler]
fn switch_branch(app: &mut App, ctx: &EventContext) {
    let Some(detail) = &app.project_detail else {
        return;
    };
    let Some(branch) = ctx.text() else {
        return;
    };
    bridge::send(Command::LoadProjectDetail {
        workspace: detail.workspace_id.clone(),
        project: detail.name.clone(),
        branch: Some(branch.to_owned()),
    });
}
