//! Shared rendering of a task run as a card — the tool transcript, the rendered-Markdown output,
//! and the verdict/feedback. Used by both the task-detail run chain and the Review inbox so each
//! shows the full story of what an agent run did, not just a text snippet.

use rwire::style::Style;
use rwire::{El, ElementBuilder, St, el};

use crate::diff::{DiffKind, line_diff};
use crate::state::{RunView, ToolCardView};

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
    // An `edit` is shown as a coloured diff; everything else as a plain tool card.
    for tool in &run.tools {
        rows.push(tool_view(tool));
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

/// Render one tool call: an `edit` becomes a coloured old→new diff; anything else falls back to
/// the shared tool card.
fn tool_view(tool: &ToolCardView) -> ElementBuilder {
    if tool.name == "edit"
        && let Some(diff) = edit_diff(tool)
    {
        return diff;
    }
    crate::ui::chat::tool_card(tool)
}

/// An `edit` tool call rendered as a labelled line diff of its `old_string` → `new_string`.
fn edit_diff(tool: &ToolCardView) -> Option<ElementBuilder> {
    let args: serde_json::Value = serde_json::from_str(&tool.arguments).ok()?;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("a file");
    let old = args.get("old_string").and_then(|v| v.as_str())?;
    let new = args.get("new_string").and_then(|v| v.as_str())?;
    let mut rows = vec![
        el(El::Div)
            .st([St::TextXs, St::TextMuted, St::FontMono])
            .text(&format!("edit {path}")),
    ];
    for line in line_diff(old, new) {
        let (prefix, color) = match line.kind {
            DiffKind::Added => ('+', St::TextSuccess),
            DiffKind::Removed => ('-', St::TextError),
            DiffKind::Equal => (' ', St::TextMuted),
        };
        rows.push(
            el(El::Div)
                .style(
                    Style::new()
                        .set("white-space", "pre-wrap")
                        .set("word-break", "break-word"),
                )
                .st([St::FontMono, St::TextXs, color])
                .text(&format!("{prefix} {}", line.text)),
        );
    }
    Some(
        el(El::Div)
            .st([
                St::BgCode,
                St::BorderDefault,
                St::RoundedSm,
                St::PSm,
                St::DisplayFlex,
                St::FlexCol,
            ])
            .append(rows),
    )
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
