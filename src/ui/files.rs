//! The Files view: the workspace file explorer + editor, backed by rwire's `FileEditor` kit
//! (`rwire_editor`). The kit owns the whole surface — split-pane tree, breadcrumb, view/edit,
//! dirty marks, autosave toggle, mtime conflict banner, managed create/rename/delete, and keyboard
//! shortcuts — and does its own jailed disk IO through the workspace's `FsSnapshot` (scanned +
//! cached in `bridge::files`). This module is just the wiring: one renderer, one dispatcher.

use rwire::{El, ElementBuilder, St, el, handler, renderer};
use rwire_components::Text;
use rwire_editor::FileEditor;

use crate::bridge::files;
use crate::state::App;

/// The Files section of a workspace view: the `FileEditor` over the route workspace's cached
/// snapshot. The bridge scans + caches it (and points `App.editor` at it) when the
/// `/w/:wid/files` route fires `OpenFiles`; here we just render the kit and route its events.
#[renderer]
pub fn section(app: &App) -> ElementBuilder {
    let Some(workspace) = app.files_workspace.as_deref() else {
        return placeholder("Select a workspace to browse its files.");
    };
    let Some(snap) = files::snapshot_for(workspace) else {
        return placeholder("Opening the workspace…");
    };
    // The kit's root is already `Flex1/MinH0/FlexCol`; the flex tokens that matter go on the
    // `#[renderer]` wrapper (see `files_section`), or the synced region collapses to `height:auto`
    // and the SplitPane loses its bounded height (stacking the tree above the editor).
    FileEditor::new(&app.editor, snap.as_ref())
        .on_event(editor_event())
        .managed(true)
        .build()
}

/// The kit's single dispatcher: every tree click, edit, save, and managed op routes here. The kit
/// decodes the action from the event params and does the jailed IO through the cached snapshot —
/// mutating `app.editor` re-renders the section.
#[handler]
fn editor_event(app: &mut App, ctx: &rwire::EventContext) {
    let Some(workspace) = app.files_workspace.clone() else {
        return;
    };
    let Some(snap) = files::snapshot_for(&workspace) else {
        return;
    };
    app.editor.apply(snap.as_ref(), ctx);
}

/// A centered muted note for the not-yet-ready states (no workspace, or the scan still in flight).
fn placeholder(text: &str) -> ElementBuilder {
    el(El::Div)
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyCenter,
        ])
        .append([Text::body(text.to_owned()).muted().build()])
}
