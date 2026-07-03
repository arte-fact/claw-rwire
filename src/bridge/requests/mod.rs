//! Request lifecycle (G1 of `docs/requests.md`): plan approval mints the branch and opens
//! execution; the last task landing triggers delivery — a `--no-ff` merge to `develop` per
//! touched repo and a receipt into the origin chat. Validation is stubbed as pass in v0
//! (the macro review against the ask verbatim is G3).

pub(crate) mod control;
mod view;
pub(crate) use view::{load_detail, pass_diff, rail_views};

use rusqlite::Connection;

use crate::db::{
    self,
    requests::{Request, RequestStatus},
    tasks::Task,
};
use crate::gitops;
use crate::state::MessageRole;
use crate::workspace;

use super::{data_dir, log_line};

/// The plan gate approved: mint the branch name (once) and open execution — from here the
/// commit sweep cuts `req/…` at each repo's first touch and the lane picks the request's tasks.
pub(crate) fn on_plan_approved(conn: &Connection, plan: &Task) {
    let Some(request) = request_of(conn, plan) else {
        return;
    };
    if request.branch.is_none()
        && let Err(err) = db::requests::set_branch(conn, &request.id, &branch_name(&request))
    {
        log_line(format!("requests: set branch {}: {err}", request.id));
    }
    if let Err(err) = db::requests::set_status(conn, &request.id, RequestStatus::Executing) {
        log_line(format!("requests: activate {}: {err}", request.id));
    }
}

/// The plan gate rejected: close the request — nothing ever touched a branch.
pub(crate) fn on_plan_rejected(conn: &Connection, plan: &Task) {
    let Some(request) = request_of(conn, plan) else {
        return;
    };
    if let Err(err) = db::requests::set_status(conn, &request.id, RequestStatus::Rejected) {
        log_line(format!("requests: reject {}: {err}", request.id));
    }
}

/// A request task reached a terminal state: deliver when the whole set has landed. Every
/// task of the request (the plan included) must be `done` — a failed or cancelled sibling
/// holds delivery until it's restarted to completion, so a partial request never merges.
pub(crate) fn maybe_deliver(conn: &Connection, task: &Task) {
    let Some(request) = request_of(conn, task) else {
        return;
    };
    if request.status != RequestStatus::Executing {
        return;
    }
    match db::tasks::open_for_request(conn, &request.id) {
        // The macro gate (G3): the branch is judged against the ask before any merge —
        // by the reviewer agent (auto-approve workspaces) or by the user from the page.
        Ok(0) => control::begin_validation(conn, &request),
        Ok(_) => {}
        Err(err) => log_line(format!("requests: count open for {}: {err}", request.id)),
    }
}

/// Delivery: merge the request branch into `develop` in every repo it touched — from the
/// **canonical** workspace (worktrees share refs and objects, so the branch is right there),
/// adopting repos born inside the request's tree first — stamp the merges, tear the tree
/// down, flip to `delivered`, and drop the receipt into the origin chat. Any merge failure
/// (a conflict with a parallel request, a dirty canonical tree) leaves the request open with
/// the trouble surfaced — nothing half-delivers silently, and `develop` is never left
/// mid-merge (the merge aborts).
pub(super) fn deliver(conn: &Connection, request: &Request) {
    let root = workspace::dir_for(&data_dir(), &request.workspace_id);
    let tree = gitops::tree_root(&data_dir(), &request.workspace_id, &request.id);
    let repos = db::requests::repos_for(conn, &request.id).unwrap_or_default();
    let message = format!("Deliver: {}\n\nRequest: {}", request.title, request.id);
    let mut errors: Vec<String> = Vec::new();
    let (mut files, mut adds, mut dels) = (0, 0, 0);
    for repo in &repos {
        if repo.merge_commit.is_some() {
            continue; // already merged on an earlier (partially failed) delivery
        }
        let dir = match gitops::repo_dir(&root, &repo.repo) {
            Ok(dir) => dir,
            Err(err) => {
                errors.push(format!("{}: {err}", repo_name(&repo.repo)));
                continue;
            }
        };
        // A repo born inside the request's tree exists only there — move it into place.
        if !gitops::is_repo(&dir)
            && let Ok(born) = gitops::repo_dir(&tree, &repo.repo)
            && gitops::is_repo(&born)
            && let Err(err) = gitops::adopt_tree_repo(&born, &dir)
        {
            errors.push(format!("{}: adopt: {err}", repo_name(&repo.repo)));
            continue;
        }
        match gitops::deliver_repo(&dir, &repo.branch, &message) {
            Ok(merge) => {
                let _ = db::requests::set_repo_merge(conn, &request.id, &repo.repo, &merge);
                if let Ok((f, a, d)) =
                    gitops::diff_numstat(&dir, &format!("{}..{merge}", repo.base_commit))
                {
                    files += f;
                    adds += a;
                    dels += d;
                }
            }
            Err(err) => errors.push(format!("{}: {err}", repo_name(&repo.repo))),
        }
    }
    if !errors.is_empty() {
        log_line(format!(
            "requests: delivery of {} incomplete: {}",
            request.id,
            errors.join("; ")
        ));
        ping(
            conn,
            request,
            &format!(
                "⚠️ Delivery of **{}** hit trouble ({}) — the request stays open; resolve \
                 (a conflicting parallel delivery needs a rebase — restart a task, or \
                 Abandon) and press Deliver to retry.",
                request.title,
                errors.join("; ")
            ),
        );
        return;
    }
    // Everything merged — the tree has served its purpose (G4). Tear it down and drop the
    // branch in repos the request composed but never touched (`-d`: trivially merged);
    // touched repos keep theirs — the passes stay browsable behind the merge.
    let touched: std::collections::HashSet<&str> =
        repos.iter().map(|repo| repo.repo.as_str()).collect();
    for err in gitops::remove_request_tree(&root, &tree) {
        log_line(format!("requests: teardown {}: {err}", request.id));
    }
    if let Some(branch) = &request.branch {
        for repo in gitops::repos(&root) {
            if touched.contains(repo.as_str()) {
                continue;
            }
            if let Ok(dir) = gitops::repo_dir(&root, &repo)
                && gitops::branch_exists(&dir, branch)
                && let Err(err) = gitops::drop_branch(&dir, branch, false)
            {
                log_line(format!(
                    "requests: drop {branch} in {}: {err}",
                    repo_name(&repo)
                ));
            }
        }
    }
    if let Err(err) = db::requests::set_status(conn, &request.id, RequestStatus::Delivered) {
        return log_line(format!("requests: mark delivered {}: {err}", request.id));
    }
    let tasks_done = db::tasks::count_for_request(conn, &request.id).unwrap_or(0);
    let receipt = if repos.is_empty() {
        format!(
            "📦 Delivered **{}** — {tasks_done} task(s); no file changes to merge.",
            request.title
        )
    } else {
        format!(
            "📦 Delivered **{}** — {tasks_done} task(s), +{adds}/−{dels} across {files} \
             file(s); merged to `develop` in {} repo(s).",
            request.title,
            repos.len()
        )
    };
    log_line(format!(
        "requests: delivered {} ({})",
        request.id, request.title
    ));
    ping(conn, request, &receipt);
}

/// Parse a run's stored pass commits (`[{"repo","sha"}]`) into (repo, sha) pairs.
pub(crate) fn parse_commits(json: Option<&str>) -> Vec<(String, String)> {
    let Some(json) = json else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .iter()
        .filter_map(|value| {
            Some((
                value.get("repo")?.as_str()?.to_owned(),
                value.get("sha")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The request a task belongs to, if any.
fn request_of(conn: &Connection, task: &Task) -> Option<Request> {
    let request_id = task.request_id.as_deref()?;
    db::requests::get(conn, request_id).ok().flatten()
}

/// The directory a request's working-tree surfaces use: its composed worktree when present,
/// else the canonical workspace (planning, composition fallback, terminal states). Never
/// composes — see [`ensure_lane_root`] for the launch path.
pub(crate) fn request_root(data_dir: &std::path::Path, request: &Request) -> std::path::PathBuf {
    let tree = gitops::tree_root(data_dir, &request.workspace_id, &request.id);
    if gitops::is_repo(&tree) {
        tree
    } else {
        workspace::dir_for(data_dir, &request.workspace_id)
    }
}

/// The directory a task's runs, sweeps, and resets use: its request's tree when composed,
/// else the canonical workspace (free-lane tasks, plan tasks, fallback).
pub(crate) fn task_root(
    conn: &Connection,
    data_dir: &std::path::Path,
    task: &Task,
) -> std::path::PathBuf {
    match request_of(conn, task) {
        Some(request) => request_root(data_dir, &request),
        None => workspace::dir_for(data_dir, &task.workspace_id),
    }
}

/// Resolve a launching task's lane root, composing its request's tree on first touch (G4):
/// every workspace repo gets a linked worktree on the request branch, cut from `develop`.
/// Composition trouble falls back to the canonical tree — the G3 single-lane behaviour —
/// with the errors logged; work is never blocked on git.
pub(crate) fn ensure_lane_root(
    conn: &Connection,
    data_dir: &std::path::Path,
    task: &Task,
) -> std::path::PathBuf {
    let canonical = workspace::dir_for(data_dir, &task.workspace_id);
    let Some(request) = request_of(conn, task) else {
        return canonical;
    };
    if !matches!(
        request.status,
        RequestStatus::Executing | RequestStatus::Validating
    ) {
        return canonical;
    }
    let Some(branch) = &request.branch else {
        return canonical;
    };
    let tree = gitops::tree_root(data_dir, &request.workspace_id, &request.id);
    for err in gitops::ensure_request_tree(&canonical, &tree, branch) {
        log_line(format!("gitops: compose {}: {err}", request.id));
    }
    if gitops::is_repo(&tree) {
        tree
    } else {
        log_line(format!(
            "gitops: tree for {} unusable — falling back to the canonical workspace",
            request.id
        ));
        canonical
    }
}

/// A cheap, always-works line into the request's origin chat (not an LLM turn).
pub(super) fn ping(conn: &Connection, request: &Request, body: &str) {
    if let Some(chat) = &request.origin_chat {
        let _ = db::chat::append_message(conn, chat, MessageRole::Agent, body);
    }
}

pub(super) fn repo_name(rel: &str) -> &str {
    if rel.is_empty() { "workspace" } else { rel }
}

/// `req/<short-id>-<slug>` — stable, readable, unique enough (the short id disambiguates).
fn branch_name(request: &Request) -> String {
    let short: String = request.id.chars().take(8).collect();
    let slug = slug(&request.title);
    if slug.is_empty() {
        format!("req/{short}")
    } else {
        format!("req/{short}-{slug}")
    }
}

/// A kebab slug of the title, capped: lowercase alphanumerics, runs of anything else
/// collapse to one `-`.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if out.len() >= 24 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_are_short_slugged_and_safe() {
        let request = Request {
            id: "abcdef1234567890".to_owned(),
            workspace_id: "ws".to_owned(),
            origin_chat: None,
            title: "Add a difficulty setting!! (v2)".to_owned(),
            ask: String::new(),
            status: RequestStatus::Planning,
            branch: None,
            validation: None,
        };
        assert_eq!(
            branch_name(&request),
            "req/abcdef12-add-a-difficulty-setting"
        );
        let untitled = Request {
            title: "→→→".to_owned(),
            ..request
        };
        assert_eq!(branch_name(&untitled), "req/abcdef12");
    }
}
