//! Request controls + macro validation (G3): the review of the whole branch diff against
//! the ask verbatim when the last task lands, and the page's explicit acts — Deliver,
//! Abandon, Roll back, Push, and per-file Restore. The harness still never touches a
//! remote unprompted: Push is a button.

use std::path::PathBuf;

use rusqlite::Connection;
use rwire::SharedServerState;

use crate::db::{
    self,
    requests::{Request, RequestStatus},
};
use crate::gitops;
use crate::tools::{CompositeToolbox, RepoToolbox, Toolbox, ValidatorToolbox, VisitToolbox};
use crate::workspace;

use super::super::{
    Command, Resolver, build_toolbox, data_dir, log_line, resolve_err, resolve_ok, send,
};
use super::{ping, repo_name};

/// Cap on the branch diff injected into the validation prompt.
const VALIDATION_DIFF_CAP_CHARS: usize = 8_000;

/// Every task landed: move to `validating`. An auto-approve workspace gets the agent macro
/// review; otherwise the request parks for the user, who resolves it from the request page.
pub(super) fn begin_validation(conn: &Connection, request: &Request) {
    if let Err(err) = db::requests::set_status(conn, &request.id, RequestStatus::Validating) {
        return log_line(format!("requests: validating {}: {err}", request.id));
    }
    let auto = db::workspaces::get(conn, &request.workspace_id)
        .ok()
        .flatten()
        .is_some_and(|ws| ws.auto_approve);
    if auto {
        launch_validation(conn, request, false);
    } else {
        ping(
            conn,
            request,
            &format!(
                "🔎 **{}** is ready for your review — every task landed. Open its request \
                 page to inspect the diff and Deliver (or Abandon).",
                request.title
            ),
        );
    }
}

/// Boot recovery: relaunch macro reviews orphaned by a server restart. A request stuck
/// `validating` with no verdict recorded is, in an auto-approve workspace, a review whose
/// worker died mid-run (a non-auto workspace parks there by design — the user's gate).
pub(crate) fn recover_validations(conn: &Connection) {
    for ws in db::workspaces::list(conn).unwrap_or_default() {
        if !ws.auto_approve {
            continue;
        }
        for request in db::requests::open_for_workspace(conn, &ws.id).unwrap_or_default() {
            if request.status == RequestStatus::Validating && request.validation.is_none() {
                log_line(format!(
                    "requests: relaunching the macro review of {} (orphaned by a restart)",
                    request.id
                ));
                launch_validation(conn, &request, false);
            }
        }
    }
}

/// The agent macro review, on a worker thread: the reviewer sees the ask verbatim and the
/// whole branch diff, may inspect the tree, and must call `verdict`. Posts
/// [`Command::RequestValidated`] back to the bridge.
fn launch_validation(conn: &Connection, request: &Request, nudge: bool) {
    let reviewer = validation_reviewer(conn, &request.workspace_id);
    let Some(reviewer) = reviewer else {
        return log_line(format!(
            "requests: no reviewer for {} — parked for the user",
            request.id
        ));
    };
    let inference =
        match db::resolution::resolve_for_agent(conn, &reviewer, &request.workspace_id, |key| {
            std::env::var(key).ok()
        }) {
            Ok(inference) => inference,
            Err(err) => return log_line(format!("requests: resolve reviewer failed: {err}")),
        };
    // Root the review at the request's tree (G4): the reviewer must read the branch's
    // files, not canonical `develop` — which no longer carries the work.
    let workspace_dir = super::request_root(&data_dir(), request);
    let role = super::super::role_prompt(conn, crate::state::RunKind::Validator);
    let system_prompt = super::super::run_system_prompt(
        &role,
        Some(&workspace_dir),
        &inference.agent_name,
        inference.agent_prompt.clone(),
        workspace::standards_layer(&workspace_dir),
    );
    let diff = branch_diff(conn, request, &workspace_dir);
    // Preview capability (P2c): a previewable request lets the final reviewer `visit` the running
    // branch before signing off — click through the change, don't just read the diff.
    let preview_block =
        match super::super::preview::preview_hint(conn, &request.workspace_id, &workspace_dir) {
            Some(hint) => format!("\n\n{hint}"),
            None => String::new(),
        };
    let brief = format!(
        "A delegated request has finished all its tasks; you are the final reviewer before \
         delivery. Judge the WHOLE change against the user's original ask, verbatim — not \
         the tasks' claims.\n\nThe ask (the contract):\n{}\n\nWhat changed on the request \
         branch (against its branch point, per repo):\n{}{}\n\nInspect further if needed \
         (`read`/`list`/`find`, `bash` for builds/tests, `repo_diff`/`repo_log`). Then call \
         the `verdict` tool: pass = deliver (merge to develop); revise or reject = hold it \
         for the user, with concrete reasons.",
        request.ask,
        if diff.is_empty() {
            "(no file changes recorded)".to_owned()
        } else {
            diff
        },
        preview_block,
    );
    let brief = if nudge {
        format!(
            "{brief}\n\nYour previous review turn ended WITHOUT calling the `verdict` \
             tool, so it decided nothing. Inspect less, conclude more: call `verdict` \
             (pass / revise / reject) in THIS turn."
        )
    } else {
        brief
    };
    let messages = crate::agent::build_messages(
        &system_prompt,
        std::slice::from_ref(&crate::state::ChatMessage {
            role: crate::state::MessageRole::User,
            text: brief,
            time: super::super::now_hm(),
        }),
    );
    let checks = build_toolbox(
        Some(vec![
            "read".to_owned(),
            "list".to_owned(),
            "find".to_owned(),
            "bash".to_owned(),
        ]),
        Some(workspace_dir.clone()),
        false,
    );
    let visit = VisitToolbox::new(request.workspace_id.clone(), workspace_dir.clone());
    let toolbox: Box<dyn Toolbox> = Box::new(CompositeToolbox::new(vec![
        Box::new(ValidatorToolbox),
        checks,
        Box::new(visit),
        Box::new(RepoToolbox::for_workspace(workspace_dir)),
    ]));
    let request_id = request.id.clone();
    std::thread::spawn(move || {
        let (decision, note) = match crate::agent::run_turn(&inference, messages, toolbox.as_ref())
        {
            Ok(turn) => {
                // The verdict, in order of trust: the structured/recovered tool call, the
                // reply prose, then the model's reasoning stream — a reasoning model can end
                // the turn with its whole conclusion (verdict included) in `reasoning` and an
                // empty reply, which used to read as "no verdict" and loop the nudge.
                let verdict = crate::tools::parse_verdict(&turn.invocations)
                    .or_else(|| {
                        crate::tools::parse_verdict_prose(turn.reply.as_deref().unwrap_or(""))
                    })
                    .or_else(|| {
                        crate::tools::parse_verdict_prose(turn.reasoning.as_deref().unwrap_or(""))
                    });
                match verdict {
                    Some((decision, feedback)) => (decision, feedback),
                    // Keep the reply's tail: a verdict-less turn used to discard what the
                    // reviewer actually said, leaving nothing to diagnose (was it prose, or
                    // tool markup the recovery didn't know?). The "returned no verdict"
                    // substring is load-bearing — `on_validated`'s nudge matches on it.
                    None => {
                        // Show what the reviewer actually produced — the reply, or its
                        // reasoning when the reply is empty (the diagnosable part).
                        let visible = turn
                            .reply
                            .as_deref()
                            .filter(|text| !text.trim().is_empty())
                            .or(turn.reasoning.as_deref())
                            .unwrap_or("");
                        (
                            "revise".to_owned(),
                            format!(
                                "(the reviewer returned no verdict) reply tail: {}",
                                reply_tail(visible, 300)
                            ),
                        )
                    }
                }
            }
            Err(err) => ("error".to_owned(), err.to_string()),
        };
        send(Command::RequestValidated {
            request_id,
            decision,
            note,
        });
        super::super::warm_context_cache(&inference);
    });
}

/// The macro reviewer: the pipeline's last validator agent (the workspace's standing
/// reviewer, e.g. Critic), else the owner.
fn validation_reviewer(conn: &Connection, workspace_id: &str) -> Option<String> {
    let steps = db::pipeline::steps_for(conn, workspace_id).unwrap_or_default();
    steps
        .iter()
        .rev()
        .find_map(|step| step.validator_agent.clone())
        .or_else(|| {
            db::workspaces::get(conn, workspace_id)
                .ok()
                .flatten()
                .and_then(|ws| ws.owner)
        })
}

/// The whole branch diff (stat + capped patch per repo) — what the macro review judges.
fn branch_diff(conn: &Connection, request: &Request, root: &std::path::Path) -> String {
    let repos = db::requests::repos_for(conn, &request.id).unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for repo in &repos {
        let Ok(dir) = gitops::repo_dir(root, &repo.repo) else {
            continue;
        };
        let range = format!("{}..{}", repo.base_commit, repo.branch);
        let stat = gitops::diff_range(&dir, &range, true).unwrap_or_default();
        if stat.is_empty() {
            continue;
        }
        let patch = gitops::diff_range(&dir, &range, false).unwrap_or_default();
        parts.push(format!("{}:\n{stat}\n{patch}", repo_name(&repo.repo)));
    }
    let joined = parts.join("\n\n");
    if joined.chars().count() > VALIDATION_DIFF_CAP_CHARS {
        let head: String = joined.chars().take(VALIDATION_DIFF_CAP_CHARS).collect();
        format!("{head}\n…[diff truncated — repo_diff shows the rest]")
    } else {
        joined
    }
}

/// The last `max_chars` of a reviewer reply (char-safe) — the tail is where a truncation
/// artifact or an unparsed tool-call emission shows.
fn reply_tail(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty reply)".to_owned();
    }
    let count = trimmed.chars().count();
    if count <= max_chars {
        return trimmed.to_owned();
    }
    let tail: String = trimmed.chars().skip(count - max_chars).collect();
    format!("…{tail}")
}

/// The reviewer's verdict came back: record it; pass delivers, anything else parks the
/// request (still `validating`) with the reasons pinged — the user decides from the page.
pub(crate) fn on_validated(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    decision: &str,
    note: &str,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return;
    };
    if request.status != RequestStatus::Validating {
        return; // resolved by the user (or superseded) while the review ran
    }
    // The reviewer's indecision gets ONE nudged retry (the macro analogue of the task-level
    // L1 rule) before the request holds for the user. The nudge stamps `validation` with a
    // marker — previously nothing recorded that a nudge happened, so a persistently
    // verdict-less reviewer looped nudge → no verdict → nudge forever (observed live: 4+
    // attempts on one request), and the request never parked for the user.
    if decision == "revise" && note.contains("returned no verdict") && request.validation.is_none()
    {
        log_line(format!(
            "requests: macro review of {} ended without a verdict — retrying with a nudge",
            request.id
        ));
        let _ = db::requests::set_validation(
            conn,
            request_id,
            // The whole note, not a 200-char first-line cap: it carries the reviewer's reply
            // tail — the forensic evidence for WHY it wouldn't render a verdict.
            &format!("(nudged — {note})"),
        );
        launch_validation(conn, &request, true);
        return;
    }
    let summary = format!("{decision} — {}", crate::text::first_line_capped(note, 200));
    let _ = db::requests::set_validation(conn, request_id, &summary);
    if decision == "pass" {
        // The macro gate sees CI (P1b): red machine checks block auto-delivery even when the
        // model reviewer passes — the reviewer judges intent, the machines judge correctness,
        // and both must be green to deliver unattended. The operator can still Deliver anyway
        // on the request page (recorded here as the override trail).
        let checks = super::view::request_checks(conn, request_id);
        if !crate::ci::all_green(&checks) {
            let names = crate::ci::failed_names(&checks).join(", ");
            let _ = db::requests::set_validation(
                conn,
                request_id,
                &format!("{summary}\n(auto-delivery held — machine checks red: {names})"),
            );
            ping(
                conn,
                &request,
                &format!(
                    "⚠️ Review passed but **machine checks are red** ({names}) for **{}** — \
                     auto-delivery held. Push a fix, or Deliver anyway on the request page.",
                    request.title
                ),
            );
            refresh(conn, shared, &request);
            return;
        }
        // Auto-delivery after a passed review: a failure already pings the origin chat and
        // logs (there's no control mid-flight to error).
        let _ = super::deliver(conn, &request);
    } else {
        ping(
            conn,
            &request,
            &format!(
                "⚠️ Validation held **{}** ({decision}): {note}\n\nReview it on the request \
                 page — Deliver anyway, or Abandon.",
                request.title
            ),
        );
    }
    refresh(conn, shared, &request);
}

/// The page's Deliver button — the human resolving the gate (or overriding a held verdict).
pub(crate) fn deliver_now(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    resolver: Option<Resolver>,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return resolve_ok(resolver);
    };
    if request.status != RequestStatus::Validating {
        return resolve_ok(resolver);
    }
    if request.validation.is_none() {
        let _ = db::requests::set_validation(conn, request_id, "pass — delivered by you");
    }
    let outcome = super::deliver(conn, &request);
    refresh(conn, shared, &request);
    match outcome {
        Ok(()) => resolve_ok(resolver),
        Err(message) => resolve_err(resolver, message),
    }
}

/// Abandon an open request: cancel its open tasks, drop its branch in every touched repo
/// (develop never saw the work), and close it.
pub(crate) fn abandon(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    resolver: Option<Resolver>,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return resolve_ok(resolver);
    };
    if !matches!(
        request.status,
        RequestStatus::Planning | RequestStatus::Executing | RequestStatus::Validating
    ) {
        return resolve_ok(resolver);
    }
    for task in db::tasks::list_for_request(conn, request_id).unwrap_or_default() {
        if !matches!(
            task.status,
            db::tasks::TaskStatus::Done
                | db::tasks::TaskStatus::Failed
                | db::tasks::TaskStatus::Rejected
                | db::tasks::TaskStatus::Cancelled
        ) {
            let _ = db::tasks::cancel(conn, &task.id);
        }
    }
    let root = workspace::dir_for(&data_dir(), &request.workspace_id);
    // The tree goes first (G4): a branch checked out in a worktree can't be deleted. Its
    // sandbox goes with it (P0) — abandon kills the lane's in-flight processes.
    let tree = gitops::tree_root(&data_dir(), &request.workspace_id, request_id);
    // Stop this branch's preview before the sandbox that hosts it is destroyed (P2).
    crate::bridge::preview::stop_request(&request.workspace_id, request_id);
    crate::sandbox::destroy(&crate::sandbox::name_for(&tree));
    let mut errors = gitops::remove_request_tree(&root, &tree);
    // Then `-D` the branch everywhere it exists — touched repos and composed-but-untouched
    // ones alike; the dropped commits are the point (develop never saw them).
    if let Some(branch) = &request.branch {
        for repo in gitops::repos(&root) {
            let Ok(dir) = gitops::repo_dir(&root, &repo) else {
                continue;
            };
            if gitops::branch_exists(&dir, branch)
                && let Err(err) = gitops::abandon_branch(&dir, branch)
            {
                errors.push(format!("{}: {err}", repo_name(&repo)));
            }
        }
    }
    let _ = db::requests::set_status(conn, request_id, RequestStatus::Abandoned);
    let note = if errors.is_empty() {
        format!(
            "🗑 Abandoned **{}** — its branch was dropped; develop never saw the work.",
            request.title
        )
    } else {
        super::super::surface_error(
            shared,
            format!("Abandon hit trouble: {}", errors.join("; ")),
        );
        format!(
            "🗑 Abandoned **{}** — with git trouble: {}",
            request.title,
            errors.join("; ")
        )
    };
    ping(conn, &request, &note);
    refresh(conn, shared, &request);
    resolve_ok(resolver);
}

/// Send a `validating` request back to the agents to revise (G3): re-open the work as one fresh
/// pending pass on the same branch — carrying the owner's feedback — and flip back to `Executing`
/// so the normal build → review → macro-validate machinery re-runs. Unlike Abandon (drop the branch)
/// or Deliver (merge it), this keeps the branch and its work: a **new** task (not a `restart`, which
/// would reset the tree to base) runs from the branch HEAD, so the producer builds on what's there,
/// fixes what's flagged, and the request re-parks `validating` with a fresh verdict + gate.
pub(crate) fn request_changes(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    feedback: &str,
    resolver: Option<Resolver>,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return resolve_ok(resolver);
    };
    if request.status != RequestStatus::Validating {
        // Resolved (delivered/abandoned) since the page loaded — ignore a stale click.
        return resolve_ok(resolver);
    }
    let feedback = feedback.trim();
    // Reuse the producer that built the work: the most recent non-plan task's agent (its build
    // tasks all ran as that producer); the owner (`None`) if the request has no such task.
    let tasks = db::tasks::list_for_request(conn, request_id).unwrap_or_default();
    let agent = tasks
        .iter()
        .rev()
        .find(|task| !task.is_plan)
        .and_then(|task| task.agent.clone());
    let verdict = request.validation.as_deref().unwrap_or("(none recorded)");
    let asked = if feedback.is_empty() {
        "(address the reviewer's verdict above)"
    } else {
        feedback
    };
    let spec = format!(
        "The delivered work for this request needs revision before it can ship. The work is ALREADY \
         on this request's branch — build on it, fix exactly what's flagged below, keep what already \
         works, and make the project build and its checks pass.\n\nThe original ask (the \
         contract):\n{}\n\nThe reviewer's verdict:\n{verdict}\n\nThe owner's requested \
         changes:\n{asked}",
        request.ask
    );
    let Ok(task_id) = crate::id::new_id() else {
        return resolve_err(resolver, "could not mint the revision task's id");
    };
    if let Err(err) = db::tasks::create(
        conn,
        &db::tasks::NewTask {
            id: &task_id,
            workspace_id: &request.workspace_id,
            agent: agent.as_deref(),
            title: &format!("Revise: {}", request.title),
            spec: &spec,
            schedule: None,
            origin_chat: request.origin_chat.as_deref(),
            max_retries: None,
        },
    ) {
        return resolve_err(
            resolver,
            format!("could not create the revision task: {err}"),
        );
    }
    if let Err(err) = db::tasks::set_request(conn, &task_id, request_id) {
        return resolve_err(
            resolver,
            format!("could not attach the revision task to the request: {err}"),
        );
    }
    // Record the trail on the page, then re-arm: `Executing` restores the request's lane and
    // re-enables `maybe_deliver`'s re-validation when the new pass lands.
    let trail = if feedback.is_empty() {
        "changes requested — re-delegated by you".to_owned()
    } else {
        format!(
            "changes requested — {}",
            crate::text::first_line_capped(feedback, 200)
        )
    };
    let _ = db::requests::set_validation(conn, request_id, &trail);
    if let Err(err) = db::requests::set_status(conn, request_id, RequestStatus::Executing) {
        return resolve_err(
            resolver,
            format!("could not re-open the request for revision: {err}"),
        );
    }
    let note = if feedback.is_empty() {
        format!(
            "↩️ Sent **{}** back to the agents to revise — it'll re-run on its branch, then \
             return for your review.",
            request.title
        )
    } else {
        format!(
            "↩️ Sent **{}** back to the agents with your changes — it'll re-run on its branch, \
             then return for your review.\n\n> {feedback}",
            request.title
        )
    };
    ping(conn, &request, &note);
    refresh(conn, shared, &request);
    resolve_ok(resolver);
}

/// Post-delivery rollback: `git revert -m1 <merge>` on develop per repo — honest history.
pub(crate) fn rollback(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    resolver: Option<Resolver>,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return resolve_ok(resolver);
    };
    if request.status != RequestStatus::Delivered {
        return resolve_ok(resolver);
    }
    let root = workspace::dir_for(&data_dir(), &request.workspace_id);
    let mut errors: Vec<String> = Vec::new();
    let mut reverted: Vec<String> = Vec::new();
    for repo in db::requests::repos_for(conn, request_id).unwrap_or_default() {
        let Some(merge) = &repo.merge_commit else {
            continue;
        };
        let Ok(dir) = gitops::repo_dir(&root, &repo.repo) else {
            continue;
        };
        match gitops::revert_merge(&dir, merge) {
            Ok(_) => reverted.push(repo.repo.clone()),
            Err(err) => errors.push(format!("{}: {err}", repo_name(&repo.repo))),
        }
    }
    let outcome = if errors.is_empty() {
        let _ = db::requests::set_status(conn, request_id, RequestStatus::RolledBack);
        ping(
            conn,
            &request,
            &format!(
                "↩️ Rolled back **{}** — the delivery merge was reverted on develop in \
                 {} repo(s); the branch and its history stay browsable.",
                request.title,
                reverted.len()
            ),
        );
        // The revert mutated develop — re-check it in the background (the CD beat), same as a
        // delivery merge.
        super::develop_ci::launch(
            request.workspace_id.clone(),
            request.id.clone(),
            format!("rolling back \"{}\"", request.title),
            reverted,
        );
        Ok(())
    } else {
        ping(
            conn,
            &request,
            &format!(
                "⚠️ Rollback of **{}** hit trouble: {} — it stays delivered.",
                request.title,
                errors.join("; ")
            ),
        );
        Err(format!("rollback failed: {}", errors.join("; ")))
    };
    refresh(conn, shared, &request);
    match outcome {
        Ok(()) => resolve_ok(resolver),
        Err(message) => resolve_err(resolver, message),
    }
}

/// The filesystem path of a project's configured SSH deploy key (P5b), or `None` when unset — the
/// push then runs keyless (ssh-agent / default identities), unchanged from before P5b.
fn project_deploy_key(conn: &Connection, workspace: &str, project: &str) -> Option<PathBuf> {
    crate::project_settings::deploy_key(conn, workspace, project)
        .and_then(|name| crate::ssh_store::key_path(&data_dir(), &name))
}

/// The explicit, user-gated push: the request branch + develop to `origin` in every
/// remote-bearing repo the request touched, each under its project's chosen deploy key (P5b).
pub(crate) fn push(
    conn: &Connection,
    shared: &SharedServerState,
    request_id: &str,
    resolver: Option<Resolver>,
) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return resolve_ok(resolver);
    };
    let root = workspace::dir_for(&data_dir(), &request.workspace_id);
    let mut pushed = 0;
    let mut errors: Vec<String> = Vec::new();
    for repo in db::requests::repos_for(conn, request_id).unwrap_or_default() {
        let Ok(dir) = gitops::repo_dir(&root, &repo.repo) else {
            continue;
        };
        if !gitops::has_remote(&dir) {
            continue;
        }
        let key = repo
            .repo
            .strip_prefix("projects/")
            .and_then(|project| project_deploy_key(conn, &request.workspace_id, project));
        match gitops::push_signed(
            &dir,
            "origin",
            &[gitops::DEVELOP, &repo.branch],
            key.as_deref(),
        ) {
            Ok(_) => pushed += 1,
            Err(err) => errors.push(format!("{}: {err}", repo_name(&repo.repo))),
        }
    }
    let outcome = if !errors.is_empty() {
        let joined = errors.join("; ");
        ping(
            conn,
            &request,
            &format!("⚠️ Push of **{}** hit trouble: {joined}", request.title),
        );
        Err(format!("push failed: {joined}"))
    } else {
        let note = if pushed == 0 {
            format!(
                "**{}**: no repo it touched has a remote — nothing to push.",
                request.title
            )
        } else {
            format!(
                "⬆️ Pushed **{}** — develop + its branch to origin in {pushed} repo(s).",
                request.title
            )
        };
        ping(conn, &request, &note);
        Ok(())
    };
    refresh(conn, shared, &request);
    match outcome {
        Ok(()) => resolve_ok(resolver),
        Err(message) => resolve_err(resolver, message),
    }
}

/// Release a project (P5a): CI-gate `develop` live, merge it into `main` **working-tree-less** (the
/// keystone — the canonical develop tree and any running preview are untouched), and push `main`
/// iff an origin exists. Project-level (a project is one repo), decoupled from requests — reported
/// via a toast, then the project page re-projects so its Release panel reflects the new state.
pub(crate) fn release_project(
    conn: &Connection,
    shared: &SharedServerState,
    workspace: &str,
    project: &str,
    resolver: Option<Resolver>,
) {
    let root = workspace::dir_for(&data_dir(), workspace);
    let repo = format!("projects/{project}");
    let Ok(dir) = gitops::repo_dir(&root, &repo) else {
        return resolve_err(
            resolver,
            format!("{project}: its repository could not be opened"),
        );
    };

    // Live floor gate: develop's checks must be green right now (the canonical tree is on develop).
    let strategy = super::super::exec_strategy_for(&root);
    let checks = crate::ci::run_for_repos(
        conn,
        workspace,
        &root,
        &strategy,
        std::slice::from_ref(&repo),
    );
    if !crate::ci::all_green(&checks) {
        let names = crate::ci::failed_names(&checks).join(", ");
        return resolve_err(
            resolver,
            format!("Release held for {project} — develop checks red: {names}"),
        );
    }

    let outcome = match gitops::release_repo(&dir, &format!("Release {project}: develop → main"))
    {
        Ok(gitops::MergeOutcome::Merged(_)) => {
            // Serve the released tree: refresh the detached `main` worktree + main-staging preview
            // in the background (a cold server start can take the full health deadline).
            crate::bridge::preview::refresh_main_staging(
                workspace,
                dir.clone(),
                gitops::main_tree_root(&data_dir(), workspace),
                project,
                data_dir(),
            );
            if gitops::has_remote(&dir) {
                let key = project_deploy_key(conn, workspace, project);
                if let Err(err) =
                    gitops::push_signed(&dir, "origin", &[gitops::MAIN], key.as_deref())
                {
                    log_line(format!(
                        "release {project}: pushed main locally; origin push failed: {err}"
                    ));
                }
            }
            Ok(())
        }
        Ok(gitops::MergeOutcome::UpToDate) => Ok(()),
        Ok(gitops::MergeOutcome::Conflict(paths)) => Err(format!(
            "Release of {project} conflicts on {} — resolve on develop first.",
            paths.join(", ")
        )),
        Err(err) => Err(format!("Release of {project} failed: {err}")),
    };
    super::view::load_project_detail(conn, shared, workspace, project, None);
    match outcome {
        Ok(()) => resolve_ok(resolver),
        Err(message) => resolve_err(resolver, message),
    }
}

/// Per-file restore from a run's diff (G3): put one path back to its pre-pass state. The
/// restored content sits in the working tree — the task's lane's tree (G4), so an open
/// request takes the restore on its branch and the next pass commit records it.
pub(crate) fn restore_file(
    conn: &Connection,
    workspace_id: &str,
    task_id: &str,
    repo: &str,
    sha: &str,
    path: &str,
    resolver: Option<Resolver>,
) {
    let root = db::tasks::get(conn, task_id).ok().flatten().map_or_else(
        || workspace::dir_for(&data_dir(), workspace_id),
        |task| super::task_root(conn, &data_dir(), &task),
    );
    let outcome = gitops::repo_dir(&root, repo)
        .map_err(|err| err.to_string())
        .and_then(|dir| gitops::restore_file(&dir, sha, path).map_err(|err| err.to_string()));
    match outcome {
        Ok(()) => {
            log_line(format!("requests: restored {path} to before {sha:.12}"));
            resolve_ok(resolver);
        }
        Err(err) => resolve_err(resolver, format!("could not restore {path}: {err}")),
    }
}

/// Re-project everything a control action can change: the request page, the rail + board.
fn refresh(conn: &Connection, shared: &SharedServerState, request: &Request) {
    super::view::load_detail(conn, shared, &request.id);
    super::super::tasks::project_if_active(conn, shared, &request.workspace_id);
}
