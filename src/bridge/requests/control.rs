//! Request controls + macro validation (G3): the review of the whole branch diff against
//! the ask verbatim when the last task lands, and the page's explicit acts — Deliver,
//! Abandon, Roll back, Push, and per-file Restore. The harness still never touches a
//! remote unprompted: Push is a button.

use rusqlite::Connection;
use rwire::SharedServerState;

use crate::db::{
    self,
    requests::{Request, RequestStatus},
};
use crate::gitops;
use crate::tools::{CompositeToolbox, RepoToolbox, Toolbox, ValidatorToolbox, VisitToolbox};
use crate::workspace;

use super::super::{Command, build_toolbox, data_dir, log_line, send};
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
                let verdict = crate::tools::parse_verdict(&turn.invocations).or_else(|| {
                    crate::tools::parse_verdict_prose(turn.reply.as_deref().unwrap_or(""))
                });
                match verdict {
                    Some((decision, feedback)) => (decision, feedback),
                    None => (
                        "revise".to_owned(),
                        "(the reviewer returned no verdict)".to_owned(),
                    ),
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
    // The reviewer's indecision gets one nudged retry (the macro analogue of the task-level
    // L1 rule) before the request holds for the user.
    if decision == "revise" && note.contains("returned no verdict") && request.validation.is_none()
    {
        log_line(format!(
            "requests: macro review of {} ended without a verdict — retrying with a nudge",
            request.id
        ));
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
        super::deliver(conn, shared, &request);
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
pub(crate) fn deliver_now(conn: &Connection, shared: &SharedServerState, request_id: &str) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return;
    };
    if request.status != RequestStatus::Validating {
        return;
    }
    if request.validation.is_none() {
        let _ = db::requests::set_validation(conn, request_id, "pass — delivered by you");
    }
    super::deliver(conn, shared, &request);
    refresh(conn, shared, &request);
}

/// Abandon an open request: cancel its open tasks, drop its branch in every touched repo
/// (develop never saw the work), and close it.
pub(crate) fn abandon(conn: &Connection, shared: &SharedServerState, request_id: &str) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return;
    };
    if !matches!(
        request.status,
        RequestStatus::Planning | RequestStatus::Executing | RequestStatus::Validating
    ) {
        return;
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
        super::super::tasks::surface(
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
}

/// Post-delivery rollback: `git revert -m1 <merge>` on develop per repo — honest history.
pub(crate) fn rollback(conn: &Connection, shared: &SharedServerState, request_id: &str) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return;
    };
    if request.status != RequestStatus::Delivered {
        return;
    }
    let root = workspace::dir_for(&data_dir(), &request.workspace_id);
    let mut errors: Vec<String> = Vec::new();
    let mut reverted = 0;
    for repo in db::requests::repos_for(conn, request_id).unwrap_or_default() {
        let Some(merge) = &repo.merge_commit else {
            continue;
        };
        let Ok(dir) = gitops::repo_dir(&root, &repo.repo) else {
            continue;
        };
        match gitops::revert_merge(&dir, merge) {
            Ok(_) => reverted += 1,
            Err(err) => errors.push(format!("{}: {err}", repo_name(&repo.repo))),
        }
    }
    if errors.is_empty() {
        let _ = db::requests::set_status(conn, request_id, RequestStatus::RolledBack);
        ping(
            conn,
            &request,
            &format!(
                "↩️ Rolled back **{}** — the delivery merge was reverted on develop in \
                 {reverted} repo(s); the branch and its history stay browsable.",
                request.title
            ),
        );
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
        super::super::tasks::surface(shared, format!("Rollback failed: {}", errors.join("; ")));
    }
    refresh(conn, shared, &request);
}

/// The explicit, user-gated push: the request branch + develop to `origin` in every
/// remote-bearing repo the request touched.
pub(crate) fn push(conn: &Connection, shared: &SharedServerState, request_id: &str) {
    let Ok(Some(request)) = db::requests::get(conn, request_id) else {
        return;
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
        match gitops::push_refs(&dir, &repo.branch) {
            Ok(_) => pushed += 1,
            Err(err) => errors.push(format!("{}: {err}", repo_name(&repo.repo))),
        }
    }
    let note = if !errors.is_empty() {
        super::super::tasks::surface(shared, format!("Push failed: {}", errors.join("; ")));
        format!(
            "⚠️ Push of **{}** hit trouble: {}",
            request.title,
            errors.join("; ")
        )
    } else if pushed == 0 {
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
    refresh(conn, shared, &request);
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
) {
    let root = workspace::dir_for(&data_dir(), workspace);
    let repo = format!("projects/{project}");
    let Ok(dir) = gitops::repo_dir(&root, &repo) else {
        return;
    };

    // Live floor gate: develop's checks must be green right now (the canonical tree is on develop).
    let strategy = super::super::exec_strategy_for(&root);
    let checks =
        crate::ci::run_for_repos(conn, workspace, &root, &strategy, std::slice::from_ref(&repo));
    if !crate::ci::all_green(&checks) {
        let names = crate::ci::failed_names(&checks).join(", ");
        super::super::tasks::surface(
            shared,
            format!("Release held for {project} — develop checks red: {names}"),
        );
        return;
    }

    let note = match gitops::release_repo(&dir, &format!("Release {project}: develop → main")) {
        Ok(gitops::MergeOutcome::Merged(_)) => {
            if !gitops::has_remote(&dir) {
                format!(
                    "🚀 Released {project} — develop → main. No origin set, so main advanced \
                     locally only."
                )
            } else if gitops::push_branch(&dir, gitops::MAIN).is_ok() {
                format!("🚀 Released {project} — develop → main, pushed to origin.")
            } else {
                format!("🚀 Released {project} — develop → main. ⚠️ Push to origin failed.")
            }
        }
        Ok(gitops::MergeOutcome::UpToDate) => {
            format!("{project} is already released — main is up to date with develop.")
        }
        Ok(gitops::MergeOutcome::Conflict(paths)) => format!(
            "Release of {project} conflicts on {} — resolve on develop first.",
            paths.join(", ")
        ),
        Err(err) => format!("Release of {project} failed: {err}"),
    };
    super::super::tasks::surface(shared, note);
    super::view::load_project_detail(conn, shared, workspace, project, None);
}

/// Per-file restore from a run's diff (G3): put one path back to its pre-pass state. The
/// restored content sits in the working tree — the task's lane's tree (G4), so an open
/// request takes the restore on its branch and the next pass commit records it.
pub(crate) fn restore_file(
    conn: &Connection,
    shared: &SharedServerState,
    workspace_id: &str,
    task_id: &str,
    repo: &str,
    sha: &str,
    path: &str,
) {
    let root = db::tasks::get(conn, task_id).ok().flatten().map_or_else(
        || workspace::dir_for(&data_dir(), workspace_id),
        |task| super::task_root(conn, &data_dir(), &task),
    );
    let outcome = gitops::repo_dir(&root, repo)
        .map_err(|err| err.to_string())
        .and_then(|dir| gitops::restore_file(&dir, sha, path).map_err(|err| err.to_string()));
    match outcome {
        Ok(()) => log_line(format!("requests: restored {path} to before {sha:.12}")),
        Err(err) => {
            super::super::tasks::surface(shared, format!("Restore of {path} failed: {err}"));
        }
    }
}

/// Re-project everything a control action can change: the request page, the rail + board.
fn refresh(conn: &Connection, shared: &SharedServerState, request: &Request) {
    super::view::load_detail(conn, shared, &request.id);
    super::super::tasks::project_if_active(conn, shared, &request.workspace_id);
}
