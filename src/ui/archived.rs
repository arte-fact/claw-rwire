//! The Archived conversations screen: a dedicated page (reached from the nav footer)
//! listing archived threads with Delete. With the one persistent concierge conversation
//! (R1) there is no active list to restore into, so archived threads are historical
//! records — kept until deleted. Delete is irreversible, so it's gated behind an inline
//! confirm via `App.pending_delete`. The list is `App.archived`, filled by
//! `Command::ListArchived` when the `/archived` route opens (see `on_route`).

use rwire::style::Style;
use rwire::{El, ElementBuilder, HandlerSpec, St, el, handler, icons, renderer};
use rwire_components::Text;

use crate::bridge::{self, Command};
use crate::state::{App, ChatSummary};
use crate::ui::widgets::page_header;

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Archived",
                "Past conversations, kept for the record — delete permanently what you no longer need.",
                el(El::Div).style(Style::new().display("none")),
            ),
            list(),
        ])
}

#[renderer]
fn list(app: &App) -> ElementBuilder {
    if app.archived.is_empty() {
        return el(El::Div)
            .st([St::PySm])
            .append([Text::body("No archived conversations.").muted().build()]);
    }
    let pending = app.pending_delete.as_deref();
    el(El::Div).st([St::DisplayFlex, St::FlexCol]).append(
        app.archived
            .iter()
            .map(|chat| row(chat, pending == Some(chat.id.as_str()))),
    )
}

/// One archived thread: title on the left, actions on the right. When this row is the
/// one awaiting confirmation, the actions become a red "Delete permanently?" prompt
/// with confirm / cancel.
fn row(chat: &ChatSummary, pending_delete: bool) -> ElementBuilder {
    let title = Text::body(chat.title.clone())
        .build()
        .style(Style::new().min_width("0"))
        .st([St::Flex1, St::Truncate, St::TextHigh]);
    let mut children = vec![title];
    if pending_delete {
        children.push(
            el(El::Span)
                .st([St::TextXs, St::TextError])
                .text("Delete permanently?"),
        );
        children.push(button(
            icons::Icon::Trash,
            "Delete",
            confirm_delete(),
            None,
            true,
        ));
        children.push(button(
            icons::Icon::Close,
            "Cancel",
            cancel_delete(),
            None,
            false,
        ));
    } else {
        children.push(button(
            icons::Icon::Trash,
            "Delete",
            ask_delete(),
            Some(&chat.id),
            false,
        ));
    }
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapMd,
            St::PyMd,
            St::BorderBDefault,
        ])
        .append(children)
}

/// A small icon+label action button (the shared style). `chat_id` rides on `data-chat`
/// for the handlers that target a specific row; `danger` colours it as destructive.
fn button(
    icon: icons::Icon,
    label: &str,
    on_click: HandlerSpec,
    chat_id: Option<&str>,
    danger: bool,
) -> ElementBuilder {
    let data = chat_id.map(|id| ("chat", id));
    crate::ui::widgets::action_icon(icon, label, on_click, danger, data)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Arm the delete confirm for the clicked row (no DB write yet).
#[handler]
fn ask_delete(app: &mut App, ctx: &EventContext) {
    if let Some(id) = ctx.data("chat") {
        app.pending_delete = Some(id.to_owned());
    }
}

/// Confirm: send the delete for the armed row and clear the prompt optimistically (the
/// bridge also clears `pending_delete` once the delete projects).
#[handler]
fn confirm_delete(app: &mut App) {
    if let Some(id) = app.pending_delete.take() {
        bridge::send(Command::DeleteChat(id));
    }
}

/// Cancel: drop the armed delete, restoring the normal row.
#[handler]
fn cancel_delete(app: &mut App) {
    app.pending_delete = None;
}
