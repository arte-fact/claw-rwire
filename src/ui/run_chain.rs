//! Shared rendering of a task run as a card — the tool transcript, the rendered-Markdown output,
//! and the verdict/feedback. Used by both the task-detail run chain and the Review inbox so each
//! shows the full story of what an agent run did, not just a text snippet.

use rwire::{El, ElementBuilder, St, el};

use crate::state::RunView;

/// One run as a card: a role label + agent + verdict chip, the tool cards (what it actually did),
/// the output as Markdown, and any reviewer feedback.
pub fn run_card(run: &RunView) -> ElementBuilder {
    // Label: the role, plus the iteration for a loop run.
    let mut label = match run.role.as_str() {
        "validator" => "Review".to_owned(),
        "producer" => "Produce".to_owned(),
        _ => "Stage".to_owned(),
    };
    if run.iteration > 0 {
        label.push_str(&format!(" · iter {}", run.iteration + 1));
    }
    let mut rows = vec![
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
            .append([
                el(El::Span)
                    .st([St::Flex1, St::TextXs, St::TextHigh, St::FontSemibold])
                    .text(&label),
                el(El::Span).st([St::TextXs, St::TextLow]).text(&run.agent),
                verdict_chip(run),
            ]),
    ];
    // The tool transcript — the full story of what the run did (files, commands, results).
    for tool in &run.tools {
        rows.push(crate::ui::chat::tool_card(tool));
    }
    // The run's reply, rendered as markdown (code blocks, lists, headings).
    if run.output.trim().is_empty() {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextMuted])
                .text("(no output)"),
        );
    } else {
        rows.push(crate::ui::widgets::markdown(run.output.clone()));
    }
    if let Some(feedback) = run.feedback.as_deref().filter(|f| !f.trim().is_empty()) {
        rows.push(
            el(El::Div)
                .st([St::TextXs, St::TextAccent])
                .text(&format!("feedback: {feedback}")),
        );
    }
    el(El::Div)
        .st([
            St::BgApp,
            St::BorderDefault,
            St::RoundedSm,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
        ])
        .append(rows)
}

fn verdict_chip(run: &RunView) -> ElementBuilder {
    let (text, color) = match run.verdict.as_deref() {
        Some("pass") => ("pass", St::TextAccent),
        Some("revise") => ("revise", St::TextMuted),
        Some("reject") => ("reject", St::TextLow),
        _ if !run.ok => ("failed", St::TextLow),
        _ => ("", St::TextLow),
    };
    if text.is_empty() {
        return el(El::Span);
    }
    el(El::Span)
        .st([St::TextXs, St::TextUppercase, St::TrackingWide, color])
        .text(text)
}
