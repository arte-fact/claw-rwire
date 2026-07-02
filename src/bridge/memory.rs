//! R3 memory mechanics: budget-aware **F→U→W→M** prompt composition, the producer-side fold
//! at task completion, and the last-task recap that keeps sequential work warm.
//!
//! Memory is the *elastic* layer — fixed identity can't compress and working headroom must not
//! compress, so memory absorbs all the sizing pressure and fills last. The same rule runs on an
//! 8k model and a 128k model: the small one remembers less and folds sooner, degrading
//! continuously rather than off a cliff.

use crate::agent;
use crate::db::{self, resolution, task_runs::TaskRun, tasks::Task};
use crate::state::{ChatMessage, MessageRole, RunKind};
use crate::tools::EmptyToolbox;

use super::{Command, log_line, send};

/// Per-role budget shares (the keystone table): `w` is the working headroom reserved *before*
/// memory (room for the tool rounds to come), `m` the ceiling memory may fill up to. Validators
/// get no memory layer at all — their unit (the replayed loop) *is* their memory. A planning
/// turn is small (one turn, one tool call), so it affords the concierge's generous shares on
/// the same workspace row its producer runs use.
pub(super) struct RoleShares {
    pub w: f64,
    pub m: f64,
}

pub(super) fn shares_for(run: RunKind) -> RoleShares {
    match run {
        RunKind::Chat | RunKind::Planning => RoleShares { w: 0.30, m: 0.40 },
        RunKind::Task => RoleShares { w: 0.50, m: 0.20 },
        RunKind::Validator => RoleShares { w: 0.60, m: 0.0 },
    }
}

/// Compose-time token estimate: ≈ chars/4 (corrected later by the endpoint's reported
/// `prompt_tokens` where a path tracks usage, as the chat fold trigger does).
pub(super) fn estimate_tokens(chars: usize) -> u32 {
    (chars.div_ceil(4)).min(u32::MAX as usize) as u32
}

/// B = C − R: the prompt budget after the output reserve (an eighth of the window, kept
/// within [512, 4096] so tiny models still get room to answer and huge ones don't over-reserve).
pub(super) fn prompt_budget(context_tokens: u32) -> u32 {
    let reserve = (context_tokens / 8).clamp(512, 4096);
    context_tokens.saturating_sub(reserve)
}

/// The memory allowance (tokens) once the fixed identity (`f`) and the current unit (`u`) are
/// placed and the role's working headroom is reserved: `min(m·B, B − F − U − w·B)`, floored
/// at 0. Fill order F → U → W → M — memory takes only what's genuinely left.
pub(super) fn memory_allowance(b: u32, f: u32, u: u32, shares: &RoleShares) -> u32 {
    let b_f = f64::from(b);
    let ceiling = (shares.m * b_f) as u32;
    let headroom = (shares.w * b_f) as u32;
    let left = b
        .saturating_sub(f)
        .saturating_sub(u)
        .saturating_sub(headroom);
    ceiling.min(left)
}

/// Marker appended when the rolling summary had to be cut to fit the allowance.
const TRIM_MARKER: &str = "… (older memory trimmed to fit)";

/// Fill the memory layer by value — recency first, then compression (degradation-ladder steps
/// 1–2): the last-task recap goes in whole if it fits, else only its headline; the rolling
/// summary takes the remainder, hard-truncated with a marker when it must. `None` when nothing
/// exists or nothing fits.
pub(super) fn fill_memory(
    allowance_tokens: u32,
    recap: Option<&str>,
    summary: Option<&str>,
) -> Option<String> {
    let mut left = allowance_tokens as usize * 4; // back to chars
    let mut parts: Vec<String> = Vec::new();
    if let Some(recap) = recap.map(str::trim).filter(|r| !r.is_empty()) {
        if recap.chars().count() <= left {
            left -= recap.chars().count();
            parts.push(recap.to_owned());
        } else if let Some(headline) = recap.lines().next().map(str::trim)
            && !headline.is_empty()
            && headline.chars().count() <= left
        {
            left -= headline.chars().count();
            parts.push(headline.to_owned());
        }
    }
    if let Some(summary) = summary.map(str::trim).filter(|s| !s.is_empty()) {
        if summary.chars().count() <= left {
            parts.push(summary.to_owned());
        } else if let Some(room) = left.checked_sub(TRIM_MARKER.chars().count() + 1)
            && room > 0
        {
            let cut: String = summary.chars().take(room).collect();
            parts.push(format!("{}\n{TRIM_MARKER}", cut.trim_end()));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Cap a run's provenance snapshot (R3.5 M4) — enough to answer "what did it remember at
/// launch?" without a big-context model's memory bloating every run row.
pub(super) fn snapshot(memory: &str) -> String {
    const CAP: usize = 4000;
    if memory.chars().count() <= CAP {
        return memory.to_owned();
    }
    let cut: String = memory.chars().take(CAP).collect();
    format!("{}…", cut.trim_end())
}

/// Frame a producer/planner's workspace memory as its system-prompt layer.
pub(super) fn wrap_workspace_memory(memory: &str) -> String {
    format!(
        "## Working memory\n\nWhat you remember from earlier work in this workspace (older \
         tasks have been condensed here):\n\n{memory}"
    )
}

// ── The last-task recap (the warm slot) ──

/// Recap the most recent task `agent_id` finished in this workspace — spec → outcome → verdict
/// → files — for the next launch's warm slot. Sequential tasks chain, so the previous task is
/// worth more, near-verbatim, than anything older.
pub(super) fn last_task_recap(
    conn: &rusqlite::Connection,
    agent_id: &str,
    workspace_id: &str,
    exclude_task: &str,
) -> Option<String> {
    let task = db::tasks::last_finished_for_agent(conn, agent_id, workspace_id, exclude_task)
        .ok()
        .flatten()?;
    let runs = db::task_runs::list_for_task_attempt(conn, &task.id, task.attempt).ok()?;
    Some(recap_text(&task, &runs))
}

/// The recap itself, pure: headline (title + how it ended), the spec, the final produced
/// outcome, the last verdict, and the files the producer touched.
pub(super) fn recap_text(task: &Task, runs: &[TaskRun]) -> String {
    let mut text = format!(
        "Last task — \"{}\" ({})\nSpec: {}",
        task.title,
        task.status.as_str(),
        snippet(&task.spec, 3, 400)
    );
    if let Some(outcome) = runs
        .iter()
        .rev()
        .filter(|run| run.ok && (run.role == "producer" || run.role == "stage"))
        .find_map(|run| run.output.as_deref())
        .map(str::trim)
        .filter(|out| !out.is_empty())
    {
        text.push_str(&format!("\nOutcome: {}", snippet(outcome, 4, 600)));
    }
    if let Some(run) = runs
        .iter()
        .rev()
        .find(|run| run.role == "validator" && run.verdict.is_some())
    {
        let verdict = run.verdict.as_deref().unwrap_or_default();
        let feedback = run.feedback.as_deref().unwrap_or_default().trim();
        text.push_str(&format!("\nVerdict: {verdict}"));
        if !feedback.is_empty() {
            text.push_str(&format!(" — {}", snippet(feedback, 2, 300)));
        }
    }
    let files = files_touched(runs);
    if !files.is_empty() {
        text.push_str(&format!("\nFiles touched: {}", files.join(", ")));
    }
    text
}

/// Distinct `write`/`edit` target paths across the runs' transcripts, in first-touch order,
/// capped so a sweeping task doesn't turn the recap into a manifest.
fn files_touched(runs: &[TaskRun]) -> Vec<String> {
    const CAP: usize = 12;
    let mut files: Vec<String> = Vec::new();
    for transcript in runs.iter().filter_map(|run| run.transcript.as_deref()) {
        let Ok(calls) = serde_json::from_str::<Vec<serde_json::Value>>(transcript) else {
            continue;
        };
        for call in calls {
            let name = call.get("name").and_then(serde_json::Value::as_str);
            if !matches!(name, Some("write") | Some("edit")) {
                continue;
            }
            if let Some(path) = call
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
                .and_then(|args| {
                    args.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                && !files.contains(&path)
            {
                files.push(path);
                if files.len() == CAP {
                    return files;
                }
            }
        }
    }
    files
}

/// At most `max_lines` lines and `max_chars` characters of `text`, with an ellipsis when cut.
fn snippet(text: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = text.trim().lines().take(max_lines).collect();
    let joined = lines.join("\n");
    let mut cut: String = joined.chars().take(max_chars).collect();
    if cut.chars().count() < text.trim().chars().count() {
        cut.push('…');
    }
    cut
}

// ── The mid-unit fold (degradation-ladder step 3) ──

/// When an attempt's replay outgrows the unit budget, condense the oldest passes to one-line
/// notes and keep only the latest producer pass onward verbatim. Returns the condensed note and
/// how many leading runs it covers; `(None, 0)` when the attempt fits (or there is no older
/// pass to fold — the latest pass is never trimmed, whatever the budget).
pub(super) fn compact_attempt(prior: &[TaskRun], budget_chars: usize) -> (Option<String>, usize) {
    let total: usize = prior.iter().map(run_chars).sum();
    if total <= budget_chars {
        return (None, 0);
    }
    let keep_from = prior
        .iter()
        .rposition(|run| run.ok && (run.role == "producer" || run.role == "stage"))
        .unwrap_or(0);
    if keep_from == 0 {
        return (None, 0);
    }
    let lines: Vec<String> = prior[..keep_from].iter().map(run_note).collect();
    (Some(lines.join("\n")), keep_from)
}

/// The replayed size of a whole attempt, for the unit (U) estimate at compose time.
pub(super) fn attempt_chars(runs: &[TaskRun]) -> usize {
    runs.iter().map(run_chars).sum()
}

fn run_chars(run: &TaskRun) -> usize {
    run.output.as_deref().map(str::len).unwrap_or(0)
        + run.transcript.as_deref().map(str::len).unwrap_or(0)
        + 64 // per-message framing overhead
}

fn run_note(run: &TaskRun) -> String {
    match run.role.as_str() {
        "validator" => {
            let verdict = run.verdict.as_deref().unwrap_or("(no verdict)");
            let feedback = run.feedback.as_deref().unwrap_or_default();
            format!(
                "- review (iteration {}): {verdict} — {}",
                run.iteration + 1,
                snippet(feedback, 1, 160)
            )
        }
        _ => format!(
            "- pass (iteration {}): {}",
            run.iteration + 1,
            snippet(run.output.as_deref().unwrap_or("(no output)"), 1, 160)
        ),
    }
}

// ── The producer-side fold writer (at task completion) ──

/// The instruction for a workspace-memory fold: merge a finished task's recap into the agent's
/// durable memory of the workspace. `{target}` is replaced with the word budget.
const FOLD_PROMPT: &str = "You maintain an agent's long-term working memory for one workspace. \
Merge the existing memory with the recap of the task that just finished into one concise \
memory. Keep durable knowledge — the workspace's layout, conventions, decisions, what shipped, \
what failed and why; drop step-by-step forensics. Write terse notes, not prose, at most \
{target} words. Output only the memory.";

/// Fold a finished task into its producing agents' (agent, workspace) memory — the task is the
/// worker's natural turn boundary, as the message is the concierge's. One summarizer call per
/// distinct producing agent (usually one), off the bridge thread; best-effort. Paused rows are
/// skipped, as are agents whose endpoint can't resolve.
pub(super) fn fold_finished_task(
    conn: &rusqlite::Connection,
    task: &Task,
    status: crate::db::tasks::TaskStatus,
) {
    let runs =
        db::task_runs::list_for_task_attempt(conn, &task.id, task.attempt).unwrap_or_default();
    let mut producers: Vec<String> = Vec::new();
    for run in &runs {
        if run.ok
            && (run.role == "producer" || run.role == "stage")
            && let Some(agent) = &run.agent
            && !producers.contains(agent)
        {
            producers.push(agent.clone());
        }
    }
    if producers.is_empty() {
        return;
    }
    let mut finished = task.clone();
    finished.status = status;
    let recap = recap_text(&finished, &runs);
    for agent_id in producers {
        let memory =
            db::agent_memory::get(conn, &agent_id, Some(&task.workspace_id)).unwrap_or_default();
        if memory.paused {
            continue;
        }
        let Ok(inference) =
            resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, |key| {
                std::env::var(key).ok()
            })
        else {
            continue;
        };
        // Size the memory to its owner's budget: the M ceiling of the agent's own model, in
        // words (≈ ¾ of the token allowance) — a small model keeps terser workspace notes.
        let budget = prompt_budget(
            super::cached_context(&inference).unwrap_or(super::DEFAULT_CONTEXT_TOKENS),
        );
        let target_words = ((f64::from(budget) * shares_for(RunKind::Task).m) * 0.75) as u32;
        let instruction = FOLD_PROMPT.replace("{target}", &target_words.to_string());
        let previous = memory.summary.clone();
        let workspace = task.workspace_id.clone();
        let recap = recap.clone();
        let task_id = task.id.clone();
        let title = task.title.clone();
        std::thread::spawn(move || {
            let user = ChatMessage {
                role: MessageRole::User,
                text: format!(
                    "Existing memory (may be empty):\n{previous}\n\n\
                     --- Recap of the task that just finished ---\n{recap}"
                ),
                time: super::now_hm(),
            };
            let messages = agent::build_messages(&instruction, std::slice::from_ref(&user));
            match agent::run_turn(&inference, messages, &EmptyToolbox) {
                Ok(turn) => {
                    if let Some(summary) = turn
                        .reply
                        .map(|reply| reply.trim().to_owned())
                        .filter(|reply| !reply.is_empty())
                    {
                        send(Command::UpdateMemory {
                            agent_id,
                            workspace: Some(workspace),
                            summary,
                            summarized_through: 0,
                            note: format!("condensed \u{201c}{title}\u{201d} into memory"),
                            source: Some(task_id),
                        });
                    }
                }
                Err(err) => log_line(format!("memory: workspace fold failed: {err}")),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{TaskKind, TaskStatus};

    #[test]
    fn prompt_budget_reserves_an_eighth_within_bounds() {
        assert_eq!(prompt_budget(8192), 8192 - 1024);
        assert_eq!(prompt_budget(2048), 2048 - 512, "reserve floors at 512");
        assert_eq!(prompt_budget(131072), 131072 - 4096, "reserve caps at 4096");
    }

    #[test]
    fn memory_allowance_takes_the_smaller_of_ceiling_and_leftover() {
        let producer = shares_for(RunKind::Task); // w 0.50, m 0.20
        // Roomy: the ceiling binds (20% of 10_000 = 2000).
        assert_eq!(memory_allowance(10_000, 500, 1000, &producer), 2000);
        // Tight: the leftover binds (10_000 − 3000 − 3000 − 5000 < 2000 → 0-floored).
        assert_eq!(memory_allowance(10_000, 3000, 3000, &producer), 0);
        // Validators have no memory ceiling at all.
        assert_eq!(
            memory_allowance(10_000, 100, 100, &shares_for(RunKind::Validator)),
            0
        );
    }

    #[test]
    fn fill_memory_prefers_the_recap_then_fits_the_summary() {
        let recap = "Last task — \"Build\" (done)\nSpec: do it";
        let summary = "knows the layout";
        // Roomy: both fit, recap first (recency beats compression).
        let full = fill_memory(1000, Some(recap), Some(summary)).expect("some");
        assert!(full.starts_with("Last task"));
        assert!(full.ends_with("knows the layout"));
        // Tight (36 chars): the recap doesn't fit whole, so only its headline survives, and
        // there's no room left for any of the summary.
        let tight = fill_memory(9, Some(recap), Some(summary)).expect("some");
        assert_eq!(tight, "Last task — \"Build\" (done)");
        // A long summary is hard-truncated to the room left, with the marker.
        let trimmed = fill_memory(25, None, Some(&"x".repeat(400))).expect("some");
        assert!(trimmed.contains(TRIM_MARKER));
        assert!(trimmed.chars().count() <= 100, "stays within the allowance");
        // Nothing to fill, or no room at all → no layer.
        assert_eq!(fill_memory(1000, None, None), None);
        assert_eq!(fill_memory(0, Some(recap), Some(summary)), None);
    }

    fn run(role: &str, output: &str, iteration: i64) -> TaskRun {
        TaskRun {
            id: "r".to_owned(),
            position: 0,
            role: role.to_owned(),
            agent: Some("ag".to_owned()),
            output: Some(output.to_owned()),
            verdict: (role == "validator").then(|| "revise".to_owned()),
            feedback: (role == "validator").then(|| "fix the edge case\nmore".to_owned()),
            iteration,
            ok: true,
            transcript: None,
            attempt: 0,
            created_at: String::new(),
            memory: None,
        }
    }

    fn done_task() -> Task {
        Task {
            id: "t1".to_owned(),
            workspace_id: "ws".to_owned(),
            agent: None,
            title: "Build the widget".to_owned(),
            spec: "make a widget\nwith care".to_owned(),
            kind: TaskKind::OneShot,
            schedule: None,
            status: TaskStatus::Done,
            result: None,
            priority: 0,
            origin_chat: None,
            next_run: None,
            step_position: 0,
            iteration: 0,
            run_epoch: 0,
            auto_retries: 0,
            max_retries: None,
            attempt: 0,
            is_plan: false,
            plan_id: None,
            archived_at: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn recap_text_reads_spec_outcome_verdict_and_files() {
        let transcript = r#"[{"name":"write","arguments":"{\"path\":\"src/widget.rs\"}","result":"ok","ok":true},
             {"name":"read","arguments":"{\"path\":\"ignored.rs\"}","result":"…","ok":true},
             {"name":"edit","arguments":"{\"path\":\"src/lib.rs\"}","result":"ok","ok":true}]"#;
        let mut producer = run("producer", "built it, tests pass", 0);
        producer.transcript = Some(transcript.to_owned());
        let runs = vec![producer, run("validator", "review", 0)];
        let recap = recap_text(&done_task(), &runs);
        assert!(recap.starts_with("Last task — \"Build the widget\" (done)"));
        assert!(recap.contains("Spec: make a widget"));
        assert!(recap.contains("Outcome: built it, tests pass"));
        assert!(recap.contains("Verdict: revise — fix the edge case"));
        assert!(recap.contains("Files touched: src/widget.rs, src/lib.rs"));
        assert!(!recap.contains("ignored.rs"), "reads are not touches");
    }

    #[test]
    fn recap_text_omits_sections_it_has_nothing_for() {
        let recap = recap_text(&done_task(), &[]);
        assert!(recap.contains("Spec:"));
        assert!(!recap.contains("Outcome:"));
        assert!(!recap.contains("Verdict:"));
        assert!(!recap.contains("Files touched:"));
    }

    #[test]
    fn compact_attempt_folds_only_what_precedes_the_latest_pass() {
        let prior = vec![
            run("producer", &"first pass output ".repeat(50), 0),
            run("validator", "needs work", 0),
            run("producer", &"second pass output ".repeat(50), 1),
            run("validator", "still not there", 1),
        ];
        // Fits → untouched.
        assert_eq!(compact_attempt(&prior, 1_000_000), (None, 0));
        // Over budget → everything before the latest producer pass becomes one-liners; the
        // latest pass + its review stay verbatim (never trimmed, whatever the budget).
        let (note, skip) = compact_attempt(&prior, 100);
        assert_eq!(skip, 2);
        let note = note.expect("condensed");
        assert!(note.contains("- pass (iteration 1): first pass output"));
        assert!(note.contains("- review (iteration 1): revise — fix the edge case"));
        assert!(
            !note.contains("second pass"),
            "the verbatim tail isn't in the note"
        );
        // A single-pass attempt has nothing older to fold, however large.
        assert_eq!(compact_attempt(&prior[2..3], 10), (None, 0));
    }

    #[test]
    fn fold_skips_paused_rows_and_unresolvable_agents_without_writing() {
        // No endpoint for the producing agent → resolve fails → no worker, no write; and a
        // paused (agent, workspace) row is skipped before resolution. Either way the call
        // returns without touching the row.
        let conn = crate::db::open_in_memory().expect("db");
        crate::db::workspaces::create(&conn, "ws", "W").expect("ws");
        // No endpoint configured — resolution must fail, and the fold must shrug it off.
        crate::db::agents::create(&conn, "ag-x", "X", None, None).expect("agent");
        crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: Some("ag-x"),
                title: "T",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("task");
        crate::db::task_runs::append(
            &conn,
            &crate::db::task_runs::NewRun {
                id: "r1",
                task_id: "t1",
                position: 0,
                role: "producer",
                agent: Some("ag-x"),
                output: Some("did it"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt: 0,
                memory: None,
            },
        )
        .expect("run");
        let task = crate::db::tasks::get(&conn, "t1")
            .expect("get")
            .expect("some");
        fold_finished_task(&conn, &task, TaskStatus::Done);
        let row = db::agent_memory::get(&conn, "ag-x", Some("ws")).expect("row");
        assert_eq!(row.summary, "", "unresolvable agent → nothing written");
    }
}
