//! Shared tool-call rendering for the chat-log thread (the task Thread tab and the main chat).
//! A tool call is a **disclosure row**: a one-line header (name + humanized summary + status,
//! click to toggle) that opens to a **structured card** — a labelled *arguments* section (each
//! field as a `key → value` row, an `edit` as a coloured diff, a `bash` as a shell line) over a
//! labelled *result* section (status + captured output). Also exposes the entry `role_tag` and
//! `verdict_chip` the thread builds its authored entries from.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler};
use rwire_components::Text;
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

/// The disclosure's **structured card**: a labelled *arguments* section (each argument as a
/// `key → value` row, an `edit` as a coloured diff, a `bash` as a shell line) over a labelled
/// *result* section (status chip + the captured output). Revealed when the row above is opened.
fn tool_body(tool: &ToolCardView) -> ElementBuilder {
    let mut sections = vec![labelled_section("arguments", None, argument_rows(tool))];
    if !tool.result.trim().is_empty() {
        sections.push(labelled_section(
            "result",
            Some(status_chip(tool)),
            vec![result_pre(&tool.result)],
        ));
    }
    el(El::Div)
        .st([
            St::BgCode,
            St::BorderDefault,
            St::Rounded0,
            St::PSm,
            St::DisplayFlex,
            St::FlexCol,
            St::GapSm,
        ])
        .append(sections)
}

/// A labelled subsection of the tool card: a small uppercase label (with an optional trailing chip —
/// the result status) over its rows.
fn labelled_section(
    label: &str,
    trailing: Option<ElementBuilder>,
    rows: Vec<ElementBuilder>,
) -> ElementBuilder {
    let mut header = vec![
        el(El::Span)
            .st([
                St::TextXs,
                St::TextMuted,
                St::TextUppercase,
                St::TrackingWide,
            ])
            .text(label),
    ];
    if let Some(chip) = trailing {
        header.push(el(El::Span).st([St::Flex1]));
        header.push(chip);
    }
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs])
        .append([
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
                .append(header),
            el(El::Div)
                .st([St::DisplayFlex, St::FlexCol, St::GapXs])
                .append(rows),
        ])
}

/// The argument rows: an `edit` diff, a `bash` shell line, else each JSON field as a `key → value`
/// row. Falls back to a raw dump for non-object / unparseable arguments.
fn argument_rows(tool: &ToolCardView) -> Vec<ElementBuilder> {
    if tool.name == "edit"
        && let Some(rows) = edit_diff_rows(tool)
    {
        return rows;
    }
    let args: Value = serde_json::from_str(&tool.arguments).unwrap_or(Value::Null);
    if tool.name == "bash"
        && let Some(command) = args.get("command").and_then(Value::as_str)
    {
        return vec![
            el(El::Div)
                .style(Style::new().set("white-space", "pre-wrap"))
                .st([St::FontMono, St::TextXs, St::TextMuted, St::BreakWords])
                .text(&format!("$ {command}")),
        ];
    }
    match &args {
        Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| field_row(key, value))
            .collect(),
        Value::Null => vec![Text::caption("(no arguments)").muted().build()],
        other => vec![result_pre(&other.to_string())],
    }
}

/// One argument as a labelled `key` → `value` row. Short scalars sit inline; a long or multi-line
/// value drops to a capped pre below the key.
fn field_row(key: &str, value: &Value) -> ElementBuilder {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let key_label = el(El::Span)
        .st([St::TextXs, St::TextMuted, St::FontMono, St::FlexShrink0])
        .text(key);
    if text.contains('\n') || text.chars().count() > 72 {
        el(El::Div)
            .st([St::DisplayFlex, St::FlexCol, St::GapXs])
            .append([key_label, result_pre(&text)])
    } else {
        el(El::Div)
            .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::MinW0])
            .append([
                key_label,
                el(El::Span)
                    .st([
                        St::TextXs,
                        St::TextLow,
                        St::FontMono,
                        St::MinW0,
                        St::BreakWords,
                    ])
                    .text(&text),
            ])
    }
}

/// The captured output as a capped, scrollable monospace block.
fn result_pre(text: &str) -> ElementBuilder {
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
        .text(&clip(text, 1200))
}

/// A `✓ ok` / `✗ failed` status chip plus the call time, for the result section header.
fn status_chip(tool: &ToolCardView) -> ElementBuilder {
    let (mark, label, color) = if tool.ok {
        ("✓", "ok", St::TextAccent)
    } else {
        ("✗", "failed", St::TextError)
    };
    let mut row = vec![
        el(El::Span)
            .st([St::TextXs, color])
            .text(&format!("{mark} {label}")),
    ];
    if !tool.time.is_empty() {
        row.push(el(El::Span).st([St::TextXs, St::TextLow]).text(&tool.time));
    }
    el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm])
        .append(row)
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

/// An `edit` call's `old_string` → `new_string` as coloured diff rows (a `path` header + `+`/`-`/
/// context lines), or `None` if the arguments don't parse as an edit.
fn edit_diff_rows(tool: &ToolCardView) -> Option<Vec<ElementBuilder>> {
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
    Some(rows)
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
