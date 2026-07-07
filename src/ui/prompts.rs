//! The Prompts inspector (`/prompts`): pick an agent + run-kind and see the 4 assembled
//! system-prompt layers (Role / Persona / Brief / Memory) and the composed result the model
//! receives. Read-only transparency — composed by the bridge via `Command::LoadPrompt`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, PromptPreview, RunKind};
use crate::ui::widgets::{accent_submit, page_header};
use crate::ui::workspace::card;
use rwire_components::{Button, ButtonSize, Select, Textarea};

pub fn page() -> ElementBuilder {
    el(El::Div)
        .style(Style::new().min_height("0").overflow("auto"))
        .st([St::Flex1, St::DisplayFlex, St::FlexCol, St::GapLg, St::PMd])
        .md([St::PXl])
        .append([
            page_header(
                "Prompts",
                "The system prompt is four layers: role · identity · workspace brief · memory. \
                 Pick an agent and a run-kind to see what the model receives.",
                el(El::Div),
            ),
            body(),
        ])
}

#[renderer]
fn body(app: &App) -> ElementBuilder {
    // Agent picker — the workspace owner / member whose persona + memory we compose.
    let mut picker_select = Select::new()
        .name("agent")
        .value(app.prompt_agent.clone().unwrap_or_default())
        .option("", "— pick an agent —");
    for agent in &app.agents {
        picker_select = picker_select.option(agent.id.clone(), agent.name.clone());
    }
    let picker = el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append([
            picker_select.build().on(Ev::Change, pick_agent()),
            run_chips(app.prompt_run),
        ]);

    let layers = match &app.prompt_preview {
        Some(preview) => el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapMd])
            .append([
                context_line(&preview.agent_name, &preview.run, &preview.workspace),
                card("1 · Role preamble", text_block(&preview.role)),
                card("2 · Identity / persona", text_block(&preview.persona)),
                card("3 · Workspace brief", text_block(&preview.brief)),
                memory_card(app.prompt_agent.as_deref(), preview),
                card(
                    "Assembled (what the model receives)",
                    text_block(&preview.assembled),
                ),
            ]),
        None => el(El::Div)
            .st([St::TextSm, St::TextMuted])
            .text("Pick an agent to preview its assembled prompt."),
    };

    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapLg])
        .append([picker, layers, defaults_section(&app.role_defaults)])
}

/// The editable global role preambles (layer 1) — they apply to every agent in that role.
/// Editing here changes behaviour without a rebuild; Reset clears the override.
fn defaults_section(defaults: &crate::state::RoleDefaults) -> ElementBuilder {
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapMd])
        .append([
            el(El::Div)
                .st([St::TextSm, St::TextHigh, St::FontSemibold, St::PtLg])
                .text("Global role defaults"),
            el(El::Div).st([St::TextXs, St::TextMuted]).text(
                "Layer 1, shared by every agent in that role. Edit to change behaviour; \
                 leave blank / Reset to use the built-in default.",
            ),
            role_default_card("Concierge (chat)", RunKind::Chat, &defaults.concierge),
            role_default_card("Executor (task)", RunKind::Task, &defaults.executor),
            role_default_card("Owner (planning)", RunKind::Planning, &defaults.planner),
            role_default_card(
                "Validator (review)",
                RunKind::Validator,
                &defaults.validator,
            ),
        ])
}

fn role_default_card(label: &str, run: RunKind, value: &str) -> ElementBuilder {
    let form = el(El::Form)
        .on(Ev::Submit, save_role())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            // The run rides as a hidden field (Submit doesn't expose data-attrs).
            crate::ui::widgets::hidden("run", run.as_str()),
            Textarea::new()
                .id(format!("role-{}", run.as_str()))
                .name("prompt")
                .rows(4)
                .value(value.to_owned())
                .build(),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    accent_submit(icons::Icon::Check, "Save"),
                    plain_button(
                        "Reset to default",
                        reset_role(),
                        Some(("run", run.as_str())),
                    ),
                ]),
        ]);
    card(label, form)
}

/// The three run-kinds as immediate-apply chips; the active one is highlighted.
fn run_chips(active: RunKind) -> ElementBuilder {
    let chips: Vec<ElementBuilder> = RunKind::ALL
        .into_iter()
        .map(|run| run_chip(run, run == active))
        .collect();
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapXs])
        .append(chips)
}

fn run_chip(run: RunKind, active: bool) -> ElementBuilder {
    crate::ui::widgets::chip(run.label(), active, pick_run()).data("run", run.as_str())
}

/// The Working-memory layer, read-only: memory is **fully automatic** (folds at task/turn
/// boundaries, budget-fitted at compose time) — the inspector shows what the run receives and
/// where it came from, nothing to manage.
fn memory_card(agent: Option<&str>, preview: &PromptPreview) -> ElementBuilder {
    let _ = agent;
    if !preview.memory_applies {
        return card(
            "4 · Working memory",
            el(El::Div)
                .st([St::TextXs, St::TextMuted, St::Italic])
                .text(&preview.memory_meta),
        );
    }
    card(
        "4 · Working memory",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append([
                el(El::Div)
                    .st([St::TextXs, St::TextMuted])
                    .text(&preview.memory_meta),
                text_block(&preview.memory),
            ]),
    )
}

/// A small secondary (`type=button`) action, optionally carrying one `data-(key)=value`
/// attribute for its click handler to read.
fn plain_button(
    label: &str,
    on_click: rwire::HandlerSpec,
    data: Option<(&str, &str)>,
) -> ElementBuilder {
    let button = Button::secondary(label.to_owned())
        .size(ButtonSize::Sm)
        .on_click(on_click);
    match data {
        Some((key, value)) => button.data(key, value),
        None => button,
    }
}

/// Re-compose the preview after a role edit, so the assembled layer reflects it.
fn refresh(app: &App) {
    bridge::send(Command::LoadPrompt {
        agent: app.prompt_agent.clone(),
        run: app.prompt_run,
    });
}

/// A short "Agent · run · workspace" context line above the layers.
fn context_line(agent: &str, run: &str, workspace: &str) -> ElementBuilder {
    el(El::Div)
        .st([St::TextXs, St::TextMuted])
        .text(&format!("{agent} · {run} run · workspace {workspace}"))
}

/// A monospace, whitespace-preserving block for a layer's text, or a muted "— none —".
fn text_block(content: &str) -> ElementBuilder {
    if content.trim().is_empty() {
        return el(El::Div)
            .st([St::TextXs, St::TextMuted, St::Italic])
            .text("— none —");
    }
    el(El::Div)
        .st([
            St::WhitespacePreWrap,
            St::BreakWords,
            St::TextXs,
            St::TextHigh,
        ])
        .text(content)
}

#[handler]
fn pick_agent(app: &mut App, ctx: &EventContext) {
    // A `<select>` change delivers its value as a `Text` payload (`ctx.text()`).
    let agent = ctx.text().map(str::to_owned);
    bridge::send(Command::LoadPrompt {
        agent,
        run: app.prompt_run,
    });
}

#[handler]
fn pick_run(app: &mut App, ctx: &EventContext) {
    if let Some(run) = ctx.data("run") {
        bridge::send(Command::LoadPrompt {
            agent: app.prompt_agent.clone(),
            run: RunKind::from_str(run),
        });
    }
}

#[handler]
fn save_role(app: &mut App, ctx: &EventContext) {
    let run = RunKind::from_str(ctx.field("run").unwrap_or_default());
    let value = ctx.field("prompt").unwrap_or_default().to_owned();
    bridge::send(Command::SetRolePrompt { run, value });
    refresh(app); // the assembled preview's role layer may have changed
}

#[handler]
fn reset_role(app: &mut App, ctx: &EventContext) {
    if let Some(run) = ctx.data("run") {
        bridge::send(Command::SetRolePrompt {
            run: RunKind::from_str(run),
            value: String::new(),
        });
        refresh(app);
    }
}
