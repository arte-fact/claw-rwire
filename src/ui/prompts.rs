//! The Prompts inspector (`/prompts`): pick an agent + run-kind and see the 4 assembled
//! system-prompt layers (Role / Persona / Brief / Memory) and the composed result the model
//! receives. Read-only transparency — composed by the bridge via `Command::LoadPrompt`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, icons, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, PromptPreview, RunKind};
use crate::ui::widgets::{accent_submit, page_header};
use crate::ui::workspace::card;

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
    let current = app.prompt_agent.clone();
    let mut options = vec![el(El::Option).attr("value", "").text("— pick an agent —")];
    for agent in &app.agents {
        let option = el(El::Option).attr("value", &agent.id).text(&agent.name);
        options.push(if current.as_deref() == Some(agent.id.as_str()) {
            option.bool_attr(rwire::At::Selected)
        } else {
            option
        });
    }
    let picker = el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::FlexWrap])
        .append([
            el(El::Select)
                .attr("name", "agent")
                .on(Ev::Change, pick_agent())
                .st(control_st())
                .append(options),
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
            el(El::Input)
                .attr("type", "hidden")
                .attr("name", "run")
                .attr("value", run.as_str()),
            el(El::Textarea)
                .id(&format!("role-{}", run.as_str()))
                .attr("name", "prompt")
                .attr("rows", "4")
                .style(
                    Style::new()
                        .set("font", "inherit")
                        .set("resize", "vertical"),
                )
                .st(control_st())
                .focus([St::BorderAccent])
                .text(value),
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
    let base = [
        St::TextXs,
        St::PxSm,
        St::PyXs,
        St::RoundedSm,
        St::BorderDefault,
        St::CursorPointer,
    ];
    let chip = el(El::Button)
        .attr("type", "button")
        .data("run", run.as_str())
        .on(Ev::Click, pick_run())
        .style(Style::new().set("font", "inherit"))
        .st(base)
        .text(run.label());
    if active {
        chip.st([St::BgAccent, St::TextOnAccent, St::BorderAccent])
    } else {
        chip.st([St::BgApp, St::TextMuted]).hover([St::TextHigh])
    }
}

/// The Working-memory layer, made operator-controllable: a size/paused line, an editable
/// summary (Save overwrites it), Clear (reset), and Pause/Resume (freeze auto-summary).
fn memory_card(agent: Option<&str>, preview: &PromptPreview) -> ElementBuilder {
    let mut meta = preview.memory_meta.clone();
    if preview.memory_paused {
        meta.push_str(" · ⏸ paused");
    }
    let editor = el(El::Form)
        .on(Ev::Submit, save_memory())
        .st([St::DisplayFlex, St::FlexCol, St::GapSm])
        .append([
            el(El::Textarea)
                .id(&format!("mem-{}", agent.unwrap_or("none")))
                .attr("name", "summary")
                .attr("rows", "5")
                .at_str(
                    rwire::At::Placeholder,
                    "(empty — fills as the conversation grows)",
                )
                .style(
                    Style::new()
                        .set("font", "inherit")
                        .set("resize", "vertical"),
                )
                .st(control_st())
                .focus([St::BorderAccent])
                .text(&preview.memory),
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append([
                    accent_submit(icons::Icon::Check, "Save"),
                    plain_button("Clear", clear_memory(), None),
                    plain_button(
                        if preview.memory_paused {
                            "Resume"
                        } else {
                            "Pause"
                        },
                        toggle_pause(),
                        Some((
                            "paused",
                            if preview.memory_paused {
                                "false"
                            } else {
                                "true"
                            },
                        )),
                    ),
                ]),
        ]);
    card(
        "4 · Working memory",
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapSm])
            .append([
                el(El::Div).st([St::TextXs, St::TextMuted]).text(&meta),
                editor,
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
    let button = el(El::Button)
        .attr("type", "button")
        .on(Ev::Click, on_click)
        .style(Style::new().set("font", "inherit"))
        .st([
            St::TextXs,
            St::PxSm,
            St::PyXs,
            St::RoundedSm,
            St::BorderDefault,
            St::BgApp,
            St::TextMuted,
            St::CursorPointer,
        ])
        .hover([St::TextHigh])
        .text(label);
    match data {
        Some((key, value)) => button.data(key, value),
        None => button,
    }
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
        .style(
            Style::new()
                .set("white-space", "pre-wrap")
                .set("word-break", "break-word")
                .set("font", "inherit"),
        )
        .st([St::TextXs, St::TextHigh])
        .text(content)
}

fn control_st() -> [St; 7] {
    [
        St::PxSm,
        St::PySm,
        St::BorderDefault,
        St::RoundedSm,
        St::BgApp,
        St::TextHigh,
        St::OutlineNone,
    ]
}

#[handler]
fn pick_agent(app: &mut App, ctx: &EventContext) {
    let agent = ctx.field("agent").map(str::to_owned);
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
fn save_memory(app: &mut App, ctx: &EventContext) {
    let Some(agent_id) = app.prompt_agent.clone() else {
        return;
    };
    let summary = ctx.field("summary").unwrap_or_default().to_owned();
    bridge::send(Command::SetMemory { agent_id, summary });
    refresh(app);
}

#[handler]
fn clear_memory(app: &mut App, ctx: &EventContext) {
    let _ = ctx;
    let Some(agent_id) = app.prompt_agent.clone() else {
        return;
    };
    bridge::send(Command::ClearMemory { agent_id });
    refresh(app);
}

#[handler]
fn toggle_pause(app: &mut App, ctx: &EventContext) {
    let Some(agent_id) = app.prompt_agent.clone() else {
        return;
    };
    let paused = ctx.data("paused") == Some("true");
    bridge::send(Command::SetMemoryPaused { agent_id, paused });
    refresh(app);
}

/// Re-compose the preview after a memory mutation, so the panel reflects it.
fn refresh(app: &App) {
    bridge::send(Command::LoadPrompt {
        agent: app.prompt_agent.clone(),
        run: app.prompt_run,
    });
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
