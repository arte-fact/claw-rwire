//! The Files view: a framed two-pane workspace manager — a directory browser (breadcrumb
//! header, inline create prompt, entry rows with hover/active states + rename/delete)
//! beside a file editor. All filesystem work happens on the bridge (`bridge::files`); this
//! view sends `Command`s and renders the projected `App.files_*` read-models. Ported from
//! rust-claw's Files manager, adapted to the Command/renderer model.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, HandlerSpec, St, el, handler, icons, renderer};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::{App, FileKind, FileNode};
use crate::ui::widgets::{accent_submit, action_icon, icon_button, submit};

/// The Files section of a workspace view: the error banner + the two-pane browser/editor,
/// scoped to the workspace named in the route (the bridge sets `App.files_*` when the
/// `/w/:wid/files` route fires `ListDir`). No workspace selector — the workspace is the
/// route's, and a header is provided by the workspace shell.
pub fn section() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapMd])
        .append([error_banner(), panes()])
}

#[renderer]
fn error_banner(app: &App) -> ElementBuilder {
    match &app.files_error {
        Some(message) => el(El::Div)
            .st([
                St::PxMd,
                St::PySm,
                St::TextXs,
                St::TextError,
                St::BgSurface,
                St::BorderDefault,
                St::RoundedSm,
            ])
            .append([el(El::Span).text(&format!("⚠ {message}"))]),
        None => el(El::Div).style(Style::new().display("none")),
    }
}

/// One bordered block holding the tree (narrow, left) and the editor (right), separated by a
/// single divider. Fills the available height; each pane scrolls internally. Stacks on mobile.
fn panes() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::FlexCol,
            St::BorderDefault,
            St::RoundedLg,
            St::BgSurface,
            St::OverflowHidden,
        ])
        .md([St::FlexRow])
        .append([
            // Tree pane: a narrow fixed width on md+, with the divider on its right edge.
            el(El::Div)
                .style(
                    Style::new()
                        .min_height("0")
                        .min_width("0")
                        .set("flex", "0 0 16rem"),
                )
                .st([St::DisplayFlex, St::FlexCol])
                .md([St::BorderRDefault])
                .append([browser()]),
            // Editor pane: takes the remaining width.
            el(El::Div)
                .style(
                    Style::new()
                        .min_height("0")
                        .min_width("0")
                        .set("flex", "1 1 0"),
                )
                .st([St::DisplayFlex, St::FlexCol])
                .append([editor()]),
        ])
}

/// A borderless pane: a header bar (single bottom divider) above a body that fills the pane. The
/// surrounding block (`panes`) supplies the outer border; this just structures one column.
fn panel(header: ElementBuilder, body: ElementBuilder) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
        .append([
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::JustifyBetween,
                    St::GapMd,
                    St::PxMd,
                    St::PySm,
                    St::BorderBSubtle,
                ])
                .append([header]),
            body,
        ])
}

// ── Browser ────────────────────────────────────────────────────────────────────

#[renderer]
fn browser(app: &App) -> ElementBuilder {
    let header = el(El::Div)
        .style(Style::new().min_width("0"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyBetween,
            St::GapMd,
            St::Flex1,
        ])
        .append([breadcrumb(&app.files_path), create_actions()]);

    let mut body_children: Vec<ElementBuilder> = Vec::new();
    if let Some(kind) = &app.files_creating {
        body_children.push(create_row(kind, &app.files_new_name));
    }
    body_children.push(tree_inner(app));

    let body = el(El::Div)
        .style(Style::new().min_height("0"))
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::FlexCol,
            St::OverflowYAuto,
            St::PSm,
        ])
        .append(body_children);
    panel(header, body)
}

/// Clickable path crumbs: `root / sub / dir`.
fn breadcrumb(path: &str) -> ElementBuilder {
    let mut crumbs = vec![crumb("root", "")];
    let mut prefix = String::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        prefix = if prefix.is_empty() {
            segment.to_owned()
        } else {
            format!("{prefix}/{segment}")
        };
        crumbs.push(el(El::Span).st([St::TextLow]).text("/"));
        crumbs.push(crumb(segment, &prefix));
    }
    el(El::Div)
        .style(Style::new().min_width("0"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapXs,
            St::TextXs,
            St::FontMono,
            St::TextMuted,
            St::Truncate,
        ])
        .append(crumbs)
}

fn crumb(label: &str, path: &str) -> ElementBuilder {
    el(El::Button)
        .attr("type", "button")
        .data("path", path)
        .on(Ev::Click, navigate())
        .style(
            Style::new()
                .set("font", "inherit")
                .set("border", "none")
                .set("background", "transparent"),
        )
        .st([St::CursorPointer, St::TextMuted, St::FlexShrink0])
        .hover([St::TextAccent])
        .text(label)
}

/// The New-file / New-folder buttons in the browser header — the shared bordered style.
fn create_actions() -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexShrink0])
        .append([
            action_icon(icons::Icon::FileText, "File", start_new_file(), false),
            action_icon(icons::Icon::Folder, "Folder", start_new_folder(), false),
        ])
}

/// The inline create prompt shown above the listing when `+ File`/`+ Folder` is clicked.
fn create_row(kind: &str, value: &str) -> ElementBuilder {
    let label = if kind == "folder" { "folder" } else { "file" };
    el(El::Form)
        .on(Ev::Submit, submit_create())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PySm,
            St::PxXs,
            St::BorderBSubtle,
        ])
        .append([
            el(El::Span)
                .st([St::TextLow, St::TextXs, St::FlexShrink0])
                .append([icons::icon_sized(
                    if kind == "folder" {
                        icons::Icon::Folder
                    } else {
                        icons::Icon::FileText
                    },
                    13,
                )]),
            el(El::Input)
                .attr("name", "newname")
                .attr("value", value)
                .attr("autocomplete", "off")
                .at_str(rwire::At::Placeholder, &format!("new {label} name…"))
                .on(Ev::Change, new_name_changed())
                .style(Style::new().set("font", "inherit"))
                .st([
                    St::Flex1,
                    St::PxSm,
                    St::PyXs,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::BgApp,
                    St::TextHigh,
                    St::OutlineNone,
                ])
                .focus([St::BorderAccent]),
            submit("Create"),
            row_action(icons::Icon::Close, cancel_create(), None, false),
        ])
}

/// The workspace tree: indented rows for the root and the children of each expanded folder. A
/// folder row toggles its expansion (and selects it as the create target); a file row opens it.
fn tree_inner(app: &App) -> ElementBuilder {
    if app.files_tree.is_empty() && app.files_creating.is_none() {
        return el(El::Div)
            .st([St::PMd, St::TextXs, St::TextMuted, St::TextCenter])
            .text("empty workspace");
    }
    let rename = app.files_rename.as_deref();
    let pending = app.files_pending_delete.as_deref();
    let open = app.files_open.as_ref().map(|f| f.path.as_str());
    let selected = app.files_path.as_str();
    let rows: Vec<ElementBuilder> = app
        .files_tree
        .iter()
        .map(|node| {
            let row = if rename == Some(node.path.as_str()) {
                rename_row(&node.name)
            } else {
                tree_row(
                    node,
                    selected == node.path,
                    pending == Some(node.path.as_str()),
                    open == Some(node.path.as_str()),
                )
            };
            indent(node.depth, row)
        })
        .collect();
    el(El::Div).st([St::DisplayFlex, St::FlexCol]).append(rows)
}

/// Indent a tree row by its depth.
fn indent(depth: usize, row: ElementBuilder) -> ElementBuilder {
    if depth == 0 {
        return row;
    }
    el(El::Div)
        .style(Style::new().set("padding-left", &format!("{:.2}rem", depth as f32 * 0.85)))
        .append([row])
}

/// One tree row: a folder (toggle + select) or file (open) on the left, rename/delete on the
/// right. `selected` highlights the create-target folder; `active` the open file.
fn tree_row(node: &FileNode, selected: bool, pending_delete: bool, active: bool) -> ElementBuilder {
    let is_dir = node.kind == FileKind::Dir;
    let (handler, suffix) = match node.kind {
        FileKind::Dir => (toggle_dir(), "/".to_owned()),
        FileKind::Symlink => (open_file(), " →".to_owned()),
        FileKind::File => (open_file(), String::new()),
    };
    let highlight = (is_dir && selected) || (!is_dir && active);
    let name_color = if highlight {
        St::TextAccent
    } else {
        St::TextDefault
    };

    let mut leading: Vec<ElementBuilder> = Vec::new();
    if is_dir {
        let chevron = if node.expanded {
            icons::Icon::ChevronDown
        } else {
            icons::Icon::ChevronRight
        };
        leading.push(
            el(El::Span)
                .st([St::TextLow])
                .append([icons::icon_sized(chevron, 12)]),
        );
        leading.push(
            el(El::Span)
                .st([St::TextAccent])
                .append([icons::icon_sized(icons::Icon::Folder, 14)]),
        );
    } else {
        leading.push(
            el(El::Span)
                .st([St::TextLow])
                .append([icons::icon_sized(icons::Icon::FileText, 14)]),
        );
    }
    leading.push(
        el(El::Span)
            .st([St::Truncate])
            .text(&format!("{}{suffix}", node.name)),
    );
    leading.push(
        el(El::Span)
            .st([St::TextLow, St::TextXs, St::FlexShrink0])
            .text(&size_label(node)),
    );

    let open_btn = el(El::Button)
        .attr("type", "button")
        .data("path", &node.path)
        .on(Ev::Click, handler)
        .style(
            Style::new()
                .set("font", "inherit")
                .set("border", "none")
                .set("background", "transparent")
                .set("text-align", "left")
                .min_width("0"),
        )
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::Flex1,
            St::TextSm,
            name_color,
            St::CursorPointer,
            St::Truncate,
        ])
        .append(leading);

    let actions = if pending_delete {
        vec![
            el(El::Span).st([St::TextXs, St::TextError]).text("delete?"),
            row_action(icons::Icon::Trash, confirm_delete(), Some(&node.path), true),
            row_action(icons::Icon::Close, cancel_delete(), None, false),
        ]
    } else {
        vec![
            row_action(
                icons::Icon::FileText,
                start_rename(),
                Some(&node.path),
                false,
            ),
            row_action(icons::Icon::Trash, ask_delete(), Some(&node.path), false),
        ]
    };
    let mut row = el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxSm,
            St::RoundedSm,
        ])
        .hover([St::BgSubtle]);
    if highlight {
        row = row.st([St::BgSubtle]);
    }
    row.append([open_btn].into_iter().chain(actions))
}

/// The inline rename editor for one row: an (uncontrolled) input + confirm/cancel.
fn rename_row(current: &str) -> ElementBuilder {
    el(El::Form)
        .on(Ev::Submit, submit_rename())
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::PyXs,
            St::PxSm,
        ])
        .append([
            el(El::Input)
                .attr("name", "to")
                .attr("value", current)
                .attr("autocomplete", "off")
                .style(Style::new().set("font", "inherit"))
                .st([
                    St::Flex1,
                    St::PxSm,
                    St::PyXs,
                    St::BorderDefault,
                    St::RoundedSm,
                    St::BgApp,
                    St::TextHigh,
                    St::OutlineNone,
                ])
                .focus([St::BorderAccent]),
            submit("Rename"),
            row_action(icons::Icon::Close, cancel_rename(), None, false),
        ])
}

/// A row's inline icon action (rename / delete / cancel) — the shared bare-icon button,
/// with the target `path` on `data-path` when the handler needs it.
fn row_action(
    icon: icons::Icon,
    on_click: HandlerSpec,
    path: Option<&str>,
    danger: bool,
) -> ElementBuilder {
    let button = icon_button(icon, on_click, danger);
    match path {
        Some(p) => button.data("path", p),
        None => button,
    }
}

// ── Editor ─────────────────────────────────────────────────────────────────────

#[renderer]
fn editor(app: &App) -> ElementBuilder {
    let Some(open) = &app.files_open else {
        return panel(
            el(El::Span)
                .st([St::TextXs, St::TextMuted, St::FontMono])
                .text("editor"),
            placeholder("Select a file to view or edit it."),
        );
    };

    let filename = el(El::Span)
        .style(Style::new().min_width("0"))
        .st([St::TextSm, St::FontMono, St::TextHigh, St::Truncate])
        .text(&open.path);

    if !open.editable {
        let header = header_row([filename, el(El::Span).style(Style::new().display("none"))]);
        return panel(
            header,
            placeholder(open.note.as_deref().unwrap_or("Not editable.")),
        );
    }

    if app.files_view_mode {
        // View mode: render Markdown, or syntax-highlight code, for clean reading.
        let controls = el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexShrink0])
            .append([mode_toggle(true)]);
        let body = el(El::Div)
            .style(Style::new().min_height("0"))
            .st([St::Flex1, St::OverflowYAuto, St::PMd])
            .append([rendered_view(open)]);
        return panel(header_row([filename, controls]), body);
    }

    // Edit mode: the editor panel IS a form, so the header's Save (a submit button) reads the
    // body's textarea. Keyed by path so switching files yields a fresh, re-seeded textarea.
    let field_id = format!("editor-{}", open.path);
    let lines = open.content.lines().count().max(1);
    let chars = open.content.chars().count();
    let controls = el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexShrink0])
        .append([
            mode_toggle(false),
            accent_submit(icons::Icon::Check, "Save"),
        ]);
    el(El::Form)
        .on(Ev::Submit, save_file())
        .style(Style::new().min_height("0"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol])
        .append([
            el(El::Div)
                .st([
                    St::DisplayFlex,
                    St::ItemsCenter,
                    St::JustifyBetween,
                    St::GapMd,
                    St::PxMd,
                    St::PySm,
                    St::BorderBSubtle,
                ])
                .append([header_row([filename, controls])]),
            el(El::Textarea)
                .id(&field_id)
                .attr("name", "content")
                .attr("spellcheck", "false")
                .st([
                    St::Flex1,
                    St::WFull,
                    St::PMd,
                    St::BgApp,
                    St::TextDefault,
                    St::FontMono,
                    St::TextXs,
                ])
                .style(
                    Style::new()
                        .set("outline", "none")
                        .set("border", "none")
                        .set("resize", "none")
                        .min_height("0")
                        .set("white-space", "pre"),
                )
                .text(&open.content),
            el(El::Div)
                .st([
                    St::PxMd,
                    St::PyXs,
                    St::BorderTSubtle,
                    St::TextLow,
                    St::TextXs,
                    St::FontMono,
                ])
                .text(&format!("{lines} lines · {chars} chars")),
        ])
}

/// The rendered body for View mode: a Markdown render for `.md`, else syntax-highlighted code
/// (flat text for an unrecognised extension).
fn rendered_view(open: &crate::state::OpenFile) -> ElementBuilder {
    if is_markdown(&open.path) {
        crate::ui::widgets::markdown(open.content.clone())
    } else {
        rwire_markdown::highlight_code(&open.content, lang_for_path(&open.path))
    }
}

/// The View | Edit switch (immediate-apply chips). `view` is the current mode.
fn mode_toggle(view: bool) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs])
        .append([
            mode_chip("View", true, view),
            mode_chip("Edit", false, !view),
        ])
}

fn mode_chip(label: &str, view: bool, active: bool) -> ElementBuilder {
    let chip = el(El::Button)
        .attr("type", "button")
        .data("view", if view { "1" } else { "0" })
        .on(Ev::Click, set_files_view())
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::CursorPointer,
        ])
        .text(label);
    if active {
        chip.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        chip.st([St::BgApp, St::TextMuted]).hover([St::TextHigh])
    }
}

/// Whether `path` is a Markdown file (rendered as prose in View mode).
fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// The highlighter language for a file extension (for View-mode code rendering), or `None` to
/// render flat. Markdown is handled separately (rendered as prose, not highlighted).
fn lang_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext)?;
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "sql" => Some("sql"),
        "json" => Some("json"),
        "sh" | "bash" => Some("bash"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

/// A centered placeholder body for the editor panel (no file / not editable).
fn placeholder(text: &str) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0"))
        .st([
            St::Flex1,
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyCenter,
            St::PMd,
            St::TextSm,
            St::TextMuted,
        ])
        .append([Text::body(text.to_owned()).muted().build()])
}

/// A spaced filename + action header row (used inside the panel's header bar / form).
fn header_row(children: [ElementBuilder; 2]) -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_width("0"))
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::JustifyBetween,
            St::GapMd,
            St::Flex1,
        ])
        .append(children)
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(p, _)| p.to_owned())
}

fn size_label(node: &FileNode) -> String {
    if node.kind == FileKind::Dir {
        return String::new();
    }
    let size = node.size;
    if size >= 1 << 20 {
        format!("{}M", size >> 20)
    } else if size >= 1 << 10 {
        format!("{}K", size >> 10)
    } else {
        format!("{size}B")
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────────

/// Navigate to (select) the directory named by the clicked element's `data-path` — the breadcrumb.
#[handler]
fn navigate(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(path)) = (app.files_workspace.clone(), ctx.data("path")) {
        bridge::send(Command::ListDir {
            workspace,
            path: path.to_owned(),
        });
    }
}

/// Toggle a folder's expansion (and select it as the create target) — a tree folder-row click.
#[handler]
fn toggle_dir(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(path)) = (app.files_workspace.clone(), ctx.data("path")) {
        bridge::send(Command::ToggleDir {
            workspace,
            path: path.to_owned(),
        });
    }
}

#[handler]
fn open_file(app: &mut App, ctx: &EventContext) {
    if let (Some(workspace), Some(path)) = (app.files_workspace.clone(), ctx.data("path")) {
        app.files_view_mode = true; // open in View mode (read-first); toggle to Edit to change it
        bridge::send(Command::OpenFile {
            workspace,
            path: path.to_owned(),
        });
    }
}

/// Switch the editor between rendered View (`data-view=1`) and raw Edit (`0`).
#[handler]
fn set_files_view(app: &mut App, ctx: &EventContext) {
    app.files_view_mode = ctx.data("view") == Some("1");
}

#[handler]
fn start_new_file(app: &mut App) {
    app.files_creating = Some("file".to_owned());
    app.files_new_name.clear();
}

#[handler]
fn start_new_folder(app: &mut App) {
    app.files_creating = Some("folder".to_owned());
    app.files_new_name.clear();
}

#[handler]
fn cancel_create(app: &mut App) {
    app.files_creating = None;
    app.files_new_name.clear();
}

#[handler]
fn new_name_changed(app: &mut App, ctx: &EventContext) {
    app.files_new_name = ctx.text().unwrap_or_default().to_owned();
}

#[handler]
fn submit_create(app: &mut App, ctx: &EventContext) {
    let Some(workspace) = app.files_workspace.clone() else {
        return;
    };
    // The name is controlled in `files_new_name`, but a fast submit may beat the change
    // event — fall back to the form field.
    let name = ctx
        .field("newname")
        .map(str::to_owned)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| app.files_new_name.clone());
    let name = name.trim().to_owned();
    if name.is_empty() {
        return;
    }
    let path = join(&app.files_path, &name);
    match app.files_creating.as_deref() {
        Some("folder") => bridge::send(Command::NewFolder { workspace, path }),
        _ => bridge::send(Command::WriteFile {
            workspace,
            path,
            content: String::new(),
        }),
    }
}

#[handler]
fn start_rename(app: &mut App, ctx: &EventContext) {
    if let Some(path) = ctx.data("path") {
        app.files_rename = Some(path.to_owned());
    }
}

#[handler]
fn cancel_rename(app: &mut App) {
    app.files_rename = None;
}

#[handler]
fn submit_rename(app: &mut App, ctx: &EventContext) {
    let (Some(workspace), Some(from)) = (app.files_workspace.clone(), app.files_rename.take())
    else {
        return;
    };
    let name = ctx.field("to").unwrap_or_default().trim().to_owned();
    if name.is_empty() {
        return;
    }
    // Rename within the entry's own directory (it may not be the selected dir).
    let to = join(&parent_of(&from), &name);
    bridge::send(Command::RenameEntry {
        workspace,
        from,
        to,
    });
}

#[handler]
fn ask_delete(app: &mut App, ctx: &EventContext) {
    if let Some(path) = ctx.data("path") {
        app.files_pending_delete = Some(path.to_owned());
    }
}

#[handler]
fn cancel_delete(app: &mut App) {
    app.files_pending_delete = None;
}

#[handler]
fn confirm_delete(app: &mut App) {
    if let (Some(workspace), Some(path)) =
        (app.files_workspace.clone(), app.files_pending_delete.take())
    {
        bridge::send(Command::DeleteEntry { workspace, path });
    }
}

#[handler]
fn save_file(app: &mut App, ctx: &EventContext) {
    let (Some(workspace), Some(open)) = (app.files_workspace.clone(), app.files_open.as_ref())
    else {
        return;
    };
    let path = open.path.clone();
    let content = ctx.field("content").unwrap_or_default().to_owned();
    bridge::send(Command::WriteFile {
        workspace,
        path,
        content,
    });
}

#[cfg(test)]
mod tests {
    use super::{is_markdown, lang_for_path};

    #[test]
    fn markdown_is_detected_case_insensitively() {
        assert!(is_markdown("docs/README.md"));
        assert!(is_markdown("notes.MARKDOWN"));
        assert!(!is_markdown("src/main.rs"));
        assert!(!is_markdown("noext"));
    }

    #[test]
    fn lang_for_path_maps_known_extensions() {
        assert_eq!(lang_for_path("src/main.rs"), Some("rust"));
        assert_eq!(lang_for_path("db/schema.SQL"), Some("sql"));
        assert_eq!(lang_for_path("Cargo.toml"), Some("toml"));
        assert_eq!(lang_for_path("ci.yml"), Some("yaml"));
        assert_eq!(lang_for_path("run.sh"), Some("bash"));
        // Unknown / extensionless → flat (None).
        assert_eq!(lang_for_path("data.bin"), None);
        assert_eq!(lang_for_path("Makefile"), None);
    }
}
