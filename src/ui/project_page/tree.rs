//! The project page's branch browser (P5c): a branch switcher + the selected branch's tree,
//! rendered with rwire's `FileTree`. The projected `ProjectTreeEntry` list maps to `FsEntry` here
//! — the rwire type derives no `Eq`, so it can't be a view field and is built at render.

use rwire::{El, ElementBuilder, EventContext, St, el, handler};
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
    let mut select = Select::new().name("branch");
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

/// Re-project the page for the chosen branch.
#[handler]
fn switch_branch(app: &mut App, ctx: &EventContext) {
    if let Some(command) = branch_command(app.project_detail.as_ref(), ctx) {
        bridge::send(command);
    }
}

/// The pure core: a `<select>` change delivers a `Form` payload, so the value is read with
/// `ctx.field(name)`, not `ctx.text()` (which is `None` for a select — the bug this guards).
fn branch_command(detail: Option<&ProjectDetail>, ctx: &EventContext) -> Option<Command> {
    let detail = detail?;
    let branch = ctx.field("branch")?;
    Some(Command::LoadProjectDetail {
        workspace: detail.workspace_id.clone(),
        project: detail.name.clone(),
        branch: Some(branch.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form(fields: serde_json::Value) -> EventContext {
        EventContext::new(json!({ "t": "form", "v": fields }).to_string().into_bytes())
    }

    fn detail() -> ProjectDetail {
        ProjectDetail {
            workspace_id: "ws-1".to_owned(),
            name: "site".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_branch_pick_re_projects_for_that_branch() {
        // The bug this guards: a <select> change is a Form payload read via ctx.field. The old
        // ctx.text() read None here, so the switcher silently did nothing.
        match branch_command(Some(&detail()), &form(json!({ "branch": "main" }))) {
            Some(Command::LoadProjectDetail {
                workspace,
                project,
                branch,
            }) => {
                assert_eq!((workspace.as_str(), project.as_str()), ("ws-1", "site"));
                assert_eq!(branch.as_deref(), Some("main"));
            }
            _ => panic!("expected LoadProjectDetail for the picked branch"),
        }
    }

    #[test]
    fn a_text_payload_yields_no_command() {
        // A select never delivers Text; the old ctx.text() path is what broke, so reading it must
        // find nothing.
        assert!(branch_command(Some(&detail()), &EventContext::from_text("main")).is_none());
    }

    #[test]
    fn a_switch_without_a_loaded_project_is_ignored() {
        assert!(branch_command(None, &form(json!({ "branch": "main" }))).is_none());
    }
}
