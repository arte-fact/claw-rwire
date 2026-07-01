//! Shared rendering of a task run as a **collapsible card** — used by the task-detail run
//! chain (the Peek modal's Activity tab). Whole runs collapse to a one-line header (role + files-changed
//! summary + verdict); only the latest run is open by default. Inside an open run, tool calls are
//! one-line rows (a humanized summary, not raw JSON) collapsed by default; click anything to toggle.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler};
use serde_json::Value;

use crate::diff::{DiffKind, line_diff};
use crate::state::{App, RunView, ToolCardView};

/// A run's **phase tag** for a thread entry: the role as a verb (`producer→Produce`,
/// `validator→Review`, else `Stage`), plus the loop iteration when past the first.
pub(crate) fn role_tag(run: &RunView) -> String {
    let base = match run.role.as_str() {
        "validator" => "Review",
        "producer" => "Produce",
        _ => "Stage",
    };
    if run.iteration > 0 {
        format!("{base} · iter {}", run.iteration + 1)
    } else {
        base.to_owned()
    }
}

/// One tool call as a collapsible row: a caret + name + humanized summary + status (the header,
/// click to toggle), and — when open — the detail (an `edit` diff, else the command/result).
/// Collapsed by default (only the user's explicit toggles open it). Shared by the task thread
/// and the main chat, so tool calls read identically on both.
pub(crate) fn tool_row(tool: &ToolCardView, key: &str, toggled: &[String]) -> ElementBuilder {
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
            St::Rounded0,
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
                St::Rounded0,
                St::PSm,
                St::DisplayFlex,
                St::FlexCol,
            ])
            .append(rows),
    )
}

pub(crate) fn verdict_chip(run: &RunView) -> ElementBuilder {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(role: &str, iteration: i64) -> RunView {
        RunView {
            role: role.to_owned(),
            agent: "Forge".to_owned(),
            output: String::new(),
            verdict: None,
            feedback: None,
            iteration,
            ok: true,
            tools: Vec::new(),
            attempt: 0,
            time: String::new(),
        }
    }

    #[test]
    fn role_tag_names_the_phase_and_iteration() {
        assert_eq!(role_tag(&run("producer", 0)), "Produce");
        assert_eq!(role_tag(&run("validator", 0)), "Review");
        assert_eq!(role_tag(&run("stage", 0)), "Stage");
        // Past the first loop turn, the 1-based iteration is shown.
        assert_eq!(role_tag(&run("producer", 1)), "Produce · iter 2");
        assert_eq!(role_tag(&run("validator", 2)), "Review · iter 3");
    }
}
