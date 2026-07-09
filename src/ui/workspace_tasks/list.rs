//! The list view: one row per task with the inline edit form. Shares the status badge and
//! control buttons with the board via the parent module.

use rwire::router::Link;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, icons};
use rwire_components::{Input, Select, Text, Textarea};

use crate::state::{App, TaskView};
use crate::ui::widgets::accent_submit;

use super::{
    close_edit, control_buttons, retries_input, save_edit, status_badge, task_matches_filter,
};

/// The flat list view (the original): cards with the inline edit drawer.
pub(super) fn list_board(app: &App, members: &[(String, String)], wid: &str) -> ElementBuilder {
    let editing = app.editing_task.as_deref();
    let mut cards: Vec<ElementBuilder> = app
        .tasks
        .iter()
        .filter(|task| task.request_id.is_none())
        .filter(|task| task_matches_filter(task, &app.board_filter))
        .map(|task| task_card(task, members, editing == Some(task.id.as_str()), wid))
        .collect();
    if cards.is_empty() {
        cards.push(Text::body("No tasks yet.").muted().build().st([St::PySm]));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append(cards)
}

fn task_card(
    task: &TaskView,
    members: &[(String, String)],
    open: bool,
    wid: &str,
) -> ElementBuilder {
    let mut head: Vec<ElementBuilder> = vec![
        // The title links to the task's detail view (its run chain).
        Link::route(&format!("/w/{wid}/tasks/{}", task.id))
            .st([
                St::Flex1,
                St::TextSm,
                St::TextHigh,
                St::FontSemibold,
                St::NoUnderline,
            ])
            .hover([St::TextAccent])
            .text(&task.title),
        status_badge(&task.status),
    ];
    for button in control_buttons(task) {
        head.push(button);
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append(head),
    ];
    if open {
        rows.push(edit_form(task, members));
    } else {
        rows.push(el(El::Div).st([St::TextXs, St::TextMuted]).text(&task.spec));
        let mut meta = task.agent.as_deref().unwrap_or("owner").to_owned();
        if task.recurrent {
            meta.push_str(" · ⟳");
            if let Some(schedule) = &task.schedule {
                meta.push_str(&format!(" {schedule}"));
            }
            if let Some(next) = &task.next_run {
                meta.push_str(&format!(" · next {next}"));
            }
        }
        if task.attempt > 0 {
            meta.push_str(&format!(" · attempt {}", task.attempt + 1));
        }
        if let Some(next) = &task.next_retry {
            meta.push_str(&format!(" · retry {} → {next}", task.auto_retries + 1));
        }
        if !task.blockers.is_empty() {
            meta.push_str(&format!(" · blocked by {}", task.blockers.join(", ")));
        }
        rows.push(el(El::Div).st([St::TextXs, St::TextLow]).text(&meta));
        if let Some(result) = task
            .result
            .as_deref()
            .filter(|r| !r.is_empty() && matches!(task.status.as_str(), "done" | "failed"))
        {
            rows.push(
                el(El::Div)
                    .st([St::TextXs, St::TextMuted, St::Italic])
                    .text(result),
            );
        }
    }
    el(El::Div)
        .st([
            St::BgSurface,
            St::BorderDefault,
            St::PMd,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

/// The inline edit drawer for one task: retitle, rewrite the spec, reassign to a member,
/// change the schedule, and edit the blocker ids — saved as one form submit.
fn edit_form(task: &TaskView, members: &[(String, String)]) -> ElementBuilder {
    let mut assignee = Select::new()
        .name("agent")
        .value(task.agent_id.clone().unwrap_or_default())
        .option("", "— owner —");
    for (id, name) in members {
        assignee = assignee.option(id.clone(), name.clone());
    }
    el(El::Form)
        .on(Ev::Submit, save_edit())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm, St::PtXs])
        .append([
            // The task id rides as a hidden field so the submit handler reads it from
            // FormData (a form's data-attribute isn't exposed on the Submit event).
            el(El::Input)
                .at(At::Type, Av::Hidden)
                .at_str(At::Name, "id")
                .at_str(At::Value, &task.id),
            Input::text()
                .name("title")
                .value(task.title.clone())
                .autocomplete("off")
                .build(),
            Textarea::new()
                .name("spec")
                .rows(2)
                .value(task.spec.clone())
                .build(),
            Input::text()
                .name("schedule")
                .value(task.schedule.clone().unwrap_or_default())
                .placeholder("Cron schedule — empty = one-shot")
                .autocomplete("off")
                .build(),
            Input::text()
                .name("blockers")
                .value(task.blockers.join(", "))
                .placeholder("Blocker task ids, comma-separated")
                .autocomplete("off")
                .build(),
            retries_input(task.max_retries),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    assignee.build().st([St::Flex1]),
                    accent_submit(icons::Icon::Check, "Save", &save_edit()),
                    crate::ui::widgets::icon_button(icons::Icon::Close, close_edit(), false),
                ]),
        ])
}
