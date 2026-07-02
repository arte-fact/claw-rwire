//! Per-role conversation continuity across a loop step's passes.
//!
//! A loop step alternates a **producer** and a **validator**, up to a few iterations. Runs
//! are stateless (each is spawned fresh and posts its result back), so without help a re-run
//! would start from a blank slate — the producer would re-`read` files it already wrote, the
//! validator would re-inspect the whole tree. Here we rebuild each role's *own* thread at
//! launch from the runs already persisted for this attempt: the producer sees its earlier
//! tool calls + replies then the reviewer's feedback; the validator sees its earlier reviews
//! then the new output. The two threads touch only at the boundary.
//!
//! Reconstruction (not a stored transcript): a run's persisted `transcript`
//! (`[{name, arguments, result, ok}]`) and `output` are replayed as wire messages, one
//! assistant→tool pair per recorded call. Round grouping and any inter-round assistant prose
//! aren't persisted, so the replay is a faithful approximation — each call becomes its own round.

use serde::Deserialize;

use crate::agent::{ChatMessage, FunctionCall, Role, ToolCall};
use crate::db::task_runs::TaskRun;

/// One tool call as persisted in a run's transcript. The stored `ok` flag isn't needed to
/// replay the exchange, so it's left out (serde ignores the extra field).
#[derive(Deserialize)]
struct Call {
    name: String,
    arguments: String,
    result: String,
}

/// Replay one finished run as `assistant(tool_calls)` / `tool(result)` pairs followed by the
/// assistant's reply. `run_idx` keeps the synthesized tool-call ids unique across the thread.
/// A run with neither calls nor output still yields one assistant turn, so a following user
/// turn never collides with the preceding one.
fn replay_run(run: &TaskRun, run_idx: usize) -> Vec<ChatMessage> {
    let mut msgs = Vec::new();
    if let Some(json) = run.transcript.as_deref()
        && let Ok(calls) = serde_json::from_str::<Vec<Call>>(json)
    {
        for (i, call) in calls.iter().enumerate() {
            let id = format!("r{run_idx}c{i}");
            msgs.push(ChatMessage::assistant_tool_calls(vec![ToolCall {
                id: id.clone(),
                kind: "function".to_owned(),
                function: FunctionCall {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            }]));
            msgs.push(ChatMessage::tool_result(id, call.result.clone()));
        }
    }
    let reply = run.output.as_deref().unwrap_or("").trim();
    if !reply.is_empty() {
        msgs.push(ChatMessage::text(Role::Assistant, reply));
    } else if msgs.is_empty() {
        msgs.push(ChatMessage::text(Role::Assistant, "(completed)"));
    }
    msgs
}

/// The producer's thread: the spec opener, then — for each prior pass — the producer's own
/// replayed work followed by the reviewer's "revise" feedback. The final feedback turn is the
/// current prompt. Empty `prior` (iteration 0) yields `[system, user(spec opener)]`.
/// `condensed` carries the mid-unit fold (R3 degradation ladder): older passes of a long loop,
/// already compacted to one-line notes, presented before the verbatim tail.
pub(super) fn producer_thread(
    system: &str,
    spec: &str,
    step_name: &str,
    condensed: Option<&str>,
    prior: &[TaskRun],
) -> Vec<ChatMessage> {
    let mut msgs = vec![
        ChatMessage::system(system),
        ChatMessage::text(Role::User, step_context(spec, &[], step_name)),
    ];
    if let Some(note) = condensed.map(str::trim).filter(|note| !note.is_empty()) {
        msgs.push(ChatMessage::text(
            Role::User,
            format!("Earlier passes of this task, condensed to fit your context:\n{note}"),
        ));
    }
    let mut run_idx = 0;
    for run in prior {
        match run.role.as_str() {
            "producer" | "stage" if run.ok => {
                msgs.extend(replay_run(run, run_idx));
                run_idx += 1;
            }
            "validator" if run.verdict.as_deref() == Some("revise") => {
                if let Some(feedback) = run
                    .feedback
                    .as_deref()
                    .map(str::trim)
                    .filter(|fb| !fb.is_empty())
                {
                    msgs.push(ChatMessage::text(
                        Role::User,
                        format!(
                            "The reviewer asked you to revise. Feedback to address:\n{feedback}"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    msgs
}

/// The validator's thread: for each prior review, the output it saw then its own replayed
/// inspection + verdict; finally the current output with the full review instructions. Empty
/// `prior` (iteration 0) yields `[system, user(validator_context)]`.
pub(super) fn validator_thread(
    system: &str,
    spec: &str,
    current_output: &str,
    files: &[String],
    prior: &[TaskRun],
) -> Vec<ChatMessage> {
    let mut msgs = vec![ChatMessage::system(system)];
    let mut last_output: Option<&str> = None;
    let mut run_idx = 0;
    for run in prior {
        match run.role.as_str() {
            "producer" | "stage" if run.ok => last_output = run.output.as_deref(),
            "validator" => {
                let reviewed = last_output.unwrap_or("").trim();
                msgs.push(ChatMessage::text(Role::User, light_present(spec, reviewed)));
                msgs.extend(replay_run(run, run_idx));
                run_idx += 1;
            }
            _ => {}
        }
    }
    let mut current = validator_context(spec, current_output, files);
    // On a re-review the validator already inspected an earlier version (above), so point it at
    // the delta instead of a full re-inspection — cheaper, and it still runs the build/tests.
    if run_idx > 0 {
        current.push_str(RE_REVIEW_FOCUS);
    }
    msgs.push(ChatMessage::text(Role::User, current));
    msgs
}

/// Appended to the current review turn only when the validator has already reviewed an earlier
/// version in this thread — keeps re-reviews incremental without weakening the first pass.
const RE_REVIEW_FOCUS: &str = "\n\nYou already reviewed an earlier version above. This is a \
re-review: focus on whether the feedback you gave was addressed and that the project still builds \
and its tests still pass (run them) — you don't need to re-inspect from scratch what you already \
checked and were satisfied with. Then call the `verdict` tool.";

/// A compact re-presentation of an already-reviewed output for the validator's history — no
/// file tree or instructions (those belong only on the current turn), just the deliverable.
fn light_present(spec: &str, output: &str) -> String {
    format!("Task spec:\n{spec}\n\nThe assignee produced this for review:\n\n{output}")
}

/// The user turn that opens a producer/stage run: the spec and (for the summary fallback path)
/// earlier outputs + the reviewer's latest "revise" feedback. In the loop it's called with no
/// prior — replay carries the real history — but it still handles prior for a plain multi-stage
/// pipeline where a later stage builds on earlier ones' outputs.
fn step_context(spec: &str, prior: &[TaskRun], step_name: &str) -> String {
    let mut text = format!("Task: {spec}");
    let outputs: Vec<&str> = prior
        .iter()
        .filter(|run| run.ok && (run.role == "stage" || run.role == "producer"))
        .filter_map(|run| run.output.as_deref())
        .filter(|out| !out.trim().is_empty())
        .collect();
    if !outputs.is_empty() {
        text.push_str("\n\nWork produced so far:");
        for (index, out) in outputs.iter().enumerate() {
            text.push_str(&format!("\n\n--- step {} ---\n{out}", index + 1));
        }
    }
    if let Some(feedback) = prior
        .iter()
        .rev()
        .find(|run| run.role == "validator" && run.verdict.as_deref() == Some("revise"))
        .and_then(|run| run.feedback.as_deref())
        .filter(|fb| !fb.trim().is_empty())
    {
        text.push_str(&format!(
            "\n\nThe reviewer asked you to revise. Feedback to address:\n{feedback}"
        ));
    }
    text.push_str(&format!(
        "\n\nYour step: \"{step_name}\". {}",
        if outputs.is_empty() {
            "Do it and briefly report what you did."
        } else {
            "Build on / revise the work above and briefly report what you did."
        }
    ));
    text
}

/// The user turn for a validator run: the spec, the producer's output to review, and the real
/// workspace paths so the reviewer reads the actual files instead of guessing bare names.
fn validator_context(spec: &str, output: &str, files: &[String]) -> String {
    let tree = if files.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = files.iter().map(|path| format!("- {path}")).collect();
        format!(
            "\n\nWorkspace files (use these exact paths with `read`):\n{}",
            lines.join("\n")
        )
    };
    format!(
        "Task spec:\n{spec}\n\nThe assignee produced this for review:\n\n{output}{tree}\n\n\
         Review it against the spec: `list` the workspace to see what's actually there, `find` to \
         locate files or grep for a pattern, `read` the files, and if it's code use `bash` to run \
         the project's build and tests yourself (e.g. `cargo build`/`cargo test`) rather than \
         trusting the report. Then call the `verdict` tool with your decision and concrete feedback."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `TaskRun` for thread tests — only the fields reconstruction reads matter.
    fn run(
        role: &str,
        output: Option<&str>,
        verdict: Option<&str>,
        feedback: Option<&str>,
        transcript: Option<&str>,
    ) -> TaskRun {
        TaskRun {
            id: "r".to_owned(),
            position: 0,
            role: role.to_owned(),
            agent: None,
            output: output.map(str::to_owned),
            verdict: verdict.map(str::to_owned),
            feedback: feedback.map(str::to_owned),
            iteration: 0,
            ok: true,
            transcript: transcript.map(str::to_owned),
            attempt: 0,
            created_at: String::new(),
            memory: None,
        }
    }

    fn roles(msgs: &[ChatMessage]) -> Vec<Role> {
        msgs.iter().map(|m| m.role).collect()
    }

    #[test]
    fn producer_thread_iteration_zero_matches_the_plain_opener() {
        let msgs = producer_thread("sys", "make X", "Build", None, &[]);
        assert_eq!(roles(&msgs), vec![Role::System, Role::User]);
        assert_eq!(
            msgs[1].content.as_deref(),
            Some(step_context("make X", &[], "Build").as_str())
        );
    }

    #[test]
    fn producer_thread_replays_own_work_then_ends_on_the_feedback() {
        let transcript = r#"[{"name":"write","arguments":"{}","result":"ok","ok":true},
                {"name":"bash","arguments":"{}","result":"tests pass","ok":true}]"#;
        let prior = vec![
            run("producer", Some("did it"), None, None, Some(transcript)),
            run("validator", None, Some("revise"), Some("add a test"), None),
        ];
        let msgs = producer_thread("sys", "make X", "Build", None, &prior);
        // system, opener, [asst/tool ×2], asst(reply), user(feedback)
        assert_eq!(
            roles(&msgs),
            vec![
                Role::System,
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
                Role::User,
            ]
        );
        // The reply and the feedback both survived, in order.
        assert_eq!(msgs[6].content.as_deref(), Some("did it"));
        let last = msgs.last().expect("non-empty");
        assert_eq!(last.role, Role::User);
        assert!(last.content.as_deref().unwrap_or("").contains("add a test"));
        // Synthesized tool-call ids are unique and matched by their tool results.
        assert_eq!(msgs[2].tool_calls[0].id, "r0c0");
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("r0c0"));
        assert_eq!(msgs[4].tool_calls[0].id, "r0c1");
        assert_eq!(msgs[5].tool_call_id.as_deref(), Some("r0c1"));
    }

    #[test]
    fn producer_thread_presents_the_condensed_note_before_the_verbatim_tail() {
        let prior = vec![run("producer", Some("latest pass"), None, None, None)];
        let msgs = producer_thread(
            "sys",
            "make X",
            "Build",
            Some("- pass (iteration 1): first pass"),
            &prior,
        );
        // system, opener, user(condensed), asst(latest pass)
        assert_eq!(
            roles(&msgs),
            vec![Role::System, Role::User, Role::User, Role::Assistant]
        );
        let note = msgs[2].content.as_deref().unwrap_or("");
        assert!(note.contains("condensed to fit your context"));
        assert!(note.contains("first pass"));
        assert_eq!(msgs[3].content.as_deref(), Some("latest pass"));
        // A blank note adds no turn.
        let msgs = producer_thread("sys", "make X", "Build", Some("  "), &[]);
        assert_eq!(roles(&msgs), vec![Role::System, Role::User]);
    }

    #[test]
    fn validator_thread_iteration_zero_is_a_single_review_turn() {
        let files = vec!["projects/x/src/lib.rs".to_owned()];
        let msgs = validator_thread("sys", "the spec", "the output", &files, &[]);
        assert_eq!(roles(&msgs), vec![Role::System, Role::User]);
        let turn = msgs[1].content.as_deref().unwrap_or("");
        assert!(turn.contains("projects/x/src/lib.rs"));
        assert!(turn.contains("verdict"));
        // A first review is not a re-review — no incremental-focus note.
        assert!(!turn.contains("re-review"));
    }

    #[test]
    fn validator_thread_replays_prior_review_then_presents_the_new_output() {
        let verdict_call = r#"[{"name":"verdict","arguments":"{}","result":"recorded","ok":true}]"#;
        let prior = vec![
            run("producer", Some("v1"), None, None, None),
            run(
                "validator",
                Some("needs work"),
                Some("revise"),
                Some("fix the edge case"),
                Some(verdict_call),
            ),
        ];
        let msgs = validator_thread("sys", "the spec", "v2", &[], &prior);
        // system, user(present v1), asst(verdict call), tool, asst(reply), user(current: v2)
        assert_eq!(
            roles(&msgs),
            vec![
                Role::System,
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
                Role::User,
            ]
        );
        assert!(msgs[1].content.as_deref().unwrap_or("").contains("v1"));
        assert_eq!(msgs[2].tool_calls[0].function.name, "verdict");
        let last = msgs.last().expect("non-empty");
        assert!(last.content.as_deref().unwrap_or("").contains("v2"));
        assert!(last.content.as_deref().unwrap_or("").contains("verdict"));
        // Second time around the reviewer is told to focus on the delta, not re-inspect.
        assert!(last.content.as_deref().unwrap_or("").contains("re-review"));
    }

    #[test]
    fn replay_run_without_a_transcript_is_just_the_reply() {
        let msgs = replay_run(&run("producer", Some("summary only"), None, None, None), 0);
        assert_eq!(roles(&msgs), vec![Role::Assistant]);
        assert_eq!(msgs[0].content.as_deref(), Some("summary only"));
        assert!(msgs[0].tool_calls.is_empty());
    }

    // Moved from scheduler: the context builders now live here.
    #[test]
    fn validator_context_lists_the_real_file_paths() {
        let files = vec!["projects/rogue/src/render.rs".to_owned()];
        let ctx = validator_context("the spec", "the output", &files);
        assert!(ctx.contains("projects/rogue/src/render.rs"));
        assert!(ctx.contains("verdict"));
        assert!(!validator_context("s", "o", &[]).contains("Workspace files"));
    }

    #[test]
    fn step_context_chains_prior_outputs() {
        let prior = vec![run("stage", Some("built the thing"), None, None, None)];
        let ctx = step_context("make X", &prior, "Refine");
        assert!(ctx.contains("Task: make X"));
        assert!(ctx.contains("built the thing"));
        assert!(ctx.contains("Refine"));
    }
}
