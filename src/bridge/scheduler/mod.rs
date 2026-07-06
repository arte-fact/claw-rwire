//! The task scheduler. A periodic tick scans every workspace and, for each **idle** lane,
//! launches its next ready task on a worker thread — so a workspace runs one task at a
//! time (sequential), while workspaces run in parallel. The worker runs the assigned
//! agent's turn in the workspace dir and posts `Command::TaskFinished` back, which the
//! (DB-owning) bridge thread persists and projects.

use std::path::Path;
use std::time::Duration;

use rwire::{ChangeSet, SharedServerState};

use crate::agent;
use crate::db::pipeline::{self, Step, StepKind};
use crate::db::resolution::{self, ResolvedInference};
use crate::db::tasks::{self, Task, TaskStatus};
use crate::db::{self, task_runs, workspaces};
use crate::state::{ActivityKind, App, ProgressCtx, RunRole};
use crate::tools::{
    ChecksToolbox, CompositeToolbox, PreviewToolbox, RepoToolbox, Toolbox, ValidatorToolbox,
    VisitToolbox,
};
use crate::workspace;

use super::{Command, build_toolbox, data_dir, log_line, memory, send};

mod verdicts;
pub use verdicts::human_verdict;

#[cfg(test)]
mod tests;

/// Default seconds between scheduler scans.
const DEFAULT_TICK_SECS: u64 = 3;

/// Start the background ticker: post a `SchedulerTick` every N seconds for the bridge to
/// service. N defaults to [`DEFAULT_TICK_SECS`], overridable with `CLAW_SCHEDULER_SECS`
/// (`0` disables the scheduler — tasks then stay queued, the C-1 manual behaviour).
pub fn spawn_ticker() {
    let secs = std::env::var("CLAW_SCHEDULER_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TICK_SECS);
    if secs == 0 {
        log_line("scheduler: disabled (CLAW_SCHEDULER_SECS=0)");
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(secs));
            send(Command::SchedulerTick);
        }
    });
}

/// One scan: for every **idle lane** of every workspace, launch that lane's next ready task.
/// A workspace's lanes (G4) are the free lane (request-less tasks, on the canonical tree)
/// plus one per active request (its tasks, in its own worktree) — so requests execute in
/// parallel while each lane stays sequential.
pub fn tick(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let Ok(workspaces) = workspaces::list(conn) else {
        return;
    };
    for ws in workspaces {
        for lane in lanes(conn, &ws.id) {
            if tasks::running_in_lane(conn, &ws.id, lane.as_deref())
                .ok()
                .flatten()
                .is_some()
            {
                continue;
            }
            if let Ok(Some(task)) = tasks::next_ready(conn, &ws.id, lane.as_deref()) {
                start(conn, shared, task);
            }
        }
    }
    // Keep the dashboard's all-lanes view + the review badge live regardless of route.
    super::tasks::project_dashboard(conn, shared);
    super::review::project_awaiting(conn, shared);
}

/// The workspace's dispatchable lanes: the free lane first, then the oldest
/// [`db::requests::MAX_ACTIVE`] executing/validating requests (FIFO). Requests beyond the
/// cap simply have no lane — their tasks are never asked for until a slot frees.
fn lanes(conn: &rusqlite::Connection, workspace_id: &str) -> Vec<Option<String>> {
    let mut lanes = vec![None];
    lanes.extend(
        db::requests::active_requests(conn, workspace_id, db::requests::MAX_ACTIVE)
            .unwrap_or_default()
            .into_iter()
            .map(|request| Some(request.id)),
    );
    lanes
}

/// Claim a task for its lane (flip to `running`), reflect it, then dispatch the worker. A
/// task that can't be dispatched at all (no assignable agent, unresolved endpoint) is
/// finished `failed` immediately rather than left stuck `running`. The lane root resolves
/// **before** the claim (G4): a request lane whose tree can't compose — and that may not
/// fall back to the canonical tree — simply isn't claimed this tick.
fn start(conn: &rusqlite::Connection, shared: &SharedServerState, task: Task) {
    let Some(root) = super::requests::ensure_lane_root(conn, &data_dir(), &task) else {
        return; // logged by the resolver; the scheduler retries next tick
    };
    if let Err(err) = tasks::set_status(conn, &task.id, TaskStatus::Running) {
        return log_line(format!("scheduler: claim {} failed: {err}", task.id));
    }
    super::tasks::project_if_active(conn, shared, &task.workspace_id);
    super::tasks::refresh_detail_if_open(conn, shared, &task.id);
    activity(shared, &task, ActivityKind::Started, String::new());
    // First run in a fresh lane: provision its sandbox in the background (overlapping the
    // LLM turn) and say so — a silent 3-minute first `bash` reads as a hang (P0 drift).
    if let crate::tools::ExecStrategy::Isola { sandbox, mount } = super::exec_strategy_for(&root)
        && crate::sandbox::warm(&sandbox, &mount, log_line)
    {
        log_line(format!(
            "sandbox: provisioning {sandbox} for task {}",
            task.id
        ));
        notify_sandbox_provisioning(conn, &task);
    }
    // Record each repo's pre-task HEAD at the task's first launch (G0) — the bases a
    // restart resets to. Failures are logged, never blocking the launch: a git-less
    // environment degrades to the pre-G0 behaviour.
    for err in crate::gitops::snapshot_task_bases(conn, &root, &task.id) {
        log_line(format!("gitops: snapshot {}: {err}", task.id));
    }
    launch_current(conn, &task);
}

/// The user-visible line for a lane's first run: the origin chat for a request-bound task
/// (where delivery receipts already land), the workspace chat via the task's own thread
/// otherwise — nobody should read a slow first `bash` as a hang.
fn notify_sandbox_provisioning(conn: &rusqlite::Connection, task: &Task) {
    let body = "🧰 First run in this lane — provisioning an isolated sandbox (~3 min) \
                before commands execute. Later runs reuse it instantly.";
    if let Some(request_id) = &task.request_id
        && let Ok(Some(request)) = crate::db::requests::get(conn, request_id)
    {
        super::requests::ping(conn, &request, body);
    }
}

/// The `pipeline.yaml` maintenance contract (P4b), appended to every buildable project's layer:
/// the producer keeps the recipe honest so the checks it's judged by track how the project really
/// builds, tests, and serves.
const PIPELINE_NOTE: &str = "## The pipeline\n\nThis project's CI checks and preview command live in \
     a `pipeline.yaml` at its root — the recipe your pass is judged by. Read it, and when you change \
     how the project builds, tests, or serves (a new check, a renamed script, a different preview \
     command), update `pipeline.yaml` in the same pass so the checks and preview stay honest. claw \
     always runs a `fmt`·`test`·`build` floor regardless of the file, so trimming a check can't \
     weaken the gate — keep the recipe complete rather than minimal.";

/// The project layer for a task's run (P1): the kind conventions + machine contracts for the
/// repo it works in. The target project is the first non-meta repo the request touched, else
/// the first `projects/<name>` named in the spec; `None` (no layer) when neither resolves or
/// the kind has no guidance. Buildable kinds also carry the [`PIPELINE_NOTE`] (P4b).
fn project_layer(conn: &rusqlite::Connection, task: &Task, workspace_dir: &Path) -> Option<String> {
    let project = task
        .request_id
        .as_deref()
        .and_then(|id| db::requests::repos_for(conn, id).ok())
        .and_then(|repos| {
            repos
                .into_iter()
                .find(|r| !r.repo.is_empty())
                .map(|r| r.repo.trim_start_matches("projects/").to_owned())
        })
        .or_else(|| crate::kinds::project_in_spec(&task.spec))?;
    let dir = workspace_dir.join("projects").join(&project);
    let kind = crate::kinds::kind_for(conn, &task.workspace_id, &project, &dir);
    let layer = kind.prompt_layer()?;
    Some(format!("{layer}\n\n{PIPELINE_NOTE}"))
}

/// Launch the task's **current step's** producer/stage run on a worker thread (self-posting
/// `TaskFinished`). The run role is `Producer` for a loop step (its validator runs next) or
/// `Stage` for a plain step. The agent runs with the step's role preamble + chained context.
fn launch_current(conn: &rusqlite::Connection, task: &Task) {
    let epoch = tasks::bump_epoch(conn, &task.id).unwrap_or_default();
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let Some(step) = steps.get(task.step_position.max(0) as usize).cloned() else {
        return send(Command::TaskFinished {
            id: task.id.clone(),
            role: RunRole::Stage,
            epoch,
            result: Err("the pipeline has no step at this position".to_owned()),
            memory: None,
        });
    };
    // Resume at the review: if this attempt already produced work at this step and no reviewer
    // ever returned a verdict on it (the validator run failed in transit, or the server was
    // restarted between produce and review), re-launch the **validator** with the recorded
    // output instead of paying for another producer pass. Agent-validated loops only — a
    // human gate parks for the inbox and must never spawn an agent review.
    if step.kind == StepKind::Loop && step.validator_kind.as_deref() != Some("human") {
        let prior =
            task_runs::list_for_task_attempt(conn, &task.id, task.attempt).unwrap_or_default();
        if let Some(output) = unreviewed_output(&prior, task.step_position) {
            let output = output.to_owned();
            return launch_validator(conn, task, &step, &output);
        }
    }
    let role = if step.kind == StepKind::Loop {
        RunRole::Producer
    } else {
        RunRole::Stage
    };
    let prepared = (|| -> Result<_, String> {
        let agent_id = step_agent(conn, task, &step)
            .ok_or_else(|| "no agent for this step and the workspace has no owner".to_owned())?;
        let inference =
            resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, env_lookup)
                .map_err(|err| err.to_string())?;
        // The lane's tree (G4): a request-bound task lives, reads, and writes in its
        // request's worktree; free tasks stay on the canonical workspace.
        let workspace_dir = super::requests::task_root(conn, &data_dir(), task);
        let preamble = step
            .prompt
            .clone()
            .unwrap_or_else(|| super::role_prompt(conn, crate::state::RunKind::Task));
        // Scope context to the current attempt: a restart (a fresh attempt) must not inherit an
        // earlier attempt's runs. Reconstruct the producer's own thread across this attempt's
        // passes so a re-run continues its work instead of rediscovering it from disk.
        let prior =
            task_runs::list_for_task_attempt(conn, &task.id, task.attempt).unwrap_or_default();
        // F→U→W→M composition against the model's real budget: place the fixed identity and
        // the current unit, reserve the producer's working headroom, and let memory fill only
        // what's left — the last-task recap first, then the workspace summary. A long loop
        // outgrowing half the budget compacts its oldest passes first (the mid-unit fold).
        let context = super::cached_context(&inference).unwrap_or(super::DEFAULT_CONTEXT_TOKENS);
        let unit_cap_chars = (f64::from(memory::prompt_budget(context)) * 0.5) as usize * 4;
        let (condensed, skip) = memory::compact_attempt(&prior, unit_cap_chars);
        let prior = &prior[skip..];
        let unit_chars = task.spec.chars().count()
            + memory::attempt_chars(prior)
            + condensed.as_deref().unwrap_or("").chars().count();
        let row = db::agent_memory::get(conn, &inference.agent_id, Some(&task.workspace_id))
            .unwrap_or_default();
        let recap =
            memory::last_task_recap(conn, &inference.agent_id, &task.workspace_id, &task.id);
        let memory_fill = memory::fill_for_launch(
            &inference,
            crate::state::RunKind::Task,
            &preamble,
            Some(&workspace_dir),
            unit_chars,
            recap.as_deref(),
            row.text().as_deref(),
        );
        // The provenance snapshot (R3.5 M4): what the run remembered at launch, capped so a
        // big-context model's memory doesn't bloat every run row.
        let snapshot = memory_fill.as_deref().map(memory::snapshot);
        let memory_text = memory_fill.map(|text| memory::wrap_workspace_memory(&text));
        let system_prompt = super::run_system_prompt_with_project(
            &preamble,
            Some(&workspace_dir),
            &inference.agent_name,
            inference.agent_prompt.clone(),
            project_layer(conn, task, &workspace_dir),
            memory_text,
        );
        let mut messages = super::replay::producer_thread(
            &system_prompt,
            &task.spec,
            &step.name,
            condensed.as_deref(),
            prior,
        );
        // Preview capability (P2c): a previewable branch tells the producer where its live server
        // lives and that `preview`/`visit` exist — the debug loop, surfaced.
        if let Some(hint) = super::preview::preview_hint(conn, &task.workspace_id, &workspace_dir) {
            messages.push(agent::ChatMessage::text(agent::Role::User, hint));
        }
        // Steer delivery (R4): queued operator messages ride this pass boundary as trailing
        // user turns — the agent reads them alongside the spec/feedback it was launched with.
        for body in consume_steers(conn, &inference.agent_id, &task.workspace_id) {
            messages.push(agent::ChatMessage::text(
                agent::Role::User,
                format!("Message from the operator (steer): {body}"),
            ));
        }
        let base = build_toolbox(inference.tools.clone(), Some(workspace_dir.clone()), true);
        // The `checks` tool (P1b): the harness's own per-repo CI, so a producer can pre-empt the
        // review by running the exact checks that will judge its pass.
        let checks = ChecksToolbox::new(
            task.workspace_id.clone(),
            workspace_dir.clone(),
            super::exec_strategy_for(&workspace_dir),
            data_dir(),
        );
        // The `preview` tool (P2): start/restart this branch's live server and get the URL back —
        // the debug loop as a tool call. Paired with `visit` — fetch the running branch to check it.
        let preview = PreviewToolbox::new(
            task.workspace_id.clone(),
            workspace_dir.clone(),
            super::exec_strategy_for(&workspace_dir),
            data_dir(),
        );
        let visit = VisitToolbox::new(task.workspace_id.clone(), workspace_dir.clone());
        // Read-only git surface, unconditionally (it can't mutate): repo_status/log/diff,
        // anchored to this task so since:"task-start" resolves via its recorded bases.
        let toolbox: Box<dyn Toolbox> = Box::new(CompositeToolbox::new(vec![
            base,
            Box::new(checks),
            Box::new(preview),
            Box::new(visit),
            Box::new(RepoToolbox::for_task(
                workspace_dir,
                data_dir(),
                task.id.clone(),
            )),
        ]));
        Ok((inference, messages, toolbox, snapshot))
    })();
    spawn_run(run_meta(conn, task), role, epoch, prepared);
}

/// Launch a loop step's **validator** run: a read-only review of the producer's `output` that
/// must call the `verdict` tool. Self-posting `TaskFinished { role: Validator }`.
fn launch_validator(conn: &rusqlite::Connection, task: &Task, step: &Step, output: &str) {
    let prepared = (|| -> Result<_, String> {
        let agent_id = step
            .validator_agent
            .clone()
            .or_else(|| owner_of(conn, &task.workspace_id))
            .ok_or_else(|| "no validator agent and the workspace has no owner".to_owned())?;
        let inference =
            resolution::resolve_for_agent(conn, &agent_id, &task.workspace_id, env_lookup)
                .map_err(|err| err.to_string())?;
        // Reviews happen where the work happened: the task's lane tree (G4).
        let workspace_dir = super::requests::task_root(conn, &data_dir(), task);
        let preamble = step
            .validator_prompt
            .clone()
            .unwrap_or_else(|| super::role_prompt(conn, crate::state::RunKind::Validator));
        // No memory layer: a validator is deliberately amnesiac beyond the task — accumulated
        // review memory breeds anchoring and rubber-stamping. Its loop continuity comes from
        // the replayed thread below. The one curated exception (R5): the workspace's
        // STANDARDS.md memo — rules, not a work log.
        let system_prompt = super::run_system_prompt_with_project(
            &preamble,
            Some(&workspace_dir),
            &inference.agent_name,
            inference.agent_prompt.clone(),
            project_layer(conn, task, &workspace_dir),
            workspace::standards_layer(&workspace_dir),
        );
        let files = workspace::list_files(&workspace_dir, 200);
        // Reconstruct the validator's own thread across this attempt's passes so a re-review
        // builds on what it already checked rather than re-inspecting the whole tree.
        let prior =
            task_runs::list_for_task_attempt(conn, &task.id, task.attempt).unwrap_or_default();
        // What the task actually changed (G2): the reviewer judges the diff, not the prose.
        let mut diff = review_diff(conn, &task.id, &workspace_dir);
        // Machine checks before model review (P1b): the per-repo CI already ran when the
        // producer pass finished — surface its *recorded* results (not a re-run) so the
        // reviewer judges with the same red/green the fail-fast loop saw. Lint/build/test
        // verdicts come from machines; judgement from the reviewer.
        if let Some(summary) = recorded_checks_summary(conn, &task.id) {
            diff = Some(format!("{}\n\n{summary}", diff.unwrap_or_default()));
        }
        // Preview capability (P2c): tell the reviewer where the running branch is and that `visit`
        // can fetch it — a claim like "the page renders" is checkable, not just readable.
        if let Some(hint) = super::preview::preview_hint(conn, &task.workspace_id, &workspace_dir) {
            diff = Some(format!("{}\n\n{hint}", diff.unwrap_or_default()));
        }
        let mut messages = super::replay::validator_thread(
            &system_prompt,
            &task.spec,
            output,
            &files,
            &prior,
            diff.as_deref(),
        );
        // The conclude-now nudge (L1): a retried review is told its previous turn ended
        // without a verdict — inspect less, conclude more.
        if verdictless_reviews(&prior, task.step_position, task.iteration) > 0 {
            messages.push(agent::ChatMessage::text(
                agent::Role::User,
                "Your previous review turn ended WITHOUT calling the `verdict` tool, so it \
                 decided nothing. Conclude NOW: weigh what you have already inspected and \
                 call `verdict` (pass / revise / reject) in this turn."
                    .to_owned(),
            ));
        }
        // `read` + `list` + `find` (browse/search the tree) + `bash` (run the project's
        // build/tests), plus the `verdict` tool. A reviewer asked to confirm code "builds and tests
        // clean" can't do so with `read` alone — the live failure was a validator calling `bash`
        // (cargo build), getting "no tool named bash", and passing anyway. `list`/`find` let it
        // discover and grep files without shelling out. No `write`/`edit`, so it verifies without
        // rewriting the deliverable.
        let coder_tools = build_toolbox(
            Some(vec![
                "read".to_owned(),
                "list".to_owned(),
                "find".to_owned(),
                "bash".to_owned(),
            ]),
            Some(workspace_dir.clone()),
            false,
        );
        // The `checks` tool (P1b): the reviewer confirms "builds and tests clean" with the
        // harness's exact per-repo recipe, not an ad-hoc `bash` guess — one source of truth.
        let checks_tool = ChecksToolbox::new(
            task.workspace_id.clone(),
            workspace_dir.clone(),
            super::exec_strategy_for(&workspace_dir),
            data_dir(),
        );
        // The reviewer can `visit` the running branch too (read-only GET) — exercise the app the
        // pass claims to have fixed, not just read its diff.
        let visit = VisitToolbox::new(task.workspace_id.clone(), workspace_dir.clone());
        // Reviewers get the read-only git surface too — a claim like "all tests pass" is
        // checked against what actually changed (repo_diff since task-start), not prose.
        let toolbox: Box<dyn Toolbox> = Box::new(CompositeToolbox::new(vec![
            Box::new(ValidatorToolbox),
            coder_tools,
            Box::new(checks_tool),
            Box::new(visit),
            Box::new(RepoToolbox::for_task(
                workspace_dir,
                data_dir(),
                task.id.clone(),
            )),
        ]));
        Ok((inference, messages, toolbox, None))
    })();
    let epoch = tasks::bump_epoch(conn, &task.id).unwrap_or_default();
    spawn_run(run_meta(conn, task), RunRole::Validator, epoch, prepared);
}

type Prepared = Result<
    (
        ResolvedInference,
        Vec<agent::ChatMessage>,
        Box<dyn Toolbox>,
        Option<String>,
    ),
    String,
>;

/// The run's linked context, minus the agent name (filled from the resolved inference).
struct RunMeta {
    task_id: String,
    task: String,
    workspace_id: String,
    workspace: String,
}

/// Spawn the worker for a prepared run, or post the preparation failure straight back. The
/// worker streams live tool events back via `AgentProgress` as the turn runs. `epoch` is the
/// dispatch guard captured at launch — the bridge drops the result if the task has moved on.
fn spawn_run(meta: RunMeta, role: RunRole, epoch: i64, prepared: Prepared) {
    match prepared {
        Ok((inference, messages, toolbox, memory)) => {
            let ctx = ProgressCtx {
                task_id: meta.task_id,
                task: meta.task,
                agent: inference.agent_name.clone(),
                workspace_id: meta.workspace_id,
                workspace: meta.workspace,
            };
            std::thread::spawn(move || {
                // Scope the sink so its borrow of `ctx` ends before we move `ctx.task_id`.
                let result = {
                    let mut sink = |event| {
                        send(Command::AgentProgress {
                            ctx: ctx.clone(),
                            event,
                        });
                    };
                    // Mid-run steering (R5): the loop polls between tool rounds through the
                    // worker's own connection. The epoch guard inside `steer_poll` keeps a
                    // superseded run (stopped / restarted) from consuming messages meant for
                    // its successor.
                    let steer_conn = crate::db::open(&data_dir().join("central.db")).ok();
                    let task_for_steer = ctx.task_id.clone();
                    let ws_for_steer = ctx.workspace_id.clone();
                    let agent_for_steer = inference.agent_id.clone();
                    let poll = move || {
                        steer_conn.as_ref().map_or_else(Vec::new, |conn| {
                            steer_poll(
                                conn,
                                &task_for_steer,
                                epoch,
                                &agent_for_steer,
                                &ws_for_steer,
                            )
                        })
                    };
                    agent::run_turn_steered(
                        &inference,
                        messages,
                        toolbox.as_ref(),
                        &mut sink,
                        &poll,
                    )
                    .map_err(|err| err.to_string())
                };
                send(Command::TaskFinished {
                    id: ctx.task_id,
                    role,
                    epoch,
                    result,
                    memory,
                });
                // Warm the model's context-window cache off the bridge thread (as the chat
                // worker does): the cache is per-process, so after a restart the F→U→W→M
                // budget fell back to the 8k default until a *chat* turn happened — task
                // launches composed with no memory (the blank iter-2 snapshot observation).
                super::warm_context_cache(&inference);
            });
        }
        Err(reason) => send(Command::TaskFinished {
            id: meta.task_id,
            role,
            epoch,
            result: Err(reason),
            memory: None,
        }),
    }
}

/// The run's linked context for a task (workspace name resolved once).
fn run_meta(conn: &rusqlite::Connection, task: &Task) -> RunMeta {
    RunMeta {
        task_id: task.id.clone(),
        task: task.title.clone(),
        workspace_id: task.workspace_id.clone(),
        workspace: workspaces::get(conn, &task.workspace_id)
            .ok()
            .flatten()
            .map(|ws| ws.name)
            .unwrap_or_else(|| task.workspace_id.clone()),
    }
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Cap on the diff injected into a validator prompt — stat + patch; `repo_diff` has the rest.
const REVIEW_DIFF_CAP_CHARS: usize = 4_000;
/// The recorded CI results injected into a validator prompt (P1b) — cap so a long failure log
/// doesn't crowd out the diff.
const CHECKS_CAP_CHARS: usize = 2_000;
/// Cap on fail-fast producer re-runs per iteration (P1b) — its own budget, separate from the
/// reviewer's `max_iterations` so machine retries and reviewer retries stay independent.
const MAX_CHECK_RETRIES: usize = 2;

/// Run the pass's per-repo CI (P1b): the repos it committed, each judged by its kind's recipe
/// in the task's lane. `pass_commits` is the `[{repo,sha}]` the sweep produced.
fn run_pass_checks(
    conn: &rusqlite::Connection,
    task: &Task,
    pass_commits: &[serde_json::Value],
) -> Vec<crate::ci::CheckOutcome> {
    let repos: Vec<String> = pass_commits
        .iter()
        .filter_map(|entry| {
            entry
                .get("repo")
                .and_then(|r| r.as_str())
                .map(str::to_owned)
        })
        .collect();
    if repos.is_empty() {
        return Vec::new();
    }
    let root = super::requests::task_root(conn, &data_dir(), task);
    let strategy = super::exec_strategy_for(&root);
    crate::ci::run_for_repos(conn, &task.workspace_id, &root, &strategy, &repos)
}

/// The latest producer pass's recorded CI, formatted for the reviewer's prompt (P1b) — a
/// one-line-per-check verdict plus red tails, capped. `None` when no pass recorded any checks.
fn recorded_checks_summary(conn: &rusqlite::Connection, task_id: &str) -> Option<String> {
    let runs = task_runs::list_for_task(conn, task_id).unwrap_or_default();
    let latest = runs
        .iter()
        .rev()
        .find(|run| run.role == "producer" && run.checks.is_some())?;
    let outcomes = crate::ci::outcomes_from_json(latest.checks.as_deref());
    if outcomes.is_empty() {
        return None;
    }
    let mut summary = String::from("Machine checks (ran in the lane after this pass):");
    for outcome in &outcomes {
        let mark = if outcome.ok { "✓" } else { "✗" };
        let where_ = if outcome.repo.is_empty() {
            outcome.check.clone()
        } else {
            format!("{} ({})", outcome.check, outcome.repo)
        };
        summary.push_str(&format!("\n{mark} {where_}"));
        if !outcome.ok && !outcome.tail.is_empty() {
            summary.push_str(&format!("\n{}", outcome.tail));
        }
    }
    Some(summary.chars().take(CHECKS_CAP_CHARS).collect())
}

/// This iteration's red-check producer passes so far (P1b) — the fail-fast counter, mirroring
/// [`verdictless_reviews`]. Counts the just-recorded run, so the first red pass returns 1.
fn check_retries(conn: &rusqlite::Connection, task: &Task) -> usize {
    task_runs::list_for_task_attempt(conn, &task.id, task.attempt)
        .unwrap_or_default()
        .iter()
        .filter(|run| {
            run.role == "producer"
                && run.position == task.step_position
                && run.iteration == task.iteration
                && !crate::ci::json_all_green(run.checks.as_deref())
        })
        .count()
}

/// What the task changed so far — its recorded bases against the (clean, post-commit) tree,
/// stat + patch per repo, capped. `None` when nothing changed or the workspace has no repos.
fn review_diff(
    conn: &rusqlite::Connection,
    task_id: &str,
    root: &std::path::Path,
) -> Option<String> {
    let bases = db::task_repos::bases_for(conn, task_id).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for base in &bases {
        let Ok(dir) = crate::gitops::repo_dir(root, &base.repo) else {
            continue;
        };
        let stat =
            crate::gitops::diff(&dir, Some(&base.base_commit), None, true).unwrap_or_default();
        if stat.is_empty() {
            continue;
        }
        let patch =
            crate::gitops::diff(&dir, Some(&base.base_commit), None, false).unwrap_or_default();
        let label = if base.repo.is_empty() {
            "workspace"
        } else {
            base.repo.as_str()
        };
        parts.push(format!("{label}:\n{stat}\n{patch}"));
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n\n");
    Some(if joined.chars().count() > REVIEW_DIFF_CAP_CHARS {
        let head: String = joined.chars().take(REVIEW_DIFF_CAP_CHARS).collect();
        format!("{head}\n…[diff truncated — repo_diff shows the rest]")
    } else {
        joined
    })
}

/// The mid-run steer poll (R5): drain the running agent's queue **only while this run is
/// still the task's current one** — a bumped epoch (stop / cancel / restart / re-dispatch)
/// means the messages belong to the successor, so a stale run gets nothing. Consumption
/// nudges the bridge to re-project the open thread (the "queued" tag flips live).
fn steer_poll(
    conn: &rusqlite::Connection,
    task_id: &str,
    epoch: i64,
    agent_id: &str,
    workspace_id: &str,
) -> Vec<String> {
    let current = tasks::get(conn, task_id)
        .ok()
        .flatten()
        .map(|task| task.run_epoch);
    if current != Some(epoch) {
        return Vec::new();
    }
    let bodies = consume_steers(conn, agent_id, workspace_id);
    if !bodies.is_empty() {
        send(Command::SteerConsumed);
    }
    bodies
}

/// Drain the scope's queued steer messages for a launching pass: returns the bodies (oldest
/// first) and marks them delivered — the queue→pickup boundary, unit-tested.
fn consume_steers(conn: &rusqlite::Connection, agent_id: &str, workspace_id: &str) -> Vec<String> {
    let queued = db::agent_messages::undelivered(conn, agent_id, workspace_id).unwrap_or_default();
    queued
        .into_iter()
        .map(|message| {
            let _ = db::agent_messages::mark_delivered(conn, &message.id);
            message.body
        })
        .collect()
}

/// How many review retries a verdict-less validator turn gets before the None verdict
/// falls through to a charged revise (L1: 1 launch + 2 retries per iteration).
const MAX_VERDICTLESS_RETRIES: usize = 2;

/// Count this iteration's inconclusive reviews — successful validator runs at `position`
/// that recorded no verdict — a pure decision, unit-tested.
fn verdictless_reviews(
    prior: &[crate::db::task_runs::TaskRun],
    position: i64,
    iteration: i64,
) -> usize {
    prior
        .iter()
        .filter(|run| {
            run.role == "validator"
                && run.ok
                && run.position == position
                && run.iteration == iteration
                && run.verdict.is_none()
        })
        .count()
}

/// The produced-but-never-reviewed output of an attempt at `position`, if any — a pure
/// decision, unit-tested. `Some` when the last successful producer/stage run at this step has
/// no *verdict-bearing* validator run after it (a validator that failed in transit records
/// `ok = 0` and no verdict — that's not a review, so the work still awaits one). A pass whose
/// machine checks came back **red** is never resumed to review (P1b): it's re-produced, so a
/// restart mid-fail-fast re-enters the producer rather than skipping to a doomed review.
fn unreviewed_output(prior: &[crate::db::task_runs::TaskRun], position: i64) -> Option<&str> {
    let produced_at = prior.iter().rposition(|run| {
        run.ok && run.position == position && (run.role == "producer" || run.role == "stage")
    })?;
    if !crate::ci::json_all_green(prior[produced_at].checks.as_deref()) {
        return None;
    }
    let reviewed = prior[produced_at + 1..]
        .iter()
        .any(|run| run.role == "validator" && run.verdict.is_some());
    if reviewed {
        return None;
    }
    prior[produced_at]
        .output
        .as_deref()
        .filter(|out| !out.trim().is_empty())
}

/// The agent that runs a step: the task's **assignee** first (assigning a card to a member must
/// mean that member does the work — the live e2e had a "Review X" task assigned to Critic but
/// *produced* by Forge because the step's pinned agent won), else the step's default agent,
/// else the workspace owner.
fn step_agent(conn: &rusqlite::Connection, task: &Task, step: &Step) -> Option<String> {
    task.agent
        .clone()
        .or_else(|| step.agent.clone())
        .or_else(|| owner_of(conn, &task.workspace_id))
}

fn owner_of(conn: &rusqlite::Connection, workspace_id: &str) -> Option<String> {
    workspaces::get(conn, workspace_id)
        .ok()
        .flatten()
        .and_then(|ws| ws.owner)
}

/// The most recent successful producer/stage output of the task's **current attempt** — the work
/// an `approve` accepts, and what the board's Peek modal shows.
pub(super) fn last_producer_output(conn: &rusqlite::Connection, task_id: &str) -> String {
    let attempt = tasks::get(conn, task_id)
        .ok()
        .flatten()
        .map(|task| task.attempt)
        .unwrap_or(0);
    task_runs::list_for_task_attempt(conn, task_id, attempt)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|run| run.ok && (run.role == "stage" || run.role == "producer"))
        .and_then(|run| run.output.clone())
        .unwrap_or_default()
}

/// The text to store for a finished turn: the model's reply, or — when it ended on tool calls
/// with no closing prose — a short summary of what the tools did (files written, else the calls
/// it ran), so a task that did real work never shows a blank result.
fn effective_output(turn: &agent::TurnResult) -> String {
    if let Some(reply) = &turn.reply {
        let cleaned = strip_dsml(reply);
        if !cleaned.trim().is_empty() {
            return cleaned;
        }
    }
    summarize_invocations(&turn.invocations)
}

/// Whether a turn made at least one tool call and *every* one failed — it tried to act but
/// nothing it did succeeded, so the task hasn't actually been done. An empty call list (a
/// pure-reasoning turn that just answers) is not "all failed" and stays a success.
fn all_calls_failed(invocations: &[agent::ToolInvocation]) -> bool {
    !invocations.is_empty() && invocations.iter().all(|inv| !inv.ok)
}

/// Strip a model's leaked native tool-call framing (deepseek's fullwidth-pipe "DSML" tags) from a
/// reply before it's stored/displayed — `client::recover_leaked_tool_calls` already pulls runnable
/// calls out of well-formed markup, but a bare/partial wrapper can still leak as raw text.
fn strip_dsml(text: &str) -> String {
    if !text.contains("DSML") && !text.contains('｜') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let (before, tail) = rest.split_at(open);
        out.push_str(before);
        let Some(close) = tail.find('>') else {
            out.push_str(tail);
            return out.replace('｜', "").trim().to_owned();
        };
        let tag = &tail[..=close];
        if !(tag.contains("DSML") || tag.contains('｜')) {
            out.push_str(tag); // a legitimate `<…>` (rare in prose) — keep it
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out.replace('｜', "").trim().to_owned()
}

fn summarize_invocations(invocations: &[agent::ToolInvocation]) -> String {
    if invocations.is_empty() {
        return String::new();
    }
    // Prefer the concrete artifacts: the files written/edited.
    let mut files: Vec<String> = Vec::new();
    for inv in invocations
        .iter()
        .filter(|i| i.name == "write" || i.name == "edit")
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&inv.arguments)
            && let Some(path) = value.get("path").and_then(serde_json::Value::as_str)
            && !files.iter().any(|f| f == path)
        {
            files.push(path.to_owned());
        }
    }
    if !files.is_empty() {
        return format!("Done — wrote {}.", files.join(", "));
    }
    // Otherwise name the tools it ran (deduped, in order).
    let mut names: Vec<&str> = Vec::new();
    for inv in invocations {
        if !names.contains(&inv.name.as_str()) {
            names.push(&inv.name);
        }
    }
    format!(
        "Done — ran {} tool call(s): {}.",
        invocations.len(),
        names.join(", ")
    )
}

/// Serialize a run's tool calls to JSON for the persisted transcript — so a completed task's
/// detail can show the full story (what files, what commands), not just the summary text.
fn transcript_json(invocations: &[agent::ToolInvocation]) -> Option<String> {
    if invocations.is_empty() {
        return None;
    }
    let array: Vec<serde_json::Value> = invocations
        .iter()
        .map(|inv| {
            serde_json::json!({
                "name": inv.name,
                "arguments": inv.arguments,
                "result": inv.result,
                "ok": inv.ok,
            })
        })
        .collect();
    serde_json::to_string(&array).ok()
}

fn role_str(role: RunRole) -> &'static str {
    match role {
        RunRole::Stage => "stage",
        RunRole::Producer => "producer",
        RunRole::Validator => "validator",
    }
}

/// What to do with a task after a stage run finishes — a pure decision, unit-tested.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    Advance(i64),
    Done,
    Fail,
}

/// Whether a finished run triggers the harness commit sweep — a pure decision, unit-tested.
/// Successful producer/stage passes only: validators are read-only by contract, and a failed
/// pass must leave the tree dirty for the revise loop.
fn commits_pass(role: RunRole, ok: bool) -> bool {
    ok && !matches!(role, RunRole::Validator)
}

/// The pass commit's subject line: `<step>: <task title>` (the `Task:`/`Run:` trailers carry
/// the identities; the subject is for the human reading `git log`).
fn pass_subject(step: Option<&Step>, task: &Task) -> String {
    let step_name = step.map_or("task", |step| step.name.as_str());
    format!(
        "{step_name}: {}",
        crate::text::first_line_capped(&task.title, 60)
    )
}

fn next_action(step_position: i64, step_count: usize, ok: bool) -> Next {
    if !ok {
        Next::Fail
    } else if step_position + 1 < step_count as i64 {
        Next::Advance(step_position + 1)
    } else {
        Next::Done
    }
}

/// The next-run timestamp for a recurrent task that just succeeded, or `None` to finish it
/// (non-recurrent, failed, or a missing/unparseable cron).
fn reschedule_at(task: &Task, status: TaskStatus) -> Option<String> {
    if !matches!(task.kind, crate::db::tasks::TaskKind::Recurrent) || status != TaskStatus::Done {
        return None;
    }
    next_cron_slot(task)
}

/// The next fire of a recurrent task's cron schedule after now, or `None` (no/unparseable cron).
fn next_cron_slot(task: &Task) -> Option<String> {
    let cron = crate::schedule::Cron::parse(task.schedule.as_deref()?)?;
    let next = cron.next_after(now_unix())?;
    Some(crate::schedule::to_rfc3339_millis(next))
}

/// Wall-clock now as unix seconds (UTC).
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `settings` key for the global auto-retry budget (per-task `max_retries` overrides it).
const RETRY_MAX_KEY: &str = "task.max_retries";
/// `settings` key for the base backoff (seconds); the delay doubles each retry, capped.
const RETRY_BASE_SECS_KEY: &str = "task.retry_base_secs";
const DEFAULT_MAX_RETRIES: i64 = 2;
const DEFAULT_RETRY_BASE_SECS: i64 = 30;
/// Ceiling on a single backoff (1h) so the delay can't grow unbounded.
const RETRY_BACKOFF_CAP_SECS: i64 = 3600;
/// Bound the doubling shift so `1 << shift` can't overflow before the cap clamps it.
const RETRY_BACKOFF_MAX_SHIFT: i64 = 16;

/// What to do with a task after an **execution** failure (a run error, not a reviewer's verdict)
/// — a pure decision, unit-tested. Retry within budget with exponential backoff; once spent, a
/// recurrent task resumes its schedule and a one-shot fails terminally.
#[derive(Debug, PartialEq, Eq)]
enum FailNext {
    Retry { backoff_secs: i64 },
    ResumeCron,
    Fail,
}

fn fail_next(auto_retries: i64, max_retries: i64, recurrent: bool, base_secs: i64) -> FailNext {
    if auto_retries < max_retries {
        let shift = auto_retries.clamp(0, RETRY_BACKOFF_MAX_SHIFT) as u32;
        let backoff_secs = base_secs
            .saturating_mul(1i64 << shift)
            .min(RETRY_BACKOFF_CAP_SECS);
        FailNext::Retry { backoff_secs }
    } else if recurrent {
        FailNext::ResumeCron
    } else {
        FailNext::Fail
    }
}

/// An integer `settings` value, or `default` when unset / unparseable.
fn setting_i64(conn: &rusqlite::Connection, key: &str, default: i64) -> i64 {
    crate::db::settings::get_or(conn, key, &default.to_string())
        .trim()
        .parse()
        .unwrap_or(default)
}

/// Route a failed run: auto-retry (bounded, backed off) at the same step, else resume a recurrent
/// task's schedule, else finish it terminally. Reuses the `next_run` clock — the next tick picks
/// the task up once the backoff elapses.
fn retry_or_fail(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task: &Task,
    output: &str,
) {
    let max = task
        .max_retries
        .unwrap_or_else(|| setting_i64(conn, RETRY_MAX_KEY, DEFAULT_MAX_RETRIES));
    let base = setting_i64(conn, RETRY_BASE_SECS_KEY, DEFAULT_RETRY_BASE_SECS);
    let recurrent = matches!(task.kind, crate::db::tasks::TaskKind::Recurrent);
    match fail_next(task.auto_retries, max, recurrent, base) {
        FailNext::Retry { backoff_secs } => {
            let next_run = crate::schedule::to_rfc3339_millis(now_unix() + backoff_secs);
            if let Err(err) = tasks::note_retry(conn, &task.id, &next_run) {
                log_line(format!("scheduler: retry {} failed: {err}", task.id));
            }
            super::tasks::project_if_active(conn, shared, &task.workspace_id);
            super::tasks::refresh_detail_if_open(conn, shared, &task.id);
            activity(
                shared,
                task,
                ActivityKind::Started,
                format!("retry {}/{max}", task.auto_retries + 1),
            );
        }
        FailNext::ResumeCron => match next_cron_slot(task) {
            Some(next_run) => {
                if let Err(err) = tasks::reschedule(conn, &task.id, &next_run, Some(output)) {
                    log_line(format!("scheduler: resume {} failed: {err}", task.id));
                }
                super::tasks::project_if_active(conn, shared, &task.workspace_id);
                super::tasks::refresh_detail_if_open(conn, shared, &task.id);
            }
            None => verdicts::terminal_finish(conn, shared, task, TaskStatus::Failed, output),
        },
        FailNext::Fail => verdicts::terminal_finish(conn, shared, task, TaskStatus::Failed, output),
    }
}

/// A run finished: record it in the chain, then route by what ran (`role`).
/// - **Stage** → advance to the next step, or finish (done/failed) after the last.
/// - **Producer** → on success, run the loop's validator; on failure, fail the task.
/// - **Validator** → route on the `verdict`: pass → advance past the loop; revise → re-run the
///   producer with feedback (up to `max_iterations`, then on-exhaust); reject → terminal.
pub fn finished(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    id: &str,
    role: RunRole,
    epoch: i64,
    result: Result<agent::TurnResult, String>,
    memory: Option<String>,
) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    // Dispatch guard: if the task was stopped / cancelled / restarted / re-dispatched since this
    // worker launched, its `run_epoch` has moved on — drop this superseded run entirely.
    if task.run_epoch != epoch {
        return;
    }
    let steps = pipeline::steps_for(conn, &task.workspace_id).unwrap_or_default();
    let step = steps.get(task.step_position.max(0) as usize).cloned();
    let (ok, output, invocations) = match result {
        // A model that ends on tool calls often returns no final text — don't store a blank
        // result; synthesize one from what the tools did so the task shows what happened.
        Ok(turn) => {
            // A turn whose *every* tool call failed did no real work, whatever its closing prose
            // claims — the research run whose writes all bounced off the jail and whose searches
            // all errored, yet whose reply still said "wrote docs/report.md". Route it as a
            // failure (retry / surface) instead of finishing `done` with no deliverable.
            let productive = !all_calls_failed(&turn.invocations);
            let output = effective_output(&turn);
            let output = if productive {
                output
            } else {
                format!("No work was completed — every tool call failed.\n\n{output}")
            };
            (productive, output, turn.invocations)
        }
        Err(err) => (false, err, Vec::new()),
    };
    let verdict = if matches!(role, RunRole::Validator) && ok {
        // Prefer the structured `verdict` tool call; fall back to a prose `Verdict: …` line so a
        // good review that forgot the tool isn't read as "no verdict" (which burns iterations).
        crate::tools::parse_verdict(&invocations)
            .or_else(|| crate::tools::parse_verdict_prose(&output))
    } else {
        None
    };

    // Record the run in the chain (context source + audit trail).
    let agent = step.as_ref().and_then(|s| match role {
        RunRole::Validator => s
            .validator_agent
            .clone()
            .or_else(|| owner_of(conn, &task.workspace_id)),
        _ => step_agent(conn, &task, s),
    });
    let run_id = crate::id::new_id().ok();
    // The harness commit step (G0 of docs/requests.md) runs BEFORE the run row is recorded
    // so the row can carry its pass commits (the G2 diff anchor). A successful producer/stage
    // pass commits every dirty repo (meta + projects) with Request:/Task:/Run: trailers.
    // Validators are read-only by contract and never commit; a failed pass deliberately
    // leaves the tree dirty so the revise loop's context matches the disk.
    let mut pass_commits: Vec<serde_json::Value> = Vec::new();
    if commits_pass(role, ok) {
        // The sweep scans the task's lane tree (G4): the request's worktree for
        // request-bound work, the canonical workspace for free tasks.
        let root = super::requests::task_root(conn, &data_dir(), &task);
        // A request-bound pass lands on the request's branch (G1) — cut from develop at
        // each repo's first touch — and carries the Request: trailer.
        let request = task
            .request_id
            .as_deref()
            .and_then(|request_id| db::requests::get(conn, request_id).ok().flatten())
            .filter(|req| req.status == db::requests::RequestStatus::Executing);
        let request_ref = request.as_ref().and_then(|req| {
            req.branch
                .as_deref()
                .map(|branch| crate::gitops::RequestRef {
                    id: &req.id,
                    branch,
                })
        });
        let (commits, errors) = crate::gitops::commit_pass(
            &root,
            agent.as_deref(),
            &pass_subject(step.as_ref(), &task),
            &task.id,
            run_id.as_deref().unwrap_or("unrecorded"),
            request_ref,
        );
        for pass in commits {
            // A repo born mid-task has no launch-time base — its pre-pass seed commit
            // becomes the base a restart resets to (first write wins for known repos).
            if let Some(base) = pass.base_before {
                let _ = db::task_repos::record_base(conn, &task.id, &pass.repo, &base);
            }
            // The request's first touch of this repo: record the branch point (the base
            // its delivery diff and pre-merge rollbacks anchor on).
            if let (Some(base), Some(req)) = (&pass.request_base, &request_ref) {
                let _ = db::requests::record_repo(conn, req.id, &pass.repo, req.branch, base);
            }
            let repo = if pass.repo.is_empty() {
                "workspace"
            } else {
                pass.repo.as_str()
            };
            log_line(format!(
                "gitops: {} committed {repo} @ {:.12}",
                task.id, pass.commit
            ));
            pass_commits.push(serde_json::json!({ "repo": pass.repo, "sha": pass.commit }));
        }
        for err in errors {
            log_line(format!("gitops: commit {}: {err}", task.id));
        }
    }

    // Machine checks (P1b): after a producer pass commits, each touched repo's kind runs its
    // CI recipe in the lane — the machine judgement the reviewer and the macro gate see, and
    // the fail-fast signal below. Validators are read-only and never checked; a pass that
    // committed nothing has nothing to check.
    let check_outcomes = if matches!(role, RunRole::Producer) && !pass_commits.is_empty() {
        run_pass_checks(conn, &task, &pass_commits)
    } else {
        Vec::new()
    };

    // Record the run in the chain (context source + audit trail), pass commits included.
    if let Some(run_id) = run_id.as_deref() {
        let transcript = transcript_json(&invocations);
        let commits_json = if pass_commits.is_empty() {
            None
        } else {
            serde_json::to_string(&pass_commits).ok()
        };
        let checks_json = if check_outcomes.is_empty() {
            None
        } else {
            serde_json::to_string(&check_outcomes).ok()
        };
        let _ = task_runs::append(
            conn,
            &task_runs::NewRun {
                id: run_id,
                task_id: id,
                position: task.step_position,
                role: role_str(role),
                agent: agent.as_deref(),
                output: Some(&output),
                verdict: verdict.as_ref().map(|(decision, _)| decision.as_str()),
                feedback: verdict.as_ref().map(|(_, feedback)| feedback.as_str()),
                iteration: task.iteration,
                ok,
                transcript: transcript.as_deref(),
                attempt: task.attempt,
                memory: memory.as_deref(),
                commits: commits_json.as_deref(),
                checks: checks_json.as_deref(),
            },
        );
    }
    // The run is recorded — the persisted transcript supersedes the live typewriter buffer.
    super::clear_live_turn(shared, id);

    match role {
        RunRole::Stage => match next_action(task.step_position, steps.len(), ok) {
            Next::Advance(next_position) => {
                verdicts::advance_to(conn, shared, &task, next_position)
            }
            _ if ok => verdicts::terminal_finish(conn, shared, &task, TaskStatus::Done, &output),
            // An execution failure on a plain stage: auto-retry (bounded), else terminal.
            _ => retry_or_fail(conn, shared, &task, &output),
        },
        RunRole::Producer => {
            let red_passes = if ok { check_retries(conn, &task) } else { 0 };
            if !ok {
                retry_or_fail(conn, shared, &task, &output);
            } else if !crate::ci::all_green(&check_outcomes) && red_passes <= MAX_CHECK_RETRIES {
                // Fail fast (P1b): machine checks are red — re-run the producer with the
                // failure tail (surfaced by `producer_thread` from the recorded checks)
                // instead of spending a reviewer round on what cargo already caught. Bounded
                // by its own counter, separate from the reviewer's `max_iterations`. Past the
                // cap the pass falls through to the normal review (the reviewer sees the red).
                log_line(format!(
                    "ci: {} checks red ({}) — re-running the producer (attempt {red_passes})",
                    task.id,
                    crate::ci::failed_names(&check_outcomes).join(", "),
                ));
                verdicts::relaunch(conn, &task);
                super::tasks::project_if_active(conn, shared, &task.workspace_id);
                super::tasks::refresh_detail_if_open(conn, shared, &task.id);
            } else if let Some(step) = &step {
                let auto = verdicts::workspace_auto_approves(conn, &task.workspace_id);
                match verdicts::gate_action(step.validator_kind.as_deref(), auto) {
                    // Auto-approve workspace: pass the sign-off gate automatically (R3 auto-approve,
                    // extended from the plan gate to the per-task gate) instead of parking.
                    verdicts::GateAction::AutoPass => {
                        verdicts::auto_pass_gate(conn, shared, &task, step, &steps)
                    }
                    // A human gate: park for the reviewer (the inbox) instead of an agent run.
                    verdicts::GateAction::ParkForHuman => {
                        verdicts::park_awaiting(conn, shared, &task, None)
                    }
                    verdicts::GateAction::LaunchAgentReview => {
                        launch_validator(conn, &task, step, &output);
                        super::tasks::project_if_active(conn, shared, &task.workspace_id);
                        super::tasks::refresh_detail_if_open(conn, shared, &task.id);
                    }
                }
            }
        }
        RunRole::Validator => {
            // L1a (docs/levers.md): a verdict-less review is the reviewer's indecision, not
            // a judgement — retry the REVIEW (bounded) instead of charging the producer an
            // iteration. The retried run replays its own inconclusive turns and is nudged
            // to conclude (launch_validator); past the cap it falls through to route_verdict,
            // whose None-fallback is today's revise.
            if ok
                && verdict.is_none()
                && let Some(step) = &step
            {
                let prior = task_runs::list_for_task_attempt(conn, &task.id, task.attempt)
                    .unwrap_or_default();
                if verdictless_reviews(&prior, task.step_position, task.iteration)
                    <= MAX_VERDICTLESS_RETRIES
                {
                    log_line(format!(
                        "scheduler: {} review ended without a verdict — retrying the review",
                        task.id
                    ));
                    launch_validator(conn, &task, step, &last_producer_output(conn, &task.id));
                    super::tasks::project_if_active(conn, shared, &task.workspace_id);
                    super::tasks::refresh_detail_if_open(conn, shared, &task.id);
                    return;
                }
            }
            if !ok {
                // A validator *execution* failure (endpoint timeout, transport error) is not a
                // review verdict — the producer's work is done and recorded. Retry within the
                // same budget as producer failures instead of failing the task terminally (the
                // live e2e failure: one timed-out review killed a task whose build succeeded
                // and cancelled its dependents). The retry re-enters via `launch_current`; the
                // replayed attempt lets the producer pass cheaply back to the review.
                return retry_or_fail(conn, shared, &task, &output);
            }
            let Some(step) = step else { return };
            verdicts::route_verdict(conn, shared, &task, &step, &steps, verdict, false);
        }
    }
}

fn activity(shared: &SharedServerState, task: &Task, kind: ActivityKind, detail: String) {
    let title = task.title.clone();
    let task_id = task.id.clone();
    let workspace_id = task.workspace_id.clone();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_ACTIVITY]),
        move |app| {
            super::push_activity(app, title, kind, detail, Some(task_id), Some(workspace_id));
        },
    );
}
