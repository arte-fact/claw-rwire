//! The scheduler's unit tests — decision fns only (no workers, no HTTP): next-action
//! stepping, retry backoff, verdict routing, gates, steer consumption, resume-at-review.

use super::verdicts::*;
use super::*;

/// Minimal run for the resume-at-review decision — only the fields it reads matter.
fn resume_run(
    role: &str,
    ok: bool,
    verdict: Option<&str>,
    output: &str,
) -> crate::db::task_runs::TaskRun {
    crate::db::task_runs::TaskRun {
        id: "r".to_owned(),
        position: 0,
        role: role.to_owned(),
        agent: None,
        output: Some(output.to_owned()),
        verdict: verdict.map(str::to_owned),
        feedback: None,
        iteration: 0,
        ok,
        transcript: None,
        attempt: 0,
        created_at: String::new(),
        memory: None,
    }
}

#[test]
fn unreviewed_output_resumes_only_when_the_work_awaits_a_verdict() {
    // Produced, then the validator died in transit (ok=0, no verdict) → resume at review.
    let prior = vec![
        resume_run("producer", true, None, "the work"),
        resume_run("validator", false, None, "request failed: timed out"),
    ];
    assert_eq!(unreviewed_output(&prior, 0), Some("the work"));
    // Produced and genuinely reviewed (revise) → no resume; the producer runs next.
    let prior = vec![
        resume_run("producer", true, None, "the work"),
        resume_run("validator", true, Some("revise"), "needs work"),
    ];
    assert_eq!(unreviewed_output(&prior, 0), None);
    // Fresh attempt (no producer yet) / failed producer only → no resume.
    assert_eq!(unreviewed_output(&[], 0), None);
    let prior = vec![resume_run("producer", false, None, "request failed")];
    assert_eq!(unreviewed_output(&prior, 0), None);
    // A later unreviewed pass resumes with the LATEST output, not the reviewed first one.
    let prior = vec![
        resume_run("producer", true, None, "v1"),
        resume_run("validator", true, Some("revise"), "fix it"),
        resume_run("producer", true, None, "v2"),
        resume_run("validator", false, None, "timed out"),
    ];
    assert_eq!(unreviewed_output(&prior, 0), Some("v2"));
    // A different step's output doesn't resume this step.
    assert_eq!(unreviewed_output(&prior, 1), None);
}

#[test]
fn consume_steers_drains_the_queue_in_order_exactly_once() {
    let conn = crate::db::open_in_memory().expect("db");
    crate::db::agents::create(&conn, "ag-1", "A", None, None).expect("agent");
    crate::db::workspaces::create(&conn, "ws", "W").expect("ws");
    crate::db::agent_messages::queue(&conn, "m1", "ag-1", "ws", "first").expect("m1");
    crate::db::agent_messages::queue(&conn, "m2", "ag-1", "ws", "second").expect("m2");
    assert_eq!(consume_steers(&conn, "ag-1", "ws"), vec!["first", "second"]);
    // Consumed: the next pass gets nothing (delivery is exactly-once).
    assert!(consume_steers(&conn, "ag-1", "ws").is_empty());
}

#[test]
fn next_action_advances_through_steps_then_completes() {
    assert_eq!(next_action(0, 2, true), Next::Advance(1)); // step 0 of 2 → advance
    assert_eq!(next_action(1, 2, true), Next::Done); // last step → done
    assert_eq!(next_action(0, 1, true), Next::Done); // single (default) stage → done
    assert_eq!(next_action(0, 3, false), Next::Fail); // failure ends regardless of position
}

#[test]
fn strip_dsml_scrubs_leaked_control_tokens_but_keeps_prose() {
    // A bare leaked wrapper scrubs to nothing (so it won't display as the result).
    assert!(strip_dsml("<｜｜DSML｜｜tool_calls>").is_empty());
    // Prose around a leaked tag keeps the prose, drops the tag + stray pipes.
    let cleaned = strip_dsml("Reviewing <｜｜DSML｜｜invoke name=\"read\"> the code");
    assert!(
        !cleaned.contains("DSML") && !cleaned.contains('｜'),
        "{cleaned}"
    );
    assert!(cleaned.contains("Reviewing") && cleaned.contains("the code"));
    // Ordinary text is untouched.
    assert_eq!(
        strip_dsml("Verdict: pass — looks good."),
        "Verdict: pass — looks good."
    );
}

#[test]
fn all_calls_failed_flags_a_run_where_nothing_succeeded() {
    let inv = |ok: bool| agent::ToolInvocation {
        name: "write".to_owned(),
        arguments: "{}".to_owned(),
        result: "error".to_owned(),
        ok,
    };
    // Every call failed → unproductive (the research run: writes bounced, searches errored).
    assert!(all_calls_failed(&[inv(false), inv(false)]));
    // At least one success → productive.
    assert!(!all_calls_failed(&[inv(false), inv(true)]));
    // No calls at all (a pure-reasoning answer) is not "all failed".
    assert!(!all_calls_failed(&[]));
}

#[test]
fn validator_toolbox_can_run_build_checks_but_not_write() {
    // Regression: a code reviewer must be able to actually build/test, not just read — the
    // live failure was a validator calling `bash` for `cargo build`, getting "no tool named
    // bash", and passing anyway. It gets `read` + `list` + `bash` + `verdict`, but no
    // `write`/`edit`, so it verifies without rewriting the deliverable. Mirrors
    // `launch_validator`'s wiring.
    use crate::tools::{
        CoderToolbox, CompositeToolbox, ExecStrategy, FilteredToolbox, ValidatorToolbox,
    };
    let dir = std::env::temp_dir().join(format!("claw-val-tb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let coder = CoderToolbox::with_exec(dir.clone(), ExecStrategy::Host { root: dir });
    let checks = FilteredToolbox::new(
        Box::new(coder),
        vec![
            "read".to_owned(),
            "list".to_owned(),
            "find".to_owned(),
            "bash".to_owned(),
        ],
    );
    let tb = CompositeToolbox::new(vec![Box::new(ValidatorToolbox), Box::new(checks)]);
    let names: Vec<String> = tb.defs().into_iter().map(|d| d.name).collect();
    assert!(
        names.iter().any(|n| n == "bash"),
        "can run builds: {names:?}"
    );
    assert!(names.iter().any(|n| n == "list"), "can browse: {names:?}");
    assert!(names.iter().any(|n| n == "find"), "can search: {names:?}");
    assert!(names.iter().any(|n| n == "verdict"), "records a verdict");
    assert!(names.iter().any(|n| n == "read"), "reads files");
    assert!(
        !names.iter().any(|n| n == "write" || n == "edit"),
        "reviewer must not rewrite the deliverable: {names:?}"
    );
}

#[test]
fn revise_reruns_covers_human_override_and_the_agent_cap() {
    // Agent loop within budget → re-run; once exhausted → don't (let on-exhaust handle it).
    assert!(revise_reruns(false, Some("agent"), 0, 2));
    assert!(
        !revise_reruns(false, Some("agent"), 1, 2),
        "exhausted agent loop stops"
    );
    // A human-validator step is never capped.
    assert!(revise_reruns(false, Some("human"), 9, 2));
    // The escalation fix: a *person* resolving an exhausted agent loop always re-runs.
    assert!(
        revise_reruns(true, Some("agent"), 1, 2),
        "a human's revise must re-run even past the agent cap"
    );
}

#[test]
fn gate_action_routes_agent_human_and_auto_approve() {
    // Agent validator → always a review run, regardless of auto-approve.
    assert_eq!(
        gate_action(Some("agent"), false),
        GateAction::LaunchAgentReview
    );
    assert_eq!(
        gate_action(Some("agent"), true),
        GateAction::LaunchAgentReview
    );
    // Human gate → park by default, auto-pass when the workspace auto-approves (R3).
    assert_eq!(gate_action(Some("human"), false), GateAction::ParkForHuman);
    assert_eq!(gate_action(Some("human"), true), GateAction::AutoPass);
    // No validator (a plain stage shape) → never a human gate.
    assert_eq!(gate_action(None, true), GateAction::LaunchAgentReview);
}

#[test]
fn fail_next_retries_within_budget_then_resumes_or_fails() {
    // Within budget: retry with exponentially growing backoff (base 30 → 30, 60, 120…).
    assert_eq!(
        fail_next(0, 2, false, 30),
        FailNext::Retry { backoff_secs: 30 }
    );
    assert_eq!(
        fail_next(1, 2, false, 30),
        FailNext::Retry { backoff_secs: 60 }
    );
    // Budget spent: a one-shot fails terminally, a recurrent task resumes its schedule.
    assert_eq!(fail_next(2, 2, false, 30), FailNext::Fail);
    assert_eq!(fail_next(2, 2, true, 30), FailNext::ResumeCron);
    // max_retries = 0 means no auto-retry at all.
    assert_eq!(fail_next(0, 0, false, 30), FailNext::Fail);
}

#[test]
fn fail_next_caps_the_backoff() {
    // The doubling is clamped at the 1h ceiling rather than growing unbounded.
    assert_eq!(
        fail_next(20, 99, false, 30),
        FailNext::Retry {
            backoff_secs: RETRY_BACKOFF_CAP_SECS
        }
    );
}

#[test]
fn dispatch_queues_when_the_lane_is_busy() {
    // Regression (B1): a task resumed from a human gate must not launch a second worker while
    // another task holds the workspace lane.
    assert_eq!(dispatch_when(true), Dispatch::Queue);
    assert_eq!(dispatch_when(false), Dispatch::LaunchNow);
}

#[test]
fn effective_output_summarizes_silent_tool_turns() {
    let inv = |name: &str, args: &str| agent::ToolInvocation {
        name: name.to_owned(),
        arguments: args.to_owned(),
        result: "ok".to_owned(),
        ok: true,
    };
    // Empty reply + a write → name the file (the dominant real case).
    let wrote = agent::TurnResult {
        invocations: vec![inv("write", r#"{"path":"docs/x.md","content":"…"}"#)],
        reply: None,
        prompt_tokens: None,
    };
    assert_eq!(effective_output(&wrote), "Done — wrote docs/x.md.");
    // Empty reply + non-file tools → name the calls.
    let ran = agent::TurnResult {
        invocations: vec![inv("bash", "{}"), inv("read", "{}"), inv("bash", "{}")],
        reply: Some("   ".to_owned()),
        prompt_tokens: None,
    };
    assert_eq!(
        effective_output(&ran),
        "Done — ran 3 tool call(s): bash, read."
    );
    // A real reply always wins; nothing-at-all stays empty.
    let spoke = agent::TurnResult {
        reply: Some("all set".to_owned()),
        ..Default::default()
    };
    assert_eq!(effective_output(&spoke), "all set");
    assert!(effective_output(&agent::TurnResult::default()).is_empty());
}
