//! Request projections (G2): the request page detail and the board's Requests rail —
//! read-models over `requests`/`request_repos`/`tasks` plus live git diffs.

use std::path::Path;

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

/// The workspace's project cards for the Work tab's Projects strip (P1): every `projects/*`
/// repo `gitops::repos` lists, with its derived kind and open-request count. Derived on read.
pub(crate) fn project_cards(
    conn: &Connection,
    workspace: &str,
) -> Vec<crate::state::ProjectCardView> {
    let root = crate::workspace::dir_for(&data_dir(), workspace);
    gitops::repos(&root)
        .into_iter()
        .filter(|repo| !repo.is_empty())
        .map(|repo| {
            let name = repo.trim_start_matches("projects/").to_owned();
            let dir = root.join("projects").join(&name);
            let kind = crate::kinds::kind_for(conn, workspace, &name, &dir);
            let open_requests = db::requests::requests_touching(conn, &repo)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| {
                    matches!(
                        r.status,
                        RequestStatus::Planning
                            | RequestStatus::Executing
                            | RequestStatus::Validating
                    )
                })
                .count();
            crate::state::ProjectCardView {
                name,
                kind_slug: kind.slug().to_owned(),
                open_requests,
            }
        })
        .collect()
}

/// Project the project page (`Command::LoadProjectDetail`, P1): the derived kind + the
/// requests that touched the project — no project table, computed on read.
pub(crate) fn load_project_detail(
    conn: &Connection,
    shared: &SharedServerState,
    workspace: &str,
    project: &str,
    branch: Option<&str>,
) {
    let dir = crate::workspace::dir_for(&data_dir(), workspace)
        .join("projects")
        .join(project);
    let kind = crate::kinds::kind_for(conn, workspace, project, &dir);
    let overridden = db::settings::get(conn, &crate::kinds::kind_key(workspace, project))
        .ok()
        .flatten()
        .is_some_and(|slug| crate::kinds::ProjectKind::from_slug(&slug).is_some());
    let requests = db::requests::requests_touching(conn, &format!("projects/{project}"))
        .unwrap_or_default()
        .into_iter()
        .map(|r| crate::state::ProjectRequestEntry {
            id: r.id,
            title: r.title,
            status: r.status.as_str().to_owned(),
        })
        .collect();
    let branches = project_branches(&dir);
    let selected = choose_branch(&branches, branch);
    let tree = project_tree(&dir, &selected);
    let pipeline = branch_pipeline(conn, workspace, &format!("projects/{project}"), &selected);
    let detail = crate::state::ProjectDetail {
        workspace_id: workspace.to_owned(),
        name: project.to_owned(),
        kind_slug: kind.slug().to_owned(),
        kind_overridden: overridden,
        requests,
        branches,
        selected_branch: selected,
        tree,
        pipeline,
    };
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_PROJECT_DETAIL]),
        move |app| app.project_detail = Some(detail),
    );
}

/// A project repo's local branches with `develop` pinned first (the integration branch is the
/// default view); the rest keep git's recency order.
fn project_branches(dir: &Path) -> Vec<String> {
    let mut branches = gitops::local_branches(dir).unwrap_or_default();
    if let Some(pos) = branches.iter().position(|b| b == gitops::DEVELOP) {
        branches.remove(pos);
        branches.insert(0, gitops::DEVELOP.to_owned());
    }
    branches
}

/// Which branch to browse: the requested one if it exists, else `develop`, else the first branch,
/// else empty (a repo with no commits).
fn choose_branch(branches: &[String], requested: Option<&str>) -> String {
    requested
        .filter(|b| branches.iter().any(|known| known == b))
        .map(str::to_owned)
        .or_else(|| {
            branches
                .iter()
                .find(|b| b.as_str() == gitops::DEVELOP)
                .or_else(|| branches.first())
                .cloned()
        })
        .unwrap_or_default()
}

/// How many recent free-lane producer runs to scan for develop's pipeline.
const DEVELOP_SCAN_LIMIT: i64 = 25;
/// How many runs to surface on develop's pipeline (a short recent history).
const DEVELOP_SHOW: usize = 5;

/// The selected branch's CI as pipeline entries (P5c), newest first. A run's branch is ground
/// truth — the pass physically ran on it — so this derives the true per-branch dimension from the
/// recorded runs: `req/*` → the request's tasks' latest checked producer runs; `develop` → the
/// free lane's recent producer runs; `main`/other → empty (no producer pass lands there).
fn branch_pipeline(
    conn: &Connection,
    workspace: &str,
    repo: &str,
    branch: &str,
) -> Vec<crate::state::BranchPipelineRun> {
    if branch == gitops::DEVELOP {
        return db::task_runs::free_lane_producer_runs(conn, workspace, DEVELOP_SCAN_LIMIT)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|run| {
                pipeline_run(&run.run_id, &run.task_title, run.checks.as_deref(), repo)
            })
            .take(DEVELOP_SHOW)
            .collect();
    }
    let Some(request_id) = request_for_branch(conn, repo, branch) else {
        return Vec::new();
    };
    db::tasks::list_for_request(conn, &request_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|task| {
            let runs = db::task_runs::list_for_task(conn, &task.id).unwrap_or_default();
            let run = runs
                .iter()
                .rev()
                .find(|r| r.role == "producer" && r.checks.is_some())?;
            pipeline_run(&run.id, &task.title, run.checks.as_deref(), repo)
        })
        .collect()
}

/// The request whose lane branch is `branch` and that touched `repo` — the branch's owning request.
fn request_for_branch(conn: &Connection, repo: &str, branch: &str) -> Option<String> {
    db::requests::requests_touching(conn, repo)
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.branch.as_deref() == Some(branch))
        .map(|r| r.id)
}

/// One run's checks as a pipeline entry, filtered to `repo`; `None` when the run checked nothing
/// in this repo.
fn pipeline_run(
    run_id: &str,
    task_title: &str,
    checks_json: Option<&str>,
    repo: &str,
) -> Option<crate::state::BranchPipelineRun> {
    let checks: Vec<crate::state::CheckBadgeView> = crate::ci::outcomes_from_json(checks_json)
        .into_iter()
        .filter(|outcome| outcome.repo == repo)
        .map(crate::bridge::tasks::check_badge)
        .collect();
    (!checks.is_empty()).then(|| crate::state::BranchPipelineRun {
        run_id: run_id.to_owned(),
        task_title: task_title.to_owned(),
        checks,
    })
}

/// A branch's file tree as view entries (P5c), capped so a huge repo can't blow one frame: drop
/// anything deeper than `TREE_MAX_DEPTH`, then truncate to `TREE_MAX_ENTRIES`. Both cut from the
/// tail, so `FileTree`'s pre-order contiguity survives. An unreadable ref yields an empty tree.
fn project_tree(dir: &Path, reference: &str) -> Vec<crate::state::ProjectTreeEntry> {
    const TREE_MAX_DEPTH: usize = 6;
    const TREE_MAX_ENTRIES: usize = 1500;
    gitops::ref_tree(dir, reference)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.depth <= TREE_MAX_DEPTH)
        .take(TREE_MAX_ENTRIES)
        .map(|entry| crate::state::ProjectTreeEntry {
            rel: entry.rel,
            name: entry.name,
            is_dir: entry.is_dir,
            depth: entry.depth,
        })
        .collect()
}

/// Set (or clear) a project's kind override (P1), then re-project the page. An empty slug
/// clears the override — detection takes over again.
pub(crate) fn set_project_kind(
    conn: &Connection,
    shared: &SharedServerState,
    workspace: &str,
    project: &str,
    slug: &str,
) {
    let key = crate::kinds::kind_key(workspace, project);
    let _ = if slug.trim().is_empty() {
        db::settings::set(conn, &key, "")
    } else {
        db::settings::set(conn, &key, slug.trim())
    };
    // Re-project on the default branch — a kind change is a settings act, not a branch switch.
    load_project_detail(conn, shared, workspace, project, None);
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
    let (checks, checks_ok) = request_checks_summary(conn, request_id);
    // The branch's live preview, if one is running (P2c): a same-origin link the operator clicks to
    // see the running branch before Deliver. Empty when nothing is up on its pooled port.
    let preview_key = crate::bridge::preview::request_key(&request.workspace_id, request_id);
    let preview_uri = match crate::bridge::preview::port_for(&preview_key) {
        Some(_) => format!("{}/", crate::bridge::preview::uri_for(&preview_key)),
        None => String::new(),
    };
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
        checks,
        checks_ok,
        preview_uri,
        has_remote,
        asked_at: stamp(&request.created_at),
        updated_at: stamp(&request.updated_at),
    })
}

/// The request's latest machine-check outcomes (P1b): each task's most recent checked producer
/// pass, unioned — the CI verdict the macro gate and the request page read.
pub(crate) fn request_checks(conn: &Connection, request_id: &str) -> Vec<crate::ci::CheckOutcome> {
    let mut outcomes = Vec::new();
    for task in db::tasks::list_for_request(conn, request_id).unwrap_or_default() {
        let runs = db::task_runs::list_for_task(conn, &task.id).unwrap_or_default();
        if let Some(latest) = runs
            .iter()
            .rev()
            .find(|run| run.role == "producer" && run.checks.is_some())
        {
            outcomes.extend(crate::ci::outcomes_from_json(latest.checks.as_deref()));
        }
    }
    outcomes
}

/// The request's CI summary line + gate signal (P1b): `("", None)` when nothing was checked,
/// `("✓ checks green", Some(true))` when all-green, `("✗ clippy, test", Some(false))` on red.
fn request_checks_summary(conn: &Connection, request_id: &str) -> (String, Option<bool>) {
    let outcomes = request_checks(conn, request_id);
    if outcomes.is_empty() {
        return (String::new(), None);
    }
    if crate::ci::all_green(&outcomes) {
        (format!("✓ {} checks green", outcomes.len()), Some(true))
    } else {
        (
            format!("✗ {}", crate::ci::failed_names(&outcomes).join(", ")),
            Some(false),
        )
    }
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
    let preview_live = crate::bridge::preview::port_for(&crate::bridge::preview::request_key(
        &request.workspace_id,
        &request.id,
    ))
    .is_some();
    RequestCardView {
        id: request.id.clone(),
        title: request.title.clone(),
        status,
        progress,
        stat: totals_line(root, &repos),
        done,
        total,
        branch: request.branch.clone().unwrap_or_default(),
        preview_live,
    }
}

/// `2026-07-04T09:11:22.123Z` → `07-04 09:11` for the history panel.
fn stamp(iso: &str) -> String {
    let date = iso.get(5..10).unwrap_or("");
    let time = iso.get(11..16).unwrap_or("");
    format!("{date} {time}").trim().to_owned()
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

#[cfg(test)]
mod tests {
    use super::{branch_pipeline, choose_branch};
    use crate::db::{open_in_memory, task_runs, tasks, workspaces};

    fn branches(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn choose_branch_honors_a_valid_request_else_defaults_to_develop() {
        let b = branches(&["develop", "main", "req/aa-x"]);
        // A valid requested branch wins.
        assert_eq!(choose_branch(&b, Some("req/aa-x")), "req/aa-x");
        // An unknown request falls back to develop.
        assert_eq!(choose_branch(&b, Some("gone")), "develop");
        // No request → develop.
        assert_eq!(choose_branch(&b, None), "develop");
        // No develop → the first branch.
        assert_eq!(choose_branch(&branches(&["main", "wip"]), None), "main");
        // No branches at all → empty.
        assert_eq!(choose_branch(&[], None), "");
    }

    /// Seed a free-lane task (no request → ran on develop) with one producer run + checks JSON.
    fn seed_free_lane_run(
        conn: &rusqlite::Connection,
        task: &str,
        run: &str,
        title: &str,
        checks: &str,
    ) {
        tasks::create(
            conn,
            &tasks::NewTask {
                id: task,
                workspace_id: "ws",
                agent: None,
                title,
                spec: "do",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("task");
        task_runs::append(
            conn,
            &task_runs::NewRun {
                id: run,
                task_id: task,
                position: 0,
                role: "producer",
                agent: None,
                output: None,
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt: 0,
                memory: None,
                commits: None,
                checks: Some(checks),
            },
        )
        .expect("run");
    }

    #[test]
    fn branch_pipeline_surfaces_free_lane_producer_checks_on_develop() {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        seed_free_lane_run(
            &conn,
            "t1",
            "r1",
            "Add feature",
            r#"[{"repo":"projects/site","check":"clippy","ok":true,"tail":"","duration_ms":1500}]"#,
        );

        let pipeline = branch_pipeline(&conn, "ws", "projects/site", "develop");
        assert_eq!(pipeline.len(), 1);
        assert_eq!(pipeline[0].run_id, "r1");
        assert_eq!(pipeline[0].task_title, "Add feature");
        assert_eq!(pipeline[0].checks.len(), 1);
        assert_eq!(pipeline[0].checks[0].check, "clippy");
        // duration_ms parses (serde) and formats.
        assert_eq!(pipeline[0].checks[0].duration, "1.5s");
    }

    #[test]
    fn branch_pipeline_is_empty_off_develop_and_for_other_repos() {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        seed_free_lane_run(
            &conn,
            "t1",
            "r1",
            "t",
            r#"[{"repo":"projects/site","check":"test","ok":true,"tail":""}]"#,
        );
        // No producer pass lands on main.
        assert!(branch_pipeline(&conn, "ws", "projects/site", "main").is_empty());
        // The run checked projects/site, so another repo's develop pipeline is empty.
        assert!(branch_pipeline(&conn, "ws", "projects/other", "develop").is_empty());
    }
}
