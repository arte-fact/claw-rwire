//! The Prompts inspector (`/prompts`): pick an agent + run-kind and see the 4 assembled
//! system-prompt layers (Role / Persona / Brief / Memory) and the composed result the model
//! receives. Read-only transparency — composed by the bridge via `Command::LoadPrompt`.

use rwire::style::Style;
use rwire::{El, ElementBuilder, Ev, St, el, handler, renderer};

use crate::bridge::{self, Command};
use crate::state::{App, RunKind};
use crate::ui::widgets::page_header;
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
                card("4 · Working memory", text_block(&preview.memory)),
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
        .append([picker, layers])
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
