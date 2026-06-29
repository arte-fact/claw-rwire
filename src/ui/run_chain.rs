//! Shared rendering of a task run as a **collapsible card** — used by both the task-detail run
//! chain and the Review inbox. Whole runs collapse to a one-line header (role + files-changed
//! summary + verdict); only the latest run is open by default. Inside an open run, tool calls are
//! one-line rows (a humanized summary, not raw JSON) collapsed by default; click anything to toggle.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler};
use serde_json::Value;

use crate::diff::{DiffKind, line_diff};
use crate::state::{App, RunView, ToolCardView};

/// One run as a **collapsible card**: a clickable header (role + agent + verdict + a files-changed
/// summary), and — when open — the tool rows, the Markdown reply, and any feedback. Runs collapse
/// by default except the latest (`is_latest`); `key` makes the toggle keys unique; `toggled` is the
/// user's open/close overrides.
pub fn run_card(run: &RunView, key: &str, is_latest: bool, toggled: &[String]) -> ElementBuilder {
    let run_key = format!("run:{key}");
    let open = is_latest ^ toggled.iter().any(|t| t == &run_key);
    let mut children = vec![run_header(run, &run_key, open)];
    if open {
        for (index, tool) in run.tools.iter().enumerate() {
            children.push(tool_row(tool, &format!("{key}.{index}"), toggled));
        }
        // The run's reply (the "bottom line") — rendered as markdown, full and prominent.
        if run.output.trim().is_empty() {
            children.push(
                el(El::Div)
                    .st([St::TextXs, St::TextMuted])
                    .text("(no output)"),
            );
        } else {
            children.push(crate::ui::widgets::markdown(run.output.clone()));
        }
        if let Some(feedback) = run.feedback.as_deref().filter(|f| !f.trim().is_empty()) {
            children.push(
                el(El::Div)
                    .st([St::TextXs, St::TextAccent])
                    .text(&format!("feedback: {feedback}")),
            );
        }
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
        .append(children)
}

/// The run's clickable header: caret + role label + a summary (files changed, else call count) +
/// agent + verdict chip. Toggles the whole run open/closed.
fn run_header(run: &RunView, run_key: &str, open: bool) -> ElementBuilder {
    let mut label = match run.role.as_str() {
        "validator" => "Review".to_owned(),
        "producer" => "Produce".to_owned(),
        _ => "Stage".to_owned(),
    };
    if run.iteration > 0 {
        label.push_str(&format!(" · iter {}", run.iteration + 1));
    }
    el(El::Button)
        .at(At::Type, Av::Button)
        .data("tool", run_key)
        .on(Ev::Click, toggle_tool())
        .st([
            St::FontInheritAll,
            St::WFull,
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::BgTransparent,
            St::BorderNone,
            St::P0,
            St::CursorPointer,
            St::TextLeft,
        ])
        .append([
            el(El::Span)
                .st([St::TextLow])
                .text(if open { "▾" } else { "▸" }),
            el(El::Span)
                .st([St::TextXs, St::TextHigh, St::FontSemibold])
                .text(&label),
            el(El::Span)
                .st([St::MinW0, St::Flex1, St::TextXs, St::TextLow, St::Truncate])
                .text(&run_summary(run)),
            el(El::Span).st([St::TextXs, St::TextLow]).text(&run.agent),
            verdict_chip(run),
        ])
}

/// A one-line summary for a collapsed run: the files it wrote/edited (the concrete artifacts),
/// else the number of tool calls.
fn run_summary(run: &RunView) -> String {
    let files = files_changed(&run.tools);
    if files.is_empty() {
        match run.tools.len() {
            0 => String::new(),
            n => format!("{n} call(s)"),
        }
    } else {
        files.join(", ")
    }
}

/// The distinct file paths a set of tool calls wrote or edited, in first-seen order.
fn files_changed(tools: &[ToolCardView]) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for tool in tools
        .iter()
        .filter(|t| t.name == "write" || t.name == "edit")
    {
        if let Ok(value) = serde_json::from_str::<Value>(&tool.arguments)
            && let Some(path) = value.get("path").and_then(Value::as_str)
            && !files.iter().any(|f| f == path)
        {
            files.push(path.to_owned());
        }
    }
    files
}

/// One tool call as a collapsible row: a caret + name + humanized summary + status (the header,
/// click to toggle), and — when open — the detail (an `edit` diff, else the command/result).
/// Collapsed by default (only the user's explicit toggles open it).
fn tool_row(tool: &ToolCardView, key: &str, toggled: &[String]) -> ElementBuilder {
    let open = toggled.iter().any(|t| t == key);
    let (mark, mark_color) = if tool.ok {
        ("✓", St::TextAccent)
    } else {
        ("✗", St::TextError)
    };
    let header = el(El::Button)
        .at(At::Type, Av::Button)
        .data("tool", key)
        .on(Ev::Click, toggle_tool())
        .st([
            St::FontInheritAll,
            St::WFull,
            St::DisplayFlex,
            St::ItemsCenter,
            St::GapSm,
            St::TextXs,
            St::FontMono,
            St::BgTransparent,
            St::BorderNone,
            St::P0,
            St::CursorPointer,
            St::TextLeft,
        ])
        .append([
            el(El::Span)
                .st([St::TextLow])
                .text(if open { "▾" } else { "▸" }),
            el(El::Span).st([St::TextMuted]).text(&tool.name),
            el(El::Span)
                .st([St::MinW0, St::Flex1, St::TextLow, St::Truncate])
                .text(&tool_summary(tool)),
            el(El::Span).st([mark_color]).text(mark),
        ]);
    let mut children = vec![header];
    if open {
        children.push(tool_body(tool));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append(children)
}

/// A humanized one-line summary of a tool call's arguments — the command, the file path, the
/// verdict — never raw JSON.
fn tool_summary(tool: &ToolCardView) -> String {
    let args: Value = serde_json::from_str(&tool.arguments).unwrap_or(Value::Null);
    let field = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let text = match tool.name.as_str() {
        "bash" => field("command"),
        "write" | "edit" | "read" => field("path"),
        "verdict" => field("decision"),
        _ => tool.arguments.clone(),
    };
    one_line(&text, 96)
}

/// The expanded detail for a tool: an `edit` becomes a coloured diff; everything else shows the
/// full command (for `bash`) and the result, capped.
fn tool_body(tool: &ToolCardView) -> ElementBuilder {
    if tool.name == "edit"
        && let Some(diff) = edit_diff(tool)
    {
        return diff;
    }
    let mut rows = Vec::new();
    if tool.name == "bash"
        && let Ok(value) = serde_json::from_str::<Value>(&tool.arguments)
        && let Some(command) = value.get("command").and_then(Value::as_str)
    {
        rows.push(
            el(El::Div)
                .style(Style::new().set("white-space", "pre-wrap"))
                .st([St::FontMono, St::TextXs, St::TextMuted])
                .text(&format!("$ {command}")),
        );
    }
    if !tool.result.trim().is_empty() {
        rows.push(
            el(El::Pre)
                .style(Style::new().set("max-height", "12rem"))
                .st([
                    St::TextXs,
                    St::TextLow,
                    St::FontMono,
                    St::WhitespacePreWrap,
                    St::M0,
                    St::OverflowAuto,
                ])
                .text(&clip(&tool.result, 1200)),
        );
    }
    el(El::Div)
        .st([
            St::DisplayFlex,
            St::FlexCol,
            St::GapXs,
            St::PSm,
            St::BorderDefault,
            St::RoundedSm,
            St::BgCode,
        ])
        .append(rows)
}

/// Toggle one tool row open/closed (flip it from its default).
#[handler]
pub fn toggle_tool(app: &mut App, ctx: &EventContext) {
    if let Some(key) = ctx.data("tool").map(str::to_owned) {
        if let Some(index) = app.tool_toggled.iter().position(|t| t == &key) {
            app.tool_toggled.remove(index);
        } else {
            app.tool_toggled.push(key);
        }
    }
}

/// An `edit` tool call rendered as a labelled line diff of its `old_string` → `new_string`.
fn edit_diff(tool: &ToolCardView) -> Option<ElementBuilder> {
    let args: Value = serde_json::from_str(&tool.arguments).ok()?;
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

/// Collapse whitespace runs to single spaces, then truncate with an ellipsis.
fn one_line(text: &str, max_chars: usize) -> String {
    clip(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
    )
}

/// Char-aware truncation with a trailing ellipsis (preserves newlines for the result body).
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}
