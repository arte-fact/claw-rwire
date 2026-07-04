//! Request projections (G2): the request page detail and the board's Requests rail —
//! read-models over `requests`/`request_repos`/`tasks` plus live git diffs.

use rusqlite::Connection;
use rwire::{ChangeSet, SharedServerState};

use crate::db::{
    self,
    requests::{Request, RequestStatus},
};
use crate::gitops;
use crate::state::{App, RequestCardView, RequestDetail, RequestRepoEntry, RequestTaskEntry};

use super::super::data_dir;

/// Cap on the request page's global patch — the page shows the shape; whole files live in git.
const PATCH_CAP_CHARS: usize = 12_000;
/// Cap on one pass's Diff disclosure in the Peek.
const PASS_PATCH_CAP_CHARS: usize = 6_000;

/// One pass's diff for the Peek's Diff disclosure: `git show` per stored commit, joined
/// with repo headers, capped. Empty when the run committed nothing.
pub(crate) fn pass_diff(root: &std::path::Path, commits_json: Option<&str>) -> String {
    let commits = super::parse_commits(commits_json);
    let mut parts: Vec<String> = Vec::new();
    for (repo, sha) in commits {
        let Ok(dir) = gitops::repo_dir(root, &repo) else {
            continue;
        };
        if let Ok(patch) = gitops::show_patch(&dir, &sha) {
            parts.push(if repo.is_empty() {
                patch
            } else {
                format!("### {repo}\n{patch}")
            });
        }
    }
    let joined = parts.join("\n\n");
    if joined.chars().count() > PASS_PATCH_CAP_CHARS {
        let head: String = joined.chars().take(PASS_PATCH_CAP_CHARS).collect();
        format!("{head}\n…[patch truncated]")
    } else {
        joined
    }
}

/// Project the request page (`Command::LoadRequestDetail`).
pub(crate) fn load_detail(conn: &Connection, shared: &SharedServerState, request_id: &str) {
    let detail = build_detail(conn, request_id);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_REQUEST_DETAIL]),
        move |app| app.request_detail = detail,
    );
}

fn build_detail(conn: &Connection, request_id: &str) -> Option<RequestDetail> {
    let request = db::requests::get(conn, request_id).ok().flatten()?;
    // Worktrees share refs and objects, so the canonical workspace answers for composed
    // repos either way — but a repo *born inside* the request's tree exists only there
    // until delivery, so an open request reads through its tree (G4).
    let root = super::request_root(&data_dir(), &request);
    let tasks = db::tasks::list_for_request(conn, request_id)
        .unwrap_or_default()
        .into_iter()
        .map(|task| RequestTaskEntry {
            stat: task_stat(conn, &root, &task.id),
            id: task.id,
            title: task.title,
            status: task.status.as_str().to_owned(),
            is_plan: task.is_plan,
        })
        .collect();
    let repos = db::requests::repos_for(conn, &request.id).unwrap_or_default();
    let (diff_stat, diff_patch) = global_diff(&root, &repos);
    let repos = repos
        .into_iter()
        .map(|repo| RequestRepoEntry {
            repo: repo_label(&repo.repo),
            base: short(&repo.base_commit),
            merge: repo.merge_commit.as_deref().map(short).unwrap_or_default(),
        })
        .collect();
    let has_remote = db::requests::repos_for(conn, &request.id)
        .unwrap_or_default()
        .iter()
        .any(|repo| {
            gitops::repo_dir(&root, &repo.repo)
                .map(|dir| gitops::has_remote(&dir))
                .unwrap_or(false)
        });
    Some(RequestDetail {
        id: request.id,
        workspace_id: request.workspace_id,
        title: request.title,
        ask: request.ask,
        status: request.status.as_str().to_owned(),
        branch: request.branch.unwrap_or_default(),
        tasks,
        repos,
        diff_stat,
        diff_patch,
        validation: request.validation.unwrap_or_default(),
        has_remote,
    })
}

/// The board rail's cards: every open request of the workspace, oldest first.
pub(crate) fn rail_views(conn: &Connection, workspace_id: &str) -> Vec<RequestCardView> {
    let active_ids: Vec<String> =
        db::requests::active_requests(conn, workspace_id, db::requests::MAX_ACTIVE)
            .unwrap_or_default()
            .into_iter()
            .map(|request| request.id)
            .collect();
    db::requests::open_for_workspace(conn, workspace_id)
        .unwrap_or_default()
        .into_iter()
        .map(|request| card_view(conn, &request, &active_ids))
        .collect()
}

fn card_view(conn: &Connection, request: &Request, active_ids: &[String]) -> RequestCardView {
    let root = super::request_root(&data_dir(), request);
    let root = root.as_path();
    let is_active = active_ids.contains(&request.id);
    let status = match request.status {
        RequestStatus::Executing if is_active => "executing".to_owned(),
        RequestStatus::Executing => "queued".to_owned(),
        other => other.as_str().to_owned(),
    };
    let (done, total) = if request.status == RequestStatus::Planning {
        (0, 0)
    } else {
        let total = db::tasks::count_for_request(conn, &request.id).unwrap_or(0);
        let open = db::tasks::open_for_request(conn, &request.id).unwrap_or(0);
        ((total - open).max(0), total)
    };
    let progress = if total > 0 {
        format!("{done}/{total} task(s) done")
    } else {
        String::new()
    };
    let repos = db::requests::repos_for(conn, &request.id).unwrap_or_default();
    RequestCardView {
        id: request.id.clone(),
        title: request.title.clone(),
        status,
        progress,
        stat: totals_line(root, &repos),
        done,
        total,
        branch: request.branch.clone().unwrap_or_default(),
    }
}

/// The request's diff totals so far — base..(merge | branch tip) summed across repos.
fn totals_line(root: &std::path::Path, repos: &[db::requests::RequestRepo]) -> String {
    let (mut files, mut adds, mut dels) = (0, 0, 0);
    for repo in repos {
        let Ok(dir) = gitops::repo_dir(root, &repo.repo) else {
            continue;
        };
        let tip = repo.merge_commit.clone().unwrap_or(repo.branch.clone());
        if let Ok((f, a, d)) = gitops::diff_numstat(&dir, &format!("{}..{tip}", repo.base_commit)) {
            files += f;
            adds += a;
            dels += d;
        }
    }
    if files == 0 {
        String::new()
    } else {
        format!("+{adds}/\u{2212}{dels} across {files} file(s)")
    }
}

/// The global diff: per-repo stat blocks joined, and one capped patch (repo headers between).
fn global_diff(root: &std::path::Path, repos: &[db::requests::RequestRepo]) -> (String, String) {
    let mut stats: Vec<String> = Vec::new();
    let mut patches: Vec<String> = Vec::new();
    for repo in repos {
        let Ok(dir) = gitops::repo_dir(root, &repo.repo) else {
            continue;
        };
        let tip = repo.merge_commit.clone().unwrap_or(repo.branch.clone());
        let range = format!("{}..{tip}", repo.base_commit);
        if let Ok(stat) = gitops::diff_range(&dir, &range, true)
            && !stat.is_empty()
        {
            stats.push(format!("{}:\n{stat}", repo_label(&repo.repo)));
        }
        if let Ok(patch) = gitops::diff_range(&dir, &range, false)
            && !patch.is_empty()
        {
            patches.push(format!("### {}\n{patch}", repo_label(&repo.repo)));
        }
    }
    let patch = patches.join("\n\n");
    let capped = if patch.chars().count() > PATCH_CAP_CHARS {
        let head: String = patch.chars().take(PATCH_CAP_CHARS).collect();
        format!("{head}\n…[patch truncated — the full diff lives in git]")
    } else {
        patch
    };
    (stats.join("\n\n"), capped)
}

/// Sum of a task's pass-commit numstats, as "+A/−D" — its activity, shown on the timeline.
fn task_stat(conn: &Connection, root: &std::path::Path, task_id: &str) -> String {
    let runs = db::task_runs::list_for_task(conn, task_id).unwrap_or_default();
    let (mut adds, mut dels) = (0, 0);
    for run in runs {
        for (repo, sha) in super::parse_commits(run.commits.as_deref()) {
            let Ok(dir) = gitops::repo_dir(root, &repo) else {
                continue;
            };
            if let Ok((_, a, d)) = gitops::show_numstat(&dir, &sha) {
                adds += a;
                dels += d;
            }
        }
    }
    if adds == 0 && dels == 0 {
        String::new()
    } else {
        format!("+{adds}/\u{2212}{dels}")
    }
}

fn repo_label(rel: &str) -> String {
    if rel.is_empty() {
        "workspace".to_owned()
    } else {
        rel.to_owned()
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}
