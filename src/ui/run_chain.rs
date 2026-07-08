//! Shared tool-call rendering for the chat-log thread (the task Thread tab and the main chat).
//! A tool call is a **disclosure row**: a one-line header (name + humanized summary + status,
//! click to toggle) that opens to a **structured card** — a labelled *arguments* section (each
//! field as a `key → value` row, an `edit` as a coloured diff, a `bash` as a shell line) over a
//! labelled *result* section (status + captured output). Also exposes the entry `role_tag` and
//! `verdict_chip` the thread builds its authored entries from.

use rwire::style::Style;
use rwire::{At, Av, El, ElementBuilder, Ev, St, el, handler};
use rwire_components::{Badge, BadgeFill, BadgeIntent, BadgeShape, Text};
use serde_json::Value;

use crate::diff::{DiffKind, line_diff};
use crate::state::{App, RunView, ToolCardView};

/// A run's **phase tag** for a thread entry: the role as a verb (`producer→Produce`,
/// `validator→Review`, else `Stage`), plus the loop iteration when past the first.
pub(crate) fn role_tag(run: &RunView) -> String {
    crate::state::role_phase(&run.role, run.iteration)
}

/// One tool call as a collapsible row: a caret + name + humanized summary + status (the header,
/// click to toggle), and — when open — the detail (an `edit` diff, else the command/result).
/// Collapsed by default (only the user's explicit toggles open it). Shared by the task thread
/// and the main chat, so tool calls read identically on both.
pub(crate) fn tool_row(tool: &ToolCardView, key: &str) -> ElementBuilder {
    let (mark, mark_color) = if tool.ok {
        ("✓", St::TextAccent)
    } else {
        ("✗", St::TextError)
    };
    let mark = el(El::Span).st([mark_color]).text(mark);
    disclosure_row(
        key,
        &tool.name,
        &tool_summary(tool),
        Some(mark),
        tool_body(tool),
        false,
    )
}

/// The run's launch-time memory as a disclosure row (R3.5 M4 provenance): a muted "memory ·
/// launched remembering…" header that opens to the snapshot — same toggle machinery as the
/// tool rows, so "why did it do that?" sits right where the forensics live.
pub(crate) fn memory_row(memory: &str, key: &str) -> ElementBuilder {
    let body = el(El::Div)
        .st([
            St::TextXs,
            St::TextMuted,
            St::WhitespacePreWrap,
            St::PSm,
            St::BgApp,
        ])
        .text(memory);
    disclosure_row(
        key,
        "memory",
        &format!("launched remembering · {}", one_line(memory, 96)),
        None,
        body,
        false,
    )
}

/// A pass's git diff as a disclosure row (G2): a muted "diff · <first stat line>" header
/// that opens to the patch, `+`/`-` lines coloured with the shared DiffLine vocabulary.
pub(crate) fn diff_row(
    diff: &str,
    files: &[crate::state::RestorableFile],
    key: &str,
) -> ElementBuilder {
    let mut rows: Vec<ElementBuilder> = crate::diff::patch_lines(diff)
        .into_iter()
        .map(|line| {
            let color = match line.kind {
                DiffKind::Added => St::TextSuccess,
                DiffKind::Removed => St::TextError,
                DiffKind::Equal => St::TextMuted,
            };
            el(El::Div)
                .st([St::WhitespacePreWrap, St::BreakWords, St::TextXs, color])
                .text(&line.text)
        })
        .collect();
    // Per-file restore (G3): put a path back to its pre-pass state — the working tree
    // takes the restore; the next pass commit records it.
    for file in files {
        let payload = serde_json::json!({
            "workspace": file.workspace,
            "task": file.task,
            "repo": file.repo,
            "sha": file.sha,
            "path": file.path,
        })
        .to_string();
        rows.push(
            el(El::Div)
                .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
                .append([
                    el(El::Span).st([St::TextMuted]).text(&file.path),
                    crate::ui::widgets::ghost(
                        "restore",
                        restore_file().with_param_bytes(payload.into_bytes()),
                    ),
                ]),
        );
    }
    let body = el(El::Div)
        .st([St::PSm, St::BgApp, St::OverflowXAuto])
        .append(rows);
    let summary = diff
        .lines()
        .find(|line| line.contains('|') || line.contains("file"))
        .unwrap_or_else(|| diff.lines().next().unwrap_or(""));
    disclosure_row(
        key,
        "diff",
        &format!("what this pass changed · {}", one_line(summary, 90)),
        None,
        body,
        false,
    )
}

/// A pass's machine checks (P1b) as a badge strip — one square outline badge per check,
/// green `✓` / red `✗` — the machine verdict at a glance. Red checks with a failure tail add a
/// disclosure below revealing the tails (the signature the reviewer/operator expands).
pub(crate) fn checks_row(
    run_id: &str,
    checks: &[crate::state::CheckBadgeView],
    key: &str,
) -> ElementBuilder {
    let badges: Vec<ElementBuilder> = checks
        .iter()
        .map(|check| {
            let intent = if check.ok {
                BadgeIntent::Success
            } else {
                BadgeIntent::Error
            };
            let mark = if check.ok { "✓" } else { "✗" };
            let text = if check.duration.is_empty() {
                format!("{mark} {}", check.label)
            } else {
                format!("{mark} {} · {}", check.label, check.duration)
            };
            let badge = Badge::new()
                .shape(BadgeShape::Square)
                .fill(BadgeFill::Outline)
                .intent(intent)
                .text(text)
                .build();
            let payload = serde_json::json!({
                "run": run_id,
                "repo": check.repo,
                "check": check.check,
            })
            .to_string();
            // The whole badge is a button — a click opens this check's full log (P4c) in the drawer.
            el(El::Button)
                .at(At::Type, Av::Button)
                .at_str(At::AriaLabel, &format!("View the {} log", check.label))
                .st([St::BorderNone, St::BgTransparent, St::P0, St::CursorPointer])
                .data("log", &payload)
                .on(Ev::Click, open_check_log())
                .append([badge])
        })
        .collect();
    let strip = el(El::Div)
        .st([St::DisplayFlex, St::FlexWrap, St::GapXs, St::ItemsCenter])
        .append(badges);
    let tails = checks
        .iter()
        .filter(|check| !check.ok && !check.tail.is_empty())
        .map(|check| format!("### {}\n{}", check.label, check.tail))
        .collect::<Vec<_>>()
        .join("\n\n");
    if tails.is_empty() {
        return el(El::Div).st([St::PySm]).append([strip]);
    }
    let body = el(El::Div)
        .st([
            St::PSm,
            St::BgApp,
            St::OverflowXAuto,
            St::WhitespacePreWrap,
            St::BreakWords,
            St::TextXs,
            St::TextError,
        ])
        .text(&tails);
    el(El::Div)
        .st([St::DisplayFlex, St::FlexCol, St::GapXs, St::PySm])
        .append([
            strip,
            disclosure_row(
                key,
                "checks",
                "machine checks — failure detail",
                None,
                body,
                false,
            ),
        ])
}

/// The shared disclosure-row scaffold: a native `<details>`/`<summary>` — the `<summary>` header
/// (name + a truncated summary + an optional trailing mark) over the body. The toggle is
/// client-side (zero-latency, keyboard/a11y for free), so there is no server round-trip and no
/// `tool_toggled` state: the body is always built and the browser reveals it only when open. `key`
/// is the element id so a re-render's morph preserves the open state; `open` sets the initial
/// state. One shape for tool calls, diffs and the memory snapshot, so the forensics read uniformly.
pub(crate) fn disclosure_row(
    key: &str,
    name: &str,
    summary: &str,
    mark: Option<ElementBuilder>,
    body: ElementBuilder,
    open: bool,
) -> ElementBuilder {
    let mut header_parts = vec![
        el(El::Span).st([St::TextMuted]).text(name),
        el(El::Span)
            .st([St::MinW0, St::Flex1, St::TextLow, St::Truncate])
            .text(summary),
    ];
    if let Some(mark) = mark {
        header_parts.push(mark);
    }
    // The native `<summary>` supplies the disclosure caret (rotates on open for free); its content
    // rides in an inner flex row so the truncating summary + trailing mark lay out as before.
    let summary_el = el(El::Summary).st([St::CursorPointer]).append([el(El::Div)
        .st([St::DisplayFlex, St::ItemsCenter, St::GapSm, St::TextXs])
        .append(header_parts)]);
    // `<details>` stays default block (rwire's `ChatDetail` pattern) so nothing competes with the
    // browser's native show/hide of the body when closed.
    let details = el(El::Details).id(key).append([summary_el, body]);
    if open {
        details.bool_attr(At::Open)
    } else {
        details
    }
}

/// A humanized one-line summary of a tool call's arguments — the command, the file path, the
/// search, the verdict. Never a raw JSON dump: an unrecognised tool falls back to a structured
/// `key: value · key: value` of its arguments.
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
        "write" | "edit" => field("path"),
        "read" => read_summary(&field("path"), &args),
        "list" => {
            let path = field("path");
            if path.is_empty() {
                "workspace root".to_owned()
            } else {
                path
            }
        }
        "find" => find_summary(&field("text"), &field("name"), &field("path")),
        "verdict" => {
            let decision = field("decision");
            let feedback = field("feedback");
            if feedback.is_empty() {
                decision
            } else {
                format!("{decision} — {}", one_line(&feedback, 60))
            }
        }
        "search" => field("query"),
        "fetch" => field("url"),
        _ => humanize_args(&args),
    };
    one_line(&text, 96)
}

/// `read` summary: the path, plus a compact line range when `offset`/`limit` are given.
fn read_summary(path: &str, args: &Value) -> String {
    let offset = args.get("offset").and_then(Value::as_i64);
    let limit = args.get("limit").and_then(Value::as_i64);
    match (offset, limit) {
        (Some(from), Some(count)) => format!("{path} · L{from}–{}", from + count),
        (Some(from), None) => format!("{path} · from L{from}"),
        _ => path.to_owned(),
    }
}

/// `find` summary: the content query (or name pattern) and where it's scoped.
fn find_summary(text: &str, name: &str, path: &str) -> String {
    let mut summary = if !text.is_empty() {
        format!("\"{}\"", one_line(text, 40))
    } else if !name.is_empty() {
        format!("name ~ {}", one_line(name, 40))
    } else {
        String::new()
    };
    if !path.is_empty() {
        summary.push_str(&format!(" in {path}"));
    }
    summary
}

/// A structured one-liner for an unrecognised tool's arguments: `key: value · key: value`, scalars
/// shortened — never the raw JSON blob.
fn humanize_args(args: &Value) -> String {
    match args {
        Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| format!("{key}: {}", scalar(value)))
            .collect::<Vec<_>>()
            .join(" · "),
        Value::Null => String::new(),
        other => one_line(&other.to_string(), 96),
    }
}

/// A short display form of a scalar argument value (strings shortened; arrays/objects compacted).
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => one_line(text, 40),
        other => one_line(&other.to_string(), 40),
    }
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
                .st([St::WhitespacePreWrap])
                .st([St::TextXs, St::TextMuted, St::BreakWords])
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
        .st([St::TextXs, St::TextMuted, St::FlexShrink0])
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
                    .st([St::TextXs, St::TextLow, St::MinW0, St::BreakWords])
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

/// Restore one file to its state before the pass that changed it (G3).
#[handler]
fn restore_file(app: &mut App, ctx: &EventContext) {
    let _ = app;
    if ctx.param_bytes().is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(ctx.param_bytes()) else {
        return;
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    crate::bridge::send_action(
        crate::bridge::Command::RestoreFile {
            workspace: field("workspace"),
            task: field("task"),
            repo: field("repo"),
            sha: field("sha"),
            path: field("path"),
        },
        ctx.defer(),
    );
}

/// Open a check's full log (P4c): the badge carries `{run, repo, check}` as one JSON `data`
/// payload; forward it to the bridge, which fetches from `ci_logs` and fills the logs drawer.
#[handler]
fn open_check_log(app: &mut App, ctx: &EventContext) {
    let _ = app;
    let Some(payload) = ctx.data("log") else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return;
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    crate::bridge::send(crate::bridge::Command::LoadCheckLog {
        run_id: field("run"),
        repo: field("repo"),
        check: field("check"),
    });
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
            .st([St::TextXs, St::TextMuted])
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
                .st([St::WhitespacePreWrap, St::BreakWords])
                .st([St::TextXs, color])
                .text(&format!("{prefix} {}", line.text)),
        );
    }
    Some(rows)
}

pub(crate) fn verdict_chip(run: &RunView) -> ElementBuilder {
    verdict_chip_of(run.verdict.as_deref(), run.ok)
}

/// The verdict / failure chip from raw parts — shared with the agent thread's turns.
pub(crate) fn verdict_chip_of(verdict: Option<&str>, ok: bool) -> ElementBuilder {
    let (text, color) = match verdict {
        Some("pass") => ("pass", St::TextAccent),
        Some("revise") => ("revise", St::TextMuted),
        Some("reject") => ("reject", St::TextLow),
        _ if !ok => ("failed", St::TextLow),
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
            id: "run-x".to_owned(),
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
            memory: String::new(),
            diff: String::new(),
            diff_files: Vec::new(),
            checks: Vec::new(),
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

    fn call(name: &str, arguments: &str) -> ToolCardView {
        ToolCardView {
            name: name.to_owned(),
            arguments: arguments.to_owned(),
            result: String::new(),
            ok: true,
            time: String::new(),
        }
    }

    #[test]
    fn tool_summary_humanizes_known_tools() {
        assert_eq!(
            tool_summary(&call("bash", r#"{"command":"cargo test"}"#)),
            "cargo test"
        );
        assert_eq!(
            tool_summary(&call("write", r#"{"path":"src/lib.rs","content":"x"}"#)),
            "src/lib.rs"
        );
        assert_eq!(
            tool_summary(&call(
                "read",
                r#"{"path":"src/lib.rs","offset":10,"limit":40}"#
            )),
            "src/lib.rs · L10–50"
        );
        assert_eq!(tool_summary(&call("list", "{}")), "workspace root");
        assert_eq!(
            tool_summary(&call("find", r#"{"text":"unwrap","path":"src"}"#)),
            "\"unwrap\" in src"
        );
        assert_eq!(
            tool_summary(&call(
                "verdict",
                r#"{"decision":"revise","feedback":"add tests"}"#
            )),
            "revise — add tests"
        );
        assert_eq!(
            tool_summary(&call("search", r#"{"query":"rust color crate"}"#)),
            "rust color crate"
        );
    }

    #[test]
    fn tool_summary_falls_back_to_structured_pairs_not_raw_json() {
        // An unrecognised tool becomes `key: value · key: value`, never the raw `{...}` blob.
        let summary = tool_summary(&call("mystery", r#"{"alpha":"one","beta":2}"#));
        assert_eq!(summary, "alpha: one · beta: 2");
        assert!(!summary.contains('{'), "no raw JSON: {summary}");
    }

    /// The old server-toggle marked its header with `data-tool`; a native disclosure carries none
    /// anywhere in its subtree.
    fn has_data_tool(element: &ElementBuilder) -> bool {
        element.attributes().iter().any(|(k, _)| k == "data-tool")
            || element.children().iter().any(has_data_tool)
    }

    /// The first `data-log` payload anywhere in the subtree — the fetch key a check badge carries.
    fn find_data_log(element: &ElementBuilder) -> Option<String> {
        if let Some((_, v)) = element.attributes().iter().find(|(k, _)| k == "data-log") {
            return Some(v.clone());
        }
        element.children().iter().find_map(find_data_log)
    }

    #[test]
    fn checks_row_badge_is_a_clickable_button_carrying_its_log_key() {
        let checks = vec![crate::state::CheckBadgeView {
            label: "clippy (projects/app)".to_owned(),
            ok: false,
            tail: "error".to_owned(),
            repo: "projects/app".to_owned(),
            check: "clippy".to_owned(),
            duration: String::new(),
        }];
        let row = checks_row("run-7", &checks, "k");
        // The badge is a button (P4c) — a click opens the drawer, so it must carry the run + repo +
        // check the bridge fetches the full log by.
        let payload = find_data_log(&row).expect("a check badge carries a data-log payload");
        let value: Value = serde_json::from_str(&payload).expect("data-log is JSON");
        assert_eq!(value["run"], "run-7");
        assert_eq!(value["repo"], "projects/app");
        assert_eq!(value["check"], "clippy");
    }

    #[test]
    fn disclosure_row_is_native_details_not_a_server_toggle() {
        let row = disclosure_row(
            "reqdiff.0",
            "diff",
            "src/lib.rs",
            None,
            el(El::Div).text("body"),
            false,
        );
        // Native <details>, keyed by `key` so a re-render's morph preserves the open state.
        assert!(matches!(row.el_type(), El::Details));
        assert!(
            row.attributes()
                .iter()
                .any(|(k, v)| k == "id" && v == "reqdiff.0"),
            "details carries the key as its id"
        );
        // A <summary> header over the body — and no server round-trip (`data-tool`) anywhere.
        let children = row.children();
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0].el_type(), El::Summary));
        assert!(
            !has_data_tool(&row),
            "native disclosure has no server toggle"
        );
    }

    #[test]
    fn tool_row_opens_client_side_and_honours_initial_state() {
        let row = tool_row(&call("bash", r#"{"command":"cargo test"}"#), "chat.0");
        assert!(matches!(row.el_type(), El::Details));
        assert!(matches!(row.children()[0].el_type(), El::Summary));
        assert!(!has_data_tool(&row));
        // `open` is the server-set *initial* state only — it changes the rendered element
        // (the native `open` attribute), then the browser owns every toggle after.
        let closed = disclosure_row("k", "n", "s", None, el(El::Div), false);
        let opened = disclosure_row("k", "n", "s", None, el(El::Div), true);
        assert_ne!(
            closed.content_hash(),
            opened.content_hash(),
            "open=true must set the native <details open> attribute"
        );
    }
}
