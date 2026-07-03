//! Task board commands, run on the bridge thread (it owns the DB). Create / cancel a task
//! and project a workspace's lane into `App.tasks`. The scheduler (`super::scheduler`) drains
//! the queued tasks; this module is the manual create/cancel + the lane projection.

use rwire::{ChangeSet, SharedServerState};

use crate::db::tasks::{self, NewTask, TaskStatus};
use crate::db::{agents, workspaces};
use crate::state::{App, LaneSummary, TaskView};

/// A workspace's lane as view-models (assignee ids resolved to names), for projection.
pub fn lane_views(conn: &rusqlite::Connection, workspace: &str) -> Vec<TaskView> {
    let names = agent_names(conn);
    tasks::list_for_workspace(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|task| TaskView {
            id: task.id.clone(),
            title: task.title,
            spec: task.spec,
            agent: task
                .agent
                .as_ref()
                .and_then(|id| names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone())),
            agent_id: task.agent,
            status: task.status.as_str().to_owned(),
            recurrent: matches!(task.kind, tasks::TaskKind::Recurrent),
            schedule: task.schedule,
            next_run: task.next_run.as_deref().map(short_time),
            result: task.result.map(|text| snippet(&text)),
            blockers: tasks::blockers_of(conn, &task.id).unwrap_or_default(),
            step_position: task.step_position,
            iteration: task.iteration,
            auto_retries: task.auto_retries,
            max_retries: task.max_retries,
            attempt: task.attempt,
            // A pending task holding on a future `next_run` after a failure is awaiting a retry.
            next_retry: if task.status == TaskStatus::Pending && task.auto_retries > 0 {
                task.next_run.as_deref().map(short_time)
            } else {
                None
            },
            request_id: task.request_id,
        })
        .collect()
}

/// The workspace's pipeline step names — the kanban column headers (the default single Execute
/// stage when none configured).
fn columns_for(conn: &rusqlite::Connection, workspace: &str) -> Vec<String> {
    crate::db::pipeline::steps_for(conn, workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|step| step.name)
        .collect()
}

/// Parse a run's persisted transcript JSON into tool cards for the detail view.
fn parse_transcript(json: Option<&str>) -> Vec<crate::state::ToolCardView> {
    let Some(json) = json else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            let string = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            crate::state::ToolCardView {
                name: string("name"),
                arguments: string("arguments"),
                result: string("result"),
                ok: value
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                time: String::new(),
            }
        })
        .collect()
}

/// Map one persisted run to its view-model — agent id resolved to a name, transcript parsed into
/// tool cards, for the Peek modal's run chain.
pub(super) fn run_view(
    names: &[(String, String)],
    run: crate::db::task_runs::TaskRun,
) -> crate::state::RunView {
    crate::state::RunView {
        agent: display_name(names, run.agent.as_deref()),
        role: run.role,
        output: run.output.unwrap_or_default(),
        verdict: run.verdict,
        feedback: run.feedback,
        iteration: run.iteration,
        ok: run.ok,
        tools: parse_transcript(run.transcript.as_deref()),
        attempt: run.attempt,
        time: super::hh_mm(&run.created_at),
        memory: run.memory.unwrap_or_default(),
        diff: String::new(),
        diff_files: Vec::new(),
    }
}

/// A plan's proposed tasks as view-models for the Peek modal's Review tab (R3, Option A) — read
/// straight from the plan's JSON proposal, before any task rows exist. `agent` is the member's name.
pub(super) fn proposed_tasks(
    conn: &rusqlite::Connection,
    plan_id: &str,
) -> Vec<crate::state::ProposedTask> {
    tasks::proposal::get(conn, plan_id)
        .unwrap_or_default()
        .into_iter()
        .map(|item| crate::state::ProposedTask {
            title: item.title,
            spec: item.spec,
            agent: item.agent,
            schedule: item.schedule,
        })
        .collect()
}

/// Build one task's detail (spec + the full run chain, and — for a plan gate — its proposed
/// children so the Peek modal's Review tab can show them). `None` if the task is gone.
fn build_detail(conn: &rusqlite::Connection, task_id: &str) -> Option<crate::state::TaskDetail> {
    let names = agent_names(conn);
    tasks::get(conn, task_id).ok().flatten().map(|task| {
        // Root the reads at the task's lane (G4): a repo born inside a request's tree — and
        // its commits — exists only there until delivery moves everything into the canonical
        // workspace (where the fallback then finds it).
        let root = super::requests::task_root(conn, &super::data_dir(), &task);
        let runs = crate::db::task_runs::list_for_task(conn, task_id)
            .unwrap_or_default()
            .into_iter()
            .map(|run| {
                // The pass's Diff disclosure (G2): `git show` over the run's stored commits,
                // plus the touched files as restore handles (G3).
                let diff = super::requests::pass_diff(&root, run.commits.as_deref());
                let files = restorable_files(&root, &task, run.commits.as_deref());
                let mut view = run_view(&names, run);
                view.diff = diff;
                view.diff_files = files;
                view
            })
            .collect();
        let proposed = if task.is_plan {
            proposed_tasks(conn, &task.id)
        } else {
            Vec::new()
        };
        crate::state::TaskDetail {
            id: task.id,
            workspace_id: task.workspace_id,
            title: task.title,
            spec: task.spec,
            awaiting: task.status == tasks::TaskStatus::Awaiting,
            archived: task.archived_at.is_some(),
            is_plan: task.is_plan,
            proposed,
            status: task.status.as_str().to_owned(),
            runs,
        }
    })
}

/// Project one task's detail into `App.task_detail` and open its Peek modal (`selected_task`) —
/// the detail opening, whether from a board card click or a deep link to `/w/:wid/tasks/:tid`.
pub fn load_detail(conn: &rusqlite::Connection, shared: &SharedServerState, task_id: &str) {
    let detail = build_detail(conn, task_id);
    let selected = task_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_TASK_DETAIL, App::FIELD_SELECTED_TASK]),
        move |app| {
            app.task_detail = detail;
            app.selected_task = Some(selected);
        },
    );
}

/// Re-project the detail **only if it's the one currently open** — so a task finishing /
/// advancing updates the open detail live without hijacking a different task's view.
pub fn refresh_detail_if_open(
    conn: &rusqlite::Connection,
    shared: &SharedServerState,
    task_id: &str,
) {
    let detail = build_detail(conn, task_id);
    let task_id = task_id.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_TASK_DETAIL]),
        move |app| {
            if app.task_detail.as_ref().map(|d| d.id.as_str()) == Some(task_id.as_str()) {
                app.task_detail = detail;
            }
        },
    );
    // A task lifecycle change may also belong to the agent whose thread is open — keep it live.
    super::agent_thread::refresh_if_open(conn, shared);
}

/// The files a run's pass commits touched, each addressable for a pre-pass restore (G3).
fn restorable_files(
    root: &std::path::Path,
    task: &tasks::Task,
    commits_json: Option<&str>,
) -> Vec<crate::state::RestorableFile> {
    let mut out = Vec::new();
    for (repo, sha) in super::requests::parse_commits(commits_json) {
        let Ok(dir) = crate::gitops::repo_dir(root, &repo) else {
            continue;
        };
        for path in crate::gitops::show_files(&dir, &sha).unwrap_or_default() {
            out.push(crate::state::RestorableFile {
                workspace: task.workspace_id.clone(),
                task: task.id.clone(),
                repo: repo.clone(),
                sha: sha.clone(),
                path,
            });
        }
    }
    out
}

/// `2026-06-22T08:00:00.000Z` → `06-22 08:00` (drop seconds/millis for the card).
fn short_time(ts: &str) -> String {
    let date = ts.get(5..10).unwrap_or("");
    let time = ts.get(11..16).unwrap_or("");
    format!("{date} {time}").trim().to_owned()
}

/// First line of a result, capped, for the card's last-run snippet.
fn snippet(text: &str) -> String {
    crate::text::first_line_capped(text, 120)
}

/// Project a workspace's lane into `App.tasks` (unconditionally — the caller is acting on
/// the active workspace). Background updates use [`project_if_active`].
pub fn load(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = lane_views(conn, workspace);
    let columns = columns_for(conn, workspace);
    let requests = super::requests::rail_views(conn, workspace);
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_TASKS,
            App::FIELD_PIPELINE_COLUMNS,
            App::FIELD_REQUESTS,
        ]),
        move |app| {
            app.tasks = views;
            app.pipeline_columns = columns;
            app.requests = requests;
        },
    );
}

/// Re-project a lane into `App.tasks`, but only when the operator is actually viewing that
/// workspace — a background lane (a scheduler run, a planning turn) must not clobber the
/// lane on screen.
pub fn project_if_active(conn: &rusqlite::Connection, shared: &SharedServerState, workspace: &str) {
    let views = lane_views(conn, workspace);
    let columns = columns_for(conn, workspace);
    let requests = super::requests::rail_views(conn, workspace);
    // Keep the Workflow editor's lock live: a task going running/awaiting (or finishing) must
    // flip the lock so the editor's controls reflect reality rather than going stale + dead.
    let locked = tasks::has_inflight(conn, workspace).unwrap_or(false);
    let workspace = workspace.to_owned();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[
            App::FIELD_TASKS,
            App::FIELD_PIPELINE_COLUMNS,
            App::FIELD_WORKFLOW_LOCKED,
            App::FIELD_REQUESTS,
        ]),
        move |app| {
            if app.active_workspace.as_deref() == Some(workspace.as_str()) {
                app.tasks = views;
                app.pipeline_columns = columns;
                app.workflow_locked = locked;
                app.requests = requests;
            }
        },
    );
}

/// Project every workspace's lane summary (running task + queued count) into
/// `App.dashboard_lanes` for the global dashboard's all-lanes view. Refreshed on each
/// scheduler tick and when the dashboard opens, so it stays live regardless of route.
pub fn project_dashboard(conn: &rusqlite::Connection, shared: &SharedServerState) {
    let lanes: Vec<LaneSummary> = workspaces::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|ws| {
            let running = tasks::running_in(conn, &ws.id)
                .ok()
                .flatten()
                .map(|task| task.title);
            let lane = tasks::list_for_workspace(conn, &ws.id).unwrap_or_default();
            let queued = lane
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .count();
            let awaiting = lane
                .iter()
                .filter(|task| task.status == TaskStatus::Awaiting)
                .count();
            LaneSummary {
                workspace_id: ws.id,
                running,
                queued,
                awaiting,
            }
        })
        .collect();
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_DASHBOARD_LANES]),
        move |app| app.dashboard_lanes = lanes,
    );
}

/// The create form's fields — the task the user is queuing from the New-task panel.
pub struct Create {
    pub workspace: String,
    pub title: String,
    pub spec: String,
    pub agent: Option<String>,
    pub schedule: Option<String>,
    pub max_retries: Option<i64>,
}

/// Create a task in its workspace, assigned to `agent` (a member; `None` = the owner), then
/// re-project the lane.
pub fn create(conn: &rusqlite::Connection, shared: &SharedServerState, new: Create) {
    let title = new.title.trim();
    let spec = new.spec.trim();
    if title.is_empty() || spec.is_empty() {
        return surface(shared, "a task needs a title and a spec".to_owned());
    }
    // A negative retry budget is meaningless — treat it as "use the global default".
    let max_retries = new.max_retries.filter(|n| *n >= 0);
    // A schedule, if given, must be a valid cron expression (else it would never fire).
    let schedule = new
        .schedule
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(cron) = &schedule
        && crate::schedule::Cron::parse(cron).is_none()
    {
        return surface(shared, format!("not a valid cron schedule: {cron:?}"));
    }
    let Ok(id) = crate::id::new_id() else {
        return surface(shared, "could not mint a task id".to_owned());
    };
    // A named assignee must be a member of the workspace; `None` stores NULL and defaults to
    // the owner at run time.
    let agent = new.agent.filter(|value| !value.trim().is_empty());
    if let Some(a) = &agent {
        let members = workspaces::members_of(conn, &new.workspace).unwrap_or_default();
        if !members.iter().any(|m| m == a) {
            return surface(
                shared,
                "the assignee must be a member of the workspace".into(),
            );
        }
    }
    if let Err(err) = tasks::create(
        conn,
        &NewTask {
            id: &id,
            workspace_id: &new.workspace,
            agent: agent.as_deref(),
            title,
            spec,
            schedule: schedule.as_deref(),
            origin_chat: None,
            max_retries,
        },
    ) {
        return surface(shared, format!("create task: {err}"));
    }
    load(conn, shared, &new.workspace);
}

/// An edit-drawer change to a task — the fields the user can rewrite from the board.
pub struct Edit {
    pub title: String,
    pub spec: String,
    pub agent: Option<String>,
    pub schedule: Option<String>,
    pub blockers: Vec<String>,
    /// Per-task auto-retry budget (`None` = use the global default).
    pub max_retries: Option<i64>,
}

/// Apply an [`Edit`] to a task, validating the assignee is a member, the schedule is valid
/// cron, and each blocker is a distinct task in the same workspace. Re-projects on success.
pub fn update(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str, edit: Edit) {
    let title = edit.title.trim();
    let spec = edit.spec.trim();
    if title.is_empty() || spec.is_empty() {
        return surface(shared, "a task needs a title and a spec".to_owned());
    }
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    // Editing a running task is a no-op on the live run (its messages were built at dispatch) —
    // refuse, so the user stops it first rather than silently editing nothing.
    if task.status == TaskStatus::Running {
        return surface(shared, "stop the task before editing it".to_owned());
    }
    let schedule = edit
        .schedule
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(cron) = &schedule
        && crate::schedule::Cron::parse(cron).is_none()
    {
        return surface(shared, format!("not a valid cron schedule: {cron:?}"));
    }
    let agent = edit.agent.filter(|value| !value.trim().is_empty());
    let members = workspaces::members_of(conn, &task.workspace_id).unwrap_or_default();
    if let Some(a) = &agent
        && !members.iter().any(|m| m == a)
    {
        return surface(
            shared,
            "the assignee must be a member of the workspace".into(),
        );
    }
    // Blockers: distinct from self, and tasks in the same workspace.
    let mut valid = Vec::new();
    for blocker in edit.blockers {
        if blocker == id {
            continue;
        }
        match tasks::get(conn, &blocker).ok().flatten() {
            Some(b) if b.workspace_id == task.workspace_id => valid.push(blocker),
            _ => {
                return surface(
                    shared,
                    format!("blocker {blocker:?} is not in this workspace"),
                );
            }
        }
    }
    let max_retries = edit.max_retries.filter(|n| *n >= 0);
    if let Err(err) = tasks::update(
        conn,
        id,
        title,
        spec,
        agent.as_deref(),
        schedule.as_deref(),
        max_retries,
    ) {
        return surface(shared, format!("update task: {err}"));
    }
    if let Err(err) = tasks::set_blockers(conn, id, &valid) {
        return surface(shared, format!("set blockers: {err}"));
    }
    load(conn, shared, &task.workspace_id);
}

/// Cancel a task and re-project its workspace's lane.
pub fn cancel(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::cancel(conn, id) {
        return surface(shared, format!("cancel task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Stop (pause) a task: park it `stopped`, abandon any in-flight run, and clear its live feed.
pub fn stop(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::stop(conn, id) {
        return surface(shared, format!("stop task: {err}"));
    }
    super::clear_live_turn(shared, id);
    reproject(conn, shared, id, &task.workspace_id);
}

/// Resume a stopped task — re-queue it to continue from its current step.
pub fn resume(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::resume(conn, id) {
        return surface(shared, format!("start task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Restart a task from a clean slate (step 0, fresh attempt) — from a stopped or terminal task.
/// Fresh attempt = fresh files (G0 of docs/requests.md): every repo the task recorded a
/// pre-task base for is reset to it first, so the new attempt no longer inherits the
/// abandoned attempt's dirty tree. A partial reset is surfaced but doesn't block the restart.
pub fn restart(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    // Reset where the task ran (G4): its request's tree while the request is open, the
    // canonical workspace for free tasks.
    let root = super::requests::task_root(conn, &super::data_dir(), &task);
    let reset_errors = crate::gitops::reset_task_tree(conn, &root, id);
    if !reset_errors.is_empty() {
        surface(
            shared,
            format!(
                "restart: tree reset incomplete: {}",
                reset_errors.join("; ")
            ),
        );
    }
    if let Err(err) = tasks::restart(conn, id) {
        return surface(shared, format!("restart task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Archive a terminal task — drop it from the board (the run chain is preserved, restorable).
pub fn archive(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::archive(conn, id) {
        return surface(shared, format!("archive task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Restore an archived task to the board.
pub fn unarchive(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str) {
    let Ok(Some(task)) = tasks::get(conn, id) else {
        return;
    };
    if let Err(err) = tasks::unarchive(conn, id) {
        return surface(shared, format!("unarchive task: {err}"));
    }
    reproject(conn, shared, id, &task.workspace_id);
}

/// Re-project after a run-control change: the active lane, the open detail (if it's this task),
/// and the dashboard's all-lanes summary.
fn reproject(conn: &rusqlite::Connection, shared: &SharedServerState, id: &str, workspace: &str) {
    load(conn, shared, workspace);
    refresh_detail_if_open(conn, shared, id);
    project_dashboard(conn, shared);
}

/// The concierge's live status digest (R5): one line per workspace with anything in flight —
/// running task (and who runs it), gates awaiting the operator, and the queued count. Present
/// state is **looked up fresh each turn, never remembered** — injected as its own prompt layer
/// so the concierge stops guessing about in-flight work from stale conversation history.
/// `None` when every lane is quiet (no layer at all).
pub(crate) fn status_digest(conn: &rusqlite::Connection) -> Option<String> {
    let names = agent_names(conn);
    let name_of = |id: &Option<String>| {
        id.as_deref()
            .and_then(|id| names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone()))
            .unwrap_or_else(|| "the owner".to_owned())
    };
    let mut lines: Vec<String> = Vec::new();
    for ws in crate::db::workspaces::list(conn).unwrap_or_default() {
        let lane = crate::db::tasks::list_for_workspace(conn, &ws.id).unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        if let Some(task) = lane.iter().find(|t| t.status == TaskStatus::Running) {
            parts.push(format!(
                "running: \"{}\" ({})",
                task.title,
                name_of(&task.agent)
            ));
        }
        let awaiting: Vec<&crate::db::tasks::Task> = lane
            .iter()
            .filter(|t| t.status == TaskStatus::Awaiting)
            .collect();
        if !awaiting.is_empty() {
            let titles: Vec<String> = awaiting
                .iter()
                .map(|t| format!("\"{}\"", t.title))
                .collect();
            parts.push(format!("awaiting the user's review: {}", titles.join(", ")));
        }
        let queued = lane
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .count();
        if queued > 0 {
            parts.push(format!("queued: {queued}"));
        }
        // Open requests (G1): the ask-level view — the user tracks requests, not task
        // confetti. The oldest MAX_ACTIVE executing/validating requests each hold a lane
        // (G4, parallel worktrees); later executing ones wait FIFO behind them.
        let active_ids: Vec<String> =
            crate::db::requests::active_requests(conn, &ws.id, crate::db::requests::MAX_ACTIVE)
                .unwrap_or_default()
                .into_iter()
                .map(|request| request.id)
                .collect();
        for request in crate::db::requests::open_for_workspace(conn, &ws.id).unwrap_or_default() {
            let progress = match request.status {
                crate::db::requests::RequestStatus::Executing
                    if active_ids.contains(&request.id) =>
                {
                    let total = crate::db::tasks::count_for_request(conn, &request.id).unwrap_or(0);
                    let open = crate::db::tasks::open_for_request(conn, &request.id).unwrap_or(0);
                    format!("executing, {}/{total} task(s) done", (total - open).max(0))
                }
                crate::db::requests::RequestStatus::Executing => {
                    "queued behind the active request(s)".to_owned()
                }
                _ => "planning — awaiting the plan".to_owned(),
            };
            parts.push(format!("request \"{}\" ({progress})", request.title));
        }
        if !parts.is_empty() {
            lines.push(format!("- {} — {}", ws.name, parts.join("; ")));
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "[Live board state, looked up at this very moment — this overrides anything the \
         conversation history says about what is running or done:]\n{}",
        lines.join("\n")
    ))
}

/// An agent id resolved against the `agent_names` list — the display name every projection
/// shows, with "you" for a human run and the raw id when the agent is gone.
pub(super) fn display_name(names: &[(String, String)], agent: Option<&str>) -> String {
    agent.map_or_else(
        || "you".to_owned(),
        |id| {
            names
                .iter()
                .find(|(i, _)| i == id)
                .map_or_else(|| id.to_owned(), |(_, n)| n.clone())
        },
    )
}

pub(super) fn agent_names(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    agents::list(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

fn surface(shared: &SharedServerState, error: String) {
    super::log_line(format!("bridge tasks: {error}"));
    shared.update_shared_changed::<App>(ChangeSet::from_fields(&[App::FIELD_ERROR]), move |app| {
        app.error = Some(error)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::db::tasks::proposal::{self, ProposalItem};

    fn seeded() -> rusqlite::Connection {
        let conn = open_in_memory().expect("db");
        workspaces::create(&conn, "ws", "Project").expect("ws");
        conn
    }

    #[test]
    fn build_detail_carries_a_plans_proposed_children() {
        let conn = seeded();
        tasks::create_plan(&conn, "p1", "ws", "Plan", "the goal", None).expect("plan");
        proposal::append(
            &conn,
            "p1",
            &[ProposalItem {
                title: "First step".to_owned(),
                spec: "do the first thing".to_owned(),
                agent: Some("Scout".to_owned()),
                schedule: None,
                blockers: Vec::new(),
            }],
        )
        .expect("append");

        let detail = build_detail(&conn, "p1").expect("detail");
        assert!(detail.is_plan, "a plan gate is flagged as a plan");
        assert!(detail.awaiting, "a plan gate parks awaiting review");
        assert_eq!(detail.proposed.len(), 1);
        assert_eq!(detail.proposed[0].title, "First step");
        assert_eq!(detail.proposed[0].agent.as_deref(), Some("Scout"));
    }

    #[test]
    fn build_detail_stamps_each_run_with_its_hh_mm_time() {
        use crate::db::task_runs::{self, NewRun};
        let conn = seeded();
        tasks::create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "Task",
                spec: "do it",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");
        task_runs::append(
            &conn,
            &NewRun {
                id: "r1",
                task_id: "t1",
                position: 0,
                role: "producer",
                agent: None,
                output: Some("done"),
                verdict: None,
                feedback: None,
                iteration: 0,
                ok: true,
                transcript: None,
                attempt: 0,
                memory: None,
                commits: None,
            },
        )
        .expect("run");

        let detail = build_detail(&conn, "t1").expect("detail");
        let run = &detail.runs[0];
        // The thread stamp is the run's created_at time-of-day, `HH:MM`.
        let created = &task_runs::list_for_task(&conn, "t1").expect("runs")[0].created_at;
        assert_eq!(run.time, &created[11..16]);
        assert_eq!(run.time.len(), 5);
        assert_eq!(run.time.as_bytes()[2], b':');
    }

    #[test]
    fn build_detail_leaves_an_ordinary_task_without_proposed_tasks() {
        let conn = seeded();
        tasks::create(
            &conn,
            &NewTask {
                id: "t1",
                workspace_id: "ws",
                agent: None,
                title: "Task",
                spec: "do it",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("create");

        let detail = build_detail(&conn, "t1").expect("detail");
        assert!(!detail.is_plan);
        assert!(
            detail.proposed.is_empty(),
            "no proposed tasks for a non-plan"
        );
    }

    #[test]
    fn status_digest_reports_lanes_in_flight_and_none_when_quiet() {
        let conn = crate::db::open_in_memory().expect("db");
        agents::create(&conn, "ag-f", "Forge", None, None).expect("agent");
        workspaces::create(&conn, "studio2", "Studio2").expect("ws");
        workspaces::create(&conn, "research2", "Research2").expect("ws2");
        // All quiet → no layer at all.
        assert_eq!(status_digest(&conn), None);

        tasks::create(
            &conn,
            &NewTask {
                id: "sd1",
                workspace_id: "studio2",
                agent: Some("ag-f"),
                title: "Build the widget",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("sd1");
        tasks::set_status(&conn, "sd1", TaskStatus::Running).expect("running");
        tasks::create(
            &conn,
            &NewTask {
                id: "sd2",
                workspace_id: "studio2",
                agent: None,
                title: "Ship it",
                spec: "s",
                schedule: None,
                origin_chat: None,
                max_retries: None,
            },
        )
        .expect("sd2");
        tasks::set_status(&conn, "sd2", TaskStatus::Awaiting).expect("awaiting");

        let digest = status_digest(&conn).expect("digest");
        assert!(digest.starts_with("[Live board state"));
        assert!(digest.contains("Studio2 — running: \"Build the widget\" (Forge)"));
        assert!(digest.contains("awaiting the user's review: \"Ship it\""));
        assert!(!digest.contains("Research2"), "quiet lanes stay out");
    }
}
